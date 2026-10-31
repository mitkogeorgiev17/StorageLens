//! Parallel directory walker for the macOS scanner.
//!
//! One thread — the caller's — owns the [`EntrySink`] (it is `!Send`) and acts as
//! both writer and dispatcher. A fixed rayon pool enumerates directories. Each
//! worker streams [`Entry`] values back in batches; the writer accepts them in
//! arrival order, assigns arena indices, and submits a task for every directory it
//! accepts.
//!
//! That dispatch rule is the whole safety argument for parallelism: a child
//! directory cannot produce a row until its own row has been accepted, so the
//! parent indices the sink relies on are always valid. Sibling subtrees interleave,
//! which the arena does not care about.
//!
//! # Sizing
//!
//! The pool is `min(cores, 8)`. Each worker buffers at most one batch of 2k entries
//! and the channel holds at most 32 batches, so the walker's own memory ceiling
//! does not depend on the size of the disk. When SQLite falls behind, workers block
//! on `send` instead of growing the queue.
//!
//! # Accounting
//!
//! Directory rows have `size_alloc == 0` because `getattrlistbulk` has no
//! directory-size attribute and APFS reports zero blocks for directories (`du`
//! agrees), so no per-directory `stat` is needed.
//!
//! Hard links are deduped in the writer (see `dedupe.rs`), and directories are
//! refused according to `skip.rs` before they are opened, so a skipped subtree
//! costs no syscalls.

use std::fs;
use std::io;
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

use crate::ffi::{throttle_current_thread, DirReader};
use crate::parse::{ObjType, RawEntry};
use crate::skip;
use crate::writer::{self, Batch, Message};

/// Upper bound on scan threads, per PLAN §6.5.
pub const MAX_THREADS: usize = 8;

/// The macOS [`StorageScanner`] implementation.
///
/// Construct it with the options for one scan; the [`StorageScanner::scan`] call
/// borrows the sink instead of storing it, because the sink cannot cross threads.
pub struct MacScanner {
    options: ScanOptions,
}

impl MacScanner {
    /// A scanner with explicit options.
    pub fn new(options: ScanOptions) -> Self {
        Self { options }
    }
}

impl Default for MacScanner {
    fn default() -> Self {
        Self::new(ScanOptions::default())
    }
}

impl StorageScanner for MacScanner {
    fn volumes(&self) -> Vec<Volume> {
        crate::volumes()
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
            // volume per session, so this is deferred rather than guessed at.
            _ => {
                return Err(ScanError::Unsupported(
                    "scanning more than one root per pass",
                ))
            }
        };

        if root.as_os_str().is_empty() || !root.is_absolute() {
            return Err(ScanError::InvalidPath(root.clone()));
        }

        scan_root(root, &self.options, sink, cancel, progress)
    }

    fn platform_cleanup_targets(&self) -> Vec<PlatformTarget> {
        // Discovered by the rules work in M4 (#29+).
        Vec::new()
    }

    fn installed_apps(&self) -> Vec<AppInfo> {
        // Discovered by the app-inventory work in M4 (#29+).
        Vec::new()
    }
}

/// State shared by the writer and every worker.
pub(crate) struct Shared {
    pub options: ScanOptions,
    pub cancel: CancelToken,
    pub sender: SyncSender<Message>,
    /// Tasks spawned but not yet finished. Reaching zero means the walk is done.
    pub pending: AtomicUsize,
    /// Device of the scan root, for the cross-filesystem rule.
    pub root_dev: u32,
}

/// One directory for a worker to enumerate.
pub(crate) struct Task {
    pub path: Arc<PathBuf>,
    /// Arena index of this directory, assigned by the writer before dispatch.
    pub id: u32,
    /// Depth below the scan root. The root is `0`.
    pub depth: u32,
    /// Device the directory lives on, from the listing that produced it.
    pub dev: u32,
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
        run_task(task, owned);
    });
}

