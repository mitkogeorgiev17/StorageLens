//! The Tauri command surface (PLAN §14): every UI action is exactly one typed
//! call here, mirrored in `ui/src/lib/api.ts`.
//!
//! # Shape
//!
//! Each `#[tauri::command]` is a thin wrapper over a plain function taking
//! [`AppState`] (and, for scans, an [`Emit`]), so the logic is tested without a
//! running app. Errors cross the boundary as `String` — the message the UI shows.
//! Nothing here panics on input: every lookup, lock, and conversion is fallible.
//!
//! # Deletion
//!
//! Every removal — cleanup suggestions, apps, duplicate copies — is a dry-run
//! receipt from a `preview_*` command that `execute_deletion` then runs:
//! it refuses a plan id that no preview produced (AGENTS.md invariant 10).
//!
//! # Scans
//!
//! One scan runs at a time (the catalog has a single writer). It runs on its own
//! thread with its own SQLite connection; the command returns its id
//! immediately, progress arrives as `scan://progress`, and the end as
//! `scan://done` or `scan://error`. When it ends, the arena is loaded and
//! `dir_rollup` written before `scan://done` is sent, so the UI can query the
//! moment it hears it.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use storagelens_analysis::rules::{
    evaluate, fingerprint, risk_name, risk_weight, Context, Evaluation, Kind, RuleSet,
};
use storagelens_analysis::{FileFilter, Risk, SyntheticTree};
use storagelens_catalog::{
    file_order, modified_times, rollup, slice, Arena, Catalog, EntryWriter, FileSort, NodeIndex,
    Platform, ScanSession, SearchCursor, SizeCursor, DEFAULT_BATCH_SIZE, UNKNOWN_TIME,
};
use storagelens_core::{
    CancelToken, EntryKind, Locations, Progress, ScanOptions, ScanResult, ScanStatus,
};
use tauri::{AppHandle, State};

use crate::dto::{
    AppRowDto, CategoryDto, ChildDto, CleanupDto, CleanupProgressDto, CrumbDto, DeletionPlanDto,
    DeletionResultDto, DupFileDto, DupGroupDto, DupesDto, FileRangeDto, FileRowDto, FileSortDto,
    FilterDto, Grouping, LeftoverDto, OverviewDto, Page, PlanItemDto, QuitDto, RegistryRemovalDto,
    RemovedKeyDto, ResultItemDto, RuleGroupDto, RuleItemDto, SatelliteDto, ScanStatusDto,
    ScanTargetDto, ScanTargetKind, SearchCursorDto, SizeCursorDto, StartupItemDto,
    SystemRemovalDto, TreeNodeDto, UndoItemDto, UndoResultDto, VolumeDto,
};
use crate::events::{
    self, DupesProgressEvent, Emit, ScanDoneEvent, ScanErrorEvent, ScanProgressEvent, Throttle,
    PROGRESS_INTERVAL,
};

/// Rows per page for flat lists (AGENTS.md §8: no component holds more than
/// ~200 rows).
pub const PAGE_SIZE: u32 = 200;

/// Default and maximum children returned by `get_children`.
pub const DEFAULT_CHILDREN: u32 = 500;
pub const MAX_CHILDREN: u32 = 5_000;

/// `get_tree` limits: levels below the parent (default and maximum), children
/// per directory, and nodes in total. The budget is a few times the treemap's
/// 1,500 drawn rects, so zooming within one load still has detail to show.
pub const DEFAULT_TREE_DEPTH: u32 = 4;
pub const MAX_TREE_DEPTH: u32 = 8;
const TREE_PER_DIR: usize = 300;
const TREE_BUDGET: usize = 6_000;

/// Largest children shown on the Overview.
const OVERVIEW_BIGGEST: usize = 10;

type CommandResult<T> = Result<T, String>;

fn lock<T>(mutex: &Mutex<T>) -> CommandResult<MutexGuard<'_, T>> {
    mutex
        .lock()
        .map_err(|_| "internal state is unavailable after an earlier failure".to_owned())
}

fn now_unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

/// The Large Files order in use, and the modified times it may need. One
/// scan at a time, like the arena: ~4 MB of order plus ~8 MB of times per
/// million files.
struct Listing {
    scan_id: i64,
    times: Option<Arc<Vec<i64>>>,
    key: Option<(FileSortDto, u64)>,
    order: Arc<Vec<NodeIndex>>,
}

/// The grouping last built: rebuilding costs 10–60 ms per million files, so
/// zooming through one view reuses it.
struct GroupingCache {
    scan_id: i64,
    by: Grouping,
    filter: FilterDto,
    /// The rules evaluation a Cleanliness tree was built from.
    rules: Option<u64>,
    tree: Arc<SyntheticTree>,
}

/// The cleanup rules' verdicts on one scan (#31), kept until the scan or the
/// rules folder changes.
struct RulesCache {
    scan_id: i64,
    fingerprint: u64,
    rules: Arc<Rules>,
}

/// One evaluation of the rule catalog over one scan.
struct Rules {
    /// Changes whenever the rules are re-run.
    key: u64,
    set: RuleSet,
    evaluation: Evaluation,
}

/// A receipt `execute_deletion` may act on: what the user saw, and what the
/// Trash service needs for each line.
struct StoredPlan {
    dto: DeletionPlanDto,
    items: Vec<storagelens_safety::Item>,
}

/// The scan currently running.
struct RunningScan {
    scan_id: i64,
    root: PathBuf,
    cancel: CancelToken,
    progress: Arc<Mutex<Progress>>,
}

/// Everything the commands share. Managed by Tauri as `Arc<AppState>` so a scan
/// thread can hold it too.
pub struct AppState {
    catalog_path: PathBuf,
    /// Location categories for new scans (#95); `None` leaves extensions alone
    /// (tests, whose temp dirs sit in `/private/var/folders`).
    locations: Option<Locations>,
    catalog: Mutex<Catalog>,
    running: Mutex<Option<RunningScan>>,
    /// The most recently used arena.
    arena: Mutex<Option<Arc<Arena>>>,
    /// Final results of scans finished in this run, for `elevated` and skips.
    results: Mutex<HashMap<i64, ScanResult>>,
    /// Dry-run receipts, by plan id.
    plans: Mutex<HashMap<String, StoredPlan>>,
    /// Paths moved to the Trash in this run and not restored: hidden from
    /// the Cleanup screen until the next scan.
    removed: Mutex<std::collections::HashSet<String>>,
    /// Set by `cancel_deletion`; checked before each item.
    cancel_deletion: std::sync::atomic::AtomicBool,
    listing: Mutex<Option<Listing>>,
    grouping: Mutex<Option<GroupingCache>>,
    rules: Mutex<Option<RulesCache>>,
    /// The last app inventory, classified. Reading it takes seconds.
    apps: Mutex<Option<Vec<storagelens_core::AppInfo>>>,
    /// The last duplicate search: its scan, groups, and what it skipped.
    dupes: Mutex<Option<DupesCache>>,
    /// Cancels the running duplicate search.
    cancel_dupes: Mutex<CancelToken>,
    next_plan: AtomicU64,
}

/// A finished (or stopped) duplicate search.
#[derive(Clone)]
struct DupesCache {
    scan_id: i64,
    groups: Vec<storagelens_analysis::dupes::Group>,
    unreadable: u64,
    cancelled: bool,
}

impl AppState {
    /// Open (or create) the catalog at `catalog_path`.
    pub fn open(catalog_path: PathBuf) -> CommandResult<Self> {
        let catalog = Catalog::open(&catalog_path).map_err(|error| error.to_string())?;
        Ok(Self {
            catalog_path,
            locations: Some(Locations::new(home_folder().as_deref())),
            catalog: Mutex::new(catalog),
            running: Mutex::new(None),
            arena: Mutex::new(None),
            results: Mutex::new(HashMap::new()),
            plans: Mutex::new(HashMap::new()),
            removed: Mutex::new(std::collections::HashSet::new()),
            cancel_deletion: std::sync::atomic::AtomicBool::new(false),
            listing: Mutex::new(None),
            grouping: Mutex::new(None),
            rules: Mutex::new(None),
            apps: Mutex::new(None),
            dupes: Mutex::new(None),
            cancel_dupes: Mutex::new(CancelToken::new()),
            next_plan: AtomicU64::new(1),
        })
    }

    fn session(&self, scan_id: i64) -> CommandResult<ScanSession> {
        lock(&self.catalog)?
            .session(scan_id)
            .map_err(|error| error.to_string())
    }

    /// The arena of `scan_id`, loading and caching it if needed.
    fn arena(&self, scan_id: i64) -> CommandResult<Arc<Arena>> {
        if let Some(arena) = lock(&self.arena)?.as_ref() {
            if arena.scan_id() == scan_id {
                return Ok(Arc::clone(arena));
            }
        }
        if self.running_id()? == Some(scan_id) {
            return Err(format!("scan {scan_id} is still running"));
        }
        let arena = {
            let catalog = lock(&self.catalog)?;
            Arena::load(catalog.connection(), scan_id).map_err(|error| error.to_string())?
        };
        let arena = Arc::new(arena);
        *lock(&self.arena)? = Some(Arc::clone(&arena));
        Ok(arena)
    }

    fn running_id(&self) -> CommandResult<Option<i64>> {
        Ok(lock(&self.running)?.as_ref().map(|running| running.scan_id))
    }

    /// The scanned path, from the catalog. `None` only for sessions recorded
    /// before schema v2 (#61).
    fn root_of(&self, scan_id: i64) -> Option<PathBuf> {
        self.session(scan_id).ok()?.root
    }

    /// User and developer rule catalogs, read beside the catalog database;
    /// edited files are picked up on the next evaluation.
    fn rules_dir(&self) -> PathBuf {
        self.data_dir().join("rules")
    }

    /// Deletion manifests (`trash-<UTC>.json`), beside the catalog.
    fn manifests_dir(&self) -> PathBuf {
        self.data_dir().join("manifests")
    }

    fn data_dir(&self) -> &Path {
        self.catalog_path.parent().unwrap_or(Path::new("."))
    }
}

// --- Scans -------------------------------------------------------------------

#[cfg(target_os = "macos")]
fn platform() -> Option<Platform> {
    Some(Platform::Macos)
}

#[cfg(windows)]
fn platform() -> Option<Platform> {
    Some(Platform::Windows)
}

#[cfg(not(any(target_os = "macos", windows)))]
fn platform() -> Option<Platform> {
    None
}

#[cfg(target_os = "macos")]
fn elevated_now() -> bool {
    false
}

#[cfg(windows)]
fn elevated_now() -> bool {
    storagelens_scan_windows::elevation::is_elevated()
}

#[cfg(not(any(target_os = "macos", windows)))]
fn elevated_now() -> bool {
    false
}

fn run_platform_scanner(
    root: &Path,
    sink: &mut EntryWriter,
    cancel: CancelToken,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> storagelens_core::Result<ScanResult> {
    use storagelens_core::StorageScanner;
    let roots = [root.to_path_buf()];
    let options = ScanOptions::default();

    #[cfg(target_os = "macos")]
    {
        storagelens_scan_macos::MacScanner::new(options).scan(&roots, sink, cancel, progress)
    }
    #[cfg(windows)]
    {
        storagelens_scan_windows::WinScanner::new(options).scan(&roots, sink, cancel, progress)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (roots, options, sink, cancel, progress);
        Err(storagelens_core::ScanError::Unsupported("this platform"))
    }
}

/// Validate the roots, open a session, and start the scan thread.
pub fn start_scan(
    state: &Arc<AppState>,
    emitter: Arc<dyn Emit>,
    roots: Vec<String>,
) -> CommandResult<i64> {
    let root = match roots.as_slice() {
        [root] => PathBuf::from(root),
        [] => return Err("choose a folder or volume to scan".to_owned()),
        _ => return Err("scan one folder or volume at a time".to_owned()),
    };
    if root.as_os_str().is_empty() || !root.is_absolute() {
        return Err(format!("not an absolute path: {}", root.display()));
    }
    // One check that the root exists, so the error names it. The scan itself
    // enumerates through the scanner (AGENTS.md invariant 4).
    if !root.exists() {
        return Err(format!("does not exist: {}", root.display()));
    }
    let platform = platform().ok_or_else(|| "scanning is not supported here".to_owned())?;

    let mut running = lock(&state.running)?;
    if let Some(current) = running.as_ref() {
        return Err(format!(
            "scan {} of {} is still running",
            current.scan_id,
            current.root.display()
        ));
    }

    let scan_id = lock(&state.catalog)?
        .begin_session(platform, elevated_now(), &root)
        .map_err(|error| error.to_string())?;
    let cancel = CancelToken::new();
    let progress = Arc::new(Mutex::new(Progress::default()));
    *running = Some(RunningScan {
        scan_id,
        root: root.clone(),
        cancel: cancel.clone(),
        progress: Arc::clone(&progress),
    });
    drop(running);

    let thread_state = Arc::clone(state);
    let spawned = std::thread::Builder::new()
        .name(format!("storagelens-scan-{scan_id}"))
        .spawn(move || {
            run_scan(
                &thread_state,
                emitter.as_ref(),
                scan_id,
                &root,
                cancel,
                &progress,
            );
        });
    if let Err(error) = spawned {
        *lock(&state.running)? = None;
        return Err(format!("could not start the scan thread: {error}"));
    }
    Ok(scan_id)
}

/// The scan thread: scan, record, load the arena, write rollups, announce.
fn run_scan(
    state: &AppState,
    emitter: &dyn Emit,
    scan_id: i64,
    root: &Path,
    cancel: CancelToken,
    progress: &Arc<Mutex<Progress>>,
) {
    let started = Instant::now();
    let outcome = scan_and_record(state, emitter, scan_id, root, cancel, progress);

    // Clear `running` before announcing, so a UI reacting to the event can start
    // the next scan immediately.
    if let Ok(mut running) = state.running.lock() {
        *running = None;
    }

    match outcome {
        Ok(status) => events::emit(
            emitter,
            events::SCAN_DONE,
            &ScanDoneEvent {
                scan_id,
                status,
                duration_ms: started.elapsed().as_millis() as u64,
            },
        ),
        Err(message) => {
            // The session has no "failed" status; record it as cancelled so it is
            // not left looking like it is still running.
            if let Ok(catalog) = state.catalog.lock() {
                let _ = catalog.finish_session(scan_id, ScanStatus::Cancelled, 0, 0, 0);
            }
            events::emit(
                emitter,
                events::SCAN_ERROR,
                &ScanErrorEvent { scan_id, message },
            );
        }
    }
}

fn scan_and_record(
    state: &AppState,
    emitter: &dyn Emit,
    scan_id: i64,
    root: &Path,
    cancel: CancelToken,
    progress: &Arc<Mutex<Progress>>,
) -> CommandResult<ScanStatus> {
    let mut writer = EntryWriter::open(&state.catalog_path, scan_id, DEFAULT_BATCH_SIZE)
        .map_err(|error| error.to_string())?;
    if let Some(locations) = &state.locations {
        writer = writer.with_locations(root, locations.clone());
    }

    let throttle = Throttle::new(PROGRESS_INTERVAL);
    let shared = Arc::clone(progress);
    let on_progress = move |snapshot: Progress| {
        if let Ok(mut cell) = shared.lock() {
            *cell = snapshot.clone();
        }
        if throttle.ready() {
            let seconds = (snapshot.elapsed_ms as f64 / 1000.0).max(0.001);
            events::emit(
                emitter,
                events::SCAN_PROGRESS,
                &ScanProgressEvent {
                    scan_id,
                    files: snapshot.files,
                    dirs: snapshot.dirs,
                    bytes_alloc: snapshot.bytes_alloc.get(),
                    elapsed_ms: snapshot.elapsed_ms,
                    current_path: snapshot
                        .current_path
                        .as_ref()
                        .map(|path| path.display().to_string()),
                    rate: snapshot.files as f64 / seconds,
                },
            );
        }
    };

    let result = run_platform_scanner(root, &mut writer, cancel, &on_progress)
        .map_err(|error| error.to_string())?;
    drop(writer);

    {
        let catalog = lock(&state.catalog)?;
        catalog
            .finish_session(
                scan_id,
                result.status,
                result.files,
                result.bytes_alloc.get(),
                result.skipped.len() as u64,
            )
            .map_err(|error| error.to_string())?;

        let arena =
            Arena::load(catalog.connection(), scan_id).map_err(|error| error.to_string())?;
        rollup::write(catalog.connection(), &arena).map_err(|error| error.to_string())?;
        // Index the names before `scan://done`, so search works the moment the
        // UI hears it (~5 s per million entries).
        catalog
            .index_names(scan_id)
            .map_err(|error| error.to_string())?;
        drop(catalog);
        *lock(&state.arena)? = Some(Arc::new(arena));
    }

    let status = result.status;
    lock(&state.results)?.insert(scan_id, result);
    Ok(status)
}

pub fn cancel_scan(state: &AppState, scan_id: i64) -> CommandResult<()> {
    match lock(&state.running)?.as_ref() {
        Some(running) if running.scan_id == scan_id => {
            running.cancel.cancel();
            Ok(())
        }
        _ => Err(format!("scan {scan_id} is not running")),
    }
}

fn elevated_for(session: &ScanSession) -> Option<bool> {
    (session.platform == Platform::Windows).then_some(session.elevated)
}

pub fn scan_status(state: &AppState, scan_id: i64) -> CommandResult<ScanStatusDto> {
    if let Some(running) = lock(&state.running)?.as_ref() {
        if running.scan_id == scan_id {
            let progress = lock(&running.progress)?.clone();
            return Ok(ScanStatusDto {
                scan_id,
                status: ScanStatus::Running,
                root: Some(running.root.display().to_string()),
                files: progress.files,
                dirs: progress.dirs,
                bytes_alloc: progress.bytes_alloc.get(),
                elapsed_ms: progress.elapsed_ms,
                skipped_dirs: 0,
                elevated: None,
            });
        }
    }

    let session = state.session(scan_id)?;
    let result = lock(&state.results)?.get(&scan_id).cloned();
    let elapsed_ms = match (&result, session.finished_at) {
        (Some(result), _) => result.elapsed_ms,
        (None, Some(finished)) => (finished - session.started_at).max(0) as u64 * 1000,
        (None, None) => 0,
    };
    Ok(ScanStatusDto {
        scan_id,
        status: session.status,
        root: session.root.as_ref().map(|root| root.display().to_string()),
        files: session.files_seen,
        dirs: result.as_ref().map_or(0, |result| result.dirs),
        bytes_alloc: session.bytes_seen,
        elapsed_ms,
        skipped_dirs: session.dirs_skipped,
        elevated: result
            .as_ref()
            .and_then(|result| result.elevated)
            .or_else(|| elevated_for(&session)),
    })
}

// --- Reading a finished scan ----------------------------------------------------

fn child_dto(arena: &Arena, index: NodeIndex) -> Option<ChildDto> {
    let node = arena.node(index)?;
    Some(ChildDto {
        id: arena.entry_id(index)?,
        name: arena.name(index)?.to_owned(),
        kind: node.kind(),
        category: node.category(),
        size_alloc: if node.kind() == EntryKind::File {
            node.size_alloc
        } else {
            node.total_alloc
        },
        file_count: arena.file_count(index).unwrap_or(0),
        has_children: node.first_child != storagelens_catalog::NIL,
        hardlink_dup: node.is_hardlink_dup(),
    })
}

/// One directory listing, biggest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildrenDto {
    pub items: Vec<ChildDto>,
    /// Children beyond `limit`: how many, and their combined size, so a treemap
    /// can draw them as one "smaller items" block.
    pub remaining_count: u64,
    pub remaining_bytes: u64,
}

