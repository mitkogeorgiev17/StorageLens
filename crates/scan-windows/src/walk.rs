//! Parallel bulk-directory-query walker for the Windows scanner.
//!
//! One thread — the caller's — owns the [`EntrySink`] (it is `!Send`) and acts as
//! both writer and dispatcher. A fixed rayon pool enumerates directories, one
//! directory handle per task (see `ffi`), and streams [`Entry`] values back in
//! batches; the writer accepts them in arrival order, assigns arena indices, and
//! submits a task for every directory it accepts. A child directory therefore
//! cannot produce a row before its own row has been accepted. See `writer.rs`.
//!
//! # Sizing
//!
//! The pool is `min(cores, 8)` (PLAN §7.4), every worker runs at background I/O
//! priority, and the bounded channel caps the walker's own buffering at about 64k
//! entries.
//!
//! # Errors
//!
//! A directory that cannot be opened or read is reported in
//! [`ScanResult::skipped`] — `PermissionDenied` for `ERROR_ACCESS_DENIED`,
//! `IoError` otherwise — and the walk continues. Unelevated, parts of `C:\` are
//! unreadable; they are reported as "not scanned", never silently left out of the
//! total (PLAN §7.3). The result records whether the scan was elevated
//! (`ScanResult::elevated`), which is what labels those skips as gaps an
//! elevated relaunch may fill.
//!
//! # Sizes and identity (#13)
//!
//! - `size_alloc` is the record's `AllocationSize`, or `GetCompressedFileSizeW`
//!   for the compressed and sparse files whose record overstates it (see `ffi`).
//!   That query is the only per-file syscall, and only flagged files pay it.
//! - `file_id` is the NTFS file reference number and `dev` the volume serial.
//!   Hard links are deduped on it in the writer (see `dedupe.rs`) — on NTFS only,
//!   where the 64-bit id is guaranteed unique.
//! - `nlink` is not in the bulk listing. It is `1` until a second name for the
//!   same id is seen; that name is flagged `hardlink_dup` and given `nlink = 2`
//!   ("at least two"), which keeps core's "duplicates have `nlink > 1`" rule.

use std::fs;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::Arc;
use std::time::Instant;

use rayon::{ThreadPool, ThreadPoolBuilder};
use storagelens_core::{
    AppInfo, CancelToken, Entry, EntryKind, EntrySink, PlatformTarget, Progress, Result, ScanError,
    ScanOptions, ScanResult, SkipReason, SkippedDir, StorageScanner, Volume,
};

use crate::ffi::{
    self, compressed_size, filetime_to_unix, kind_of, needs_compressed_size, strip_verbatim,
    throttle_current_thread, DirReader, RawEntry, VolumeInfo,
};
use crate::skip::Rules;
use crate::writer::{self, Batch, Item, Message};

/// Upper bound on scan threads, per PLAN §7.4.
pub const MAX_THREADS: usize = 8;

/// The Windows [`StorageScanner`] implementation.
///
/// Construct it with the options for one scan; [`StorageScanner::scan`] borrows
/// the sink instead of storing it, because the sink cannot cross threads.
pub struct WinScanner {
    options: ScanOptions,
}

impl WinScanner {
    /// A scanner with explicit options.
    pub fn new(options: ScanOptions) -> Self {
        Self { options }
    }
}

impl Default for WinScanner {
    fn default() -> Self {
        Self::new(ScanOptions::default())
    }
}

impl StorageScanner for WinScanner {
    fn volumes(&self) -> Vec<Volume> {
        crate::ffi::drives()
    }

    fn scan(
        &self,
        roots: &[PathBuf],
        sink: &mut dyn EntrySink,
        cancel: CancelToken,
        progress: &(dyn Fn(Progress) + Send + Sync),
    ) -> Result<ScanResult> {
        let root = match roots {
            [] => return Err(ScanError::InvalidPath(PathBuf::new())),
            [root] => root,
            // A multi-root scan needs a synthetic arena root; the app scans one
            // volume per session, matching the macOS scanner.
            _ => {
                return Err(ScanError::Unsupported(
                    "scanning more than one root per pass",
                ))
            }
        };

        if root.as_os_str().is_empty() || !root.is_absolute() {
            return Err(ScanError::InvalidPath(root.clone()));
        }

        let root = normalize_root(root)?;
        scan_root(&root, &self.options, sink, cancel, progress)
    }

    fn platform_cleanup_targets(&self) -> Vec<PlatformTarget> {
        // Discovered by the rules work in M4 (#29+).
        Vec::new()
    }

