//! The M1 accuracy gate from PLAN §17.
//!
//! Every check drives the `scanbench` binary, so the thing under test is the path
//! the app ships: `MacScanner` → `Catalog`/`EntryWriter` → JSON totals. A library
//! call would skip the catalog and could hide a bug in the writer.
//!
//! Two comparisons run:
//!
//! 1. The generated fixture tree must match its manifest **exactly** — file and
//!    directory counts, symlinks, allocated bytes, logical bytes.
//! 2. A generated 100k-file tree must match `du -sx` within **2%**.
//!
//! `--reference` is also exercised, because a disagreement between the two
//! backends is easier to bisect than a disagreement with `du`.
//!
//! # Why a generated tree instead of a system directory
//!
//! CI machines differ, and system directories are full of things that legitimately
//! make `du` and a scanner disagree: TCC-protected folders the process cannot
//! read, local Time Machine snapshots (invisible to `du` but real space), APFS
//! clones (both names report the full size), purgeable space, and `/private/var/vm`.
//! They also change between runs, so a failure would not reproduce.
//!
//! The generated tree has known contents and none of those complications, so any
//! drift is a scanner bug. The exclusions above are documented in PLAN §6.3 and
//! §17; they are why a full `/` scan cannot be compared line-for-line with `du`.
//!
//! # `du` and `df` semantics
//!
//! macOS `du` reports 512-byte blocks by default; `-k` makes it 1024-byte blocks,
//! and `-x` stops at filesystem boundaries. (BSD `du` treats `-s` and `-d depth`
//! as mutually exclusive, so the `du -sxd` spelling in AGENTS.md exits 64;
//! `-s` is `-d 0`.) `du` counts each hard-linked inode once,
//! which is exactly the scanner's rule, and uses the same `st_blocks`/allocated
//! size the scanner displays.
//!
//! `df -k` describes the whole volume, not the scanned subtree, so it is only used
//! as an upper bound (a subtree cannot occupy more than its volume) and to fail
//! early with a clear message when the runner lacks room for the tree.

#![cfg(target_os = "macos")]

use std::error::Error;
use std::fs::{self, File};
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

/// PLAN §17: the real-tree tolerance.
const TOLERANCE_PERCENT: u64 = 2;

/// Space the generated tree needs before it is created. Each 1-byte file occupies
/// one 4 KiB block on APFS, so 100k files is ~400 MB.
const REQUIRED_FREE_BYTES: u64 = 1024 * 1024 * 1024;

