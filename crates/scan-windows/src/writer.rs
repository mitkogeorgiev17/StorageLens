//! The single writer side of the parallel walker.
//!
//! Worker threads enumerate directories and send [`Batch`]es over a bounded
//! channel; this module owns the only handle to the [`EntrySink`], which is
//! `!Send`. [`Entry`] values are `Send`, so they cross threads and the sink does
//! not.
//!
//! The writer is also the dispatcher. It accepts entries in arena order, and when
//! it accepts a directory it assigns that directory its index and then submits a
//! task to enumerate it. A child task can therefore never produce rows before its
//! parent has been accepted, no matter how the workers interleave.
//!
//! The same design as the macOS writer, kept as a separate copy on purpose
//! (AGENTS.md §2.4): the platforms share a trait, not an implementation. Hard
//! links are deduped here too (see `dedupe.rs`). The one structural difference is [`Item::path`]: the worker builds a child
//! directory's path from the raw `OsString` name, because [`Entry::name`] is a
//! lossy `String` and an NTFS name may contain an unpaired surrogate.

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rayon::ThreadPool;
use storagelens_core::{
    Bytes, Entry, EntryKind, EntrySink, NodeId, Progress, Result, ScanError, ScanResult,
    ScanStatus, SkippedDir,
};

use crate::dedupe::Dedupe;
use crate::walk::{self, Shared, Task};

/// Entries accumulated by one worker before it hands them to the writer.
///
/// A worker checks cancellation between entries and sends at least once per
/// batch, which bounds cancellation latency even in a directory with millions of
/// entries.
pub(crate) const BATCH_ENTRIES: usize = 2_048;

/// Batches that may be in flight before workers block on `send`.
///
/// Caps the walker's own buffering at roughly 64k entries regardless of disk
/// speed; backpressure keeps peak RSS bounded when SQLite is the bottleneck.
pub(crate) const CHANNEL_BATCHES: usize = 32;

/// How often the progress callback may fire.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// How long the writer sleeps before re-checking worker state.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// One enumerated entry.
pub(crate) struct Item {
    pub entry: Entry,
    /// Full path, set only for directories the walker may enter.
    pub path: Option<PathBuf>,
}

/// Entries produced by enumerating one directory.
pub(crate) struct Batch {
    /// The directory the entries came from.
    pub dir: Arc<PathBuf>,
    /// Arena index of that directory.
    pub dir_id: NodeId,
    /// Depth of that directory below the scan root.
    pub depth: u32,
    /// Entries, in the order the filesystem returned them.
    pub items: Vec<Item>,
}

/// Everything a worker can tell the writer.
pub(crate) enum Message {
    /// Entries ready to persist.
    Batch(Batch),
    /// A directory that could not or would not be walked.
    Skipped(SkippedDir),
}

/// Receives batches and feeds one [`EntrySink`].
pub(crate) struct Writer<'a> {
    sink: &'a mut dyn EntrySink,
    /// Arena index the next accepted entry will receive.
    next_index: NodeId,
    files: u64,
    dirs: u64,
    bytes_alloc: u64,
    bytes_logical: u64,
    skipped: Vec<SkippedDir>,
    dedupe: Dedupe,
    last_dir: Option<Arc<PathBuf>>,
    last_progress: Instant,
}

impl<'a> Writer<'a> {
    /// Wrap the sink. Nothing is accepted until [`accept_root`](Self::accept_root).
    pub(crate) fn new(sink: &'a mut dyn EntrySink) -> Self {
        Self {
            sink,
            next_index: 0,
            files: 0,
            dirs: 0,
            bytes_alloc: 0,
            bytes_logical: 0,
            skipped: Vec::new(),
            dedupe: Dedupe::new(),
            last_dir: None,
            last_progress: Instant::now(),
        }
    }

    /// Accept the scan root as arena index `0`.
    ///
    /// A directory root is not counted in `dirs`: it is the container, not a
    /// child, matching the fixture manifest. A file root counts like any file.
    pub(crate) fn accept_root(&mut self, entry: Entry) -> Result<()> {
        debug_assert_eq!(entry.parent, 0, "the root is its own parent");
        self.next_index = 1;
        if entry.kind == EntryKind::File {
            self.count(&entry);
        }
        self.sink.accept(entry)
    }

