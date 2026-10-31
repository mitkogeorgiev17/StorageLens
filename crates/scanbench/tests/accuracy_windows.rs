//! The M2 accuracy gate from PLAN §17.
//!
//! Every check drives the `scanbench` binary, so the thing under test is the path
//! the app ships: `WinScanner` → `Catalog`/`EntryWriter` → JSON totals.
//!
//! Three comparisons run:
//!
//! 1. The generated fixture tree must match **exactly**: file, directory and link
//!    counts and logical bytes against its manifest, and allocated bytes against
//!    a per-file `GetFileInformationByHandleEx(FileStandardInfo)` oracle. (The
//!    manifest's Windows allocation is the logical size — stable `std` has no
//!    allocation query — so it cannot be the oracle for that one number.)
//! 2. A generated 100k-file tree must match
//!    `Get-ChildItem -Recurse -Force -File | Measure-Object Length -Sum` within
//!    **2%** — and exactly, once `Get-ChildItem`'s per-name hard-link counting is
//!    accounted for.
//! 3. `--reference` must agree with the platform scanner on everything the
//!    portable walker can know.
//!
//! # What `Get-ChildItem` measures
//!
//! `Length` is the **logical** size, and every hard-link name is listed, so its
//! sum is compared with `logical_bytes`, not `allocated_bytes`. PowerShell 7 does
//! not follow junctions or symlinks during `-Recurse`, the scanner's rule too.
//!
//! # Exclusions
//!
//! A generated tree is used instead of a system directory for the same reasons as
//! on macOS (CI machines differ; system folders change between runs). On a real
//! `C:\`, these legitimately make the scanner and `Get-ChildItem` / Explorer's
//! drive properties disagree, and are excluded from any comparison (PLAN §17):
//! `pagefile.sys`, `hiberfil.sys`, `swapfile.sys`, `System Volume Information`
//! (restore points, unreadable unelevated), `$Recycle.Bin` (skipped by default),
//! NTFS dedup and block clones (overcounted by design), and the MFT and other
//! metadata files Explorer's "used space" includes but no directory lists.
//!
//! Explorer's drive properties cannot be read from a test. That comparison is a
//! manual step: `scanbench C:\ --json`, against "Used space" minus the exclusions.

#![cfg(windows)]

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;
use storagelens_core::fixtures::FixtureTree;

/// PLAN §17: the real-tree tolerance.
const TOLERANCE_PERCENT: u64 = 2;

/// Bytes in each hard-linked file of the generated tree.
const HARD_LINK_BYTES: u64 = 9_000;
/// Names for that one file.
const HARD_LINK_NAMES: u64 = 3;

fn scanbench(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_scanbench"))
        .args(args)
        .output()
        .expect("run scanbench")
}

