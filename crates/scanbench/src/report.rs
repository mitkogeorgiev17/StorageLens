//! Text and JSON output for `scanbench`.
//!
//! The text table is the PLAN §17 example; the JSON form is the stable,
//! machine-readable version of the same numbers (`schema_version: 1`). Both are
//! built from one [`Report`], so they cannot drift apart.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Serialize;
use storagelens_core::ScanStatus;
#[cfg(test)]
use storagelens_core::SkipReason;

use crate::budget::BudgetReport;
use crate::fixtures_check::FixtureCheck;
use crate::platform::Backend;
use crate::walk::ScanOutcome;

/// Version of the `--json` document. Bump only on a breaking change.
pub const SCHEMA_VERSION: u32 = 1;

/// One skipped directory, flattened for output.
#[derive(Debug, Clone, Serialize)]
pub struct SkippedEntry {
    /// Absolute path that was skipped.
    pub path: String,
    /// Stable reason name, e.g. `permission_denied`.
    pub reason: &'static str,
}

/// Everything one `scanbench` run produced.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    /// Always [`SCHEMA_VERSION`] for this build.
    pub schema_version: u32,
    /// The scanned root, canonicalized.
    pub path: String,
    /// Which walker ran: `"platform"` or `"reference"`.
    pub backend: &'static str,
    /// How the walk ended.
    pub status: ScanStatus,
    /// Unique regular files (hard links counted once where the platform allows).
    pub files: u64,
    /// Directories below the root; the root itself is not counted.
    pub dirs: u64,
    /// Symlinks and Windows reparse points. Never followed.
    pub links: u64,
    /// Entries streamed to the catalog, including the root.
    pub entries: u64,
    /// Allocated bytes — the number the app shows.
    pub allocated_bytes: u64,
    /// Apparent size of the same files.
    pub logical_bytes: u64,
    /// Skipped directories, in walk order.
    pub skipped: Vec<SkippedEntry>,
    /// Skip counts by reason, with stable alphabetical keys.
    pub skipped_by_reason: BTreeMap<String, u64>,
    /// Wall time of the walk itself, excluding catalog open and session bookkeeping.
    pub elapsed_ms: u64,
    /// Highest sampled resident set size. `0` when the platform refused to report it.
    pub peak_rss_bytes: u64,
    /// Catalog file this run wrote through.
    pub db: String,
    /// Manifest comparison, only with `--fixtures`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fixture: Option<FixtureCheck>,
    /// Speed-target comparison, only with `--budget`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget: Option<BudgetReport>,
}

impl Report {
    /// Assemble a report from one walk.
    pub fn new(
        root: &Path,
        backend: Backend,
        outcome: &ScanOutcome,
        db: &Path,
        fixture: Option<FixtureCheck>,
        budget: Option<BudgetReport>,
    ) -> Self {
        let skipped: Vec<SkippedEntry> = outcome
            .result
            .skipped
            .iter()
            .map(|skipped| SkippedEntry {
                path: skipped.path.display().to_string(),
                reason: skipped.reason.as_str(),
            })
            .collect();

        let mut skipped_by_reason: BTreeMap<String, u64> = BTreeMap::new();
        for skipped in &outcome.result.skipped {
            *skipped_by_reason
                .entry(skipped.reason.as_str().to_owned())
                .or_insert(0) += 1;
        }

        Self {
            schema_version: SCHEMA_VERSION,
            path: root.display().to_string(),
            backend: backend.as_str(),
            status: outcome.result.status,
            files: outcome.result.files,
            dirs: outcome.result.dirs,
            links: outcome.links,
            entries: outcome.entries,
            allocated_bytes: outcome.result.bytes_alloc.get(),
            logical_bytes: outcome.result.bytes_logical.get(),
            skipped,
            skipped_by_reason,
            elapsed_ms: outcome.result.elapsed_ms,
            peak_rss_bytes: outcome.peak_rss_bytes,
            db: db.display().to_string(),
            fixture,
            budget,
        }
    }