    /// Run until every task has finished, feeding the sink as batches arrive.
    ///
    /// Dispatch is turned off once cancellation is observed: rows already
    /// enumerated are still accepted, but no new directory is queued, so the walk
    /// drains quickly and the persisted tree stays consistent.
    pub(crate) fn run(
        mut self,
        pool: &ThreadPool,
        shared: &Arc<Shared>,
        receiver: &Receiver<Message>,
        progress: &(dyn Fn(Progress) + Send + Sync),
        started: Instant,
    ) -> Result<ScanResult> {
        let mut cancelled = false;
        let mut dispatch = true;
        let mut failure: Option<ScanError> = None;

        'run: loop {
            observe_cancel(shared, &mut cancelled, &mut dispatch);
            self.maybe_progress(progress, started);

            // Accept everything already queued before deciding whether the walk is
            // finished. A drained batch can dispatch child directories, which keeps
            // `pending` above zero and sends the loop around again.
            let mut disconnected = false;
            loop {
                // Re-check between messages: the sink may cancel from inside
                // `accept`, and the queued batch may be the last one.
                observe_cancel(shared, &mut cancelled, &mut dispatch);
                match receiver.try_recv() {
                    Ok(message) => {
                        if let Err(error) = self.handle(message, pool, shared, dispatch) {
                            failure = Some(error);
                            break 'run;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }

            if shared.pending.load(Ordering::Acquire) == 0 || disconnected {
                break;
            }

            match receiver.recv_timeout(POLL_INTERVAL) {
                Ok(message) => {
                    if let Err(error) = self.handle(message, pool, shared, dispatch) {
                        failure = Some(error);
                        break 'run;
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }

        if let Some(error) = failure {
            // Stop the workers and keep draining so none of them block forever on a
            // full channel. Flushing is best-effort and the original error wins.
            shared.cancel.cancel();
            wait_for_workers(shared, receiver);
            let _ = self.sink.flush();
            return Err(error);
        }

        let status = if cancelled {
            ScanStatus::Cancelled
        } else if self.skipped.is_empty() {
            ScanStatus::Complete
        } else {
            ScanStatus::Partial
        };

        // Parallel arrival order is not deterministic; sort the summary so reports
        // and tests are stable.
        self.skipped
            .sort_by(|left, right| left.path.cmp(&right.path));

        let result = ScanResult {
            status,
            files: self.files,
            dirs: self.dirs,
            bytes_alloc: Bytes::new(self.bytes_alloc),
            bytes_logical: Bytes::new(self.bytes_logical),
            elapsed_ms: started.elapsed().as_millis() as u64,
            skipped: std::mem::take(&mut self.skipped),
        };

        progress(self.snapshot(started));
        self.sink.flush()?;
        Ok(result)
    }

    fn handle(
        &mut self,
        message: Message,
        pool: &ThreadPool,
        shared: &Arc<Shared>,
        dispatch: bool,
    ) -> Result<()> {
        match message {
            Message::Batch(batch) => self.accept_batch(batch, pool, shared, dispatch),
            Message::Skipped(skipped) => {
                self.skipped.push(skipped);
                Ok(())
            }
        }
    }

    fn accept_batch(
        &mut self,
        batch: Batch,
        pool: &ThreadPool,
        shared: &Arc<Shared>,
        dispatch: bool,
    ) -> Result<()> {
        let Batch {
            dir,
            dir_id,
            depth,
            items,
        } = batch;

        self.last_dir = Some(dir);

        let dedupe_ids = shared.dedupe_ids();
        for Item { mut entry, path } in items {
            let index = self.next_index;
            self.next_index = self.next_index.checked_add(1).ok_or_else(|| {
                ScanError::Other("more than u32::MAX entries in one scan".to_owned())
            })?;

            // The first name accepted for a file owns its bytes; later names keep
            // their row and true size but are flagged. A flagged name has at least
            // two links by definition, which the listing cannot otherwise report.
            if dedupe_ids && self.dedupe.is_duplicate(&entry) {
                entry.hardlink_dup = true;
                entry.nlink = entry.nlink.max(2);
            }

            self.count(&entry);
            self.sink.accept(entry)?;

            // Accepted first, dispatched second: the child can only start producing
            // rows after its own row has been handed to the sink.
            if let Some(path) = path.filter(|_| dispatch) {
                walk::dispatch(
                    pool,
                    shared,
                    Task {
                        path: Arc::new(path),
                        id: index,
                        depth: depth.saturating_add(1),
                    },
                );
            }
        }

        debug_assert!(dir_id < self.next_index);
        Ok(())
    }

    /// Update the totals.
    ///
    /// Only regular files contribute bytes, matching the macOS scanner and the
    /// fixture manifest. Directories and links are rows but not disk usage.
    fn count(&mut self, entry: &Entry) {
        if entry.hardlink_dup {
            return;
        }

        match entry.kind {
            EntryKind::File => {
                self.files = self.files.saturating_add(1);
                self.bytes_alloc = self.bytes_alloc.saturating_add(entry.size_alloc);
                self.bytes_logical = self.bytes_logical.saturating_add(entry.size_logical);
            }
            EntryKind::Dir => self.dirs = self.dirs.saturating_add(1),
            EntryKind::Symlink | EntryKind::Other => {}
        }
    }

    fn maybe_progress(&mut self, progress: &(dyn Fn(Progress) + Send + Sync), started: Instant) {
        if self.last_progress.elapsed() >= PROGRESS_EVERY {
            self.last_progress = Instant::now();
            progress(self.snapshot(started));
        }
    }

    fn snapshot(&self, started: Instant) -> Progress {
        Progress {
            files: self.files,
            dirs: self.dirs,
            bytes_alloc: Bytes::new(self.bytes_alloc),
            elapsed_ms: started.elapsed().as_millis() as u64,
            current_path: self.last_dir.as_ref().map(|path| (**path).clone()),
        }
    }
}

/// Fold the current cancellation state into the run flags. Once cancellation is
/// seen, dispatch stays off for the rest of the scan.
fn observe_cancel(shared: &Shared, cancelled: &mut bool, dispatch: &mut bool) {
    if shared.cancel.is_cancelled() {
        *cancelled = true;
        *dispatch = false;
    }
}

/// Drain the channel until every worker has decremented its task count.
fn wait_for_workers(shared: &Shared, receiver: &Receiver<Message>) {
    while shared.pending.load(Ordering::Acquire) > 0 {
        match receiver.recv_timeout(POLL_INTERVAL) {
            Ok(_) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}