/// Run `scanbench [flags] PATH --json` and parse the report.
fn scan_report_with(flags: &[&str], path: &Path) -> Value {
    let path = path.to_str().expect("utf-8 path");
    let mut args: Vec<&str> = flags.to_vec();
    args.extend([path, "--json"]);

    let output = scanbench(&args);
    assert_eq!(
        output.status.code(),
        Some(0),
        "scanbench {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("json report")
}

fn scan_report(path: &Path) -> Value {
    scan_report_with(&[], path)
}

fn field(report: &Value, name: &str) -> u64 {
    report[name]
        .as_u64()
        .unwrap_or_else(|| panic!("{name} missing from {report}"))
}

/// The long-name form of a temp path. `%TEMP%` on CI contains an 8.3 short name
/// (`RUNNER~1`), which `Get-ChildItem` would expand.
fn long_form(path: &Path) -> PathBuf {
    let canonical = fs::canonicalize(path).expect("canonicalize");
    let text = canonical.to_string_lossy();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
}

/// Run a PowerShell 7 snippet with `$env:SL_ROOT` set, returning stdout.
fn pwsh(root: &Path, script: &str) -> String {
    let output = Command::new("pwsh")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("SL_ROOT", root)
        .stdin(Stdio::null())
        .output()
        .expect("run pwsh (PowerShell 7 is preinstalled on GitHub runners)");
    assert!(
        output.status.success(),
        "pwsh failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("utf-8 pwsh output")
}

/// `(file count, Length sum)` per `Get-ChildItem -Recurse -Force -File`.
fn get_child_item(root: &Path) -> (u64, u64) {
    let stdout = pwsh(
        root,
        "$m = Get-ChildItem -LiteralPath $env:SL_ROOT -Recurse -Force -File | \
         Measure-Object -Property Length -Sum; \
         '{0} {1}' -f $m.Count, [int64]$m.Sum",
    );
    let mut parts = stdout.split_whitespace();
    let count = parts.next().and_then(|n| n.parse().ok());
    let sum = parts.next().and_then(|n| n.parse().ok());
    match (count, sum) {
        (Some(count), Some(sum)) => (count, sum),
        _ => panic!("unexpected Get-ChildItem output: {stdout:?}"),
    }
}

/// Used bytes on the volume holding `root`.
fn volume_used(root: &Path) -> u64 {
    pwsh(
        root,
        "$d = [System.IO.DriveInfo]::new($env:SL_ROOT); $d.TotalSize - $d.TotalFreeSpace",
    )
    .trim()
    .parse()
    .expect("volume used bytes")
}

fn drift(scanned: u64, expected: u64) -> i128 {
    i128::from(scanned) - i128::from(expected)
}

/// 2% of `expected`, in integer maths.
fn tolerance(expected: u64) -> u64 {
    (u128::from(expected) * u128::from(TOLERANCE_PERCENT) / 100) as u64
}

/// A per-top-level-child breakdown for a failing comparison, biggest drift
/// first. Only computed on the failure path.
fn describe_drift(root: &Path, scanned: u64, expected: u64) -> String {
    let mut rows: Vec<(String, u64, u64, i128)> = Vec::new();
    let mut children: Vec<_> = fs::read_dir(root)
        .expect("read the scanned root")
        .map(|entry| entry.expect("directory entry"))
        .filter(|entry| entry.file_type().expect("file type").is_dir())
        .collect();
    children.sort_by_key(|entry| entry.file_name());

    for entry in children {
        let path = long_form(&entry.path());
        let child_scanned = field(&scan_report(&path), "logical_bytes");
        let (_, child_expected) = get_child_item(&path);
        rows.push((
            entry.file_name().to_string_lossy().into_owned(),
            child_scanned,
            child_expected,
            drift(child_scanned, child_expected),
        ));
    }
    rows.sort_by_key(|row| std::cmp::Reverse(row.3.abs()));

    let mut out = format!(
        "scan {scanned} bytes vs Get-ChildItem {expected} bytes (drift {:+} bytes, {:.4}%)\n",
        drift(scanned, expected),
        (drift(scanned, expected).abs() as f64 / expected.max(1) as f64) * 100.0
    );
    for (name, child_scanned, child_expected, child_drift) in rows {
        out.push_str(&format!(
            "  {name:<16} scan {child_scanned:>12}  gci {child_expected:>12}  \
             drift {child_drift:>+12}  ({:+.2}%)\n",
            (child_drift as f64 / child_expected.max(1) as f64) * 100.0
        ));
    }
    out
}

/// Allocation and file index of one file, from its handle — the slow per-file
/// path the scanner avoids, used as the oracle.
fn handle_info(path: &Path) -> (u64, u64) {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;

    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FileStandardInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
        BY_HANDLE_FILE_INFORMATION, FILE_STANDARD_INFO,
    };

    let file = fs::OpenOptions::new()
        .access_mode(0)
        .open(path)
        .unwrap_or_else(|error| panic!("open {}: {error}", path.display()));
    let handle = HANDLE(file.as_raw_handle());

    let mut by_handle = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `handle` is open for the lifetime of `file`, and `by_handle` is a
    // valid, writable BY_HANDLE_FILE_INFORMATION.
    unsafe { GetFileInformationByHandle(handle, &mut by_handle) }.expect("by-handle info");

    let mut standard = FILE_STANDARD_INFO::default();
    // SAFETY: as above; the size passed is exactly that of `standard`.
    unsafe {
        GetFileInformationByHandleEx(
            handle,
            FileStandardInfo,
            (&mut standard as *mut FILE_STANDARD_INFO).cast(),
            std::mem::size_of::<FILE_STANDARD_INFO>() as u32,
        )
    }
    .expect("standard info");

    let index = (u64::from(by_handle.nFileIndexHigh) << 32) | u64::from(by_handle.nFileIndexLow);
    (standard.AllocationSize as u64, index)
}

/// Allocated bytes under `root`, each file index counted once, links not
/// followed. Allowed here: a test over a temp dir (AGENTS.md §5.4 governs
/// application code).
fn allocated_oracle(root: &Path) -> u64 {
    let mut seen = BTreeSet::new();
    let mut total = 0u64;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read_dir") {
            let path = entry.expect("dir entry").path();
            let metadata = fs::symlink_metadata(&path).expect("symlink_metadata");
            if metadata.file_type().is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                stack.push(path);
            } else {
                let (alloc, index) = handle_info(&path);
                if seen.insert(index) {
                    total += alloc;
                }
            }
        }
    }
    total
}

