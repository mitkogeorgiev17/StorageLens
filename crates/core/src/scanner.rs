//! The `StorageScanner` trait and the plumbing scanners share.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::error::{Result, ScanError};
use crate::types::{AppInfo, Entry, PlatformTarget, Progress, ScanResult, Volume};

/// Cooperative cancellation.
///
/// Cloning shares the flag, so a UI thread can cancel a scan running on worker
/// threads. Scanners must check it between directories, not per file, to keep the
/// hot loop cheap.
#[derive(Debug, Clone, Default)]
pub struct CancelToken {
    flag: Arc<AtomicBool>,
}

impl CancelToken {
    /// A fresh token that has not been cancelled.
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the scan to stop. Idempotent, and safe from any thread.
    pub fn cancel(&self) {
        self.flag.store(true, Ordering::Relaxed);
    }

    /// True once [`cancel`](Self::cancel) has been called.
    pub fn is_cancelled(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }

    /// `Ok(())` while running, [`ScanError::Cancelled`] afterwards.
    pub fn check(&self) -> Result<()> {
        if self.is_cancelled() {
            Err(ScanError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Destination for entries produced by a scan.
///
/// Scanners stream into a sink instead of returning a `Vec<Entry>`: a million
/// entries would otherwise sit in RAM before the first row reached SQLite. The
/// catalog crate implements this over a batched writer.
///
/// Deliberately **not** `Send`: the sink lives on the scanner's single writer
/// thread, and a `rusqlite::Connection` is `!Sync`, so requiring `Send` would make
/// it impossible to implement this over a borrowed connection.
pub trait EntrySink {
    /// Accept one entry. Called once per filesystem object.
    ///
    /// Entries must arrive in **pre-order**: a parent is accepted before its
    /// children, and acceptance order is the arena order. `Entry` carries only
    /// [`Entry::parent`], not its own index, so a sink that persists to SQLite
    /// assigns indices sequentially and relies on this contract. The first entry
    /// of a scan is its root, with `parent == 0`.
    fn accept(&mut self, entry: Entry) -> Result<()>;

    /// Flush anything buffered. Called once at the end of a scan, including a
    /// cancelled one, so a partial scan stays consistent.
    fn flush(&mut self) -> Result<()>;
}

/// The one interface both platform scanners implement.
///
/// Everything above this trait — schema, rules, rollups, dedupe, safety, UI — is
/// written once. Implementations live in `storagelens-scan-macos` and
/// `storagelens-scan-windows`; both compile everywhere and return
/// [`ScanError::Unsupported`] on the wrong platform.
pub trait StorageScanner: Send + Sync {
    /// Mounted volumes this scanner can see, with capacity and purgeable space.
    fn volumes(&self) -> Vec<Volume>;

    /// Walk `roots`, streaming every entry to `sink`.
    ///
    /// The sink is a parameter rather than a field of the scanner because
    /// [`EntrySink`] is deliberately `!Send`: it must stay on the thread that
    /// called this method. Scanners that enumerate in parallel forward `Entry`
    /// values to that thread, which accepts them in arena order.
    ///
    /// `progress` is called at most ~10 times per second. Cancellation is
    /// cooperative: implementations stop between directories and return `Ok` with
    /// [`ScanStatus::Cancelled`](crate::ScanStatus::Cancelled), having flushed the
    /// rows accepted so far.
    fn scan(
        &self,
        roots: &[PathBuf],
        sink: &mut dyn EntrySink,
        cancel: CancelToken,
        progress: &(dyn Fn(Progress) + Send + Sync),
    ) -> Result<ScanResult>;

    /// Platform-specific cleanup locations discovered without a full scan.
    fn platform_cleanup_targets(&self) -> Vec<PlatformTarget>;

    /// Installed applications, with usage signals where available.
    fn installed_apps(&self) -> Vec<AppInfo>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancel_token_clones_share_state() {
        let token = CancelToken::new();
        let clone = token.clone();

        assert!(!token.is_cancelled());
        assert!(token.check().is_ok());

        clone.cancel();
        assert!(token.is_cancelled());
        assert!(matches!(token.check(), Err(ScanError::Cancelled)));
    }

    #[test]
    fn trait_is_object_safe() {
        fn takes_scanner(_: &dyn StorageScanner) {}
        let _ = takes_scanner;
    }

    #[test]
    fn trait_is_send_and_sync() {
        fn require<T: Send + Sync + ?Sized>() {}
        require::<dyn StorageScanner>();
    }
}