fn sorted_children(arena: &Arena, index: NodeIndex) -> Vec<ChildDto> {
    let mut children: Vec<ChildDto> = arena
        .children(index)
        .filter_map(|child| child_dto(arena, child))
        .collect();
    children.sort_by(|left, right| {
        right
            .size_alloc
            .cmp(&left.size_alloc)
            .then_with(|| left.name.cmp(&right.name))
    });
    children
}

pub fn get_children(
    state: &AppState,
    scan_id: i64,
    parent_id: i64,
    limit: Option<u32>,
) -> CommandResult<ChildrenDto> {
    let arena = state.arena(scan_id)?;
    let index = arena
        .index_of(parent_id)
        .ok_or_else(|| format!("entry {parent_id} is not part of scan {scan_id}"))?;
    let limit = limit.unwrap_or(DEFAULT_CHILDREN).clamp(1, MAX_CHILDREN) as usize;

    let mut items = sorted_children(&arena, index);
    let rest = items.split_off(items.len().min(limit));
    Ok(ChildrenDto {
        items,
        remaining_count: rest.len() as u64,
        remaining_bytes: rest
            .iter()
            .filter(|child| !child.hardlink_dup)
            .map(|child| child.size_alloc)
            .sum(),
    })
}

/// Children worth drawing: everything except extra names of hard links, whose
/// bytes are already counted under their first name.
fn counted_children(arena: &Arena, index: NodeIndex) -> Vec<ChildDto> {
    let mut children = sorted_children(arena, index);
    children.retain(|child| !child.hardlink_dup);
    children
}

/// One expanded directory: the children kept, and the rest aggregated.
struct Expansion {
    kept: Vec<ChildDto>,
    remaining_count: u64,
    remaining_bytes: u64,
}

/// `parent_id`'s subtree, `depth` levels deep, capped at [`TREE_BUDGET`] nodes.
///
/// Expansion is breadth-first, biggest first within each directory, so the
/// budget goes to what the treemap can actually draw: the top levels and the
/// largest directories, not one deep corner.
pub fn get_tree(
    state: &AppState,
    scan_id: i64,
    parent_id: i64,
    depth: Option<u32>,
) -> CommandResult<TreeNodeDto> {
    let arena = state.arena(scan_id)?;
    let root = arena
        .index_of(parent_id)
        .ok_or_else(|| format!("entry {parent_id} is not part of scan {scan_id}"))?;
    let root_dto = child_dto(&arena, root).ok_or("the entry has no data")?;
    let max_depth = depth.unwrap_or(DEFAULT_TREE_DEPTH).clamp(1, MAX_TREE_DEPTH);

    let mut expanded: HashMap<i64, Expansion> = HashMap::new();
    let mut queue = VecDeque::from([(root, 0u32)]);
    let mut budget = TREE_BUDGET;
    while let Some((index, level)) = queue.pop_front() {
        if level >= max_depth || budget == 0 {
            continue;
        }
        let Some(id) = arena.entry_id(index) else {
            continue;
        };
        let mut kept = counted_children(&arena, index);
        let rest = kept.split_off(kept.len().min(TREE_PER_DIR).min(budget));
        budget -= kept.len();
        for child in kept.iter().filter(|child| child.has_children) {
            if let Some(child_index) = arena.index_of(child.id) {
                queue.push_back((child_index, level + 1));
            }
        }
        expanded.insert(
            id,
            Expansion {
                kept,
                remaining_count: rest.len() as u64,
                remaining_bytes: rest.iter().map(|child| child.size_alloc).sum(),
            },
        );
    }
    Ok(assemble(root_dto, &mut expanded))
}

fn assemble(node: ChildDto, expanded: &mut HashMap<i64, Expansion>) -> TreeNodeDto {
    let (children, remaining_count, remaining_bytes) = match expanded.remove(&node.id) {
        Some(expansion) => (
            Some(
                expansion
                    .kept
                    .into_iter()
                    .map(|child| assemble(child, expanded))
                    .collect(),
            ),
            expansion.remaining_count,
            expansion.remaining_bytes,
        ),
        None => (None, 0, 0),
    };
    TreeNodeDto {
        id: node.id,
        name: node.name,
        kind: node.kind,
        category: node.category,
        size_alloc: node.size_alloc,
        file_count: node.file_count,
        has_children: node.has_children,
        children,
        remaining_count,
        remaining_bytes,
    }
}

// --- Grouping views ---------------------------------------------------------------

/// Modified times of `arena`'s scan, loaded once and shared with the file
/// listing.
fn cached_times(state: &AppState, arena: &Arena, scan_id: i64) -> CommandResult<Arc<Vec<i64>>> {
    let mut listing = lock(&state.listing)?;
    if let Some(times) = listing
        .as_ref()
        .filter(|current| current.scan_id == scan_id)
        .and_then(|current| current.times.clone())
    {
        return Ok(times);
    }
    let loaded = {
        let catalog = lock(&state.catalog)?;
        Arc::new(modified_times(catalog.connection(), arena).map_err(|error| error.to_string())?)
    };
    match listing
        .as_mut()
        .filter(|current| current.scan_id == scan_id)
    {
        Some(current) => current.times = Some(Arc::clone(&loaded)),
        None => {
            *listing = Some(Listing {
                scan_id,
                times: Some(Arc::clone(&loaded)),
                key: None,
                order: Arc::new(Vec::new()),
            });
        }
    }
    Ok(loaded)
}

const DAY_SECONDS: i64 = 86_400;

fn grouping_tree(
    state: &AppState,
    arena: &Arena,
    scan_id: i64,
    by: Grouping,
    filter: &FilterDto,
) -> CommandResult<Arc<SyntheticTree>> {
    let times = cached_times(state, arena, scan_id)?;
    let root = state.root_of(scan_id);
    let rules = match by {
        Grouping::Cleanliness => rule_evaluation(state, arena, scan_id, root.as_deref(), &times)?,
        _ => None,
    };
    let rules_key = rules.as_ref().map(|rules| rules.key);
    if let Some(cached) = lock(&state.grouping)?.as_ref() {
        if cached.scan_id == scan_id
            && cached.by == by
            && &cached.filter == filter
            && cached.rules == rules_key
        {
            return Ok(Arc::clone(&cached.tree));
        }
    }
    let now = now_unix();
    let within = match filter.within {
        None => None,
        Some(id) => Some(
            arena
                .index_of(id)
                .filter(|&index| {
                    arena
                        .node(index)
                        .is_some_and(|node| node.kind() == EntryKind::Dir)
                })
                .ok_or_else(|| format!("folder {id} is not part of this scan"))?,
        ),
    };
    let file_filter = FileFilter {
        within,
        categories: filter.categories.clone(),
        min_size: filter.min_size.unwrap_or(0),
        modified_before: filter
            .older_than_days
            .map(|days| now - i64::from(days) * DAY_SECONDS),
    };
    let risk = rules
        .as_ref()
        .map(|rules| |index: NodeIndex| rules.evaluation.risk_of(index));
    let input = storagelens_analysis::GroupingInput {
        arena,
        root_path: root.as_deref(),
        modified: Some(&times),
        now,
        risk: risk
            .as_ref()
            .map(|risk| risk as &dyn Fn(NodeIndex) -> Option<Risk>),
        filter: &file_filter,
    };
    let kind = match by {
        Grouping::Folder => storagelens_analysis::Grouping::Folder,
        Grouping::Type => storagelens_analysis::Grouping::Type,
        Grouping::App => storagelens_analysis::Grouping::App,
        Grouping::Age => storagelens_analysis::Grouping::Age,
        Grouping::Size => storagelens_analysis::Grouping::Size,
        Grouping::Cleanliness => storagelens_analysis::Grouping::Cleanliness,
    };
    let tree = Arc::new(storagelens_analysis::build(kind, &input));
    *lock(&state.grouping)? = Some(GroupingCache {
        scan_id,
        by,
        filter: filter.clone(),
        rules: rules_key,
        tree: Arc::clone(&tree),
    });
    Ok(tree)
}

/// The user's home folder, from the environment.
fn home_dir() -> Option<String> {
    std::env::var(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .ok()
        .filter(|home| !home.is_empty())
}

/// What rule globs may name: `~`, and on Windows the usual folders.
fn rules_context() -> Context {
    let mut context = Context::new(storagelens_safety::Platform::CURRENT, home_dir().as_deref());
    if cfg!(windows) {
        for name in [
            "LOCALAPPDATA",
            "APPDATA",
            "TEMP",
            "USERPROFILE",
            "SystemRoot",
            "SystemDrive",
            "ProgramData",
        ] {
            if let Ok(value) = std::env::var(name) {
                context = context.with_var(name, &value);
            }
        }
    }
    context
}

/// The cleanup rules' verdicts on `scan_id`. Re-run when the scan or the
/// rules folder changes; each run replaces the scan's `rule_hit` rows. `None`
/// for a scan with no recorded root (the engine matches absolute paths).
fn rule_evaluation(
    state: &AppState,
    arena: &Arena,
    scan_id: i64,
    root: Option<&Path>,
    times: &[i64],
) -> CommandResult<Option<Arc<Rules>>> {
    let Some(root) = root.and_then(Path::to_str) else {
        return Ok(None);
    };
    let dir = state.rules_dir();
    let print = fingerprint(&dir);
    if let Some(cached) = lock(&state.rules)?.as_ref() {
        if cached.scan_id == scan_id && cached.fingerprint == print {
            return Ok(Some(Arc::clone(&cached.rules)));
        }
    }
    let context = rules_context();
    let set = RuleSet::load(&context, &dir);
    for problem in &set.problems {
        eprintln!("cleanup rule skipped: {problem}");
    }
    let guard =
        storagelens_safety::Guard::from_resolved(context.platform, root, context.home.as_deref())
            .map_err(|refusal| format!("the scanned folder {root} cannot be cleaned: {refusal}"))?;
    let evaluation = evaluate(&set, arena, root, Some(times), now_unix(), &guard);
    lock(&state.catalog)?
        .replace_rule_hits(scan_id, &evaluation.rows(arena))
        .map_err(|error| error.to_string())?;
    let rules = Arc::new(Rules {
        key: print ^ (state.next_plan.fetch_add(1, Ordering::Relaxed) << 1),
        set,
        evaluation,
    });
    *lock(&state.rules)? = Some(RulesCache {
        scan_id,
        fingerprint: print,
        rules: Arc::clone(&rules),
    });
    Ok(Some(rules))
}

/// The rules over `scan_id`, loading what they need.
fn rules_for(state: &AppState, scan_id: i64) -> CommandResult<Arc<Rules>> {
    let arena = state.arena(scan_id)?;
    let times = cached_times(state, &arena, scan_id)?;
    let root = state.root_of(scan_id);
    rule_evaluation(state, &arena, scan_id, root.as_deref(), &times)?.ok_or_else(|| {
        "this scan predates path recording; rescan to see cleanup suggestions".to_owned()
    })
}

/// The id a group goes by: its folder's entry id when it stands for one
/// (so it can be revealed), otherwise a negative id, `-(index + 1)`.
fn group_id(tree: &SyntheticTree, arena: &Arena, group: u32) -> i64 {
    tree.groups[group as usize]
        .entry
        .and_then(|entry| arena.entry_id(entry))
        .unwrap_or(-(i64::from(group) + 1))
}

/// One child of an expanded group: a subgroup or a file.
#[derive(Clone, Copy)]
enum Item {
    Group(u32),
    File(NodeIndex),
}

/// A group's biggest `limit` children (subgroups and files together), and
/// what is left over.
fn top_items(
    tree: &SyntheticTree,
    arena: &Arena,
    group: u32,
    limit: usize,
) -> (Vec<Item>, u64, u64) {
    let source = &tree.groups[group as usize];
    let size_of = |file: NodeIndex| arena.node(file).map_or(0, |node| node.size_alloc);
    let mut files = source.files.clone();
    if files.len() > limit {
        files.select_nth_unstable_by(limit, |&a, &b| size_of(b).cmp(&size_of(a)).then(a.cmp(&b)));
        files.truncate(limit);
    }
    let mut items: Vec<(u64, Item)> = source
        .children
        .iter()
        .map(|&child| (tree.groups[child as usize].size, Item::Group(child)))
        .chain(
            files
                .into_iter()
                .map(|file| (size_of(file), Item::File(file))),
        )
        .collect();
    // Stable sort keeps a bucketed view's natural order among equal sizes.
    items.sort_by_key(|item| std::cmp::Reverse(item.0));
    let total_items = source.children.len() + source.files.len();
    items.truncate(limit);
    let kept: u64 = items.iter().map(|(size, _)| size).sum();
    (
        items.into_iter().map(|(_, item)| item).collect(),
        (total_items - total_items.min(limit)) as u64,
        source.size.saturating_sub(kept),
    )
}

/// `parent_id`'s part of a grouping (the root when `None`), as the same tree
/// shape `get_tree` returns — so the treemap draws every view unchanged.
pub fn get_grouping(
    state: &AppState,
    scan_id: i64,
    by: Grouping,
    filter: FilterDto,
    parent_id: Option<i64>,
    depth: Option<u32>,
) -> CommandResult<TreeNodeDto> {
    let arena = state.arena(scan_id)?;
    let tree = grouping_tree(state, &arena, scan_id, by, &filter)?;
    let start = match parent_id {
        None => 0,
        Some(id) if id < 0 => u32::try_from(-(id + 1))
            .ok()
            .filter(|&group| (group as usize) < tree.groups.len())
            .ok_or_else(|| format!("group {id} is not part of this view"))?,
        Some(id) => arena
            .index_of(id)
            .and_then(|entry| tree.group_of_entry(entry))
            .ok_or_else(|| format!("entry {id} is not a group of this view"))?,
    };
    let max_depth = depth.unwrap_or(DEFAULT_TREE_DEPTH).clamp(1, MAX_TREE_DEPTH);

    // Breadth-first under one node budget, like `get_tree`.
    let mut expanded: HashMap<u32, (Vec<Item>, u64, u64)> = HashMap::new();
    let mut queue = VecDeque::from([(start, 0u32)]);
    let mut budget = TREE_BUDGET;
    while let Some((group, level)) = queue.pop_front() {
        if level >= max_depth || budget == 0 {
            continue;
        }
        let (items, remaining_count, remaining_bytes) =
            top_items(&tree, &arena, group, TREE_PER_DIR.min(budget));
        budget -= items.len();
        for item in &items {
            if let Item::Group(child) = item {
                queue.push_back((*child, level + 1));
            }
        }
        expanded.insert(group, (items, remaining_count, remaining_bytes));
    }
    Ok(assemble_group(&tree, &arena, start, &mut expanded))
}

fn assemble_group(
    tree: &SyntheticTree,
    arena: &Arena,
    group: u32,
    expanded: &mut HashMap<u32, (Vec<Item>, u64, u64)>,
) -> TreeNodeDto {
    let source = &tree.groups[group as usize];
    let (children, remaining_count, remaining_bytes) = match expanded.remove(&group) {
        Some((items, count, bytes)) => (
            Some(
                items
                    .into_iter()
                    .filter_map(|item| match item {
                        Item::Group(child) => Some(assemble_group(tree, arena, child, expanded)),
                        Item::File(file) => child_dto(arena, file).map(|child| TreeNodeDto {
                            id: child.id,
                            name: child.name,
                            kind: child.kind,
                            category: child.category,
                            size_alloc: child.size_alloc,
                            file_count: child.file_count,
                            has_children: false,
                            children: None,
                            remaining_count: 0,
                            remaining_bytes: 0,
                        }),
                    })
                    .collect(),
            ),
            count,
            bytes,
        ),
        None => (None, 0, 0),
    };
    TreeNodeDto {
        id: group_id(tree, arena, group),
        name: source.label.clone(),
        kind: EntryKind::Dir,
        category: source.category.unwrap_or(storagelens_core::Category::Other),
        size_alloc: source.size,
        file_count: source.file_count,
        has_children: !source.children.is_empty() || !source.files.is_empty(),
        children,
        remaining_count,
        remaining_bytes,
    }
}

// --- Reveal and preview ------------------------------------------------------------

/// The on-disk path of a catalogued entry, checked to still exist.
///
/// Reveal and preview take catalog ids, never paths from the UI: only what the
/// scan saw can be handed to the OS.
fn existing_path(state: &AppState, scan_id: i64, entry_id: i64) -> CommandResult<PathBuf> {
    let root = state
        .root_of(scan_id)
        .ok_or("this scan predates path recording; rescan to open files from it")?;
    let arena = state.arena(scan_id)?;
    let index = arena
        .index_of(entry_id)
        .ok_or_else(|| format!("entry {entry_id} is not part of scan {scan_id}"))?;
    let mut path = root;
    for name in arena.path_names(index) {
        path.push(name);
    }
    std::fs::symlink_metadata(&path).map_err(|_| format!("{} no longer exists", path.display()))?;
    Ok(path)
}

/// Show an entry selected in Finder / Explorer.
pub fn reveal_entry(state: &AppState, scan_id: i64, entry_id: i64) -> CommandResult<()> {
    shell::reveal(&existing_path(state, scan_id, entry_id)?)
}

/// Quick Look on macOS. Windows has no system quick-preview, so it reveals the
/// entry in Explorer instead, where the preview pane shows it.
pub fn preview_entry(state: &AppState, scan_id: i64, entry_id: i64) -> CommandResult<()> {
    shell::preview(&existing_path(state, scan_id, entry_id)?)
}

/// Handing paths to the OS shell. Programs are spawned by absolute path with
/// arguments passed directly — no shell parses them — and reaped on a thread.
mod shell {
    use std::path::Path;
    use std::process::{Command, Stdio};

    fn spawn(mut command: Command) -> Result<(), String> {
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("could not open the file manager: {error}"))?;
        std::thread::spawn(move || child.wait());
        Ok(())
    }

    #[cfg(target_os = "macos")]
    pub fn reveal(path: &Path) -> Result<(), String> {
        let mut command = Command::new("/usr/bin/open");
        command.arg("-R").arg(path);
        spawn(command)
    }

    #[cfg(target_os = "macos")]
    pub fn preview(path: &Path) -> Result<(), String> {
        let mut command = Command::new("/usr/bin/qlmanage");
        command.arg("-p").arg(path);
        spawn(command)
    }

    #[cfg(windows)]
    pub fn reveal(path: &Path) -> Result<(), String> {
        use std::os::windows::process::CommandExt;

        // Explorer rejects verbatim (`\\?\`) paths. A Windows path cannot
        // contain `"`, so quoting it is enough for Explorer's own parser.
        let text = path.display().to_string();
        let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
        let windows = std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into());
        let mut command = Command::new(Path::new(&windows).join("explorer.exe"));
        command.raw_arg(format!("/select,\"{text}\""));
        spawn(command)
    }

    #[cfg(windows)]
    pub fn preview(path: &Path) -> Result<(), String> {
        reveal(path)
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    pub fn reveal(_path: &Path) -> Result<(), String> {
        Err("revealing files is supported on macOS and Windows".to_owned())
    }

    #[cfg(not(any(target_os = "macos", windows)))]
    pub fn preview(_path: &Path) -> Result<(), String> {
        reveal(_path)
    }
}