    /// The PLAN §17 text table, one line per field.
    pub fn render_text(&self) -> String {
        let mut out = String::new();
        push_line(&mut out, "path", self.path.clone());
        push_line(&mut out, "backend", self.backend.to_owned());
        push_line(&mut out, "files", group_thousands(self.files));
        push_line(&mut out, "dirs", group_thousands(self.dirs));
        push_line(&mut out, "allocated", human_bytes(self.allocated_bytes));
        push_line(&mut out, "logical", human_bytes(self.logical_bytes));
        push_line(&mut out, "skipped", self.skipped_summary());
        push_line(
            &mut out,
            "elapsed",
            format!("{:.2} s", self.elapsed_ms as f64 / 1000.0),
        );
        push_line(&mut out, "peak rss", human_bytes(self.peak_rss_bytes));

        if let Some(fixture) = &self.fixture {
            push_line(&mut out, "fixture", fixture.summary());
        }
        if let Some(budget) = &self.budget {
            push_line(&mut out, "budget", budget.summary());
        }

        out
    }

    /// `37 dirs (permission denied)`, with every reason listed once.
    fn skipped_summary(&self) -> String {
        let count = self.skipped.len() as u64;
        if count == 0 {
            return "0 dirs".to_owned();
        }

        // Commonest reason first, then alphabetical so the line is deterministic.
        let mut reasons: Vec<(&String, &u64)> = self.skipped_by_reason.iter().collect();
        reasons.sort_by(|left, right| right.1.cmp(left.1).then_with(|| left.0.cmp(right.0)));

        let names: Vec<String> = reasons
            .iter()
            .map(|(reason, _)| reason.replace('_', " "))
            .collect();

        format!("{} dirs ({})", group_thousands(count), names.join(", "))
    }
}

fn push_line(out: &mut String, label: &str, value: String) {
    out.push_str(&format!("{label:<16}{value}\n"));
}

/// Binary units, two decimals below 100 and none above.
///
/// Matches the PLAN §17 examples: `48.21 GB`, `212 MB`. `B` is printed as a
/// whole number because fractional bytes are meaningless.
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];

    if bytes < 1024 {
        return format!("{bytes} B");
    }

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }

    // Round first: 1023.996 MB prints as `1024 MB`, which is wrong — promote it.
    let decimals = decimals_for(value);
    if round_to(value, decimals) >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }

    let decimals = decimals_for(value);
    format!("{value:.decimals$} {}", UNITS[unit])
}

fn decimals_for(value: f64) -> usize {
    if value >= 100.0 {
        0
    } else {
        2
    }
}

fn round_to(value: f64, decimals: usize) -> f64 {
    let factor = 10_f64.powi(decimals as i32);
    (value * factor).round() / factor
}