    fn installed_apps(&self) -> Vec<AppInfo> {
        crate::apps::inventory()
    }
}

/// State shared by the writer and every worker.
pub(crate) struct Shared {
    pub rules: Rules,
    pub cancel: CancelToken,
    pub sender: SyncSender<Message>,
    /// Tasks spawned but not yet finished. Reaching zero means the walk is done.
    pub pending: AtomicUsize,
    /// Serial number of the scan root's volume. Reparse points are never
    /// followed, so every entry lives on this volume.
    pub root_dev: u32,
    /// The root volume is NTFS; see [`VolumeInfo::ntfs`].
    pub ntfs: bool,
    /// The process token is elevated, recorded once in `ScanResult::elevated`.
    pub elevated: bool,
}

impl Shared {
    /// Whether file ids on this volume are unique, so hard links may be deduped.
    pub(crate) fn dedupe_ids(&self) -> bool {
        self.ntfs
    }
}

/// One directory for a worker to enumerate.
pub(crate) struct Task {
    pub path: Arc<PathBuf>,
    /// Arena index of this directory, assigned by the writer before dispatch.
    pub id: u32,
    /// Depth below the scan root. The root is `0`.
    pub depth: u32,
}

/// Decrements the pending count even if a worker unwinds.
struct PendingGuard(Arc<Shared>);

