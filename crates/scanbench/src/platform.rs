//! Which walker `scanbench` runs.
//!
//! On macOS the default is [`MacScanner`](storagelens_scan_macos::MacScanner), the
//! backend the app ships. Measuring the real scanner is the point of the accuracy
//! gate: the portable [`walk`](crate::walk) baseline is only useful as a second
//! opinion when triaging a disagreement.
//!
//! `--reference` forces the portable walker. If the reference agrees with `du` but
//! the platform scanner does not, the bug is in the platform backend.
//!
//! Off macOS there is no platform backend yet (#12), so `main` resolves
//! [`Backend::Platform`] to [`Backend::Reference`] and the report says
//! `"reference"`. That keeps the JSON honest about what actually ran.

use std::path::Path;

use storagelens_core::{CancelToken, Entry, EntryKind, EntrySink, Result, ScanOptions};

use crate::walk::{self, ScanOutcome};

/// How often to sample resident memory, in accepted entries.
const RSS_SAMPLE_EVERY: u64 = 4_096;

/// The implementation a run uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// The platform scanner: `MacScanner` on macOS.
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

/// Run the platform scanner and adapt its [`ScanResult`](storagelens_core::ScanResult)
/// to the richer [`ScanOutcome`] the report wants.
///
/// The platform scanner reports counts and bytes but not links, stored entries, or
/// peak RSS, so a forwarding sink counts those as the entries stream past. The sink
/// stays on this thread, which is what `EntrySink`'s `!Send` contract requires.
#[cfg(target_os = "macos")]
fn platform_scan<S: EntrySink>(
    root: &Path,
    options: &ScanOptions,
    cancel: &CancelToken,
    sink: &mut S,
) -> Result<ScanOutcome> {
    use storagelens_core::StorageScanner;
    use storagelens_scan_macos::MacScanner;

    let mut counting = Counting::new(sink);
    let result = MacScanner::new(options.clone()).scan(
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

/// No platform backend exists off macOS yet, so `main` never selects
/// [`Backend::Platform`] there. Delegating keeps the match arms total.
#[cfg(not(target_os = "macos"))]
fn platform_scan<S: EntrySink>(
    root: &Path,
    options: &ScanOptions,
    cancel: &CancelToken,
    sink: &mut S,
) -> Result<ScanOutcome> {
    walk::scan(root, options, cancel, sink)
}

/// Forwards entries to the real sink while counting what the report needs.
struct Counting<'a, S: EntrySink> {
    inner: &'a mut S,
    links: u64,
    entries: u64,
    peak_rss_bytes: u64,
}

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
}