/// Mounted volumes the platform scanner can see.
fn all_volumes() -> Vec<storagelens_core::Volume> {
    #[cfg(target_os = "macos")]
    {
        storagelens_scan_macos::volumes()
    }
    #[cfg(windows)]
    {
        use storagelens_core::StorageScanner;
        storagelens_scan_windows::WinScanner::default().volumes()
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Vec::new()
    }
}

/// The current user's home folder, from the environment the OS set up.
fn home_folder() -> Option<PathBuf> {
    let variable = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let home = PathBuf::from(std::env::var_os(variable)?);
    (home.is_absolute() && home.is_dir()).then_some(home)
}

/// What the scan picker offers: the home folder first, then every volume,
/// fixed before removable, largest first.
pub fn list_scan_targets() -> Vec<ScanTargetDto> {
    let startup = startup_mount();
    let mut volumes: Vec<ScanTargetDto> = all_volumes()
        .into_iter()
        .map(|volume| ScanTargetDto {
            kind: ScanTargetKind::Volume,
            startup: startup
                .as_deref()
                .is_some_and(|startup| same_mount(&volume.mount_point, startup)),
            label: volume.name,
            path: volume.mount_point.display().to_string(),
            total: Some(volume.total.get()),
            available: Some(volume.available.get()),
            removable: volume.removable,
        })
        .collect();
    // Startup disk first, then fixed before removable, largest first.
    volumes.sort_by(|left, right| {
        right
            .startup
            .cmp(&left.startup)
            .then_with(|| left.removable.cmp(&right.removable))
            .then_with(|| right.total.cmp(&left.total))
    });
    let home = home_folder().map(|home| ScanTargetDto {
        kind: ScanTargetKind::Home,
        label: home.file_name().map_or_else(
            || "Home".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        ),
        path: home.display().to_string(),
        total: None,
        available: None,
        removable: false,
        startup: false,
    });
    let split = usize::from(volumes.first().is_some_and(|volume| volume.startup));
    let rest = volumes.split_off(split);
    volumes.into_iter().chain(home).chain(rest).collect()
}

/// Where the OS booted from: `/`, or `%SystemDrive%` on Windows.
fn startup_mount() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("SystemDrive").map(PathBuf::from)
    } else {
        Some(PathBuf::from("/"))
    }
}

/// Mount points compare case-insensitively and without a trailing separator
/// on Windows (`C:\` vs `c:`).
fn same_mount(left: &Path, right: &Path) -> bool {
    if cfg!(windows) {
        let clean = |path: &Path| {
            path.display()
                .to_string()
                .trim_end_matches(['\\', '/'])
                .to_ascii_lowercase()
        };
        clean(left) == clean(right)
    } else {
        left == right
    }
}

fn volume_for(root: Option<&Path>) -> Option<VolumeDto> {
    let root = root?;
    all_volumes()
        .into_iter()
        .filter(|volume| on_volume(root, &volume.mount_point))
        .max_by_key(|volume| volume.mount_point.as_os_str().len())
        .map(|volume| VolumeDto {
            name: volume.name,
            mount_point: volume.mount_point.display().to_string(),
            total: volume.total.get(),
            available: volume.available.get(),
            used: volume.total.get().saturating_sub(volume.available.get()),
            purgeable: volume.purgeable.get().min(volume.available.get()),
        })
}

/// Whether `root` lies on the volume mounted at `mount_point`. Windows drive
/// letters compare case-insensitively (`c:\` is `C:\`).
fn on_volume(root: &Path, mount_point: &Path) -> bool {
    if cfg!(windows) {
        let root = root.display().to_string().to_ascii_lowercase();
        let mount = mount_point.display().to_string().to_ascii_lowercase();
        let root = root.strip_prefix(r"\\?\").unwrap_or(&root);
        root.starts_with(&mount)
    } else {
        root.starts_with(mount_point)
    }
}

pub fn get_categories(state: &AppState, scan_id: i64) -> CommandResult<Vec<CategoryDto>> {
    // Fail with "not found" for an unknown scan rather than an empty list.
    state.session(scan_id)?;
    let totals = lock(&state.catalog)?
        .category_totals(scan_id)
        .map_err(|error| error.to_string())?;
    Ok(totals
        .into_iter()
        .map(|total| CategoryDto {
            category: total.category,
            label: total.category.label(),
            bytes_alloc: total.bytes_alloc,
            file_count: total.file_count,
        })
        .collect())
}

pub fn get_overview(state: &AppState, scan_id: i64) -> CommandResult<OverviewDto> {
    let scan = scan_status(state, scan_id)?;
    let arena = state.arena(scan_id)?;
    let root = arena.root();
    let mut biggest = sorted_children(&arena, root);
    biggest.truncate(OVERVIEW_BIGGEST);

    Ok(OverviewDto {
        root_id: arena.entry_id(root).ok_or("the scan has no root")?,
        root_name: arena.name(root).unwrap_or_default().to_owned(),
        volume: volume_for(state.root_of(scan_id).as_deref()),
        categories: get_categories(state, scan_id)?,
        biggest,
        scan,
    })
}

/// A node's full path under `root`, or under the root's name when the session
/// predates schema v2 and has no recorded root.
fn path_of(root: Option<&Path>, arena: &Arena, index: NodeIndex) -> String {
    let mut path = root.map_or_else(
        || PathBuf::from(arena.name(arena.root()).unwrap_or_default()),
        Path::to_path_buf,
    );
    for name in arena.path_names(index) {
        path.push(name);
    }
    path.display().to_string()
}

/// Rows per `get_files` call: the table holds at most two of these.
pub const MAX_FILE_RANGE: u32 = 200;

/// A file order and the modified times its rows read from.
type OrderAndTimes = (Arc<Vec<NodeIndex>>, Arc<Vec<i64>>);

/// The file order for `(sort, min_size)`, built on first use and cached.
fn file_listing(
    state: &AppState,
    arena: &Arena,
    scan_id: i64,
    sort: FileSortDto,
    min_size: u64,
) -> CommandResult<OrderAndTimes> {
    let mut listing = lock(&state.listing)?;
    if listing
        .as_ref()
        .is_none_or(|current| current.scan_id != scan_id)
    {
        *listing = Some(Listing {
            scan_id,
            times: None,
            key: None,
            order: Arc::new(Vec::new()),
        });
    }
    let current = listing.as_mut().ok_or("listing cache is unavailable")?;
    let times = match &current.times {
        Some(times) => Arc::clone(times),
        None => {
            let loaded = {
                let catalog = lock(&state.catalog)?;
                modified_times(catalog.connection(), arena).map_err(|error| error.to_string())?
            };
            let loaded = Arc::new(loaded);
            current.times = Some(Arc::clone(&loaded));
            loaded
        }
    };
    if current.key != Some((sort, min_size)) {
        let key = match sort {
            FileSortDto::Size => FileSort::Size,
            FileSortDto::Modified => FileSort::Modified,
            FileSortDto::Type => FileSort::Type,
        };
        current.order = Arc::new(file_order(arena, key, min_size, Some(&times)));
        current.key = Some((sort, min_size));
    }
    Ok((Arc::clone(&current.order), times))
}

/// Rows `start .. start + count` of every counted file of at least `min_size`
/// bytes, in `sort` order (backwards when `reversed`). Positional, so a table
/// can jump anywhere in a million rows; never more than [`MAX_FILE_RANGE`].
#[allow(clippy::too_many_arguments)]
pub fn get_files(
    state: &AppState,
    scan_id: i64,
    sort: FileSortDto,
    reversed: bool,
    min_size: u64,
    start: u64,
    count: u32,
) -> CommandResult<FileRangeDto> {
    let arena = state.arena(scan_id)?;
    let root = state.root_of(scan_id);
    let (order, times) = file_listing(state, &arena, scan_id, sort, min_size)?;
    let start_position = usize::try_from(start).unwrap_or(usize::MAX);
    let count = count.clamp(1, MAX_FILE_RANGE) as usize;
    let items = slice(&order, start_position, count, reversed)
        .into_iter()
        .filter_map(|index| {
            let node = arena.node(index)?;
            Some(FileRowDto {
                kind: node.kind(),
                id: arena.entry_id(index)?,
                name: arena.name(index)?.to_owned(),
                path: path_of(root.as_deref(), &arena, index),
                category: node.category(),
                size_alloc: node.size_alloc,
                size_logical: arena.size_logical(index).unwrap_or(0),
                modified: times
                    .get(index as usize)
                    .copied()
                    .filter(|&time| time != UNKNOWN_TIME),
            })
        })
        .collect();
    Ok(FileRangeDto {
        total: order.len() as u64,
        start,
        items,
    })
}

/// Search results per page; the UI shows at most four pages (AGENTS.md §8).
pub const SEARCH_PAGE: u32 = 50;

/// Entries of `scan_id` whose name matches `query`, biggest first, then most
/// recent (`catalog::search`). A folder's size is its subtree total.
pub fn search(
    state: &AppState,
    scan_id: i64,
    query: &str,
    cursor: Option<SearchCursorDto>,
) -> CommandResult<Page<FileRowDto, SearchCursorDto>> {
    let arena = state.arena(scan_id)?;
    let root = state.root_of(scan_id);
    let hits = lock(&state.catalog)?
        .search(
            scan_id,
            query,
            cursor.map(|cursor| SearchCursor {
                size: cursor.size_alloc,
                modified: cursor.modified,
                id: cursor.id,
            }),
            SEARCH_PAGE,
        )
        .map_err(|error| error.to_string())?;
    let next_cursor = (hits.len() == SEARCH_PAGE as usize)
        .then(|| hits.last())
        .flatten()
        .map(|hit| SearchCursorDto {
            size_alloc: hit.size,
            modified: hit.modified,
            id: hit.id,
        });
    let items = hits
        .into_iter()
        .map(|hit| FileRowDto {
            path: arena.index_of(hit.id).map_or_else(
                || hit.name.clone(),
                |index| path_of(root.as_deref(), &arena, index),
            ),
            id: hit.id,
            name: hit.name,
            kind: hit.kind,
            category: hit.category,
            size_alloc: hit.size,
            size_logical: hit.size_logical,
            modified: (hit.modified != 0).then_some(hit.modified),
        })
        .collect();
    Ok(Page { items, next_cursor })
}

/// The folders from the scan root down to `entry_id`, inclusive — a
/// breadcrumb, for opening a search result in the Map.
pub fn get_ancestors(
    state: &AppState,
    scan_id: i64,
    entry_id: i64,
) -> CommandResult<Vec<CrumbDto>> {
    let arena = state.arena(scan_id)?;
    let index = arena
        .index_of(entry_id)
        .ok_or_else(|| format!("entry {entry_id} is not part of scan {scan_id}"))?;
    let mut chain: Vec<CrumbDto> = std::iter::once(index)
        .chain(arena.ancestors(index))
        .filter_map(|index| {
            Some(CrumbDto {
                id: arena.entry_id(index)?,
                name: arena.name(index)?.to_owned(),
            })
        })
        .collect();
    chain.reverse();
    Ok(chain)
}

pub fn get_largest(
    state: &AppState,
    scan_id: i64,
    min_size: u64,
    cursor: Option<SizeCursorDto>,
) -> CommandResult<Page<FileRowDto, SizeCursorDto>> {
    let arena = state.arena(scan_id)?;
    // One session read per page, not per row.
    let root = state.root_of(scan_id);
    let rows = lock(&state.catalog)?
        .largest(
            scan_id,
            min_size,
            cursor.map(|cursor| SizeCursor {
                size_alloc: cursor.size_alloc,
                id: cursor.id,
            }),
            PAGE_SIZE,
        )
        .map_err(|error| error.to_string())?;

    let next_cursor = (rows.len() == PAGE_SIZE as usize)
        .then(|| rows.last())
        .flatten()
        .map(|row| SizeCursorDto {
            size_alloc: row.size_alloc,
            id: row.id,
        });
    let items = rows
        .into_iter()
        .map(|row| FileRowDto {
            kind: row.kind,
            path: arena
                .index_of(row.id)
                .map(|index| path_of(root.as_deref(), &arena, index))
                .unwrap_or_else(|| row.name.clone()),
            id: row.id,
            name: row.name,
            category: row.category,
            size_alloc: row.size_alloc,
            size_logical: row.size_logical,
            modified: row.modified,
        })
        .collect();
    Ok(Page { items, next_cursor })
}

// --- Cleanup: suggestions, receipt, Trash, undo (#34) -------------------------------

/// Over this, execution needs a second confirmation (PLAN §15.7).
pub const CONFIRM_ABOVE: u64 = 5 * 1024 * 1024 * 1024;

const KIND_ORDER: [Kind; 6] = [
    Kind::Regenerable,
    Kind::Stale,
    Kind::Duplicate,
    Kind::Leftover,
    Kind::Valuable,
    Kind::System,
];

fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Regenerable => "regenerable",
        Kind::Stale => "stale",
        Kind::Duplicate => "duplicate",
        Kind::Leftover => "leftover",
        Kind::Valuable => "valuable",
        Kind::System => "system",
    }
}

/// Local Time Machine snapshots, as a report-only card (macOS).
#[cfg(target_os = "macos")]
fn snapshot_card(problems: &mut Vec<String>) -> Option<RuleGroupDto> {
    use storagelens_scan_macos::snapshots;
    let found = match snapshots::list(Path::new("/")) {
        Ok(found) => found,
        Err(error) => {
            problems.push(format!("local snapshots could not be listed: {error}"));
            return None;
        }
    };
    (!found.is_empty()).then(|| RuleGroupDto {
        rule_id: snapshots::RULE_ID.to_owned(),
        title: snapshots::TITLE.to_owned(),
        kind: kind_name(Kind::System).to_owned(),
        risk: "safe".to_owned(),
        explain: snapshots::EXPLAIN.to_owned(),
        command: Some("tmutil deletelocalsnapshots <date>".to_owned()),
        deletable: false,
        bytes: 0,
        items: found
            .into_iter()
            .map(|snapshot| RuleItemDto {
                path: snapshot.name,
                entry_id: None,
                bytes: 0,
                selected: false,
            })
            .collect(),
    })
}

#[cfg(not(target_os = "macos"))]
fn snapshot_card(_problems: &mut Vec<String>) -> Option<RuleGroupDto> {
    None
}

/// The Cleanup screen's cards for `scan_id`.
pub fn get_rules(state: &AppState, scan_id: i64) -> CommandResult<CleanupDto> {
    let rules = rules_for(state, scan_id)?;
    let removed = lock(&state.removed)?.clone();
    let mut groups: Vec<(usize, RuleGroupDto)> = Vec::new();
    for compiled in &rules.set.rules {
        let rule = &compiled.rule;
        let mut items: Vec<RuleItemDto> = rules
            .evaluation
            .hits
            .iter()
            .filter(|hit| hit.rule_id == rule.id && !removed.contains(&hit.path))
            .map(|hit| RuleItemDto {
                path: hit.path.clone(),
                entry_id: None,
                bytes: hit.bytes,
                selected: hit.selected,
            })
            .collect();
        if items.is_empty() {
            continue;
        }
        items.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.path.cmp(&b.path)));
        let order = KIND_ORDER
            .iter()
            .position(|kind| *kind == rule.kind)
            .unwrap_or(0);
        groups.push((
            order,
            RuleGroupDto {
                rule_id: rule.id.clone(),
                title: rule.title.clone(),
                kind: kind_name(rule.kind).to_owned(),
                risk: risk_name(rule.risk()).to_owned(),
                explain: rule.explain.clone(),
                command: rule.command.clone(),
                deletable: rule.deletable(),
                bytes: items.iter().map(|item| item.bytes).sum(),
                items,
            },
        ));
    }
    let mut problems = rules.set.problems.clone();
    if let Some(card) = snapshot_card(&mut problems) {
        groups.push((KIND_ORDER.len() - 1, card));
    }
    groups.sort_by(|(a, x), (b, y)| a.cmp(b).then_with(|| y.bytes.cmp(&x.bytes)));
    Ok(CleanupDto {
        scan_id,
        groups: groups.into_iter().map(|(_, group)| group).collect(),
        refused: rules.evaluation.refused.len() as u64,
        reclaim_safe: rules.evaluation.reclaim.safe.round() as u64,
        reclaim_review: (rules.evaluation.reclaim.review / risk_weight(Risk::Review)).round()
            as u64,
        problems,
    })
}

/// Whether `inner` is `outer` or below it, component by component.
fn is_within(inner: &str, outer: &str) -> bool {
    Path::new(inner).starts_with(Path::new(outer))
}

/// Make the receipt for `paths`, each a deletable suggestion of `scan_id`.
pub fn preview_deletion(
    state: &AppState,
    scan_id: i64,
    paths: Vec<String>,
) -> CommandResult<DeletionPlanDto> {
    if paths.is_empty() {
        return Err("nothing selected to delete".to_owned());
    }
    let mut unique = std::collections::BTreeSet::new();
    for path in &paths {
        if !Path::new(path).is_absolute() {
            return Err(format!("not an absolute path: {path}"));
        }
        if !unique.insert(path.as_str()) {
            return Err(format!("listed twice: {path}"));
        }
    }
    for (i, a) in paths.iter().enumerate() {
        for b in &paths[i + 1..] {
            if is_within(a, b) || is_within(b, a) {
                return Err(format!("{a} and {b} overlap; select only one of them"));
            }
        }
    }

    let rules = rules_for(state, scan_id)?;
    let removed = lock(&state.removed)?.clone();
    let candidates: HashMap<&str, &storagelens_analysis::rules::Hit> = rules
        .evaluation
        .candidates()
        .map(|candidate| (candidate.hit().path.as_str(), candidate.hit()))
        .collect();
    let arena = state.arena(scan_id)?;
    let catalog = lock(&state.catalog)?;
    let mut items = Vec::new();
    let mut lines = Vec::new();
    for path in &paths {
        if removed.contains(path) {
            return Err(format!("{path} was already moved to the Trash"));
        }
        let hit = candidates
            .get(path.as_str())
            .ok_or_else(|| format!("{path} is not a cleanup suggestion of this scan"))?;
        let entry = arena
            .entry_id(hit.node)
            .and_then(|id| catalog.entry(scan_id, id).ok().flatten())
            .ok_or_else(|| format!("{path} is not in the catalog"))?;
        let title = rules.set.get(&hit.rule_id).map_or_else(
            || hit.rule_id.clone(),
            |compiled| compiled.rule.title.clone(),
        );
        items.push(storagelens_safety::Item {
            path: PathBuf::from(path),
            bytes: hit.bytes,
            rule_id: Some(hit.rule_id.clone()),
            stamp: storagelens_core::FileStamp {
                kind: entry.kind,
                size_logical: entry.size_logical,
                modified: entry.modified.unwrap_or(0),
                dev: entry.dev,
                file_id: entry.file_id,
                attrs: entry.attrs,
            },
        });
        lines.push(PlanItemDto {
            path: path.clone(),
            bytes: hit.bytes,
            rule_id: hit.rule_id.clone(),
            title,
            risk: risk_name(hit.risk).to_owned(),
        });
    }
    drop(catalog);
    store_plan(state, scan_id, items, lines)
}

