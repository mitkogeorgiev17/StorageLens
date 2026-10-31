//! `scanbench` — measure scanner correctness and speed without a UI.
//!
//! One run walks a path with the portable reference walker, streams every entry
//! through the real catalog ([`Catalog`] + [`EntryWriter`]), verifies that the row
//! count matches what the walker produced, and prints the PLAN §17 table (or the
//! stable `--json` document).
//!
//! ```text
//! scanbench <PATH> [--json] [--budget] [--db PATH]
//! scanbench --fixtures [--json] [--budget]
//! ```
//!
//! # Exit codes
//!
//! | Code | Meaning |
//! |---|---|
//! | `0` | Scan completed and every requested check passed |
//! | `1` | `--budget` was requested and a PLAN §6.5 target was missed |
//! | `2` | The scan, the catalog write, or `--fixtures` validation failed |
//!
//! A fixture mismatch is a validation error (`2`) and takes precedence over an
//! over-budget result (`1`): a fast but wrong scanner is not a pass.
//!
//! # Catalog
//!
//! Every run persists through the same code path the app uses, so the harness
//! exercises the real writer instead of a mock. By default the catalog lives in a
//! temporary directory that is deleted on exit; `--db PATH` keeps it for
//! inspection with any SQLite client.

#![forbid(unsafe_code)]

mod budget;
mod fixtures_check;
mod report;
mod walk;

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use storagelens_catalog::{Catalog, Platform};
use storagelens_core::{CancelToken, EntrySink, FixtureTree, ScanError, ScanOptions, ScanStatus};

use crate::report::Report;

/// Everything passed checks.
const EXIT_OK: u8 = 0;
/// `--budget` requested and a target was missed.
const EXIT_OVER_BUDGET: u8 = 1;
/// Scan, catalog, or fixture validation failure.
const EXIT_SCAN_ERROR: u8 = 2;

#[derive(Debug, Parser)]
#[command(
    name = "scanbench",
    version,
    about = "Measure scanner correctness and speed.",
    after_help = "Exit codes: 0 ok, 1 over budget, 2 scan or validation error."
)]
struct Cli {
    /// Directory or file to scan. Required unless --fixtures is used.
    #[arg(
        value_name = "PATH",
        required_unless_present = "fixtures",
        conflicts_with = "fixtures"
    )]
    path: Option<PathBuf>,

    /// Print the stable JSON report instead of the text table.
    #[arg(long)]
    json: bool,

    /// Scan the built-in fixture tree and assert exact counts and byte totals.
    #[arg(long)]
    fixtures: bool,

    /// Exit 1 when a PLAN §6.5 speed target is missed.
    #[arg(long)]
    budget: bool,

    /// Keep the catalog at PATH instead of a temporary directory.
    #[arg(long, value_name = "PATH")]
    db: Option<PathBuf>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("scanbench: {error}");
            ExitCode::from(EXIT_SCAN_ERROR)
        }
    }
}

