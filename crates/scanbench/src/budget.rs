//! The PLAN §6.5 speed gate.
//!
//! `--budget` compares one run against the published targets and exits `1` when
//! any of them is missed. Targets that depend on the size of the scan only apply
//! once the scan is large enough to be comparable.

use serde::Serialize;

/// PLAN §6.5: peak RSS under 400 MB.
pub const PEAK_RSS_LIMIT_BYTES: u64 = 400 * 1024 * 1024;

/// PLAN §6.5: 1M files, cold cache, under 90 s.
pub const ONE_MILLION_ELAPSED_LIMIT_MS: u64 = 90_000;

/// PLAN §6.5: 3M files under 4 minutes.
pub const THREE_MILLION_ELAPSED_LIMIT_MS: u64 = 240_000;

/// File counts at which an elapsed-time target starts to apply.
pub const ONE_MILLION: u64 = 1_000_000;
/// File count at which the larger elapsed-time target applies.
pub const THREE_MILLION: u64 = 3_000_000;

/// One comparison against a target.
#[derive(Debug, Clone, Serialize)]
pub struct BudgetCheck {
    /// Stable identifier, e.g. `peak_rss`.
    pub name: &'static str,
    /// The limit that must not be exceeded.
    pub limit: u64,
    /// What this run measured.
    pub actual: u64,
    /// Whether the run stayed within the limit.
    pub ok: bool,
}

/// The result of every applicable check.
#[derive(Debug, Clone, Serialize)]
pub struct BudgetReport {
    /// True only when every check passed.
    pub ok: bool,
    /// Checks that ran. Empty for scans below 1M files except for RSS.
    pub checks: Vec<BudgetCheck>,
}

impl BudgetReport {
    /// A one-line summary for the text output.
    pub fn summary(&self) -> String {
        if self.ok {
            return "ok".to_owned();
        }

        let failures: Vec<String> = self
            .checks
            .iter()
            .filter(|check| !check.ok)
            .map(|check| format!("{} {} > {}", check.name, check.actual, check.limit))
            .collect();
        format!("FAILED ({})", failures.join(", "))
    }
}

/// Evaluate the targets that apply to a scan of `files` files.
pub fn evaluate(files: u64, elapsed_ms: u64, peak_rss_bytes: u64) -> BudgetReport {
    let mut checks = vec![BudgetCheck {
        name: "peak_rss",
        limit: PEAK_RSS_LIMIT_BYTES,
        actual: peak_rss_bytes,
        ok: peak_rss_bytes <= PEAK_RSS_LIMIT_BYTES,
    }];

    // Below 1M files the target table has no elapsed-time entry: a small scan
    // finishing in 2 s says nothing about a million-file scan.
    let elapsed_limit = if files >= THREE_MILLION {
        Some(THREE_MILLION_ELAPSED_LIMIT_MS)
    } else if files >= ONE_MILLION {
        Some(ONE_MILLION_ELAPSED_LIMIT_MS)
    } else {
        None
    };

    if let Some(limit) = elapsed_limit {
        checks.push(BudgetCheck {
            name: "elapsed",
            limit,
            actual: elapsed_ms,
            ok: elapsed_ms <= limit,
        });
    }

    BudgetReport {
        ok: checks.iter().all(|check| check.ok),
        checks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_scan_only_checks_memory() {
        let report = evaluate(1_000, 500, 10 * 1024 * 1024);
        assert!(report.ok);
        assert_eq!(report.checks.len(), 1);
        assert_eq!(report.checks[0].name, "peak_rss");
    }

    #[test]
    fn one_million_files_must_finish_within_ninety_seconds() {
        let report = evaluate(ONE_MILLION, 90_001, 1);
        assert!(!report.ok);
        assert_eq!(report.summary(), "FAILED (elapsed 90001 > 90000)");

        assert!(evaluate(ONE_MILLION, 90_000, 1).ok);
    }

    #[test]
    fn three_million_files_get_the_four_minute_target() {
        let report = evaluate(THREE_MILLION, 120_000, 1);
        assert!(report.ok);
        assert_eq!(report.checks[1].limit, THREE_MILLION_ELAPSED_LIMIT_MS);
    }

    #[test]
    fn peak_rss_over_the_limit_fails() {
        let report = evaluate(10, 1, PEAK_RSS_LIMIT_BYTES + 1);
        assert!(!report.ok);
    }
}