/// Record a dry-run receipt: the only thing `execute_deletion` accepts.
fn store_plan(
    state: &AppState,
    scan_id: i64,
    items: Vec<storagelens_safety::Item>,
    lines: Vec<PlanItemDto>,
) -> CommandResult<DeletionPlanDto> {
    let total_bytes = lines.iter().map(|line| line.bytes).sum();
    let number = state.next_plan.fetch_add(1, Ordering::Relaxed);
    let dto = DeletionPlanDto {
        plan_id: format!("plan-{number}-{}", now_unix()),
        scan_id,
        items: lines,
        total_bytes,
        needs_confirmation: total_bytes > CONFIRM_ABOVE,
        created_at: now_unix(),
    };
    lock(&state.plans)?.insert(
        dto.plan_id.clone(),
        StoredPlan {
            dto: dto.clone(),
            items,
        },
    );
    Ok(dto)
}

/// Installed apps, classified, without system apps or anything already moved
/// to the Trash. `refresh` re-reads the machine (seconds); otherwise the last
/// inventory is reused.
pub fn list_apps(
    state: &AppState,
    refresh: bool,
    inventory: &dyn Fn() -> Vec<storagelens_core::AppInfo>,
) -> CommandResult<Vec<AppRowDto>> {
    let cached = lock(&state.apps)?.clone();
    let apps = match cached {
        Some(apps) if !refresh => apps,
        _ => {
            // Not under the lock: the inventory takes seconds.
            let mut apps = inventory();
            storagelens_analysis::apps::assess(&mut apps, now_unix());
            *lock(&state.apps)? = Some(apps.clone());
            apps
        }
    };
    let removed = lock(&state.removed)?.clone();
    Ok(apps
        .iter()
        .filter(|app| !app.system && !removed.contains(&app.path.display().to_string()))
        .map(|app| {
            let satellites: Vec<SatelliteDto> = app
                .satellites
                .iter()
                .map(|satellite| SatelliteDto {
                    path: satellite.path.display().to_string(),
                    bytes: satellite.bytes.get(),
                })
                .filter(|satellite| !removed.contains(&satellite.path))
                .collect();
            let satellite_bytes = satellites.iter().map(|s| s.bytes).sum();
            AppRowDto {
                id: app.id.clone(),
                name: app.name.clone(),
                version: app.version.clone(),
                path: app.path.display().to_string(),
                app_bytes: app.bytes.get(),
                satellite_bytes,
                total_bytes: app.bytes.get().saturating_add(satellite_bytes),
                last_used: app.last_used,
                use_count: app.use_count,
                status: app.status,
                running: app.running,
                autostart: app.autostart,
                default_handler: app.default_handler,
                has_uninstaller: app.uninstaller.is_some(),
                satellites,
            }
        })
        .collect())
}

/// Force quit the processes of the listed app at `path` (#113). Only an app
/// from the last inventory, never a system one, so the UI cannot name an
/// arbitrary folder. The cached row is marked not running when every process
/// stopped.
pub fn force_quit_app(
    state: &AppState,
    path: &str,
    quit: &dyn Fn(&Path) -> (usize, usize),
) -> CommandResult<QuitDto> {
    let mut apps = lock(&state.apps)?;
    let app = apps
        .as_mut()
        .and_then(|apps| apps.iter_mut().find(|app| app.path == Path::new(path)))
        .filter(|app| !app.system && !app.path.as_os_str().is_empty())
        .ok_or_else(|| format!("{path} is not a listed app"))?;
    let (stopped, failed) = quit(&app.path);
    if failed == 0 {
        app.running = false;
    }
    Ok(QuitDto { stopped, failed })
}

/// Start the own uninstaller of the listed app `id` (#115): on Windows the
/// command "Add or remove apps" runs, on macOS a vendor `Uninstall….app`.
/// Returns once it has started; the UI refreshes the list afterwards.
pub fn uninstall_app(
    state: &AppState,
    id: &str,
    run: &dyn Fn(&str) -> Result<(), String>,
) -> CommandResult<()> {
    let command = lock(&state.apps)?
        .as_ref()
        .and_then(|apps| apps.iter().find(|app| app.id == id && !app.system))
        .and_then(|app| app.uninstaller.clone())
        .ok_or_else(|| format!("{id} has no uninstaller"))?;
    run(&command)
}

fn platform_uninstaller(command: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        storagelens_scan_macos::apps::run_uninstaller(command)
    }
    #[cfg(windows)]
    {
        storagelens_scan_windows::apps::run_uninstaller(command)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = command;
        Err("uninstalling is not supported on this platform".to_owned())
    }
}

// --- Startup items (#114) -----------------------------------------------------------

/// Everything that starts at login on this platform.
pub fn list_startup() -> Vec<StartupItemDto> {
    #[cfg(target_os = "macos")]
    let items = home_folder()
        .map(|home| storagelens_scan_macos::startup::startup_items(&home))
        .unwrap_or_default();
    #[cfg(windows)]
    let items = storagelens_scan_windows::startup::startup_items();
    #[cfg(not(any(target_os = "macos", windows)))]
    let items: Vec<storagelens_core::StartupItem> = Vec::new();
    items.into_iter().map(StartupItemDto::from).collect()
}

/// Switch startup item `id` for future logins; returns the refreshed list.
pub fn set_startup(id: &str, enabled: bool) -> CommandResult<Vec<StartupItemDto>> {
    #[cfg(target_os = "macos")]
    {
        let home = home_folder().ok_or_else(|| "no home folder".to_owned())?;
        storagelens_scan_macos::startup::set_startup_enabled(&home, id, enabled)?;
    }
    #[cfg(windows)]
    storagelens_scan_windows::startup::set_startup_enabled(id, enabled)?;
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = (id, enabled);
        return Err("startup items are not supported on this platform".to_owned());
    }
    #[allow(unreachable_code)]
    Ok(list_startup())
}

/// Open the system's own startup settings, for the items this app cannot
/// switch.
pub fn open_startup_settings() -> CommandResult<()> {
    #[cfg(target_os = "macos")]
    let (program, url) = ("open", storagelens_scan_macos::startup::LOGIN_ITEMS_URL);
    #[cfg(windows)]
    let (program, url) = (
        "explorer",
        storagelens_scan_windows::startup::STARTUP_SETTINGS_URL,
    );
    #[cfg(not(any(target_os = "macos", windows)))]
    let (program, url) = ("xdg-open", "");
    std::process::Command::new(program)
        .arg(url)
        .spawn()
        .map(drop)
        .map_err(|error| format!("could not open the settings: {error}"))
}

/// This platform's force quit.
fn platform_force_quit(path: &Path) -> (usize, usize) {
    #[cfg(target_os = "macos")]
    {
        storagelens_scan_macos::apps::force_quit(path)
    }
    #[cfg(windows)]
    {
        storagelens_scan_windows::apps::force_quit(path)
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        let _ = path;
        (0, 0)
    }
}

/// Dry-run receipt for removing one app: the bundle / install directory, and
/// with `include_data` its associated data. Executed by `execute_deletion`,
/// so the guard, preflight re-stat, Trash and undo are the cleanup ones.
///
/// A running app is refused unless `allow_running` (the UI's explicit
/// "remove anyway"); a system app is always refused.
pub fn preview_app_removal(
    state: &AppState,
    scan_id: i64,
    path: &str,
    include_data: bool,
    allow_running: bool,
    stamp: &dyn Fn(&Path) -> std::io::Result<storagelens_core::FileStamp>,
) -> CommandResult<DeletionPlanDto> {
    let app = lock(&state.apps)?
        .as_ref()
        .and_then(|apps| {
            apps.iter()
                .find(|app| !path.is_empty() && app.path.display().to_string() == path)
                .cloned()
        })
        .ok_or_else(|| format!("{path} is not in the app list; reload it"))?;
    if app.system {
        return Err(format!(
            "{} ships with the system and is never removed",
            app.name
        ));
    }
    if app.id.starts_with(STORE_APP_PREFIX) {
        return Err(format!(
            "{} is a Microsoft Store app; use Uninstall, which Windows carries out",
            app.name
        ));
    }
    if app.running && !allow_running {
        return Err(format!(
            "{} is running; quit it first, or confirm removing it anyway",
            app.name
        ));
    }

    let mut targets = vec![(
        app.path.clone(),
        app.bytes.get(),
        "app.bundle",
        app.name.clone(),
    )];
    if include_data {
        let title = format!("{} — associated data (estimated)", app.name);
        targets.extend(
            app.satellites
                .iter()
                .map(|s| (s.path.clone(), s.bytes.get(), "app.data", title.clone())),
        );
    }

    plan_for(state, scan_id, &app.name, targets, stamp)
}

/// A dry-run receipt for removing `targets` of the app `name`, skipping
/// anything already moved to the Trash.
fn plan_for(
    state: &AppState,
    scan_id: i64,
    name: &str,
    targets: Vec<(PathBuf, u64, &str, String)>,
    stamp: &dyn Fn(&Path) -> std::io::Result<storagelens_core::FileStamp>,
) -> CommandResult<DeletionPlanDto> {
    let removed = lock(&state.removed)?.clone();
    let mut items = Vec::new();
    let mut lines = Vec::new();
    for (target, bytes, rule_id, title) in targets {
        let shown = target.display().to_string();
        if removed.contains(&shown) {
            continue;
        }
        let stamped = stamp(&target).map_err(|error| format!("cannot read {shown}: {error}"))?;
        items.push(storagelens_safety::Item {
            path: target,
            bytes,
            rule_id: Some(rule_id.to_owned()),
            stamp: stamped,
        });
        lines.push(PlanItemDto {
            path: shown,
            bytes,
            rule_id: rule_id.to_owned(),
            title,
            risk: "review".to_owned(),
        });
    }
    if items.is_empty() {
        return Err(format!("{} was already moved to the Trash", name));
    }
    store_plan(state, scan_id, items, lines)
}

// --- Leftovers after an uninstall (#120) ----------------------------------------------

/// Ids of Microsoft Store apps (#122); mirrors `scan_windows::store_apps::PREFIX`,
/// which only builds on Windows.
const STORE_APP_PREFIX: &str = "msix:";

/// The cached, non-system app `id`.
fn listed_app(state: &AppState, id: &str) -> CommandResult<storagelens_core::AppInfo> {
    lock(&state.apps)?
        .as_ref()
        .and_then(|apps| apps.iter().find(|app| app.id == id && !app.system).cloned())
        .ok_or_else(|| format!("{id} is not in the app list; reload it"))
}

/// The app's own uninstaller has finished: its bundle is gone (macOS) or its
/// uninstall entry is (Windows). Waiting on the process would not do: many
/// uninstallers hand off to a copy of themselves and exit at once.
pub fn uninstall_finished(
    state: &AppState,
    id: &str,
    still_installed: &dyn Fn(&storagelens_core::AppInfo) -> bool,
) -> CommandResult<bool> {
    Ok(!still_installed(&listed_app(state, id)?))
}

fn platform_still_installed(app: &storagelens_core::AppInfo) -> bool {
    #[cfg(windows)]
    {
        storagelens_scan_windows::apps::is_registered(&app.id)
    }
    #[cfg(not(windows))]
    {
        app.path.exists()
    }
}

/// What the app `id` left behind: its install folder or bundle if it is still
/// there, and the data folders found for it that still exist. Strong matches
/// first.
pub fn app_leftovers(state: &AppState, id: &str) -> CommandResult<Vec<LeftoverDto>> {
    let app = listed_app(state, id)?;
    let removed = lock(&state.removed)?.clone();
    let exists = |path: &Path| std::fs::symlink_metadata(path).is_ok();
    let mut out = Vec::new();
    // A Store app's folder belongs to Windows (WindowsApps), which removes it
    // on its own schedule (#122); never offered.
    let store_app = app.id.starts_with(STORE_APP_PREFIX);
    if !store_app && !app.path.as_os_str().is_empty() && exists(&app.path) {
        out.push(LeftoverDto {
            path: app.path.display().to_string(),
            bytes: app.bytes.get(),
            strong: true,
            kind: "install".to_owned(),
            can_remove: true,
        });
    }
    out.extend(
        app.satellites
            .iter()
            .filter(|satellite| exists(&satellite.path))
            .map(|satellite| LeftoverDto {
                path: satellite.path.display().to_string(),
                bytes: satellite.bytes.get(),
                strong: satellite.strong,
                kind: "data".to_owned(),
                can_remove: true,
            }),
    );
    out.retain(|leftover| !removed.contains(&leftover.path));
    out.sort_by_key(|leftover| !leftover.strong);
    Ok(out)
}

/// A system-domain leftover as the platform reports it: `(path or package
/// id, kind, strong)`.
type SystemItem = (String, String, bool);

/// Removes system items into a Trash folder: `(items, trash_dir, app_name)`.
type RemoveSystem<'a> = dyn Fn(&[SystemItem], &Path, &str) -> Result<(), String> + 'a;

/// What app `id` left in the system domain (#123, macOS): launchd jobs,
/// helper tools and installer receipts. Strong first.
pub fn system_leftovers(
    state: &AppState,
    id: &str,
    find: &dyn Fn(&Path, &str) -> Vec<SystemItem>,
) -> CommandResult<Vec<LeftoverDto>> {
    let app = listed_app(state, id)?;
    let mut out: Vec<LeftoverDto> = find(&app.path, &app.id)
        .into_iter()
        .map(|(path, kind, strong)| LeftoverDto {
            path,
            bytes: 0,
            strong,
            kind,
            can_remove: true,
        })
        .collect();
    out.sort_by_key(|item| !item.strong);
    Ok(out)
}

fn platform_find_system(app: &Path, bundle_id: &str) -> Vec<SystemItem> {
    #[cfg(target_os = "macos")]
    {
        storagelens_scan_macos::system_leftovers::find(app, bundle_id)
            .into_iter()
            .map(|item| (item.path, item.kind.to_owned(), item.strong))
            .collect()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, bundle_id);
        Vec::new()
    }
}

fn platform_remove_system(
    items: &[SystemItem],
    trash_dir: &Path,
    app_name: &str,
) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use storagelens_scan_macos::system_leftovers::{remove, SystemLeftover};
        let items: Vec<SystemLeftover> = items
            .iter()
            .map(|(path, kind, strong)| SystemLeftover {
                path: path.clone(),
                kind: if kind == "receipt" {
                    "receipt"
                } else {
                    "helper"
                },
                strong: *strong,
            })
            .collect();
        let owner = std::env::var("USER").map_err(|_| "no user name".to_owned())?;
        remove(&items, trash_dir, &owner, app_name)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (items, trash_dir, app_name);
        Err("system leftovers are a macOS feature".to_owned())
    }
}

/// Remove the chosen system leftovers of app `id` (#123) with one
/// administrator prompt. Only items [`system_leftovers`] lists; files are
/// moved into a folder in the user's Trash, receipts are forgotten.
pub fn remove_system_leftovers(
    state: &AppState,
    id: &str,
    paths: &[String],
    find: &dyn Fn(&Path, &str) -> Vec<SystemItem>,
    remove: &RemoveSystem<'_>,
) -> CommandResult<SystemRemovalDto> {
    let app = listed_app(state, id)?;
    let found = find(&app.path, &app.id);
    let chosen = paths
        .iter()
        .map(|path| {
            found
                .iter()
                .find(|(item, ..)| item == path)
                .cloned()
                .ok_or_else(|| format!("{path} is not a leftover of {}", app.name))
        })
        .collect::<CommandResult<Vec<SystemItem>>>()?;
    if chosen.is_empty() {
        return Err("nothing chosen".to_owned());
    }
    let home = home_folder().ok_or_else(|| "no home folder".to_owned())?;
    let safe_name: String = app
        .name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trash_dir = home
        .join(".Trash")
        .join(format!("{safe_name} leftovers {}", now_unix()));
    remove(&chosen, &trash_dir, &app.name)?;
    Ok(SystemRemovalDto {
        removed: chosen.len(),
        trash_dir: trash_dir.display().to_string(),
    })
}

/// The registry keys app `id` left behind that still exist (#121), strong
/// first. Machine-wide ones are listed but not removable unless elevated.
pub fn registry_leftovers(
    state: &AppState,
    id: &str,
    key_exists: &dyn Fn(&str) -> bool,
    elevated: bool,
) -> CommandResult<Vec<LeftoverDto>> {
    let app = listed_app(state, id)?;
    let mut out: Vec<LeftoverDto> = app
        .registry_keys
        .iter()
        .filter(|key| key_exists(&key.path))
        .map(|key| LeftoverDto {
            path: key.path.clone(),
            bytes: 0,
            strong: key.strong,
            kind: "registry".to_owned(),
            can_remove: elevated || !key.path.starts_with("HKEY_LOCAL_MACHINE"),
        })
        .collect();
    out.sort_by_key(|key| !key.strong);
    Ok(out)
}