fn run(cli: &Cli) -> Result<u8, Box<dyn Error>> {
    // The fixture root is used exactly as generated: on macOS `temp_dir()` is
    // `/var/...`, which canonicalizes to `/private/var/...`, and the manifest
    // records the former. Canonicalizing would break the skipped-path comparison.
    let fixture_tree = if cli.fixtures {
        Some(FixtureTree::generate()?)
    } else {
        None
    };

    let root = match (&fixture_tree, &cli.path) {
        (Some(tree), _) => tree.root().to_path_buf(),
        (None, Some(path)) => canonical_root(path)?,
        (None, None) => return Err("no path given and --fixtures not set".into()),
    };

    // Keep the temporary catalog alive for the whole run, including the report.
    let temp_dir = match &cli.db {
        Some(_) => None,
        None => Some(tempfile::tempdir()?),
    };
    let db_path = match (&cli.db, &temp_dir) {
        (Some(path), _) => path.clone(),
        (None, Some(dir)) => dir.path().join("scanbench.db"),
        (None, None) => return Err("internal error: no catalog path".into()),
    };

    let catalog = Catalog::open(&db_path)?;
    let scan_id = catalog.begin_session(Platform::current(), false)?;
    let mut writer = catalog.writer(scan_id)?;

    let cancel = CancelToken::new();
    let options = ScanOptions::default();
    let scanned = walk::scan(&root, &options, &cancel, &mut writer);

    // Flush even after a failure so a partial scan stays consistent.
    let flushed = writer.flush();
    drop(writer);

    let outcome = match (scanned, flushed) {
        (Ok(outcome), Ok(())) => outcome,
        (Ok(_), Err(error)) => return Err(error.into()),
        (Err(error), _) => {
            let status = if matches!(error, ScanError::Cancelled) {
                ScanStatus::Cancelled
            } else {
                ScanStatus::Partial
            };
            // A failed walk has no final counts, so the session records zeros
            // rather than a misleading partial total.
            let _ = catalog.finish_session(scan_id, status, 0, 0, 0);
            return Err(error.into());
        }
    };

    catalog.finish_session(
        scan_id,
        outcome.result.status,
        outcome.result.files,
        outcome.result.bytes_alloc.get(),
        outcome.result.skipped.len() as u64,
    )?;

    // The writer's row count is the thing the app depends on; a mismatch means
    // entries were dropped between the walker and SQLite.
    let persisted = catalog.entry_count(scan_id)?;
    if persisted != outcome.entries {
        return Err(format!(
            "catalog holds {persisted} entries, the walk produced {}",
            outcome.entries
        )
        .into());
    }

    let fixture = fixture_tree
        .as_ref()
        .map(|tree| fixtures_check::check(&outcome, tree.manifest()));
    let budget = cli.budget.then(|| {
        budget::evaluate(
            outcome.result.files,
            outcome.result.elapsed_ms,
            outcome.peak_rss_bytes,
        )
    });

    let report = Report::new(&root, &outcome, &db_path, fixture, budget);

    if cli.json {
        print!("{}", report::render_json(&report)?);
    } else {
        print!("{}", report.render_text());
    }

    let mut failed_validation = false;
    if let Some(check) = &report.fixture {
        for mismatch in &check.mismatches {
            eprintln!("fixture mismatch: {mismatch}");
        }
        failed_validation = !check.ok;
    }

    if let Some(budget) = &report.budget {
        for check in budget.checks.iter().filter(|check| !check.ok) {
            eprintln!(
                "over budget: {} = {} (limit {})",
                check.name, check.actual, check.limit
            );
        }
    }

    if failed_validation {
        return Ok(EXIT_SCAN_ERROR);
    }
    if report.budget.as_ref().is_some_and(|budget| !budget.ok) {
        return Ok(EXIT_OVER_BUDGET);
    }
    Ok(EXIT_OK)
}

/// Resolve a user path to an absolute one so the report and skip rules agree.
fn canonical_root(path: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let canonical = fs::canonicalize(path).map_err(|source| {
        std::io::Error::new(
            source.kind(),
            format!("cannot open {}: {source}", path.display()),
        )
    })?;
    Ok(strip_verbatim(canonical))
}

/// Windows `canonicalize` returns a `\\?\` verbatim path. Keeping it would defeat
/// the `C:\Windows` prefix check in the walker, so strip it for display and use.
#[cfg(windows)]
fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path
}

#[cfg(not(windows))]
fn strip_verbatim(path: PathBuf) -> PathBuf {
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn a_path_is_required_unless_fixtures_is_set() {
        assert!(Cli::try_parse_from(["scanbench"]).is_err());
        assert!(Cli::try_parse_from(["scanbench", "--fixtures"]).is_ok());
        assert!(Cli::try_parse_from(["scanbench", "/tmp"]).is_ok());
        assert!(Cli::try_parse_from(["scanbench", "/tmp", "--fixtures"]).is_err());
    }

    #[test]
    fn flags_parse() {
        let cli = Cli::try_parse_from([
            "scanbench",
            "/tmp",
            "--json",
            "--budget",
            "--db",
            "/tmp/catalog.db",
        ])
        .expect("parse");
        assert!(cli.json);
        assert!(cli.budget);
        assert!(!cli.fixtures);
        assert_eq!(cli.db.as_deref(), Some(Path::new("/tmp/catalog.db")));
    }
}