fn scanbench(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_scanbench"))
        .args(args)
        .output()
        .expect("run scanbench")
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Run `scanbench PATH --json` and parse the report, failing with its stderr.
fn scan_report(path: &Path) -> Value {
    let path = path.to_str().expect("utf-8 path");
    let output = scanbench(&[path, "--json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "scanbench {path} failed: {}",
        stderr(&output)
    );
    serde_json::from_slice(&output.stdout).expect("json report")
}

fn scan_bytes(path: &Path) -> u64 {
    scan_report(path)["allocated_bytes"]
        .as_u64()
        .expect("allocated_bytes")
}

/// `du -sk PATH` in bytes.
///
/// `-s` summarises, `-k` prints 1024-byte blocks, `-x` stays on one filesystem.
/// A failure is a test bug, not a scanner result, so it panics with stderr.
fn du_bytes(path: &Path) -> u64 {
    let output = Command::new("du")
        .args(["-s", "-k", path.to_str().expect("utf-8 path")])
        .output()
        .expect("run du");
    assert!(
        output.status.success(),
        "du failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 du output");
    let kib: u64 = stdout
        .split_whitespace()
        .next()
        .expect("du printed a number")
        .parse()
        .expect("du printed a decimal number");
    kib.saturating_mul(1024)
}

/// Available and used bytes from `df -k PATH`.
fn df_bytes(path: &Path) -> (u64, u64) {
    let output = Command::new("df")
        .args(["-k", path.to_str().expect("utf-8 path")])
        .output()
        .expect("run df");
    assert!(
        output.status.success(),
        "df failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8(output.stdout).expect("utf-8 df output");
    let fields: Vec<&str> = stdout
        .lines()
        .nth(1)
        .expect("df printed a data row")
        .split_whitespace()
        .collect();
    assert!(fields.len() >= 4, "unexpected df output: {stdout:?}");

    let used: u64 = fields[2].parse().expect("df used");
    let available: u64 = fields[3].parse().expect("df available");
    (available.saturating_mul(1024), used.saturating_mul(1024))
}

/// Bytes of drift between two totals, signed.
fn drift(scanned: u64, expected: u64) -> i128 {
    i128::from(scanned) - i128::from(expected)
}

/// 2% of `expected`, rounded down but never zero when there is something to
/// compare. Integer maths keeps the gate exact and free of float surprises.
fn tolerance(expected: u64) -> u64 {
    (u128::from(expected) * u128::from(TOLERANCE_PERCENT) / 100) as u64
}

/// A per-top-level-child breakdown for a failing comparison.
///
/// Scanning each child separately is only done on the failure path, so a passing
/// run pays nothing for it. Sorted by absolute drift, biggest first: the subtree
/// that caused the failure is the first line the reader sees.
fn describe_drift(root: &Path, scanned: u64, expected: u64) -> String {
    let mut rows: Vec<(String, u64, u64, i128)> = Vec::new();

    let mut children: Vec<_> = fs::read_dir(root)
        .expect("read the scanned root")
        .map(|entry| entry.expect("directory entry"))
        .filter(|entry| entry.file_type().expect("file type").is_dir())
        .collect();
    children.sort_by_key(|entry| entry.file_name());

    for entry in children {
        let path = entry.path();
        let child_scanned = scan_bytes(&path);
        let child_expected = du_bytes(&path);
        rows.push((
            entry.file_name().to_string_lossy().into_owned(),
            child_scanned,
            child_expected,
            drift(child_scanned, child_expected),
        ));
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.3.abs()));

    let mut out = format!(
        "scan {scanned} bytes vs du {expected} bytes (drift {:+} bytes, {:.4}%)\n",
        drift(scanned, expected),
        (drift(scanned, expected).abs() as f64 / expected as f64) * 100.0
    );
    if rows.is_empty() {
        out.push_str("  the root has no directory children to break down\n");
    }
    for (name, child_scanned, child_expected, child_drift) in rows {
        out.push_str(&format!(
            "  {name:<16} scan {child_scanned:>12}  du {child_expected:>12}  \
             drift {child_drift:>+12}  ({:+.2}%)\n",
            (child_drift as f64 / child_expected.max(1) as f64) * 100.0
        ));
    }
    out
}

/// Build a deterministic tree with the shapes the scanner must agree on.
///
/// 100 directories × 1000 one-byte files (one 4 KiB block each on APFS), plus a
/// multi-block file, an empty file, a sparse file, and three names for one inode.
/// Together they cover allocated vs logical, zero allocation, and hard-link
/// deduplication — the three places a size scanner usually goes wrong.
fn generate_tree(root: &Path) -> Result<(), Box<dyn Error>> {
    for dir in 0..100 {
        let dir = root.join(format!("dir{dir:03}"));
        fs::create_dir(&dir)?;
        for file in 0..1000 {
            fs::write(dir.join(format!("file{file:04}.dat")), b"x")?;
        }
    }

    fs::write(root.join("big.bin"), vec![b'y'; 5_000])?;
    fs::write(root.join("empty.bin"), b"")?;

    // Ten megabytes apparent, no blocks allocated. `du` counts the blocks, so the
    // scanner must too; counting the logical size would show up as a huge drift.
    let sparse = File::create(root.join("sparse.bin"))?;
    sparse.set_len(10 * 1024 * 1024)?;
    drop(sparse);

    // One inode, three names: counted once by both `du` and the scanner.
    let owner = root.join("hard-a.txt");
    fs::write(&owner, vec![b'z'; 9_000])?;
    fs::hard_link(&owner, root.join("hard-b.txt"))?;
    fs::hard_link(&owner, root.join("hard-c.txt"))?;

    Ok(())
}

#[test]
fn the_fixture_tree_matches_its_manifest_exactly() {
    let output = scanbench(&["--fixtures", "--json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "fixture scan failed: {}",
        stderr(&output)
    );

    let report: Value = serde_json::from_slice(&output.stdout).expect("json report");
    assert_eq!(
        report["backend"], "platform",
        "the gate must exercise MacScanner, not the reference walker"
    );
    assert_eq!(report["fixture"]["ok"], true, "report: {report}");
    assert_eq!(
        report["fixture"]["mismatches"]
            .as_array()
            .expect("mismatches array")
            .len(),
        0,
        "report: {report}"
    );

    // The manifest is the oracle, so 0 bytes of drift is the acceptance criterion.
    assert!(report["allocated_bytes"].as_u64().expect("allocated") > 0);
    assert!(report["logical_bytes"].as_u64().expect("logical") > 0);
}

#[test]
fn the_reference_walker_passes_the_same_fixture_gate() {
    // A separate generated tree, but the same exact manifest comparison. This
    // proves the baseline is honest, which is what makes a platform/reference
    // disagreement meaningful.
    let output = scanbench(&["--fixtures", "--reference", "--json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "reference fixture scan failed: {}",
        stderr(&output)
    );

    let report: Value = serde_json::from_slice(&output.stdout).expect("json report");
    assert_eq!(report["backend"], "reference");
    assert_eq!(report["fixture"]["ok"], true, "report: {report}");
}

#[test]
fn a_generated_100k_file_tree_is_within_two_percent_of_du() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    let (available, _) = df_bytes(root);
    assert!(
        available >= REQUIRED_FREE_BYTES,
        "the test tree needs at least {REQUIRED_FREE_BYTES} free bytes, \
         the volume has {available}"
    );

    generate_tree(root).expect("generate the real tree");

    let report = scan_report(root);
    assert_eq!(
        report["backend"], "platform",
        "the accuracy gate must exercise MacScanner"
    );

    let scanned = report["allocated_bytes"].as_u64().expect("allocated_bytes");
    let expected = du_bytes(root);

    // A subtree cannot occupy more than its whole volume. `df` is coarse and
    // whole-volume, so this is a sanity bound, not an accuracy check.
    let (_, volume_used) = df_bytes(root);
    assert!(
        scanned <= volume_used,
        "the scan reports {scanned} bytes, more than the whole volume's {volume_used}"
    );

    assert!(
        drift(scanned, expected).abs() <= i128::from(tolerance(expected)),
        "the real tree drifted more than {TOLERANCE_PERCENT}% from du:\n{}",
        describe_drift(root, scanned, expected)
    );

    // The tree is deliberately shaped, so the exact expectations are stable too.
    assert_eq!(
        report["files"].as_u64().expect("files"),
        100_004,
        "100k small files + big + empty + sparse + one hard-linked inode"
    );
    assert_eq!(report["dirs"].as_u64().expect("dirs"), 100);
}

#[test]
fn the_reference_walker_agrees_with_the_platform_scanner() {
    // A smaller tree keeps this test fast; agreement on every size rule is what
    // matters, not the entry count.
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();

    for dir in 0..8 {
        let dir = root.join(format!("dir{dir}"));
        fs::create_dir(&dir).expect("dir");
        for file in 0..64 {
            fs::write(dir.join(format!("file{file}.dat")), vec![b'x'; file + 1]).expect("file");
        }
    }
    let sparse = File::create(root.join("sparse.bin")).expect("sparse");
    sparse.set_len(1024 * 1024).expect("set_len");
    drop(sparse);

    let platform = scan_report(root);
    let reference = scan_report_with(&["--reference"], root);

    assert_eq!(platform["backend"], "platform");
    assert_eq!(reference["backend"], "reference");
    for field in ["files", "dirs", "links", "allocated_bytes", "logical_bytes"] {
        assert_eq!(
            platform[field], reference[field],
            "platform and reference disagree on {field}: {platform} vs {reference}"
        );
    }
}

/// `scan_report` with extra flags before the path.
fn scan_report_with(flags: &[&str], path: &Path) -> Value {
    let path = path.to_str().expect("utf-8 path");
    let mut args: Vec<&str> = flags.to_vec();
    args.push(path);
    args.push("--json");

    let output = scanbench(&args);
    assert_eq!(
        output.status.code(),
        Some(0),
        "scanbench {args:?} failed: {}",
        stderr(&output)
    );
    serde_json::from_slice(&output.stdout).expect("json report")
}