fn platform_key_exists(path: &str) -> bool {
    #[cfg(windows)]
    {
        storagelens_scan_windows::leftover_keys::key_exists(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

fn platform_delete_key(path: &str, backup: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        storagelens_scan_windows::leftover_keys::delete_with_backup(path, backup)
    }
    #[cfg(not(windows))]
    {
        let _ = (path, backup);
        Err("the registry is Windows only".to_owned())
    }
}

fn platform_restore_key(backup: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        storagelens_scan_windows::leftover_keys::restore_backup(backup)
    }
    #[cfg(not(windows))]
    {
        let _ = backup;
        Err("the registry is Windows only".to_owned())
    }
}

/// Where registry backups are written.
fn registry_backup_dir(state: &AppState) -> PathBuf {
    state.data_dir().join("registry-backups")
}

/// `1700000000-HKEY_CURRENT_USER_Software_Acme_Foo.reg`: unique per second
/// and key, safe as a file name.
fn backup_file_name(key: &str, unix_secs: i64) -> String {
    let safe: String = key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .take(120)
        .collect();
    format!("{unix_secs}-{safe}.reg")
}

/// Back up, then delete, the chosen registry leftovers of app `id` (#121).
/// Only keys [`registry_leftovers`] lists as removable; a key whose backup
/// cannot be written is left in place.
pub fn remove_registry_leftovers(
    state: &AppState,
    id: &str,
    keys: &[String],
    key_exists: &dyn Fn(&str) -> bool,
    elevated: bool,
    delete: &dyn Fn(&str, &Path) -> Result<(), String>,
) -> CommandResult<RegistryRemovalDto> {
    let listed = registry_leftovers(state, id, key_exists, elevated)?;
    let dir = registry_backup_dir(state);
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    let now = now_unix();
    let mut removed = Vec::new();
    let mut failed = Vec::new();
    for key in keys {
        if !listed
            .iter()
            .any(|item| &item.path == key && item.can_remove)
        {
            failed.push((
                key.clone(),
                "not a removable leftover of this app".to_owned(),
            ));
            continue;
        }
        let backup = dir.join(backup_file_name(key, now));
        match delete(key, &backup) {
            Ok(()) => removed.push(RemovedKeyDto {
                path: key.clone(),
                backup: backup.display().to_string(),
            }),
            Err(reason) => failed.push((key.clone(), reason)),
        }
    }
    Ok(RegistryRemovalDto {
        removed,
        failed,
        backup_dir: dir.display().to_string(),
    })
}

/// Re-import registry backups written by [`remove_registry_leftovers`]. Only
/// `.reg` files in the backup folder. Returns how many were restored.
pub fn restore_registry_backups(
    state: &AppState,
    backups: &[String],
    restore: &dyn Fn(&Path) -> Result<(), String>,
) -> CommandResult<usize> {
    let dir = registry_backup_dir(state);
    let mut restored = 0;
    for backup in backups {
        let path = Path::new(backup);
        let ours = path.parent() == Some(dir.as_path())
            && path.extension().is_some_and(|ext| ext == "reg");
        if !ours {
            return Err(format!("{backup} is not a StorageLens registry backup"));
        }
        restore(path)?;
        restored += 1;
    }
    Ok(restored)
}

/// Dry-run receipt for removing the chosen leftovers of app `id`. Only paths
/// [`app_leftovers`] lists, so the UI cannot name anything else; executed by
/// `execute_deletion` like any cleanup plan (guard, re-stat, Trash, undo).
pub fn preview_leftover_removal(
    state: &AppState,
    scan_id: i64,
    id: &str,
    paths: &[String],
    stamp: &dyn Fn(&Path) -> std::io::Result<storagelens_core::FileStamp>,
) -> CommandResult<DeletionPlanDto> {
    let app = listed_app(state, id)?;
    let leftovers = app_leftovers(state, id)?;
    let title = format!("{} — left after uninstalling", app.name);
    let targets = paths
        .iter()
        .map(|path| {
            let leftover = leftovers
                .iter()
                .find(|leftover| &leftover.path == path)
                .ok_or_else(|| format!("{path} is not a leftover of {}", app.name))?;
            let rule = if leftover.kind == "install" {
                "app.bundle"
            } else {
                "app.data"
            };
            Ok((PathBuf::from(path), leftover.bytes, rule, title.clone()))
        })
        .collect::<CommandResult<Vec<_>>>()?;
    plan_for(state, scan_id, &app.name, targets, stamp)
}

/// Byte-identical files in a scan (PLAN §12). Reuses the last search of the
/// same scan unless `refresh`; groups shrink as copies go to the Trash, and a
/// group with one copy left is no longer listed.
///
/// Runs on the calling thread, which `throttle` slows down first (I/O policy);
/// hashing paces its own CPU use. `cancel_duplicates` stops it between
/// chunks.
pub fn find_duplicates(
    state: &AppState,
    sink: &dyn Emit,
    scan_id: i64,
    refresh: bool,
    options: storagelens_analysis::dupes::Options,
    throttle: &dyn Fn(),
) -> CommandResult<DupesDto> {
    use storagelens_analysis::dupes;

    let cached = lock(&state.dupes)?
        .clone()
        .filter(|cache| cache.scan_id == scan_id && !refresh);
    let cache = match cached {
        Some(cache) => cache,
        None => {
            let arena = state.arena(scan_id)?;
            let root = state
                .root_of(scan_id)
                .ok_or("this scan predates path recording; rescan first")?;
            let home = home_dir();
            let home = home.as_deref().map(Path::new);
            let guard = storagelens_safety::Guard::new(&root, home)
                .map_err(|refusal| format!("cannot search {}: {refusal}", root.display()))?;
            let candidates = dupes::candidates(&arena, &root, &guard, home, options.min_size);

            let cancel = CancelToken::new();
            *lock(&state.cancel_dupes)? = cancel.clone();
            throttle();
            let pace = Throttle::new(PROGRESS_INTERVAL);
            let report = dupes::find(candidates, options, &cancel, &mut |progress| {
                if pace.ready() || progress.groups_done == progress.groups_total {
                    events::emit(
                        sink,
                        events::DUPES_PROGRESS,
                        &DupesProgressEvent {
                            scan_id,
                            groups_done: progress.groups_done,
                            groups_total: progress.groups_total,
                            files_hashed: progress.files_hashed,
                            bytes_read: progress.bytes_read,
                        },
                    );
                }
            });
            let cache = DupesCache {
                scan_id,
                groups: report.groups,
                unreadable: report.unreadable,
                cancelled: report.cancelled,
            };
            *lock(&state.dupes)? = Some(cache.clone());
            cache
        }
    };

    let removed = lock(&state.removed)?.clone();
    let arena = state.arena(scan_id)?;
    let catalog = lock(&state.catalog)?;
    let groups = cache
        .groups
        .iter()
        .filter_map(|group| {
            let files: Vec<DupFileDto> = group
                .files
                .iter()
                .filter(|file| !removed.contains(&file.path.display().to_string()))
                .map(|file| DupFileDto {
                    path: file.path.display().to_string(),
                    size_alloc: file.size_alloc,
                    modified: arena
                        .entry_id(file.node)
                        .and_then(|id| catalog.entry(scan_id, id).ok().flatten())
                        .and_then(|entry| entry.modified),
                })
                .collect();
            (files.len() >= 2).then(|| {
                let total: u64 = files.iter().map(|file| file.size_alloc).sum();
                let largest = files.iter().map(|file| file.size_alloc).max().unwrap_or(0);
                DupGroupDto {
                    hash: group.hash.clone(),
                    size_logical: group.size_logical,
                    reclaimable: total - largest,
                    files,
                }
            })
        })
        .collect();
    Ok(DupesDto {
        scan_id,
        groups,
        unreadable: cache.unreadable,
        cancelled: cache.cancelled,
    })
}

/// Stop the running duplicate search before its next chunk.
pub fn cancel_duplicates(state: &AppState) -> CommandResult<()> {
    lock(&state.cancel_dupes)?.cancel();
    Ok(())
}

/// Dry-run receipt for trashing duplicate copies. Every path must be a copy
/// found by the last search of this scan, and every group must keep at least
/// one copy that is neither selected nor already in the Trash. Stamps come
/// from the scan, so the preflight re-stat catches a copy edited since.
pub fn preview_duplicate_removal(
    state: &AppState,
    scan_id: i64,
    paths: Vec<String>,
) -> CommandResult<DeletionPlanDto> {
    if paths.is_empty() {
        return Err("nothing selected to delete".to_owned());
    }
    let cache = lock(&state.dupes)?
        .clone()
        .filter(|cache| cache.scan_id == scan_id)
        .ok_or("no duplicate search for this scan; search first")?;
    let removed = lock(&state.removed)?.clone();

    let mut wanted: HashMap<&str, bool> = HashMap::new();
    for path in &paths {
        if wanted.insert(path.as_str(), false).is_some() {
            return Err(format!("listed twice: {path}"));
        }
    }
    let mut chosen = Vec::new();
    for group in &cache.groups {
        let mut kept = 0usize;
        for file in &group.files {
            let shown = file.path.display().to_string();
            if removed.contains(&shown) {
                continue;
            }
            match wanted.get_mut(shown.as_str()) {
                Some(seen) => {
                    *seen = true;
                    chosen.push((file.clone(), group.hash.clone()));
                }
                None => kept += 1,
            }
        }
        if kept == 0
            && group
                .files
                .iter()
                .any(|f| wanted.contains_key(f.path.display().to_string().as_str()))
        {
            return Err(format!(
                "that would trash every copy of {}; keep at least one",
                group
                    .files
                    .first()
                    .map_or_else(String::new, |f| f.path.display().to_string())
            ));
        }
    }
    if let Some((path, _)) = wanted.iter().find(|(_, seen)| !**seen) {
        return Err(format!("{path} is not a duplicate found in this scan"));
    }

    let arena = state.arena(scan_id)?;
    let catalog = lock(&state.catalog)?;
    let mut items = Vec::new();
    let mut lines = Vec::new();
    for (file, hash) in chosen {
        let shown = file.path.display().to_string();
        let entry = arena
            .entry_id(file.node)
            .and_then(|id| catalog.entry(scan_id, id).ok().flatten())
            .ok_or_else(|| format!("{shown} is not in the catalog"))?;
        items.push(storagelens_safety::Item {
            path: file.path.clone(),
            bytes: file.size_alloc,
            rule_id: Some("duplicate".to_owned()),
            stamp: storagelens_core::FileStamp {
                kind: entry.kind,
                size_logical: entry.size_logical,
                modified: entry.modified.unwrap_or(0),
                dev: entry.dev,
                file_id: entry.file_id,
                attrs: entry.attrs,
            },
        });
        lines.push(PlanItemDto {
            path: shown,
            bytes: file.size_alloc,
            rule_id: "duplicate".to_owned(),
            title: format!("Duplicate copy ({})", &hash[..hash.len().min(12)]),
            risk: "review".to_owned(),
        });
    }
    drop(catalog);
    store_plan(state, scan_id, items, lines)
}

/// Throttle the calling thread's disk I/O (and, on Windows, its priority).
fn throttle_this_thread() {
    #[cfg(target_os = "macos")]
    storagelens_scan_macos::throttle_current_thread();
    #[cfg(windows)]
    storagelens_scan_windows::ffi::throttle_current_thread();
}

/// Installed apps on this platform.
fn platform_apps() -> Vec<storagelens_core::AppInfo> {
    #[cfg(any(target_os = "macos", windows))]
    use storagelens_core::StorageScanner as _;
    #[cfg(target_os = "macos")]
    {
        storagelens_scan_macos::MacScanner::default().installed_apps()
    }
    #[cfg(windows)]
    {
        storagelens_scan_windows::WinScanner::default().installed_apps()
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        Vec::new()
    }
}

/// The platform's Trash, for the user whose home is `home`.
#[cfg(target_os = "macos")]
fn system_bin(home: &Path) -> storagelens_safety::bin::SystemBin {
    storagelens_safety::bin::SystemBin::new(home)
}

#[cfg(windows)]
fn system_bin(home: &Path) -> storagelens_safety::bin::SystemBin {
    storagelens_safety::bin::SystemBin::new(home)
}

#[cfg(target_os = "macos")]
fn platform_stamp(path: &Path) -> std::io::Result<storagelens_core::FileStamp> {
    storagelens_scan_macos::stamp(path)
}

#[cfg(windows)]
fn platform_stamp(path: &Path) -> std::io::Result<storagelens_core::FileStamp> {
    storagelens_scan_windows::stamp(path)
}

fn status_name(status: storagelens_safety::Status) -> &'static str {
    use storagelens_safety::Status;
    match status {
        Status::Planned => "planned",
        Status::Trashed => "trashed",
        Status::Refused => "refused",
        Status::Skipped => "skipped",
        Status::Failed => "failed",
        Status::Cancelled => "cancelled",
    }
}

/// Move a previewed plan to the Trash. `confirmed` is the second click a plan
/// over 5 GB needs. A plan runs once: it is used up whatever happens.
pub fn execute_deletion(
    state: &AppState,
    sink: &dyn Emit,
    plan_id: &str,
    confirmed: bool,
    bin: &dyn storagelens_safety::Bin,
    stamp: &dyn Fn(&Path) -> std::io::Result<storagelens_core::FileStamp>,
) -> CommandResult<DeletionResultDto> {
    let plan = {
        let mut plans = lock(&state.plans)?;
        let Some(plan) = plans.remove(plan_id) else {
            return Err(format!(
                "no dry-run receipt for plan '{plan_id}': call preview_deletion first"
            ));
        };
        if plan.dto.needs_confirmation && !confirmed {
            plans.insert(plan_id.to_owned(), plan);
            return Err("this deletion is over 5 GB and needs a second confirmation".to_owned());
        }
        plan
    };
    let root = state
        .root_of(plan.dto.scan_id)
        .ok_or("this scan predates path recording; rescan first")?;
    let home = home_dir();
    let guard = storagelens_safety::Guard::new(&root, home.as_deref().map(Path::new))
        .map_err(|refusal| format!("cannot clean {}: {refusal}", root.display()))?;
    let dir = state.manifests_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;

    state.cancel_deletion.store(false, Ordering::SeqCst);
    let total = plan.items.len() as u64;
    let throttle = Throttle::new(PROGRESS_INTERVAL);
    let mut before = |index: usize| {
        if index == 0 || throttle.ready() {
            events::emit(
                sink,
                events::CLEANUP_PROGRESS,
                &CleanupProgressDto {
                    plan_id: plan_id.to_owned(),
                    done: index as u64,
                    total,
                },
            );
        }
        !state.cancel_deletion.load(Ordering::SeqCst)
    };
    let now = now_unix();
    let outcome =
        storagelens_safety::execute_with(&guard, &plan.items, &dir, bin, stamp, now, &mut before)
            .map_err(|error| error.to_string())?;

    let manifest_path = outcome.manifest.display().to_string();
    let trashed: Vec<(String, u64, Option<String>)> = outcome
        .items
        .iter()
        .filter(|item| item.status == storagelens_safety::Status::Trashed)
        .map(|item| {
            (
                item.path.display().to_string(),
                item.bytes,
                item.rule_id.clone(),
            )
        })
        .collect();
    lock(&state.catalog)?
        .record_trashed(now, &manifest_path, &trashed)
        .map_err(|error| error.to_string())?;
    lock(&state.removed)?.extend(trashed.iter().map(|(path, _, _)| path.clone()));

    Ok(DeletionResultDto {
        manifest_id: outcome
            .manifest
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default(),
        items: outcome
            .items
            .iter()
            .map(|item| ResultItemDto {
                path: item.path.display().to_string(),
                bytes: item.bytes,
                status: status_name(item.status).to_owned(),
                reason: item.reason.clone(),
            })
            .collect(),
        trashed: trashed.len() as u64,
        trashed_bytes: outcome.trashed_bytes,
        warning: outcome.manifest_warning,
    })
}

/// Stop an executing deletion before its next item.
pub fn cancel_deletion(state: &AppState) {
    state.cancel_deletion.store(true, Ordering::SeqCst);
}

/// The manifest `manifest_id` names: a bare `trash-….json` in the manifests
/// folder, never a path.
fn manifest_path(state: &AppState, manifest_id: &str) -> CommandResult<PathBuf> {
    let plain = manifest_id.starts_with("trash-")
        && manifest_id.ends_with(".json")
        && !manifest_id.contains(['/', '\\'])
        && !manifest_id.contains("..");
    if !plain {
        return Err(format!("'{manifest_id}' is not a deletion manifest"));
    }
    Ok(state.manifests_dir().join(manifest_id))
}

/// Put back what the deletion `manifest_id` moved.
pub fn undo_deletion(
    state: &AppState,
    manifest_id: &str,
    bin: &dyn storagelens_safety::Bin,
) -> CommandResult<UndoResultDto> {
    use storagelens_safety::Undone;
    let path = manifest_path(state, manifest_id)?;
    let now = now_unix();
    let report = storagelens_safety::undo(&path, bin, now).map_err(|error| error.to_string())?;
    let restored: Vec<String> = report
        .items
        .iter()
        .filter(|item| item.result == Undone::Restored)
        .map(|item| item.path.display().to_string())
        .collect();
    lock(&state.catalog)?
        .mark_restored(&path.display().to_string(), &restored, now)
        .map_err(|error| error.to_string())?;
    {
        let mut removed = lock(&state.removed)?;
        for path in &restored {
            removed.remove(path);
        }
    }
    let items = report
        .items
        .iter()
        .map(|item| {
            let (result, reason) = match &item.result {
                Undone::Restored => ("restored", None),
                Undone::AlreadyRestored => ("already-restored", None),
                Undone::NotTrashed => ("not-trashed", None),
                Undone::NotInTrash => (
                    "not-in-trash",
                    Some("it is no longer in the Trash".to_owned()),
                ),
                Undone::Occupied => (
                    "occupied",
                    Some(
                        "something else is at its old place now, so it stays in the Trash"
                            .to_owned(),
                    ),
                ),
                Undone::Failed(why) => ("failed", Some(why.clone())),
            };
            UndoItemDto {
                path: item.path.display().to_string(),
                result: result.to_owned(),
                reason,
            }
        })
        .collect();
    Ok(UndoResultDto {
        items,
        restored: restored.len() as u64,
        warning: report.manifest_warning,
    })
}

// --- Full Disk Access --------------------------------------------------------------

/// What the Full Disk Access probe found. Off macOS there is no such permission.
// Each platform constructs only its own variants (macOS the first three,
// everything else `NotApplicable`), but the wire type is shared.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiskAccess {
    Granted,
    Denied,
    /// The probe could not tell (no protected folder to try on this machine).
    Unknown,
    /// Off macOS.
    NotApplicable,
}

