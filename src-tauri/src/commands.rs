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
//! # Not built yet
//!
//! Rules, deletion, apps, and duplicates are declared with
//! their final types and return [`not_built`] until their issues land. The
//! preview-before-execute rule is enforced **now**: `execute_deletion` refuses a
//! plan id that no `preview_deletion` produced (AGENTS.md invariant 10).
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
use storagelens_analysis::{FileFilter, SyntheticTree};
use storagelens_catalog::{
    file_order, modified_times, rollup, slice, Arena, Catalog, EntryWriter, FileSort, NodeIndex,
    Platform, ScanSession, SearchCursor, SizeCursor, DEFAULT_BATCH_SIZE, UNKNOWN_TIME,
};
use storagelens_core::{CancelToken, EntryKind, Progress, ScanOptions, ScanResult, ScanStatus};
use tauri::{AppHandle, State};

use crate::dto::{
    AppRowDto, CategoryDto, ChildDto, CrumbDto, DeletionPlanDto, DeletionResultDto, DupGroupDto,
    FileRangeDto, FileRowDto, FileSortDto, FilterDto, Grouping, OverviewDto, Page, RuleGroupDto,
    ScanStatusDto, ScanTargetDto, ScanTargetKind, SearchCursorDto, SizeCursorDto, TreeNodeDto,
    VolumeDto,
};
use crate::events::{
    self, Emit, ScanDoneEvent, ScanErrorEvent, ScanProgressEvent, Throttle, PROGRESS_INTERVAL,
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

/// The error for a command whose backend is not built yet.
pub fn not_built(feature: &str, issue: &str) -> String {
    format!("{feature} is not available yet (built in {issue})")
}

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
    tree: Arc<SyntheticTree>,
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
    catalog: Mutex<Catalog>,
    running: Mutex<Option<RunningScan>>,
    /// The most recently used arena.
    arena: Mutex<Option<Arc<Arena>>>,
    /// Final results of scans finished in this run, for `elevated` and skips.
    results: Mutex<HashMap<i64, ScanResult>>,
    /// Dry-run receipts, by plan id.
    plans: Mutex<HashMap<String, DeletionPlanDto>>,
    listing: Mutex<Option<Listing>>,
    grouping: Mutex<Option<GroupingCache>>,
    next_plan: AtomicU64,
}