#[test]
fn the_fixture_tree_matches_exactly() {
    // The harness's own fixture run first: it is what `scanbench --fixtures`
    // users see.
    let output = scanbench(&["--fixtures", "--json"]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "fixture scan failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("json report");
    assert_eq!(
        report["backend"], "platform",
        "the gate must exercise WinScanner, not the reference walker"
    );
    assert_eq!(report["fixture"]["ok"], true, "report: {report}");

    // Then a tree this test keeps, so allocation has an oracle.
    let tree = FixtureTree::generate().expect("generate fixture tree");
    let manifest = tree.manifest();
    let root = long_form(tree.root());
    let report = scan_report(&root);

    assert_eq!(report["backend"], "platform");
    assert_eq!(field(&report, "files"), manifest.files, "files");
    assert_eq!(field(&report, "dirs"), manifest.dirs, "dirs");
    assert_eq!(
        field(&report, "links"),
        manifest.symlinks + manifest.junctions,
        "links"
    );
    assert_eq!(
        field(&report, "logical_bytes"),
        manifest.bytes_logical,
        "logical bytes"
    );
    // 0 bytes of drift is the acceptance criterion.
    assert_eq!(
        field(&report, "allocated_bytes"),
        allocated_oracle(&root),
        "allocated bytes vs the per-file handle oracle"
    );
}

/// 100 directories × 1000 small files, a multi-cluster file, an empty file, a
/// sparse file, and three names for one file.
fn generate_tree(root: &Path) -> Result<(), Box<dyn Error>> {
    for dir in 0..100 {
        let dir = root.join(format!("dir{dir:03}"));
        fs::create_dir(&dir)?;
        for file in 0..1000 {
            fs::write(dir.join(format!("file{file:04}.dat")), b"x")?;
        }
    }

    fs::write(root.join("big.bin"), vec![b'y'; 50_000])?;
    fs::write(root.join("empty.bin"), b"")?;

    // Ten megabytes apparent, nothing allocated. `Get-ChildItem` sums `Length`,
    // so the logical total includes it in full.
    let sparse = root.join("sparse.bin");
    File::create(&sparse)?;
    let status = Command::new("fsutil")
        .args(["sparse", "setflag"])
        .arg(&sparse)
        .stdout(Stdio::null())
        .status()?;
    if !status.success() {
        return Err("fsutil sparse setflag failed".into());
    }
    fs::OpenOptions::new()
        .write(true)
        .open(&sparse)?
        .set_len(10 * 1024 * 1024)?;

    let owner = root.join("hard-a.txt");
    fs::write(&owner, vec![b'z'; HARD_LINK_BYTES as usize])?;
    fs::hard_link(&owner, root.join("hard-b.txt"))?;
    fs::hard_link(&owner, root.join("hard-c.txt"))?;
    Ok(())
}

#[test]
fn a_generated_100k_file_tree_is_within_two_percent_of_get_child_item() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = long_form(temp.path());
    generate_tree(&root).expect("generate the real tree");

    let report = scan_report(&root);
    assert_eq!(
        report["backend"], "platform",
        "the gate must exercise WinScanner"
    );

    let scanned = field(&report, "logical_bytes");
    let (gci_files, gci_bytes) = get_child_item(&root);

    assert!(
        drift(scanned, gci_bytes).abs() <= i128::from(tolerance(gci_bytes)),
        "the real tree drifted more than {TOLERANCE_PERCENT}% from Get-ChildItem:\n{}",
        describe_drift(&root, scanned, gci_bytes)
    );

    // Exact, once Get-ChildItem's per-name counting of the hard link is undone.
    let extra_names = HARD_LINK_NAMES - 1;
    assert_eq!(
        scanned,
        gci_bytes - extra_names * HARD_LINK_BYTES,
        "logical bytes, hard link counted once"
    );
    assert_eq!(
        field(&report, "files"),
        gci_files - extra_names,
        "files, hard link counted once"
    );
    assert_eq!(field(&report, "files"), 100_004);
    assert_eq!(field(&report, "dirs"), 100);

    // A subtree cannot occupy more than its whole volume. Coarse by design: a
    // sanity bound, not an accuracy check.
    let allocated = field(&report, "allocated_bytes");
    let used = volume_used(&root);
    assert!(
        allocated <= used,
        "the scan reports {allocated} bytes on disk, more than the volume's {used}"
    );
    // The sparse file must not be counted at its apparent size.
    assert!(
        allocated < scanned,
        "allocated {allocated} must be below logical {scanned}: the sparse file \
         allocates almost nothing and tiny files live inside the MFT"
    );
}

#[test]
fn the_reference_walker_agrees_with_the_platform_scanner() {
    // No hard links or sparse files: the reference walker can neither dedupe nor
    // see allocation on Windows, so the tree sticks to what both can know.
    let temp = tempfile::tempdir().expect("tempdir");
    let root = long_form(temp.path());
    for dir in 0..8 {
        let dir = root.join(format!("dir{dir}"));
        fs::create_dir(&dir).expect("dir");
        for file in 0..64 {
            fs::write(dir.join(format!("file{file}.dat")), vec![b'x'; file + 1]).expect("file");
        }
    }

    let platform = scan_report(&root);
    let reference = scan_report_with(&["--reference"], &root);
    assert_eq!(platform["backend"], "platform");
    assert_eq!(reference["backend"], "reference");

    let fields: BTreeMap<&str, (u64, u64)> = ["files", "dirs", "links", "logical_bytes"]
        .into_iter()
        .map(|name| (name, (field(&platform, name), field(&reference, name))))
        .collect();
    for (name, (platform_value, reference_value)) in &fields {
        assert_eq!(
            platform_value, reference_value,
            "platform and reference disagree on {name}"
        );
    }
}
