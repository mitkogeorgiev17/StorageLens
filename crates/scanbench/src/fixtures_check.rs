//! Compare a scan of the fixture tree against its manifest.
//!
//! This is the accuracy gate from PLAN §17: the generator knows the exact file
//! and directory counts and the exact allocated/logical byte totals, so a
//! scanner either agrees or is wrong.

use std::fs;

use serde::Serialize;
use storagelens_core::Manifest;
#[cfg(unix)]
use storagelens_core::SkipReason;

use crate::platform::{self, Backend};
use crate::walk::ScanOutcome;

/// The result of comparing a scan with the manifest.
#[derive(Debug, Clone, Serialize)]
pub struct FixtureCheck {
    /// True when every count and total matched.
    pub ok: bool,
    /// Human-readable differences, empty when `ok`.
    pub mismatches: Vec<String>,
}

impl FixtureCheck {
    /// A one-line summary for the text output.
    pub fn summary(&self) -> String {
        if self.ok {
            "ok".to_owned()
        } else {
            format!("FAILED ({} mismatches)", self.mismatches.len())
        }
    }
}

/// Compare `outcome` with the manifest of the tree `backend` scanned.
///
/// Two backend-specific allowances, both from [`platform`]: hard-link names are
/// added back only for a walker that cannot dedupe, and the allocated total is
/// compared only where the manifest can know it exactly.
pub fn check(outcome: &ScanOutcome, manifest: &Manifest, backend: Backend) -> FixtureCheck {
    let mut mismatches = Vec::new();
    let (extra_files, extra_bytes) = if platform::dedupes_hard_links(backend) {
        (0, 0)
    } else {
        hardlink_extras(manifest)
    };

    compare(
        &mut mismatches,
        "files",
        outcome.result.files,
        manifest.files + extra_files,
    );
    compare(&mut mismatches, "dirs", outcome.result.dirs, manifest.dirs);
    compare(
        &mut mismatches,
        "links",
        outcome.links,
        manifest.symlinks + manifest.junctions,
    );
    if platform::manifest_alloc_is_exact(backend) {
        compare(
            &mut mismatches,
            "allocated_bytes",
            outcome.result.bytes_alloc.get(),
            manifest.bytes_alloc + extra_bytes,
        );
    }
    compare(
        &mut mismatches,
        "logical_bytes",
        outcome.result.bytes_logical.get(),
        manifest.bytes_logical + extra_bytes,
    );

    // Every skip must be one the fixture deliberately planted.
    let mut expected_skips: Vec<&std::path::Path> = Vec::new();
    if let Some(denied) = manifest.paths.denied_dir.as_deref() {
        expected_skips.push(denied);
    }

    for skipped in &outcome.result.skipped {
        let expected = expected_skips.iter().any(|path| *path == skipped.path);
        if !expected {
            mismatches.push(format!(
                "unexpected skip: {} ({})",
                skipped.path.display(),
                skipped.reason.as_str()
            ));
        }
    }

    #[cfg(unix)]
    if let Some(denied) = manifest.paths.denied_dir.as_deref() {
        let reported = outcome.result.skipped.iter().any(|skipped| {
            skipped.path == denied && skipped.reason == SkipReason::PermissionDenied
        });
        if !reported {
            mismatches.push(format!(
                "the unreadable directory was not reported: {}",
                denied.display()
            ));
        }
    }

    FixtureCheck {
        ok: mismatches.is_empty(),
        mismatches,
    }
}

fn compare(mismatches: &mut Vec<String>, field: &str, actual: u64, expected: u64) {
    if actual != expected {
        mismatches.push(format!("{field}: expected {expected}, got {actual}"));
    }
}

/// Hard-link names a non-deduping walker counts but the manifest folds into one
/// file, and their bytes.
///
/// Only the Windows reference walker needs this: stable `std` has no file index
/// there. Every other backend dedupes and gets no compensation.
fn hardlink_extras(manifest: &Manifest) -> (u64, u64) {
    let extras = manifest.hardlink_names.saturating_sub(1);
    let size = fs::metadata(&manifest.paths.hardlinks[0])
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    (extras, extras.saturating_mul(size))
}

#[cfg(test)]
mod tests {
    use super::*;
    use storagelens_core::{Bytes, CancelToken, FixtureTree, ScanOptions, ScanResult, ScanStatus};

    use crate::walk;

    struct NullSink;

    impl storagelens_core::EntrySink for NullSink {
        fn accept(&mut self, _entry: storagelens_core::Entry) -> storagelens_core::Result<()> {
            Ok(())
        }

        fn flush(&mut self) -> storagelens_core::Result<()> {
            Ok(())
        }
    }

    fn outcome_for(tree: &FixtureTree) -> ScanOutcome {
        walk::scan(
            tree.root(),
            &ScanOptions::default(),
            &CancelToken::new(),
            &mut NullSink,
        )
        .expect("scan")
    }

    #[test]
    fn a_correct_scan_passes() {
        let tree = FixtureTree::generate().expect("generate");
        let outcome = outcome_for(&tree);
        let check = check(&outcome, tree.manifest(), Backend::Reference);

        assert!(check.ok, "{:?}", check.mismatches);
        assert_eq!(check.summary(), "ok");
    }

    #[test]
    fn a_wrong_total_is_reported() {
        let tree = FixtureTree::generate().expect("generate");
        let mut outcome = outcome_for(&tree);
        outcome.result.files = outcome.result.files.saturating_add(1);

        let check = check(&outcome, tree.manifest(), Backend::Reference);
        assert!(!check.ok);
        assert!(check
            .mismatches
            .iter()
            .any(|message| message.starts_with("files:")));
    }

    #[test]
    fn an_unexpected_skip_is_reported() {
        let tree = FixtureTree::generate().expect("generate");
        let mut outcome = outcome_for(&tree);
        outcome.result.skipped.push(storagelens_core::SkippedDir {
            path: tree.root().join("made-up"),
            reason: storagelens_core::SkipReason::IoError,
        });

        let check = check(&outcome, tree.manifest(), Backend::Reference);
        assert!(!check.ok);
        assert!(check
            .mismatches
            .iter()
            .any(|message| message.starts_with("unexpected skip:")));
    }

    #[test]
    fn an_empty_result_fails_loudly() {
        let tree = FixtureTree::generate().expect("generate");
        let outcome = ScanOutcome {
            result: ScanResult {
                status: ScanStatus::Complete,
                files: 0,
                dirs: 0,
                bytes_alloc: Bytes::ZERO,
                bytes_logical: Bytes::ZERO,
                elapsed_ms: 0,
                skipped: Vec::new(),
            },
            links: 0,
            entries: 0,
            peak_rss_bytes: 0,
        };

        let check = check(&outcome, tree.manifest(), Backend::Reference);
        assert!(!check.ok);
        assert!(check.mismatches.len() >= 5);
    }
}