/// `184203` → `184,203`.
pub fn group_thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// A stable JSON document, pretty-printed and newline-terminated.
pub fn render_json(report: &Report) -> serde_json::Result<String> {
    let mut json = serde_json::to_string_pretty(report)?;
    json.push('\n');
    Ok(json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use storagelens_core::{Bytes, ScanResult, SkippedDir};

    fn outcome(files: u64, dirs: u64, skipped: Vec<SkippedDir>) -> ScanOutcome {
        ScanOutcome {
            result: ScanResult {
                status: if skipped.is_empty() {
                    ScanStatus::Complete
                } else {
                    ScanStatus::Partial
                },
                files,
                dirs,
                bytes_alloc: Bytes::new(51_765_093_335),
                bytes_logical: Bytes::new(54_835_994_951),
                elapsed_ms: 4_310,
                skipped,
                elevated: None,
            },
            links: 7,
            entries: files + dirs + 1,
            peak_rss_bytes: 222_298_112,
        }
    }

    #[test]
    fn the_text_table_matches_the_plan_example() {
        let report = Report::new(
            Path::new("/Users/me/Documents"),
            Backend::Platform,
            &outcome(184_203, 12_884, Vec::new()),
            Path::new("/tmp/scanbench.db"),
            None,
            None,
        );

        assert_eq!(
            report.render_text(),
            "\
path            /Users/me/Documents
backend         platform
files           184,203
dirs            12,884
allocated       48.21 GB
logical         51.07 GB
skipped         0 dirs
elapsed         4.31 s
peak rss        212 MB
"
        );
    }

    #[test]
    fn skips_are_summarised_by_reason() {
        let skipped = vec![
            SkippedDir {
                path: "/a".into(),
                reason: SkipReason::PermissionDenied,
            },
            SkippedDir {
                path: "/b".into(),
                reason: SkipReason::PermissionDenied,
            },
            SkippedDir {
                path: "/c".into(),
                reason: SkipReason::IoError,
            },
        ];
        let report = Report::new(
            Path::new("/tmp"),
            Backend::Platform,
            &outcome(1, 1, skipped),
            Path::new("/tmp/scanbench.db"),
            None,
            None,
        );

        assert!(report
            .render_text()
            .contains("skipped         3 dirs (permission denied, io error)\n"));
        assert_eq!(report.skipped_by_reason["permission_denied"], 2);
        assert_eq!(report.skipped_by_reason["io_error"], 1);
    }

    #[test]
    fn fixture_and_budget_lines_only_appear_when_requested() {
        let report = Report::new(
            Path::new("/tmp"),
            Backend::Platform,
            &outcome(1, 1, Vec::new()),
            Path::new("/tmp/scanbench.db"),
            None,
            None,
        );
        assert!(!report.render_text().contains("fixture"));
        assert!(!report.render_text().contains("budget"));

        let report = Report::new(
            Path::new("/tmp"),
            Backend::Platform,
            &outcome(1, 1, Vec::new()),
            Path::new("/tmp/scanbench.db"),
            Some(FixtureCheck {
                ok: true,
                mismatches: Vec::new(),
            }),
            Some(crate::budget::evaluate(1, 1, 1)),
        );
        let text = report.render_text();
        assert!(text.contains("fixture         ok\n"));
        assert!(text.contains("budget          ok\n"));
    }

    #[test]
    fn json_is_versioned_and_omits_absent_checks() {
        let report = Report::new(
            Path::new("/tmp"),
            Backend::Platform,
            &outcome(1, 1, Vec::new()),
            Path::new("/tmp/scanbench.db"),
            None,
            None,
        );
        let json = render_json(&report).expect("serialise");
        let value: serde_json::Value = serde_json::from_str(&json).expect("parse");

        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["path"], "/tmp");
        assert_eq!(value["backend"], "platform");
        assert_eq!(value["files"], 1);
        assert_eq!(value["allocated_bytes"], 51_765_093_335_u64);
        assert!(value.get("fixture").is_none());
        assert!(value.get("budget").is_none());
        assert!(json.ends_with('\n'));
    }

    #[test]
    fn human_bytes_uses_binary_units() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.00 KB");
        assert_eq!(human_bytes(1536), "1.50 KB");
        assert_eq!(human_bytes(1024 * 1024), "1.00 MB");
        assert_eq!(human_bytes(222_298_112), "212 MB");
        assert_eq!(human_bytes(51_765_093_335), "48.21 GB");
        assert_eq!(human_bytes(1024_u64.pow(5)), "1.00 PB");
    }

    #[test]
    fn rounding_does_not_print_1024_of_a_smaller_unit() {
        // 1023.999 MB rounds to 1024 MB at zero decimals; it must promote to GB.
        let bytes = (1023.999 * 1024.0 * 1024.0) as u64;
        assert!(
            human_bytes(bytes).ends_with(" GB"),
            "{}",
            human_bytes(bytes)
        );
    }

    #[test]
    fn thousands_are_grouped() {
        assert_eq!(group_thousands(0), "0");
        assert_eq!(group_thousands(999), "999");
        assert_eq!(group_thousands(1000), "1,000");
        assert_eq!(group_thousands(184_203), "184,203");
        assert_eq!(group_thousands(1_000_000), "1,000,000");
    }
}