pub fn check_full_disk_access() -> DiskAccess {
    #[cfg(target_os = "macos")]
    {
        use storagelens_scan_macos::FullDiskAccess;
        match storagelens_scan_macos::full_disk_access() {
            FullDiskAccess::Granted => DiskAccess::Granted,
            FullDiskAccess::Denied => DiskAccess::Denied,
            FullDiskAccess::Unknown => DiskAccess::Unknown,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        DiskAccess::NotApplicable
    }
}

// --- Tauri wrappers ---------------------------------------------------------------

/// The `#[tauri::command]` functions, named exactly as PLAN §14 and `api.ts`
/// call them. Each one only unwraps Tauri's arguments and delegates.
pub mod ipc {
    use super::*;

    type AppStateRef<'a> = State<'a, Arc<AppState>>;

    #[tauri::command]
    pub async fn start_scan(
        app: AppHandle,
        state: AppStateRef<'_>,
        roots: Vec<String>,
    ) -> CommandResult<i64> {
        super::start_scan(state.inner(), Arc::new(app), roots)
    }

    #[tauri::command]
    pub async fn list_scan_targets() -> CommandResult<Vec<ScanTargetDto>> {
        Ok(super::list_scan_targets())
    }

    #[tauri::command]
    pub async fn cancel_scan(state: AppStateRef<'_>, scan_id: i64) -> CommandResult<()> {
        super::cancel_scan(state.inner(), scan_id)
    }

    #[tauri::command]
    pub async fn scan_status(state: AppStateRef<'_>, scan_id: i64) -> CommandResult<ScanStatusDto> {
        super::scan_status(state.inner(), scan_id)
    }

    #[tauri::command]
    pub async fn latest_scan(state: AppStateRef<'_>) -> CommandResult<Option<ScanStatusDto>> {
        let latest = lock(&state.catalog)?
            .latest_session()
            .map_err(|error| error.to_string())?;
        latest
            .map(|session| super::scan_status(state.inner(), session.id))
            .transpose()
    }

    #[tauri::command]
    pub async fn get_overview(state: AppStateRef<'_>, scan_id: i64) -> CommandResult<OverviewDto> {
        super::get_overview(state.inner(), scan_id)
    }

    #[tauri::command]
    pub async fn get_categories(
        state: AppStateRef<'_>,
        scan_id: i64,
    ) -> CommandResult<Vec<CategoryDto>> {
        super::get_categories(state.inner(), scan_id)
    }

    #[tauri::command]
    pub async fn get_grouping(
        state: AppStateRef<'_>,
        scan_id: i64,
        by: Grouping,
        filter: Option<FilterDto>,
        parent_id: Option<i64>,
        depth: Option<u32>,
    ) -> CommandResult<TreeNodeDto> {
        super::get_grouping(
            state.inner(),
            scan_id,
            by,
            filter.unwrap_or_default(),
            parent_id,
            depth,
        )
    }

    #[tauri::command]
    pub async fn get_children(
        state: AppStateRef<'_>,
        scan_id: i64,
        parent_id: i64,
        limit: Option<u32>,
    ) -> CommandResult<ChildrenDto> {
        super::get_children(state.inner(), scan_id, parent_id, limit)
    }

    #[tauri::command]
    pub async fn get_tree(
        state: AppStateRef<'_>,
        scan_id: i64,
        parent_id: i64,
        depth: Option<u32>,
    ) -> CommandResult<TreeNodeDto> {
        super::get_tree(state.inner(), scan_id, parent_id, depth)
    }

    #[tauri::command]
    pub async fn reveal_entry(
        state: AppStateRef<'_>,
        scan_id: i64,
        entry_id: i64,
    ) -> CommandResult<()> {
        super::reveal_entry(state.inner(), scan_id, entry_id)
    }

    #[tauri::command]
    pub async fn preview_entry(
        state: AppStateRef<'_>,
        scan_id: i64,
        entry_id: i64,
    ) -> CommandResult<()> {
        super::preview_entry(state.inner(), scan_id, entry_id)
    }

    #[tauri::command]
    #[allow(clippy::too_many_arguments)]
    pub async fn get_files(
        state: AppStateRef<'_>,
        scan_id: i64,
        sort: FileSortDto,
        reversed: bool,
        min_size: u64,
        start: u64,
        count: u32,
    ) -> CommandResult<FileRangeDto> {
        super::get_files(
            state.inner(),
            scan_id,
            sort,
            reversed,
            min_size,
            start,
            count,
        )
    }

    #[tauri::command]
    pub async fn get_largest(
        state: AppStateRef<'_>,
        scan_id: i64,
        min_size: u64,
        cursor: Option<SizeCursorDto>,
    ) -> CommandResult<Page<FileRowDto, SizeCursorDto>> {
        super::get_largest(state.inner(), scan_id, min_size, cursor)
    }

    #[tauri::command]
    pub async fn search(
        state: AppStateRef<'_>,
        scan_id: i64,
        query: String,
        cursor: Option<SearchCursorDto>,
    ) -> CommandResult<Page<FileRowDto, SearchCursorDto>> {
        super::search(state.inner(), scan_id, &query, cursor)
    }

    #[tauri::command]
    pub async fn get_ancestors(
        state: AppStateRef<'_>,
        scan_id: i64,
        entry_id: i64,
    ) -> CommandResult<Vec<CrumbDto>> {
        super::get_ancestors(state.inner(), scan_id, entry_id)
    }

    #[tauri::command]
    pub async fn get_rules(state: AppStateRef<'_>, scan_id: i64) -> CommandResult<CleanupDto> {
        super::get_rules(state.inner(), scan_id)
    }

    #[tauri::command]
    pub async fn preview_deletion(
        state: AppStateRef<'_>,
        scan_id: i64,
        paths: Vec<String>,
    ) -> CommandResult<DeletionPlanDto> {
        super::preview_deletion(state.inner(), scan_id, paths)
    }

    #[tauri::command]
    pub async fn execute_deletion(
        app: AppHandle,
        state: AppStateRef<'_>,
        plan_id: String,
        confirmed: bool,
    ) -> CommandResult<DeletionResultDto> {
        let state = Arc::clone(state.inner());
        tauri::async_runtime::spawn_blocking(move || {
            let home = super::home_dir().unwrap_or_default();
            let bin = super::system_bin(Path::new(&home));
            super::execute_deletion(
                &state,
                &app,
                &plan_id,
                confirmed,
                &bin,
                &super::platform_stamp,
            )
        })
        .await
        .map_err(|error| error.to_string())?
    }

    #[tauri::command]
    pub async fn cancel_deletion(state: AppStateRef<'_>) -> CommandResult<()> {
        super::cancel_deletion(state.inner());
        Ok(())
    }

    #[tauri::command]
    pub async fn undo_deletion(
        state: AppStateRef<'_>,
        manifest_id: String,
    ) -> CommandResult<UndoResultDto> {
        let state = Arc::clone(state.inner());
        tauri::async_runtime::spawn_blocking(move || {
            let home = super::home_dir().unwrap_or_default();
            let bin = super::system_bin(Path::new(&home));
            super::undo_deletion(&state, &manifest_id, &bin)
        })
        .await
        .map_err(|error| error.to_string())?
    }

    #[tauri::command]
    pub async fn list_apps(state: AppStateRef<'_>, refresh: bool) -> CommandResult<Vec<AppRowDto>> {
        let state = Arc::clone(state.inner());
        tauri::async_runtime::spawn_blocking(move || {
            super::list_apps(&state, refresh, &super::platform_apps)
        })
        .await
        .map_err(|error| error.to_string())?
    }

    #[tauri::command]
    pub async fn uninstall_app(state: AppStateRef<'_>, id: String) -> CommandResult<()> {
        // A Store app's removal runs to completion (seconds): off the main thread.
        let state = Arc::clone(state.inner());
        tauri::async_runtime::spawn_blocking(move || {
            super::uninstall_app(&state, &id, &super::platform_uninstaller)
        })
        .await
        .map_err(|error| error.to_string())?
    }

    #[tauri::command]
    pub async fn uninstall_finished(state: AppStateRef<'_>, id: String) -> CommandResult<bool> {
        super::uninstall_finished(state.inner(), &id, &super::platform_still_installed)
    }

    #[tauri::command]
    pub async fn app_leftovers(
        state: AppStateRef<'_>,
        id: String,
    ) -> CommandResult<Vec<LeftoverDto>> {
        let mut out = super::app_leftovers(state.inner(), &id)?;
        out.extend(super::registry_leftovers(
            state.inner(),
            &id,
            &super::platform_key_exists,
            super::elevated_now(),
        )?);
        out.extend(super::system_leftovers(
            state.inner(),
            &id,
            &super::platform_find_system,
        )?);
        Ok(out)
    }

    #[tauri::command]
    pub async fn remove_system_leftovers(
        state: AppStateRef<'_>,
        id: String,
        paths: Vec<String>,
    ) -> CommandResult<SystemRemovalDto> {
        // Waits on the administrator prompt: off the main thread.
        let state = Arc::clone(state.inner());
        tauri::async_runtime::spawn_blocking(move || {
            super::remove_system_leftovers(
                &state,
                &id,
                &paths,
                &super::platform_find_system,
                &super::platform_remove_system,
            )
        })
        .await
        .map_err(|error| error.to_string())?
    }

    #[tauri::command]
    pub async fn remove_registry_leftovers(
        state: AppStateRef<'_>,
        id: String,
        keys: Vec<String>,
    ) -> CommandResult<RegistryRemovalDto> {
        let state = Arc::clone(state.inner());
        tauri::async_runtime::spawn_blocking(move || {
            super::remove_registry_leftovers(
                &state,
                &id,
                &keys,
                &super::platform_key_exists,
                super::elevated_now(),
                &super::platform_delete_key,
            )
        })
        .await
        .map_err(|error| error.to_string())?
    }

    #[tauri::command]
    pub async fn restore_registry_backups(
        state: AppStateRef<'_>,
        backups: Vec<String>,
    ) -> CommandResult<usize> {
        super::restore_registry_backups(state.inner(), &backups, &super::platform_restore_key)
    }

    #[tauri::command]
    pub async fn preview_leftover_removal(
        state: AppStateRef<'_>,
        scan_id: i64,
        id: String,
        paths: Vec<String>,
    ) -> CommandResult<DeletionPlanDto> {
        super::preview_leftover_removal(state.inner(), scan_id, &id, &paths, &super::platform_stamp)
    }

    #[tauri::command]
    pub async fn list_startup() -> CommandResult<Vec<StartupItemDto>> {
        // Runs launchctl / plutil per job: off the main thread.
        tauri::async_runtime::spawn_blocking(super::list_startup)
            .await
            .map_err(|error| error.to_string())
    }

    #[tauri::command]
    pub async fn set_startup(id: String, enabled: bool) -> CommandResult<Vec<StartupItemDto>> {
        tauri::async_runtime::spawn_blocking(move || super::set_startup(&id, enabled))
            .await
            .map_err(|error| error.to_string())?
    }

    #[tauri::command]
    pub async fn open_startup_settings() -> CommandResult<()> {
        super::open_startup_settings()
    }

    #[tauri::command]
    pub async fn force_quit_app(state: AppStateRef<'_>, path: String) -> CommandResult<QuitDto> {
        super::force_quit_app(state.inner(), &path, &super::platform_force_quit)
    }

    #[tauri::command]
    pub async fn preview_app_removal(
        state: AppStateRef<'_>,
        scan_id: i64,
        path: String,
        include_data: bool,
        allow_running: bool,
    ) -> CommandResult<DeletionPlanDto> {
        super::preview_app_removal(
            state.inner(),
            scan_id,
            &path,
            include_data,
            allow_running,
            &super::platform_stamp,
        )
    }

    #[tauri::command]
    pub async fn find_duplicates(
        app: AppHandle,
        state: AppStateRef<'_>,
        scan_id: i64,
        refresh: bool,
    ) -> CommandResult<DupesDto> {
        let state = Arc::clone(state.inner());
        tauri::async_runtime::spawn_blocking(move || {
            super::find_duplicates(
                &state,
                &app,
                scan_id,
                refresh,
                storagelens_analysis::dupes::Options::default(),
                &super::throttle_this_thread,
            )
        })
        .await
        .map_err(|error| error.to_string())?
    }

    #[tauri::command]
    pub async fn cancel_duplicates(state: AppStateRef<'_>) -> CommandResult<()> {
        super::cancel_duplicates(state.inner())
    }

    #[tauri::command]
    pub async fn preview_duplicate_removal(
        state: AppStateRef<'_>,
        scan_id: i64,
        paths: Vec<String>,
    ) -> CommandResult<DeletionPlanDto> {
        super::preview_duplicate_removal(state.inner(), scan_id, paths)
    }

    #[tauri::command]
    pub async fn check_full_disk_access() -> CommandResult<DiskAccess> {
        Ok(super::check_full_disk_access())
    }

    /// One harmless Apple Event to Finder (#149), so macOS asks for
    /// Automation access at launch rather than in the middle of a removal.
    #[tauri::command]
    pub async fn request_finder_access() -> CommandResult<bool> {
        #[cfg(target_os = "macos")]
        {
            tauri::async_runtime::spawn_blocking(|| {
                std::process::Command::new("/usr/bin/osascript")
                    .args(["-e", "tell application \"Finder\" to get name"])
                    .output()
                    .is_ok_and(|output| output.status.success())
            })
            .await
            .map_err(|error| error.to_string())
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(true)
        }
    }

    /// May StorageLens modify apps (App Management, #155)? `null` when it
    /// cannot tell, and off macOS.
    #[tauri::command]
    pub async fn check_app_management() -> CommandResult<Option<bool>> {
        #[cfg(target_os = "macos")]
        {
            tauri::async_runtime::spawn_blocking(
                storagelens_scan_macos::apps::app_management_access,
            )
            .await
            .map_err(|error| error.to_string())
        }
        #[cfg(not(target_os = "macos"))]
        {
            Ok(None)
        }
    }

    /// Open a Privacy & Security pane (#155): `AllFiles` (Full Disk
    /// Access), `AppBundles` (App Management) or `Automation`.
    #[tauri::command]
    pub async fn open_privacy_pane(pane: String) -> CommandResult<()> {
        if !["AllFiles", "AppBundles", "Automation"].contains(&pane.as_str()) {
            return Err(format!("unknown pane {pane}"));
        }
        std::process::Command::new("/usr/bin/open")
            .arg(format!(
                "x-apple.systempreferences:com.apple.preference.security?Privacy_{pane}"
            ))
            .status()
            .map_err(|error| error.to_string())
            .and_then(|status| {
                status
                    .success()
                    .then_some(())
                    .ok_or_else(|| "could not open System Settings".to_owned())
            })
    }

    #[tauri::command]
    pub async fn open_full_disk_access_settings() -> CommandResult<()> {
        #[cfg(target_os = "macos")]
        {
            storagelens_scan_macos::open_full_disk_access_settings()
                .map_err(|error| error.to_string())
        }
        #[cfg(not(target_os = "macos"))]
        {
            Err("Full Disk Access is a macOS setting".to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::Duration;

    use super::*;
    use crate::events::testing::Recorder;

    fn state(dir: &tempfile::TempDir) -> Arc<AppState> {
        let mut state = AppState::open(dir.path().join("catalog.db")).expect("open state");
        state.locations = None;
        Arc::new(state)
    }

    /// A small tree: two directories, four files, one of them large.
    fn tree(dir: &tempfile::TempDir) -> PathBuf {
        let root = fs::canonicalize(dir.path())
            .expect("canonical")
            .join("scanned");
        fs::create_dir_all(root.join("photos/2024")).expect("dirs");
        fs::create_dir_all(root.join("docs")).expect("dirs");
        fs::write(root.join("photos/2024/a.jpg"), vec![1u8; 300_000]).expect("file");
        fs::write(root.join("photos/b.png"), vec![2u8; 20_000]).expect("file");
        fs::write(root.join("docs/c.pdf"), vec![3u8; 5_000]).expect("file");
        fs::write(root.join("notes.txt"), b"hi").expect("file");
        // Windows paths come back from canonicalize with a verbatim prefix.
        let text = root.display().to_string();
        PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
    }

    /// Start a scan and wait for its terminal event.
    fn scan(state: &Arc<AppState>, root: &Path) -> (i64, Arc<Recorder>) {
        let recorder = Arc::new(Recorder::default());
        let scan_id = start_scan(
            state,
            Arc::clone(&recorder) as Arc<dyn Emit>,
            vec![root.display().to_string()],
        )
        .expect("start");

        let deadline = Instant::now() + Duration::from_secs(60);
        while recorder.named(events::SCAN_DONE).is_empty()
            && recorder.named(events::SCAN_ERROR).is_empty()
        {
            assert!(Instant::now() < deadline, "the scan never finished");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            recorder.named(events::SCAN_ERROR).is_empty(),
            "scan failed: {:?}",
            recorder.named(events::SCAN_ERROR)
        );
        (scan_id, recorder)
    }

    #[test]
    fn a_scan_runs_end_to_end_and_every_read_command_returns_typed_data() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state(&dir);
        let root = tree(&dir);
        let (scan_id, recorder) = scan(&state, &root);

        let done = recorder.named(events::SCAN_DONE);
        assert_eq!(done.len(), 1, "exactly one done event");
        assert_eq!(done[0]["scanId"], scan_id);
        assert_eq!(done[0]["status"], "complete");

        let status = scan_status(&state, scan_id).expect("status");
        assert_eq!(status.status, ScanStatus::Complete);
        assert_eq!(status.files, 4);
        assert_eq!(status.dirs, 3);
        assert_eq!(
            status.root.as_deref(),
            Some(root.display().to_string().as_str())
        );

        let overview = get_overview(&state, scan_id).expect("overview");
        assert_eq!(overview.root_name, "scanned");
        assert_eq!(overview.scan.bytes_alloc, status.bytes_alloc);
        assert_eq!(overview.biggest[0].name, "photos", "biggest child first");
        let category_total: u64 = overview.categories.iter().map(|c| c.bytes_alloc).sum();
        assert_eq!(category_total, status.bytes_alloc, "invariant 11");

        let children = get_children(&state, scan_id, overview.root_id, None).expect("children");
        let names: Vec<&str> = children.items.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names.len(), 3);
        assert_eq!(names[0], "photos");
        let photos = &children.items[0];
        assert!(photos.has_children);
        assert_eq!(photos.file_count, 2);

        let limited = get_children(&state, scan_id, overview.root_id, Some(1)).expect("limited");
        assert_eq!(limited.items.len(), 1);
        assert_eq!(limited.remaining_count, 2);
        assert_eq!(
            limited.items[0].size_alloc + limited.remaining_bytes,
            status.bytes_alloc,
            "the remainder accounts for every byte"
        );

        let page = get_largest(&state, scan_id, 0, None).expect("largest");
        assert_eq!(page.items.len(), 4);
        assert!(page.next_cursor.is_none(), "one page holds four files");
        assert_eq!(page.items[0].name, "a.jpg");
        assert_eq!(
            page.items[0].path,
            root.join("photos")
                .join("2024")
                .join("a.jpg")
                .display()
                .to_string()
        );
        assert!(page
            .items
            .windows(2)
            .all(|pair| pair[0].size_alloc >= pair[1].size_alloc));
    }

    #[test]
    fn bad_input_is_an_error_never_a_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state(&dir);
        let recorder: Arc<dyn Emit> = Arc::new(Recorder::default());

        for roots in [
            vec![],
            vec!["relative/path".to_owned()],
            vec![String::new()],
            vec!["/a".to_owned(), "/b".to_owned()],
            vec![dir.path().join("missing").display().to_string()],
        ] {
            assert!(
                start_scan(&state, Arc::clone(&recorder), roots.clone()).is_err(),
                "{roots:?}"
            );
        }

        assert!(scan_status(&state, 999).is_err());
        assert!(get_overview(&state, 999).is_err());
        assert!(get_categories(&state, 999).is_err());
        assert!(get_children(&state, 999, 1, None).is_err());
        assert!(get_largest(&state, 999, 0, None).is_err());
        assert!(cancel_scan(&state, 999).is_err());

        let root = tree(&dir);
        let (scan_id, _) = scan(&state, &root);
        assert!(
            get_children(&state, scan_id, -5, None).is_err(),
            "an id outside the scan"
        );
        assert!(get_children(&state, scan_id, i64::MAX, Some(0)).is_err());
    }

    /// A Trash that is a folder in the test's temp dir: never the real one.
    #[cfg(unix)]
    struct FolderBin {
        dir: PathBuf,
        /// Called on each move, after it.
        after: Box<dyn Fn() + Send + Sync>,
    }

    #[cfg(unix)]
    impl FolderBin {
        fn new(dir: &tempfile::TempDir) -> Self {
            let trash = dir.path().join("Trash");
            fs::create_dir_all(&trash).expect("trash");
            Self {
                dir: trash,
                after: Box::new(|| {}),
            }
        }
    }

    #[cfg(unix)]
    impl storagelens_safety::Bin for FolderBin {
        fn trash(&self, path: &Path) -> std::io::Result<()> {
            let name = path
                .file_name()
                .expect("name")
                .to_string_lossy()
                .into_owned();
            let mut target = self.dir.join(&name);
            let mut n = 1;
            while fs::symlink_metadata(&target).is_ok() {
                n += 1;
                target = self.dir.join(format!("{name} {n}"));
            }
            fs::rename(path, target)?;
            (self.after)();
            Ok(())
        }
        fn locate(
            &self,
            wanted: &[storagelens_safety::Wanted<'_>],
        ) -> std::io::Result<Vec<Option<String>>> {
            storagelens_safety::bin::locate_in(std::slice::from_ref(&self.dir), wanted)
        }
        fn restore(&self, location: &str, original: &Path) -> std::io::Result<()> {
            storagelens_safety::bin::restore_by_rename(Path::new(location), original)
        }
    }

    /// A file last changed 400 days ago: old enough for every stale rule.
    #[cfg(unix)]
    fn old_file(path: &Path, bytes: usize) {
        fs::write(path, vec![4u8; bytes]).expect("file");
        let file = fs::File::options().write(true).open(path).expect("open");
        let then = std::time::SystemTime::now() - std::time::Duration::from_secs(400 * 86_400);
        file.set_modified(then).expect("mtime");
    }

    /// The scan fixture plus two Python caches the built-in rules suggest.
    #[cfg(unix)]
    fn cleanup_fixture(dir: &tempfile::TempDir) -> (Arc<AppState>, i64, PathBuf) {
        let state = state(dir);
        let root = tree(dir);
        for (folder, bytes) in [("tool", 40_000), ("lib", 9_000)] {
            fs::create_dir_all(root.join(folder).join("__pycache__")).expect("dirs");
            old_file(&root.join(folder).join("__pycache__/m.pyc"), bytes);
        }
        let (scan_id, _) = scan(&state, &root);
        (state, scan_id, root)
    }

    #[cfg(unix)]
    fn suggestions(state: &AppState, scan_id: i64) -> Vec<String> {
        get_rules(state, scan_id)
            .expect("rules")
            .groups
            .into_iter()
            .filter(|group| group.deletable)
            .flat_map(|group| group.items.into_iter().map(|item| item.path))
            .collect()
    }

    #[cfg(unix)]
    #[test]
    fn duplicates_keep_one_copy_trash_the_rest_and_undo() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state(&dir);
        let root = tree(&dir);
        let x: Vec<u8> = (0..1_500_000u32).map(|i| (i % 251) as u8).collect();
        let y: Vec<u8> = (0..1_200_000u32).map(|i| (i % 241) as u8).collect();
        fs::create_dir_all(root.join("sub")).expect("sub");
        for name in ["x1", "x2", "sub/x3"] {
            fs::write(root.join(name), &x).expect("x");
        }
        for name in ["y1", "y2"] {
            fs::write(root.join(name), &y).expect("y");
        }
        let (scan_id, _) = scan(&state, &root);
        let recorder = Recorder::default();
        let options = storagelens_analysis::dupes::Options {
            duty: 1.0,
            ..Default::default()
        };
        let throttled = std::sync::atomic::AtomicBool::new(false);
        let throttle = || throttled.store(true, Ordering::SeqCst);

        let found =
            find_duplicates(&state, &recorder, scan_id, false, options, &throttle).expect("find");
        assert!(
            throttled.load(Ordering::SeqCst),
            "the hashing thread is throttled"
        );
        assert!(!found.cancelled);
        assert!(!recorder.named(events::DUPES_PROGRESS).is_empty());
        assert_eq!(found.groups.len(), 2, "{:?}", found.groups);
        let path = |name: &str| root.join(name).display().to_string();
        let xs: Vec<String> = found.groups[0]
            .files
            .iter()
            .map(|f| f.path.clone())
            .collect();
        assert_eq!(xs, [path("sub/x3"), path("x1"), path("x2")]);
        assert_eq!(
            found.groups[0].reclaimable,
            2 * found.groups[0].files[0].size_alloc
        );
        assert!(found.groups[0].files.iter().all(|f| f.modified.is_some()));

        // The guard: never every copy of a group.
        let error = preview_duplicate_removal(&state, scan_id, xs.clone()).expect_err("all");
        assert!(error.contains("every copy"), "{error}");
        let error = preview_duplicate_removal(&state, scan_id, vec![path("notes.txt")])
            .expect_err("not a duplicate");
        assert!(error.contains("not a duplicate"), "{error}");

        let plan = preview_duplicate_removal(&state, scan_id, vec![path("x1"), path("sub/x3")])
            .expect("preview");
        assert_eq!(plan.items.len(), 2);
        let bin = FolderBin::new(&dir);
        let result = execute_deletion(
            &state,
            &recorder,
            &plan.plan_id,
            false,
            &bin,
            &platform_stamp,
        )
        .expect("execute");
        assert_eq!(result.trashed, 2, "{:?}", result.items);
        assert!(root.join("x2").exists(), "the kept copy stays");

        // Cached: no rehash. The x group has one copy left and is gone.
        let never = || panic!("cached results do not rehash or throttle");
        let after =
            find_duplicates(&state, &recorder, scan_id, false, options, &never).expect("again");
        assert_eq!(after.groups.len(), 1);
        assert_eq!(after.groups[0].files.len(), 2, "the y group is untouched");
        // x2 is now the only copy: trashing it is refused.
        let error = preview_duplicate_removal(&state, scan_id, vec![path("x2")]).expect_err("last");
        assert!(
            error.contains("not a duplicate") || error.contains("every copy"),
            "{error}"
        );

        let undone = undo_deletion(&state, &result.manifest_id, &bin).expect("undo");
        assert_eq!(undone.restored, 2);
        let back =
            find_duplicates(&state, &recorder, scan_id, false, options, &never).expect("back");
        assert_eq!(
            back.groups.len(),
            2,
            "restored copies make the group whole again"
        );
    }

    #[cfg(unix)]
    #[test]
    fn apps_list_preview_trash_and_undo_through_the_cleanup_path() {
        use storagelens_core::{AppInfo, AppStatus, Bytes, Satellite, Signature};

        let dir = tempfile::tempdir().expect("tempdir");
        let state = state(&dir);
        let root = tree(&dir);
        let bundle = root.join("Foo.app");
        let data = root.join("Support/com.example.foo");
        fs::create_dir_all(bundle.join("Contents")).expect("bundle");
        fs::write(bundle.join("Contents/Info.plist"), b"x").expect("plist");
        fs::create_dir_all(&data).expect("data");
        fs::write(data.join("db"), vec![0u8; 10_000]).expect("db");
        let (scan_id, _) = scan(&state, &root);
        let bin = FolderBin::new(&dir);
        let recorder = Recorder::default();

        let app = |name: &str, path: PathBuf| AppInfo {
            id: format!("com.example.{name}"),
            name: name.to_owned(),
            version: None,
            path,
            bytes: Bytes::new(4_096),
            last_used: Some(0),
            status: AppStatus::Unknown,
            use_count: Some(1),
            signature: Signature::Valid,
            running: false,
            autostart: false,
            default_handler: false,
            system: false,
            satellites: Vec::new(),
            uninstaller: None,
            registry_keys: Vec::new(),
        };
        let inventory = || {
            let mut foo = app("Foo", bundle.clone());
            foo.satellites = vec![Satellite {
                path: data.clone(),
                bytes: Bytes::new(12_288),
                strong: true,
            }];
            foo.uninstaller = Some("/Applications/Foo/Uninstall Foo.app".into());
            foo.registry_keys = vec![
                storagelens_core::RegistryKey {
                    path: r"HKEY_CURRENT_USER\Software\Acme\Foo".into(),
                    strong: true,
                },
                storagelens_core::RegistryKey {
                    path: r"HKEY_LOCAL_MACHINE\SOFTWARE\Foo".into(),
                    strong: false,
                },
                storagelens_core::RegistryKey {
                    path: r"HKEY_CURRENT_USER\Software\Gone".into(),
                    strong: false,
                },
            ];
            let mut busy = app("Busy", root.join("Busy.app"));
            busy.running = true;
            let mut mail = app("Mail", PathBuf::from("/System/Applications/Mail.app"));
            mail.system = true;
            vec![foo, busy, mail]
        };

        let rows = list_apps(&state, false, &inventory).expect("list");
        let names: Vec<&str> = rows.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names, ["Foo", "Busy"], "system apps are excluded entirely");
        let foo = &rows[0];
        assert_eq!(foo.status, AppStatus::Unused, "classified on the way out");
        assert_eq!(foo.total_bytes, 4_096 + 12_288);

        let busy = root.join("Busy.app").display().to_string();
        let error = preview_app_removal(&state, scan_id, &busy, false, false, &platform_stamp)
            .expect_err("running");
        assert!(error.contains("running"), "{error}");
        let error = preview_app_removal(
            &state,
            scan_id,
            "/System/Applications/Mail.app",
            false,
            true,
            &platform_stamp,
        )
        .expect_err("system");
        assert!(
            error.contains("not in the app list") || error.contains("system"),
            "{error}"
        );

        // Force quit (#113): only a listed, non-system app; a clean quit
        // clears the cached running flag.
        // Uninstall (#115): only an app that has an uninstaller, by its own command.
        assert!(rows[0].has_uninstaller && !rows[1].has_uninstaller);
        let ran = std::cell::RefCell::new(String::new());
        uninstall_app(&state, "com.example.Foo", &|command| {
            ran.replace(command.to_owned());
            Ok(())
        })
        .expect("uninstall");
        assert_eq!(*ran.borrow(), "/Applications/Foo/Uninstall Foo.app");
        assert!(uninstall_app(&state, "com.example.Busy", &|_| Ok(())).is_err());

        // Leftovers (#120): finished once the app is gone from the system's list;
        // then the bundle (if still there) and its data, only those paths.
        assert!(!uninstall_finished(&state, "com.example.Foo", &|_| true).expect("waiting"));
        assert!(uninstall_finished(&state, "com.example.Foo", &|_| false).expect("done"));
        let leftovers = app_leftovers(&state, "com.example.Foo").expect("leftovers");
        let kinds: Vec<(&str, bool)> = leftovers
            .iter()
            .map(|leftover| (leftover.kind.as_str(), leftover.strong))
            .collect();
        assert_eq!(kinds, [("install", true), ("data", true)]);
        let chosen = vec![data.display().to_string()];
        let plan =
            preview_leftover_removal(&state, scan_id, "com.example.Foo", &chosen, &platform_stamp)
                .expect("plan");
        assert_eq!(plan.items.len(), 1);
        assert_eq!(plan.items[0].rule_id, "app.data");
        // Registry leftovers (#121): only existing keys; machine-wide ones need
        // elevation; each is backed up before it is deleted; restore takes only
        // our own backups.
        let exists = |key: &str| !key.ends_with("Gone");
        let keys = registry_leftovers(&state, "com.example.Foo", &exists, false).expect("keys");
        let listed: Vec<(bool, bool)> = keys.iter().map(|k| (k.strong, k.can_remove)).collect();
        assert_eq!(listed, [(true, true), (false, false)]);
        let backups = std::cell::RefCell::new(Vec::new());
        let removal = remove_registry_leftovers(
            &state,
            "com.example.Foo",
            &[
                r"HKEY_CURRENT_USER\Software\Acme\Foo".to_owned(),
                r"HKEY_LOCAL_MACHINE\SOFTWARE\Foo".to_owned(),
                r"HKEY_CURRENT_USER\Software\Microsoft".to_owned(),
            ],
            &exists,
            false,
            &|_, backup| {
                backups.borrow_mut().push(backup.to_path_buf());
                Ok(())
            },
        )
        .expect("remove");
        assert_eq!(removal.removed.len(), 1);
        assert_eq!(removal.failed.len(), 2, "not elevated, and not a leftover");
        assert_eq!(
            backups.borrow().len(),
            1,
            "only the allowed key was touched"
        );
        let backup = removal.removed[0].backup.clone();
        assert!(backup.ends_with("HKEY_CURRENT_USER_Software_Acme_Foo.reg"));
        assert_eq!(
            restore_registry_backups(&state, &[backup], &|_| Ok(())).expect("restore"),
            1
        );
        assert!(restore_registry_backups(&state, &["/etc/x.reg".to_owned()], &|_| Ok(())).is_err());

        // System leftovers (#123): listed from the platform, removable only as
        // listed, into a folder in the Trash, behind one admin prompt.
        let find = |_: &Path, _: &str| -> Vec<SystemItem> {
            vec![
                (
                    "/Library/LaunchDaemons/com.example.weak.plist".into(),
                    "helper".into(),
                    false,
                ),
                (
                    "/Library/LaunchDaemons/com.example.Foo.plist".into(),
                    "helper".into(),
                    true,
                ),
                ("com.example.Foo.pkg".into(), "receipt".into(), true),
            ]
        };
        let system = system_leftovers(&state, "com.example.Foo", &find).expect("system");
        assert!(system[0].strong && !system[2].strong, "strong first");
        let removed_items = std::cell::RefCell::new(0usize);
        let done = remove_system_leftovers(
            &state,
            "com.example.Foo",
            &["com.example.Foo.pkg".to_owned()],
            &find,
            &|items, trash, _| {
                assert!(trash.starts_with(home_folder().expect("home").join(".Trash")));
                removed_items.replace(items.len());
                Ok(())
            },
        )
        .expect("remove");
        assert_eq!((done.removed, *removed_items.borrow()), (1, 1));
        assert!(remove_system_leftovers(
            &state,
            "com.example.Foo",
            &["/etc/passwd".to_owned()],
            &find,
            &|_, _, _| Ok(()),
        )
        .is_err());

        // A Store app (#122): no Trash path, no install-folder leftover.
        {
            let mut apps = lock(&state.apps).expect("apps");
            let foo = apps
                .as_mut()
                .and_then(|apps| apps.iter_mut().find(|app| app.id == "com.example.Foo"))
                .expect("foo");
            foo.id = "msix:Foo_1_x64__p".into();
        }
        let shown_foo = bundle.display().to_string();
        assert!(
            preview_app_removal(&state, scan_id, &shown_foo, false, false, &platform_stamp)
                .expect_err("store")
                .contains("Microsoft Store")
        );
        let store_left = app_leftovers(&state, "msix:Foo_1_x64__p").expect("leftovers");
        assert!(store_left.iter().all(|leftover| leftover.kind != "install"));
        lock(&state.apps)
            .expect("apps")
            .as_mut()
            .and_then(|apps| apps.iter_mut().find(|app| app.id.starts_with("msix:")))
            .expect("store foo")
            .id = "com.example.Foo".into();

        let foreign = vec![root.display().to_string()];
        assert!(preview_leftover_removal(
            &state,
            scan_id,
            "com.example.Foo",
            &foreign,
            &platform_stamp
        )
        .is_err());

        let quit = force_quit_app(&state, &busy, &|_| (2, 0)).expect("quit");
        assert_eq!((quit.stopped, quit.failed), (2, 0));
        let rows = list_apps(&state, false, &|| unreachable!("cached")).expect("list");
        assert!(rows.iter().all(|row| !row.running));
        assert!(force_quit_app(&state, "/System/Applications/Mail.app", &|_| (0, 0)).is_err());
        assert!(force_quit_app(&state, "/tmp/anything", &|_| (0, 0)).is_err());

        let shown = bundle.display().to_string();
        let app_only = preview_app_removal(&state, scan_id, &shown, false, false, &platform_stamp)
            .expect("app");
        assert_eq!(app_only.items.len(), 1);
        let plan = preview_app_removal(&state, scan_id, &shown, true, false, &platform_stamp)
            .expect("all");
        assert_eq!(plan.items.len(), 2);
        assert!(
            plan.items[1].title.contains("estimated"),
            "{:?}",
            plan.items[1]
        );

        let result = execute_deletion(
            &state,
            &recorder,
            &plan.plan_id,
            false,
            &bin,
            &platform_stamp,
        )
        .expect("execute");
        assert_eq!(result.trashed, 2, "{:?}", result.items);
        assert!(!bundle.exists() && !data.exists());
        let rows = list_apps(&state, false, &|| unreachable!("cached")).expect("list");
        assert!(
            rows.iter().all(|row| row.name != "Foo"),
            "removed apps are hidden"
        );

        let undone = undo_deletion(&state, &result.manifest_id, &bin).expect("undo");
        assert_eq!(undone.restored, 2);
        assert!(bundle.exists() && data.exists());
        let rows = list_apps(&state, false, &|| unreachable!("cached")).expect("list");
        assert!(
            rows.iter().any(|row| row.name == "Foo"),
            "restored apps come back"
        );
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_previews_trashes_and_undoes_exactly_what_the_receipt_lists() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, scan_id, root) = cleanup_fixture(&dir);
        let bin = FolderBin::new(&dir);
        let recorder = Recorder::default();

        let cleanup = get_rules(&state, scan_id).expect("rules");
        assert!(cleanup.problems.is_empty(), "{:?}", cleanup.problems);
        let cache = cleanup
            .groups
            .iter()
            .find(|group| group.rule_id == "dev.python_bytecode")
            .expect("card");
        assert!(cache.deletable && cache.kind == "regenerable" && cache.risk == "safe");
        assert_eq!(cache.items.len(), 2);
        assert!(
            cache.items[0].bytes >= cache.items[1].bytes,
            "biggest first"
        );
        let paths = suggestions(&state, scan_id);

        // The receipt rule: no preview, no execution.
        let error = execute_deletion(
            &state,
            &recorder,
            "plan-made-up",
            false,
            &bin,
            &platform_stamp,
        )
        .expect_err("no receipt");
        assert!(error.contains("preview_deletion"), "{error}");
        // Only real suggestions, each once, never one inside another.
        assert!(preview_deletion(&state, scan_id, vec![]).is_err());
        assert!(preview_deletion(&state, scan_id, vec!["relative".into()]).is_err());
        assert!(
            preview_deletion(&state, scan_id, vec![paths[0].clone(), paths[0].clone()]).is_err()
        );
        let notes = root.join("notes.txt").display().to_string();
        let error = preview_deletion(&state, scan_id, vec![notes]).expect_err("not a suggestion");
        assert!(error.contains("not a cleanup suggestion"), "{error}");
        let inside = format!("{}/m.pyc", paths[0]);
        let error =
            preview_deletion(&state, scan_id, vec![paths[0].clone(), inside]).expect_err("overlap");
        assert!(error.contains("overlap"), "{error}");

        let plan = preview_deletion(&state, scan_id, paths.clone()).expect("preview");
        assert!(!plan.needs_confirmation);
        assert_eq!(
            plan.total_bytes,
            plan.items.iter().map(|item| item.bytes).sum::<u64>()
        );
        assert!(plan
            .items
            .iter()
            .all(|item| item.title == "Python bytecode caches"));

        let result = execute_deletion(
            &state,
            &recorder,
            &plan.plan_id,
            false,
            &bin,
            &platform_stamp,
        )
        .expect("execute");
        assert_eq!(result.trashed, 2, "{:?}", result.items);
        assert_eq!(result.trashed_bytes, plan.total_bytes);
        assert!(!recorder.named(events::CLEANUP_PROGRESS).is_empty());
        for path in &paths {
            assert!(!Path::new(path).exists(), "{path} still there");
        }

        // The receipt and the manifest list the same items, in the same order.
        let manifest =
            storagelens_safety::Manifest::read(&state.manifests_dir().join(&result.manifest_id))
                .expect("manifest");
        let in_manifest: Vec<(String, u64, Option<String>)> = manifest
            .items
            .iter()
            .map(|item| {
                (
                    item.path.display().to_string(),
                    item.bytes,
                    item.rule_id.clone(),
                )
            })
            .collect();
        let on_receipt: Vec<(String, u64, Option<String>)> = plan
            .items
            .iter()
            .map(|item| (item.path.clone(), item.bytes, Some(item.rule_id.clone())))
            .collect();
        assert_eq!(in_manifest, on_receipt);

        // A receipt is used once; moved items leave the screen; the log has them.
        assert!(execute_deletion(
            &state,
            &recorder,
            &plan.plan_id,
            false,
            &bin,
            &platform_stamp
        )
        .is_err());
        assert!(suggestions(&state, scan_id).is_empty());
        let log = lock(&state.catalog)
            .expect("catalog")
            .trash_log()
            .expect("log");
        assert_eq!(log.len(), 2);

        let undone = undo_deletion(&state, &result.manifest_id, &bin).expect("undo");
        assert_eq!(undone.restored, 2, "{:?}", undone.items);
        for path in &paths {
            assert!(Path::new(path).exists(), "{path} not restored");
        }
        assert_eq!(suggestions(&state, scan_id).len(), 2, "back on the screen");
        let log = lock(&state.catalog)
            .expect("catalog")
            .trash_log()
            .expect("log");
        assert!(log.iter().all(|row| row.restored_at.is_some()));
        for bad in ["../catalog.db", "trash-x/../../y.json", "notes.json"] {
            assert!(undo_deletion(&state, bad, &bin).is_err(), "{bad}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_cancelled_or_changed_item_is_explained_and_nothing_else_moves() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, scan_id, _) = cleanup_fixture(&dir);
        let recorder = Recorder::default();
        let paths = suggestions(&state, scan_id);

        // Changed after the scan: skipped, with the reason.
        let plan = preview_deletion(&state, scan_id, paths.clone()).expect("preview");
        fs::write(format!("{}/new.pyc", paths[1]), b"written after the scan").expect("write");
        // Times are compared to the second; make the change visible even
        // when the test runs within the scan's second.
        fs::File::open(&paths[1])
            .and_then(|folder| {
                folder.set_modified(std::time::SystemTime::now() + Duration::from_secs(60))
            })
            .expect("touch");
        let bin = FolderBin::new(&dir);
        let result = execute_deletion(
            &state,
            &recorder,
            &plan.plan_id,
            false,
            &bin,
            &platform_stamp,
        )
        .expect("execute");
        let statuses: Vec<&str> = result
            .items
            .iter()
            .map(|item| item.status.as_str())
            .collect();
        assert_eq!(statuses, ["trashed", "skipped"]);
        assert!(
            result.items[1]
                .reason
                .as_deref()
                .is_some_and(|why| why.contains("modified")),
            "{:?}",
            result.items[1]
        );
        assert!(Path::new(&paths[1]).exists());
        undo_deletion(&state, &result.manifest_id, &bin).expect("undo");

        // Cancelled during the first move: the second never starts.
        let plan = preview_deletion(&state, scan_id, vec![paths[0].clone()]).expect("preview");
        let plan_two =
            preview_deletion(&state, scan_id, vec![paths[0].clone()]).expect("second receipt");
        let shared = Arc::clone(&state);
        let mut bin = FolderBin::new(&dir);
        bin.after = Box::new(move || cancel_deletion(&shared));
        let first = execute_deletion(
            &state,
            &recorder,
            &plan.plan_id,
            false,
            &bin,
            &platform_stamp,
        )
        .expect("execute");
        assert_eq!(first.trashed, 1);
        // A new deletion starts uncancelled; this one finds its item gone.
        let second = execute_deletion(
            &state,
            &recorder,
            &plan_two.plan_id,
            false,
            &bin,
            &platform_stamp,
        )
        .expect("execute");
        assert_eq!(second.items[0].status, "skipped");
        assert!(second.items[0]
            .reason
            .as_deref()
            .is_some_and(|why| why.contains("no longer there")));
    }

    #[cfg(unix)]
    #[test]
    fn cancelling_mid_way_leaves_the_rest_untouched() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (state, scan_id, _) = cleanup_fixture(&dir);
        let recorder = Recorder::default();
        let paths = suggestions(&state, scan_id);
        let plan = preview_deletion(&state, scan_id, paths.clone()).expect("preview");
        let shared = Arc::clone(&state);
        let mut bin = FolderBin::new(&dir);
        bin.after = Box::new(move || cancel_deletion(&shared));
        let result = execute_deletion(
            &state,
            &recorder,
            &plan.plan_id,
            false,
            &bin,
            &platform_stamp,
        )
        .expect("execute");
        let statuses: Vec<&str> = result
            .items
            .iter()
            .map(|item| item.status.as_str())
            .collect();
        assert_eq!(statuses, ["trashed", "cancelled"]);
        assert!(Path::new(&paths[1]).exists());
        assert_eq!(
            suggestions(&state, scan_id),
            [paths[1].clone()],
            "only the moved one leaves"
        );
        let undone = undo_deletion(&state, &result.manifest_id, &bin).expect("undo");
        assert_eq!(undone.restored, 1);
        assert_eq!(suggestions(&state, scan_id).len(), 2);
    }

    #[test]
    fn plans_over_5_gb_need_a_second_confirmation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state(&dir);
        let root = tree(&dir);
        let (scan_id, _) = scan(&state, &root);
        let recorder = Recorder::default();
        let dto = DeletionPlanDto {
            plan_id: "plan-big".to_owned(),
            scan_id,
            items: Vec::new(),
            total_bytes: CONFIRM_ABOVE + 1,
            needs_confirmation: true,
            created_at: 0,
        };
        lock(&state.plans).expect("plans").insert(
            dto.plan_id.clone(),
            StoredPlan {
                dto,
                items: Vec::new(),
            },
        );
        let home = home_dir().unwrap_or_default();
        let bin = system_bin(Path::new(&home));
        let error = execute_deletion(&state, &recorder, "plan-big", false, &bin, &platform_stamp)
            .expect_err("one click is not enough");
        assert!(error.contains("second confirmation"), "{error}");
        // The receipt survives the refusal; the second click goes through (and,
        // with nothing on it, moves nothing).
        let result = execute_deletion(&state, &recorder, "plan-big", true, &bin, &platform_stamp)
            .expect("confirmed");
        assert_eq!(result.trashed, 0);
    }

    #[test]
    fn progress_events_are_throttled() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state(&dir);
        let root = fs::canonicalize(dir.path())
            .expect("canonical")
            .join("many");
        for index in 0..300 {
            let sub = root.join(format!("d{index}"));
            fs::create_dir_all(&sub).expect("dir");
            for file in 0..20 {
                fs::write(sub.join(format!("f{file}")), b"x").expect("file");
            }
        }
        let text = root.display().to_string();
        let root = PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text));

        let started = Instant::now();
        let (_, recorder) = scan(&state, &root);
        let elapsed = started.elapsed();
        let progress = recorder.named(events::SCAN_PROGRESS).len();

        // One event per 100 ms at most, plus the final snapshot.
        let ceiling = (elapsed.as_millis() / 100) as usize + 2;
        assert!(
            progress <= ceiling,
            "{progress} progress events in {elapsed:?} (ceiling {ceiling})"
        );
    }

    #[test]
    fn only_one_scan_runs_at_a_time() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state(&dir);
        let root = tree(&dir);

        // Pretend a scan is running.
        *state.running.lock().expect("lock") = Some(RunningScan {
            scan_id: 41,
            root: root.clone(),
            cancel: CancelToken::new(),
            progress: Arc::new(Mutex::new(Progress::default())),
        });
        let recorder: Arc<dyn Emit> = Arc::new(Recorder::default());
        let error =
            start_scan(&state, recorder, vec![root.display().to_string()]).expect_err("busy");
        assert!(error.contains("still running"), "{error}");

        assert_eq!(
            scan_status(&state, 41).expect("live").status,
            ScanStatus::Running
        );
        assert!(cancel_scan(&state, 41).is_ok());
        assert!(get_overview(&state, 41).is_err(), "no arena while running");
    }

    #[test]
    fn paths_stay_absolute_after_a_restart() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = tree(&dir);
        let scan_id = {
            let state = state(&dir);
            scan(&state, &root).0
        };

        // A fresh state over the same catalog: nothing carried over in memory.
        let reopened = state(&dir);
        let status = scan_status(&reopened, scan_id).expect("status");
        assert_eq!(
            status.root.as_deref(),
            Some(root.display().to_string().as_str())
        );

        let page = get_largest(&reopened, scan_id, 0, None).expect("largest");
        assert_eq!(
            page.items[0].path,
            root.join("photos")
                .join("2024")
                .join("a.jpg")
                .display()
                .to_string(),
            "the root comes from the catalog, not this run's memory"
        );
    }

    fn sum_check(node: &TreeNodeDto) {
        if let Some(children) = &node.children {
            let total: u64 = children.iter().map(|child| child.size_alloc).sum();
            assert_eq!(
                total + node.remaining_bytes,
                node.size_alloc,
                "{}: children and remainder account for every byte",
                node.name
            );
            children.iter().for_each(sum_check);
        }
    }

    #[test]
    fn get_tree_nests_and_keeps_every_byte() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state(&dir);
        let root = tree(&dir);
        let (scan_id, _) = scan(&state, &root);
        let root_id = get_overview(&state, scan_id).expect("overview").root_id;

        let full = get_tree(&state, scan_id, root_id, None).expect("tree");
        assert_eq!(full.name, "scanned");
        sum_check(&full);
        let children = full.children.as_ref().expect("root expanded");
        assert_eq!(children[0].name, "photos", "biggest first");
        let photos = children[0].children.as_ref().expect("photos expanded");
        let year = photos.iter().find(|c| c.name == "2024").expect("2024");
        assert_eq!(
            year.children.as_ref().expect("2024 expanded")[0].name,
            "a.jpg"
        );

        let shallow = get_tree(&state, scan_id, root_id, Some(1)).expect("shallow");
        sum_check(&shallow);
        let top = shallow.children.as_ref().expect("root expanded");
        assert!(
            top.iter().all(|child| child.children.is_none()),
            "one level only"
        );
        assert!(top[0].has_children, "an unexpanded directory still says so");

        assert!(get_tree(&state, scan_id, i64::MAX, None).is_err());
    }

    #[test]
    fn reveal_resolves_catalogued_entries_and_refuses_missing_ones() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state(&dir);
        let root = tree(&dir);
        let (scan_id, _) = scan(&state, &root);
        let page = get_largest(&state, scan_id, 0, None).expect("largest");
        let jpg = page
            .items
            .iter()
            .find(|row| row.name == "a.jpg")
            .expect("a.jpg");

        let path = existing_path(&state, scan_id, jpg.id).expect("resolves");
        assert_eq!(path, root.join("photos").join("2024").join("a.jpg"));

        fs::remove_file(&path).expect("remove");
        let error = existing_path(&state, scan_id, jpg.id).expect_err("gone");
        assert!(error.contains("no longer exists"), "{error}");
        assert!(reveal_entry(&state, scan_id, jpg.id).is_err());
        assert!(preview_entry(&state, scan_id, jpg.id).is_err());
        assert!(existing_path(&state, scan_id, i64::MAX).is_err());
    }

    #[test]
    fn scan_targets_are_absolute_and_scannable_paths() {
        let targets = list_scan_targets();
        for target in &targets {
            assert!(
                Path::new(&target.path).is_absolute(),
                "{} is not absolute",
                target.path
            );
            assert!(!target.label.is_empty());
            if target.kind == ScanTargetKind::Volume {
                assert!(target.total.is_some() && target.available.is_some());
            }
        }
        let homes = targets
            .iter()
            .filter(|target| target.kind == ScanTargetKind::Home)
            .count();
        assert!(homes <= 1, "at most one home folder");
        let startups = targets.iter().filter(|target| target.startup).count();
        assert!(startups <= 1, "at most one startup disk");
        if startups == 1 {
            assert!(targets[0].startup, "the startup disk comes first");
            assert_eq!(targets[0].kind, ScanTargetKind::Volume);
        }
        #[cfg(target_os = "macos")]
        assert_eq!(targets[0].path, "/", "the startup disk is listed on macOS");
        if homes == 1 {
            let at = usize::from(startups == 1);
            assert_eq!(
                targets[at].kind,
                ScanTargetKind::Home,
                "home right after it"
            );
        }
    }

    #[test]
    fn get_files_slices_every_sort_by_position() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state(&dir);
        let root = tree(&dir);
        let (scan_id, _) = scan(&state, &root);

        let all = get_files(&state, scan_id, FileSortDto::Size, false, 0, 0, 200).expect("all");
        assert_eq!(all.total, 4);
        let names: Vec<&str> = all.items.iter().map(|row| row.name.as_str()).collect();
        assert_eq!(names[0], "a.jpg", "largest first");
        assert_eq!(
            all.items[0].path,
            root.join("photos")
                .join("2024")
                .join("a.jpg")
                .display()
                .to_string()
        );
        assert!(all.items.iter().all(|row| row.modified.is_some()));

        let reversed = get_files(&state, scan_id, FileSortDto::Size, true, 0, 0, 200).expect("rev");
        let mut backwards: Vec<i64> = all.items.iter().map(|row| row.id).collect();
        backwards.reverse();
        assert_eq!(
            reversed.items.iter().map(|row| row.id).collect::<Vec<_>>(),
            backwards
        );

        let page = get_files(&state, scan_id, FileSortDto::Size, false, 0, 1, 2).expect("page");
        assert_eq!(page.start, 1);
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].id, all.items[1].id);

        let big =
            get_files(&state, scan_id, FileSortDto::Size, false, 100_000, 0, 200).expect("min");
        assert_eq!(big.total, 1, "only a.jpg is ≥ 100 kB");

        for sort in [FileSortDto::Modified, FileSortDto::Type] {
            let sorted = get_files(&state, scan_id, sort, false, 0, 0, 200).expect("sorted");
            assert_eq!(sorted.total, 4, "{sort:?}");
        }
        let past = get_files(&state, scan_id, FileSortDto::Type, false, 0, 99, 200).expect("past");
        assert!(past.items.is_empty());
        assert!(get_files(&state, 999, FileSortDto::Size, false, 0, 0, 10).is_err());
    }

    #[test]
    fn search_finds_files_and_folders_with_paths_and_ancestors() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state(&dir);
        let root = tree(&dir);
        let (scan_id, _) = scan(&state, &root);

        let page = search(&state, scan_id, "a.j", None).expect("search");
        assert_eq!(page.items.len(), 1);
        let jpg = &page.items[0];
        assert_eq!(jpg.name, "a.jpg");
        assert_eq!(jpg.kind, EntryKind::File);
        assert_eq!(
            jpg.path,
            root.join("photos")
                .join("2024")
                .join("a.jpg")
                .display()
                .to_string()
        );
        assert!(page.next_cursor.is_none());

        let folders = search(&state, scan_id, "PHOTO", None).expect("folders");
        assert_eq!(folders.items[0].name, "photos");
        assert_eq!(folders.items[0].kind, EntryKind::Dir);
        assert!(
            folders.items[0].size_alloc >= 300_000,
            "a folder carries its subtree"
        );

        assert!(search(&state, scan_id, "nothing-like-this", None)
            .expect("empty")
            .items
            .is_empty());
        assert!(search(&state, scan_id, "", None)
            .expect("blank")
            .items
            .is_empty());

        let chain = get_ancestors(&state, scan_id, jpg.id).expect("ancestors");
        let names: Vec<&str> = chain.iter().map(|crumb| crumb.name.as_str()).collect();
        assert_eq!(names, ["scanned", "photos", "2024", "a.jpg"]);
        assert!(get_ancestors(&state, scan_id, i64::MAX).is_err());
    }

    fn tree_sum(node: &TreeNodeDto) -> u64 {
        node.children.as_ref().map_or(node.size_alloc, |children| {
            let total: u64 = children.iter().map(|child| child.size_alloc).sum();
            assert_eq!(
                total + node.remaining_bytes,
                node.size_alloc,
                "{}",
                node.name
            );
            children.iter().for_each(|child| {
                tree_sum(child);
            });
            node.size_alloc
        })
    }

    #[test]
    fn every_grouping_view_totals_the_scan_and_zooms_by_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state(&dir);
        let root = tree(&dir);
        let (scan_id, _) = scan(&state, &root);
        let total = scan_status(&state, scan_id).expect("status").bytes_alloc;

        for by in [
            Grouping::Folder,
            Grouping::Type,
            Grouping::App,
            Grouping::Age,
            Grouping::Size,
            Grouping::Cleanliness,
        ] {
            let view =
                get_grouping(&state, scan_id, by, FilterDto::default(), None, None).expect("view");
            assert_eq!(view.size_alloc, total, "{by:?}");
            tree_sum(&view);
        }

        let by_type = get_grouping(
            &state,
            scan_id,
            Grouping::Type,
            FilterDto::default(),
            None,
            Some(1),
        )
        .expect("type");
        let images = by_type
            .children
            .as_ref()
            .expect("expanded")
            .iter()
            .find(|child| child.name == "Images")
            .expect("images")
            .clone();
        assert!(images.id < 0, "synthetic groups have negative ids");
        let zoomed = get_grouping(
            &state,
            scan_id,
            Grouping::Type,
            FilterDto::default(),
            Some(images.id),
            None,
        )
        .expect("zoom");
        assert_eq!(zoomed.size_alloc, images.size_alloc);

        // Folder groups go by their entry id.
        let folders = get_grouping(
            &state,
            scan_id,
            Grouping::Folder,
            FilterDto::default(),
            None,
            Some(1),
        )
        .expect("folder");
        let photos = folders.children.as_ref().expect("expanded")[0].clone();
        assert_eq!(photos.name, "photos");
        assert!(photos.id > 0);
        let inside = get_grouping(
            &state,
            scan_id,
            Grouping::Folder,
            FilterDto::default(),
            Some(photos.id),
            None,
        )
        .expect("zoom folder");
        assert_eq!(inside.name, "photos");

        let filtered = get_grouping(
            &state,
            scan_id,
            Grouping::Size,
            FilterDto {
                min_size: Some(100_000),
                ..FilterDto::default()
            },
            None,
            None,
        )
        .expect("filtered");
        assert!(filtered.size_alloc >= 300_000 && filtered.size_alloc < total);

        // "Within" a folder keeps only that folder's files, in any view.
        let within = |id: i64| FilterDto {
            within: Some(id),
            ..FilterDto::default()
        };
        let in_photos = get_grouping(
            &state,
            scan_id,
            Grouping::Type,
            within(photos.id),
            None,
            None,
        )
        .expect("within photos");
        assert_eq!(in_photos.size_alloc, photos.size_alloc);
        let root_id = folders.id;
        let everywhere = get_grouping(&state, scan_id, Grouping::Age, within(root_id), None, None)
            .expect("within root");
        assert_eq!(everywhere.size_alloc, total);
        assert!(
            get_grouping(&state, scan_id, Grouping::Type, within(-1), None, None).is_err(),
            "a group id is not a folder"
        );

        assert!(get_grouping(
            &state,
            scan_id,
            Grouping::Type,
            FilterDto::default(),
            Some(-9999),
            None
        )
        .is_err());
    }

    /// Bytes per Cleanliness bucket of `scan_id`.
    fn cleanliness(state: &AppState, scan_id: i64) -> Vec<(String, u64)> {
        let view = get_grouping(
            state,
            scan_id,
            Grouping::Cleanliness,
            FilterDto::default(),
            None,
            Some(1),
        )
        .expect("cleanliness");
        view.children
            .expect("expanded")
            .into_iter()
            .map(|child| (child.name, child.size_alloc))
            .collect()
    }

    #[test]
    fn cleanliness_follows_the_rules_and_reloads_them() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state(&dir);
        let root = tree(&dir);
        fs::create_dir_all(root.join("tool/__pycache__")).expect("dirs");
        old_file(&root.join("tool/__pycache__/m.pyc"), 40_000);
        let (scan_id, _) = scan(&state, &root);
        let total = scan_status(&state, scan_id).expect("status").bytes_alloc;

        let buckets = cleanliness(&state, scan_id);
        let safe = buckets
            .iter()
            .find(|(name, _)| name == "Safe to remove")
            .expect("safe")
            .1;
        assert!(safe >= 40_000, "{buckets:?}");
        assert_eq!(buckets.iter().map(|(_, bytes)| bytes).sum::<u64>(), total);
        let hits = lock(&state.catalog)
            .expect("catalog")
            .rule_hits(scan_id)
            .expect("hits");
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].rule_id, "dev.python_bytecode");
        assert!(hits[0].path.ends_with("__pycache__"));

        // A rule added to the rules folder applies on the next request.
        fs::create_dir_all(state.rules_dir()).expect("rules dir");
        fs::write(
            state.rules_dir().join("mine.json"),
            r#"[{"id": "mine.photos", "platform": "any", "title": "Photos", "glob": "**/photos",
                 "kind": "valuable", "explain": "Test rule."}]"#,
        )
        .expect("rule");
        let buckets = cleanliness(&state, scan_id);
        assert!(
            buckets
                .iter()
                .any(|(name, bytes)| name == "Keep" && *bytes >= 320_000),
            "{buckets:?}"
        );
        let hits = lock(&state.catalog)
            .expect("catalog")
            .rule_hits(scan_id)
            .expect("hits");
        assert_eq!(hits.len(), 2, "re-running replaced the rows: {hits:?}");
    }
}