impl AppState {
    /// Open (or create) the catalog at `catalog_path`.
    pub fn open(catalog_path: PathBuf) -> CommandResult<Self> {
        let catalog = Catalog::open(&catalog_path).map_err(|error| error.to_string())?;
        Ok(Self {
            catalog_path,
            catalog: Mutex::new(catalog),
            running: Mutex::new(None),
            arena: Mutex::new(None),
            results: Mutex::new(HashMap::new()),
            plans: Mutex::new(HashMap::new()),
            listing: Mutex::new(None),
            grouping: Mutex::new(None),
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
    if let Some(cached) = lock(&state.grouping)?.as_ref() {
        if cached.scan_id == scan_id && cached.by == by && &cached.filter == filter {
            return Ok(Arc::clone(&cached.tree));
        }
    }
    let times = cached_times(state, arena, scan_id)?;
    let root = state.root_of(scan_id);
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
    let input = storagelens_analysis::GroupingInput {
        arena,
        root_path: root.as_deref(),
        modified: Some(&times),
        now,
        // Rule verdicts arrive with the rules engine (#31).
        risk: None,
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
        tree: Arc::clone(&tree),
    });
    Ok(tree)
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
    let mut volumes = all_volumes();
    volumes.sort_by(|left, right| {
        left.removable
            .cmp(&right.removable)
            .then_with(|| right.total.get().cmp(&left.total.get()))
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
    });
    home.into_iter()
        .chain(volumes.into_iter().map(|volume| ScanTargetDto {
            kind: ScanTargetKind::Volume,
            label: volume.name,
            path: volume.mount_point.display().to_string(),
            total: Some(volume.total.get()),
            available: Some(volume.available.get()),
            removable: volume.removable,
        }))
        .collect()
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

// --- Deletion: the receipt rule is enforced now, execution comes with #30 -------

pub fn preview_deletion(state: &AppState, paths: Vec<String>) -> CommandResult<DeletionPlanDto> {
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

    let number = state.next_plan.fetch_add(1, Ordering::Relaxed);
    let plan = DeletionPlanDto {
        plan_id: format!("plan-{number}-{}", now_unix()),
        paths,
        created_at: now_unix(),
    };
    lock(&state.plans)?.insert(plan.plan_id.clone(), plan.clone());
    Ok(plan)
}

pub fn execute_deletion(state: &AppState, plan_id: &str) -> CommandResult<DeletionResultDto> {
    if !lock(&state.plans)?.contains_key(plan_id) {
        return Err(format!(
            "no dry-run receipt for plan '{plan_id}': call preview_deletion first"
        ));
    }
    Err(not_built("Moving files to the Trash", "#30"))
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
    pub async fn get_rules(scan_id: i64) -> CommandResult<Vec<RuleGroupDto>> {
        let _ = scan_id;
        Err(not_built("Cleanup rules", "#31"))
    }

    #[tauri::command]
    pub async fn preview_deletion(
        state: AppStateRef<'_>,
        paths: Vec<String>,
    ) -> CommandResult<DeletionPlanDto> {
        super::preview_deletion(state.inner(), paths)
    }

    #[tauri::command]
    pub async fn execute_deletion(
        state: AppStateRef<'_>,
        plan_id: String,
    ) -> CommandResult<DeletionResultDto> {
        super::execute_deletion(state.inner(), &plan_id)
    }

    #[tauri::command]
    pub async fn undo_deletion(manifest_id: String) -> CommandResult<()> {
        let _ = manifest_id;
        Err(not_built("Undo", "#30"))
    }

    #[tauri::command]
    pub async fn list_apps(scan_id: i64) -> CommandResult<Vec<AppRowDto>> {
        let _ = scan_id;
        Err(not_built("App inventory", "#36"))
    }

    #[tauri::command]
    pub async fn find_duplicates(scan_id: i64, min_size: u64) -> CommandResult<Vec<DupGroupDto>> {
        let _ = (scan_id, min_size);
        Err(not_built("Duplicate detection", "#39"))
    }

    #[tauri::command]
    pub async fn check_full_disk_access() -> CommandResult<DiskAccess> {
        Ok(super::check_full_disk_access())
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
        Arc::new(AppState::open(dir.path().join("catalog.db")).expect("open state"))
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

    #[test]
    fn execute_deletion_requires_a_preview() {
        let dir = tempfile::tempdir().expect("tempdir");
        let state = state(&dir);

        let error = execute_deletion(&state, "plan-made-up").expect_err("no receipt");
        assert!(error.contains("preview_deletion"), "{error}");

        assert!(preview_deletion(&state, vec![]).is_err());
        assert!(preview_deletion(&state, vec!["relative".to_owned()]).is_err());
        let absolute = dir.path().join("x").display().to_string();
        assert!(preview_deletion(&state, vec![absolute.clone(), absolute.clone()]).is_err());

        let plan = preview_deletion(&state, vec![absolute]).expect("preview");
        let error = execute_deletion(&state, &plan.plan_id).expect_err("not built yet");
        assert!(
            !error.contains("preview_deletion"),
            "a previewed plan passes the receipt check: {error}"
        );
        assert!(error.contains("#30"), "{error}");
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
        if homes == 1 {
            assert_eq!(targets[0].kind, ScanTargetKind::Home, "home comes first");
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

    #[test]
    fn unbuilt_features_name_their_issue() {
        assert_eq!(
            not_built("Search", "#24"),
            "Search is not available yet (built in #24)"
        );
    }
}
