//! Which walker `scanbench` runs.
//!
//! The default is the platform scanner the app ships —
//! [`MacScanner`](storagelens_scan_macos::MacScanner) on macOS, `WinScanner` on
//! Windows. Measuring the real scanner is the point of the accuracy gates: the
//! portable [`walk`](crate::walk) baseline is only useful as a second opinion
//! when triaging a disagreement.
//!
//! `--reference` forces the portable walker. If the reference agrees with `du` /
//! `Get-ChildItem` but the platform scanner does not, the bug is in the platform
//! backend.
//!
//! On any other OS there is no platform backend, so `main` resolves
//! [`Backend::Platform`] to [`Backend::Reference`] and the report says
//! `"reference"`. That keeps the JSON honest about what actually ran.

use std::path::Path;

use storagelens_core::{CancelToken, EntrySink, Result, ScanOptions};
#[cfg(any(target_os = "macos", windows))]
use storagelens_core::{Entry, EntryKind, StorageScanner};

use crate::walk::{self, ScanOutcome};

/// Whether this build has a platform scanner at all.
pub const HAS_PLATFORM: bool = cfg!(any(target_os = "macos", windows));

/// Whether the platform scanner dedupes hard links itself.
///
/// The reference walker cannot on Windows (stable `std` has no file index), so
/// the fixture check adds the extra names back for it — but must not for a
/// backend that already folded them.
pub const fn dedupes_hard_links(backend: Backend) -> bool {
    match backend {
        Backend::Platform => HAS_PLATFORM,
        Backend::Reference => cfg!(unix),
    }
}

/// Whether the fixture manifest's allocated total is exact for this backend.
///
/// On Windows the manifest can only record logical sizes (stable `std` exposes no
/// allocation), which is what the reference walker reports too. `WinScanner`
/// reports true `AllocationSize`, so the comparison is left to
/// `tests/accuracy_windows.rs`, which has a per-file Win32 oracle.
pub const fn manifest_alloc_is_exact(backend: Backend) -> bool {
    match backend {
        Backend::Platform => cfg!(unix),
        Backend::Reference => true,
    }
}

/// How often to sample resident memory, in accepted entries.
#[cfg(any(target_os = "macos", windows))]
const RSS_SAMPLE_EVERY: u64 = 4_096;

/// The implementation a run uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// The platform scanner: `MacScanner` on macOS, `WinScanner` on Windows.
    Platform,
    /// The portable `std::fs` reference walker.
    Reference,
}

impl Backend {
    /// Stable name for the `--json` report.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Platform => "platform",
            Self::Reference => "reference",
        }
    }
}

/// Scan `root` with `backend`, streaming into `sink`.
pub fn scan<S: EntrySink>(
    backend: Backend,
    root: &Path,
    options: &ScanOptions,
    cancel: &CancelToken,
    sink: &mut S,
) -> Result<ScanOutcome> {
    match backend {
        Backend::Reference => walk::scan(root, options, cancel, sink),
        Backend::Platform => platform_scan(root, options, cancel, sink),
    }
}

#[cfg(target_os = "macos")]
fn platform_scanner(options: &ScanOptions) -> impl StorageScanner {
    storagelens_scan_macos::MacScanner::new(options.clone())
}

#[cfg(windows)]
fn platform_scanner(options: &ScanOptions) -> impl StorageScanner {
    storagelens_scan_windows::WinScanner::new(options.clone())
}

/// Run the platform scanner and adapt its [`ScanResult`](storagelens_core::ScanResult)
/// to the richer [`ScanOutcome`] the report wants.
///
/// The platform scanner reports counts and bytes but not links, stored entries, or
/// peak RSS, so a forwarding sink counts those as the entries stream past. The sink
/// stays on this thread, which is what `EntrySink`'s `!Send` contract requires.
#[cfg(any(target_os = "macos", windows))]
fn platform_scan<S: EntrySink>(
    root: &Path,
    options: &ScanOptions,
    cancel: &CancelToken,
    sink: &mut S,
) -> Result<ScanOutcome> {
    let mut counting = Counting::new(sink);
    let result = platform_scanner(options).scan(
        std::slice::from_ref(&root.to_path_buf()),
        &mut counting,
        cancel.clone(),
        &|_| {},
    )?;
    counting.sample_rss();

    Ok(ScanOutcome {
        result,
        links: counting.links,
        entries: counting.entries,
        peak_rss_bytes: counting.peak_rss_bytes,
    })
}

/// No platform backend exists here, so `main` never selects
/// [`Backend::Platform`]. Delegating keeps the match arms total.
#[cfg(not(any(target_os = "macos", windows)))]
fn platform_scan<S: EntrySink>(
    root: &Path,
    options: &ScanOptions,
    cancel: &CancelToken,
    sink: &mut S,
) -> Result<ScanOutcome> {
    walk::scan(root, options, cancel, sink)
}

/// Forwards entries to the real sink while counting what the report needs.
#[cfg(any(target_os = "macos", windows))]
struct Counting<'a, S: EntrySink> {
    inner: &'a mut S,
    links: u64,
    entries: u64,
    peak_rss_bytes: u64,
}

#[cfg(any(target_os = "macos", windows))]
impl<'a, S: EntrySink> Counting<'a, S> {
    fn new(inner: &'a mut S) -> Self {
        Self {
            inner,
            links: 0,
            entries: 0,
            peak_rss_bytes: 0,
        }
    }

    fn sample_rss(&mut self) {
        if let Some(stats) = memory_stats::memory_stats() {
            self.peak_rss_bytes = self.peak_rss_bytes.max(stats.physical_mem as u64);
        }
    }
}

#[cfg(any(target_os = "macos", windows))]
impl<S: EntrySink> EntrySink for Counting<'_, S> {
    fn accept(&mut self, entry: Entry) -> Result<()> {
        if entry.kind == EntryKind::Symlink {
            self.links = self.links.saturating_add(1);
        }
        self.entries = self.entries.saturating_add(1);
        if self.entries % RSS_SAMPLE_EVERY == 0 {
            self.sample_rss();
        }
        self.inner.accept(entry)
    }

    fn flush(&mut self) -> Result<()> {
        self.sample_rss();
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backends_have_stable_names() {
        assert_eq!(Backend::Platform.as_str(), "platform");
        assert_eq!(Backend::Reference.as_str(), "reference");
    }

    #[test]
    fn the_reference_walker_is_always_compared_against_the_manifest() {
        assert!(manifest_alloc_is_exact(Backend::Reference));
        // Unix std has inodes, so the reference walker dedupes there.
        assert_eq!(dedupes_hard_links(Backend::Reference), cfg!(unix));
        assert_eq!(dedupes_hard_links(Backend::Platform), HAS_PLATFORM);
    }
}