fn scan_root(
    root: &Path,
    options: &ScanOptions,
    sink: &mut dyn EntrySink,
    cancel: CancelToken,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<ScanResult> {
    let started = Instant::now();

    // Follow a symlinked root: the caller asked to scan its target. Children are
    // still enumerated without following links (`FSOPT_NOFOLLOW`).
    let metadata = fs::metadata(root).map_err(|source| ScanError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    let kind = entry_kind(&metadata);
    let root_entry = entry_from_metadata(root, kind, &metadata);
    let root_dev = root_entry.dev;

    let pool = ThreadPoolBuilder::new()
        .num_threads(pool_threads(options.threads))
        .thread_name(|index| format!("storagelens-scan-{index}"))
        .build()
        .map_err(|error| ScanError::Other(format!("failed to start scan pool: {error}")))?;

    let (sender, receiver) = mpsc::sync_channel(writer::CHANNEL_BATCHES);
    let shared = Arc::new(Shared {
        options: options.clone(),
        cancel: cancel.clone(),
        sender,
        pending: AtomicUsize::new(0),
        root_dev,
    });

    let mut writer = writer::Writer::new(sink);
    writer.accept_root(root_entry)?;

    if kind == EntryKind::Dir {
        dispatch(
            &pool,
            &shared,
            Task {
                path: Arc::new(root.to_path_buf()),
                id: 0,
                depth: 0,
                dev: root_dev,
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

fn run_task(task: Task, shared: Arc<Shared>) {
    if shared.cancel.is_cancelled() {
        return;
    }
    throttle_current_thread();

    if let Some(reason) = skip::skip_reason(
        &task.path,
        task.depth,
        task.dev,
        shared.root_dev,
        &shared.options,
    ) {
        report_skip(&shared, &task, reason);
        return;
    }

    let reader = match DirReader::open(&*task.path) {
        Ok(reader) => reader,
        Err(error) => {
            report_skip(&shared, &task, io_skip_reason(&error));
            return;
        }
    };

    let mut entries = Vec::with_capacity(writer::BATCH_ENTRIES);
    for item in reader {
        if shared.cancel.is_cancelled() {
            break;
        }

        match item {
            Ok(raw) => entries.push(entry_from_raw(&task, raw)),
            Err(error) => {
                // A malformed buffer leaves no way to resynchronise this directory.
                // Keep the rows accepted so far and report the directory itself.
                report_skip(&shared, &task, io_skip_reason(&error));
                entries.clear();
                return;
            }
        }

        if entries.len() >= writer::BATCH_ENTRIES {
            send_batch(&shared, &task, &mut entries);
        }
    }

    send_batch(&shared, &task, &mut entries);
}

fn send_batch(shared: &Shared, task: &Task, entries: &mut Vec<Entry>) {
    if entries.is_empty() {
        return;
    }

    let batch = Batch {
        dir: Arc::clone(&task.path),
        dir_id: task.id,
        depth: task.depth,
        entries: std::mem::replace(entries, Vec::with_capacity(writer::BATCH_ENTRIES)),
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

fn entry_from_raw(task: &Task, raw: RawEntry) -> Entry {
    let kind = match raw.obj_type {
        ObjType::Directory => EntryKind::Dir,
        ObjType::Regular => EntryKind::File,
        ObjType::Symlink => EntryKind::Symlink,
        _ => EntryKind::Other,
    };

    Entry {
        parent: task.id,
        name: raw.name,
        kind,
        size_alloc: raw.size_alloc,
        size_logical: raw.size_logical,
        dev: raw.dev,
        file_id: raw.file_id,
        nlink: raw.nlink,
        attrs: raw.flags,
        created: raw.created,
        modified: raw.modified,
        accessed: raw.accessed,
        hardlink_dup: false,
    }
}

fn entry_from_metadata(root: &Path, kind: EntryKind, metadata: &fs::Metadata) -> Entry {
    use std::os::macos::fs::MetadataExt as _;
    use std::os::unix::fs::MetadataExt;

    Entry {
        parent: 0,
        name: root_name(root),
        kind,
        size_alloc: metadata.blocks().saturating_mul(512),
        size_logical: metadata.len(),
        dev: metadata.dev() as u32,
        file_id: metadata.ino(),
        nlink: metadata.nlink() as u32,
        attrs: metadata.st_flags(),
        created: metadata.st_birthtime(),
        modified: metadata.mtime(),
        accessed: metadata.atime(),
        hardlink_dup: false,
    }
}

fn root_name(root: &Path) -> String {
    root.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string())
}

fn entry_kind(metadata: &fs::Metadata) -> EntryKind {
    if metadata.is_dir() {
        EntryKind::Dir
    } else if metadata.is_symlink() {
        EntryKind::Symlink
    } else if metadata.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}