impl Drop for PendingGuard {
    fn drop(&mut self) {
        let _ = self.0.pending.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Queue `task` on the pool, counting it as pending until it finishes.
pub(crate) fn dispatch(pool: &ThreadPool, shared: &Arc<Shared>, task: Task) {
    shared.pending.fetch_add(1, Ordering::AcqRel);
    let owned = Arc::clone(shared);
    pool.spawn(move || {
        let _guard = PendingGuard(Arc::clone(&owned));
        run_task(task, &owned);
    });
}

/// Bring a root into the internal path form: no `\\?\` prefix, `\` separators,
/// no `.` or `..` components.
///
/// The prefix is stripped so reported paths and excludes use the form users type;
/// [`ffi::verbatim`] adds it back for every Win32 call.
fn normalize_root(root: &Path) -> Result<PathBuf> {
    let wide: Vec<u16> = root.as_os_str().encode_wide().collect();
    let stripped = PathBuf::from(std::ffi::OsString::from_wide(&strip_verbatim(&wide)));
    std::path::absolute(&stripped).map_err(|_| ScanError::InvalidPath(root.to_path_buf()))
}

fn scan_root(
    root: &Path,
    options: &ScanOptions,
    sink: &mut dyn EntrySink,
    cancel: CancelToken,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<ScanResult> {
    let started = Instant::now();

    // Follow a linked root: the caller asked to scan its target. Children are
    // still never followed.
    let metadata = fs::metadata(root).map_err(|source| ScanError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    let VolumeInfo {
        serial: root_dev,
        ntfs,
    } = ffi::volume_info(root).unwrap_or(VolumeInfo {
        serial: 0,
        ntfs: false,
    });
    let root_entry = entry_from_metadata(root, &metadata, root_dev);
    let is_dir = root_entry.kind == EntryKind::Dir;

    let pool = ThreadPoolBuilder::new()
        .num_threads(pool_threads(options.threads))
        .thread_name(|index| format!("storagelens-scan-{index}"))
        .start_handler(|_| throttle_current_thread())
        .build()
        .map_err(|error| ScanError::Other(format!("failed to start scan pool: {error}")))?;

    let (sender, receiver) = mpsc::sync_channel(writer::CHANNEL_BATCHES);
    let shared = Arc::new(Shared {
        rules: Rules::new(options),
        cancel,
        sender,
        pending: AtomicUsize::new(0),
        root_dev,
        ntfs,
        elevated: crate::elevation::is_elevated(),
    });

    let mut writer = writer::Writer::new(sink);
    writer.accept_root(root_entry)?;

    if is_dir {
        dispatch(
            &pool,
            &shared,
            Task {
                path: Arc::new(root.to_path_buf()),
                id: 0,
                depth: 0,
            },
        );
    }

    writer.run(&pool, &shared, &receiver, progress, started)
}

fn pool_threads(requested: usize) -> usize {
    if requested == 0 {
        std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1)
            .clamp(1, MAX_THREADS)
    } else {
        requested.clamp(1, MAX_THREADS)
    }
}

fn run_task(task: Task, shared: &Shared) {
    if shared.cancel.is_cancelled() {
        return;
    }

    if let Some(reason) = shared.rules.skip_reason(&task.path, task.depth) {
        report_skip(shared, &task, reason);
        return;
    }

    let reader = match DirReader::open(&task.path) {
        Ok(reader) => reader,
        Err(error) => {
            report_skip(shared, &task, io_skip_reason(&error));
            return;
        }
    };

    let mut items = Vec::with_capacity(writer::BATCH_ENTRIES);
    for raw in reader {
        if shared.cancel.is_cancelled() {
            break;
        }

        match raw {
            Ok(raw) => items.push(item_from_raw(&task, shared.root_dev, raw)),
            Err(error) => {
                // The find handle cannot resume after an error. Keep the batches
                // already sent and report the directory itself.
                report_skip(shared, &task, io_skip_reason(&error));
                return;
            }
        }

        if items.len() >= writer::BATCH_ENTRIES {
            send_batch(shared, &task, &mut items);
        }
    }

    send_batch(shared, &task, &mut items);
}

fn send_batch(shared: &Shared, task: &Task, items: &mut Vec<Item>) {
    if items.is_empty() {
        return;
    }

    let batch = Batch {
        dir: Arc::clone(&task.path),
        dir_id: task.id,
        depth: task.depth,
        items: std::mem::replace(items, Vec::with_capacity(writer::BATCH_ENTRIES)),
    };

    // A send error means the writer has already returned after a failure; there is
    // nobody left to report to.
    let _ = shared.sender.send(Message::Batch(batch));
}

fn report_skip(shared: &Shared, task: &Task, reason: SkipReason) {
    let _ = shared.sender.send(Message::Skipped(SkippedDir {
        path: (*task.path).clone(),
        reason,
    }));
}

fn io_skip_reason(error: &io::Error) -> SkipReason {
    if error.kind() == io::ErrorKind::PermissionDenied {
        SkipReason::PermissionDenied
    } else {
        SkipReason::IoError
    }
}

fn item_from_raw(task: &Task, root_dev: u32, raw: RawEntry) -> Item {
    let kind = raw.kind();
    let name = std::ffi::OsString::from_wide(&raw.name);
    let path = (kind == EntryKind::Dir).then(|| task.path.join(&name));

    // Only regular files have sizes that count. Directories and links keep their
    // rows but report zero, matching the macOS scanner.
    let (size_alloc, size_logical) = if kind == EntryKind::File {
        let alloc = if needs_compressed_size(raw.attributes) {
            // A failed query keeps the record's figure rather than dropping the
            // file's bytes; it overstates, never hides.
            compressed_size(&task.path.join(&name)).unwrap_or(raw.size_alloc)
        } else {
            raw.size_alloc
        };
        (alloc, raw.size_logical)
    } else {
        (0, 0)
    };

    let entry = Entry {
        parent: task.id,
        name: name.to_string_lossy().into_owned(),
        kind,
        size_alloc,
        size_logical,
        dev: root_dev,
        file_id: raw.file_id,
        nlink: 1,
        attrs: raw.attributes,
        created: filetime_to_unix(raw.created),
        modified: filetime_to_unix(raw.modified),
        accessed: filetime_to_unix(raw.accessed),
        hardlink_dup: false,
    };

    Item { entry, path }
}

fn entry_from_metadata(root: &Path, metadata: &fs::Metadata, root_dev: u32) -> Entry {
    // `fs::metadata` followed any link, so this is the target's kind.
    let kind = kind_of(metadata.file_attributes(), 0);
    let size = if kind == EntryKind::File {
        metadata.file_size()
    } else {
        0
    };
    // A file root is a one-off, so one extra query is fine. For an ordinary file
    // `GetCompressedFileSizeW` returns the logical size, not the cluster-rounded
    // allocation; only the scan's children carry exact `AllocationSize` figures.
    let alloc = if kind == EntryKind::File {
        compressed_size(root).unwrap_or(size)
    } else {
        0
    };

    Entry {
        parent: 0,
        name: root_name(root),
        kind,
        size_alloc: alloc,
        size_logical: size,
        dev: root_dev,
        file_id: 0,
        nlink: 1,
        attrs: metadata.file_attributes(),
        created: filetime_to_unix(metadata.creation_time()),
        modified: filetime_to_unix(metadata.last_write_time()),
        accessed: filetime_to_unix(metadata.last_access_time()),
        hardlink_dup: false,
    }
}

fn root_name(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string())
}
