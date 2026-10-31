//! End-to-end tests for the Windows walker (#12, #13).
//!
//! The fixture tree from [`storagelens_core::fixtures`] is the input. Independent
//! oracles check the result:
//!
//! - `Get-ChildItem -Recurse -Force` (PowerShell 7) for **which paths exist**.
//!   PowerShell 7 does not follow junctions or symlinks during `-Recurse`, which
//!   is the walker's rule too.
//! - A `std::fs` walk for **what each entry is**: kind and logical size.
//! - A per-file handle query — `GetFileInformationByHandle` for the file index
//!   and link count, `GetFileInformationByHandleEx(FileStandardInfo)` for the
//!   allocation — for **the numbers the bulk parser decoded**. AGENTS.md §7: if a
//!   record offset is wrong, sizes are silently wrong, and this is the check that
//!   catches it.
//!
//! Sets are compared, not sequences: sibling subtrees interleave in a parallel
//! scan. What the sink does check on every accept is the arena contract — root
//! first, and `parent < index`.

#![cfg(windows)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use storagelens_core::fixtures::FixtureTree;
use storagelens_core::{
    CancelToken, Entry, EntryKind, EntrySink, FileStamp, Progress, Result, ScanError, ScanOptions,
    ScanResult, ScanStatus, SkipReason, SkippedDir, StorageScanner,
};
use storagelens_scan_windows::WinScanner;

/// Records every entry and enforces the arena contract as it goes.
struct CollectSink {
    root: PathBuf,
    entries: Vec<Entry>,
    paths: Vec<PathBuf>,
    flush_count: usize,
    cancel: Option<(CancelToken, usize)>,
}

impl CollectSink {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            entries: Vec::new(),
            paths: Vec::new(),
            flush_count: 0,
            cancel: None,
        }
    }

    fn cancelling_at(root: &Path, cancel: CancelToken, after: usize) -> Self {
        Self {
            cancel: Some((cancel, after)),
            ..Self::new(root)
        }
    }

    /// Every entry below the root, keyed by root-relative path.
    fn relative(&self) -> BTreeMap<PathBuf, &Entry> {
        self.paths
            .iter()
            .zip(&self.entries)
            .skip(1)
            .map(|(path, entry)| {
                let relative = path
                    .strip_prefix(&self.root)
                    .expect("every accepted path is below the root");
                (relative.to_path_buf(), entry)
            })
            .collect()
    }
}

impl EntrySink for CollectSink {
    fn accept(&mut self, entry: Entry) -> Result<()> {
        let index = self.entries.len();
        if index == 0 {
            assert_eq!(entry.parent, 0, "the root is its own parent");
            self.paths.push(self.root.clone());
        } else {
            let parent = entry.parent as usize;
            assert!(
                parent < index,
                "entry {index} ({:?}) arrived before its parent {parent}",
                entry.name
            );
            let parent_path = self.paths[parent].clone();
            self.paths.push(parent_path.join(&entry.name));
        }
        self.entries.push(entry);

        if let Some((cancel, after)) = &self.cancel {
            if self.entries.len() >= *after {
                cancel.cancel();
            }
        }
        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.flush_count += 1;
        Ok(())
    }
}

/// The long-name form of a temp path.
///
/// `%TEMP%` on CI runners contains an 8.3 short name (`RUNNER~1`). The walker
/// keeps whatever form it is given, but `Get-ChildItem` reports long names, so
/// both are handed the long form.
fn long_form(path: &Path) -> PathBuf {
    let canonical = fs::canonicalize(path).expect("canonicalize the tree root");
    let text = canonical.to_string_lossy();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
}

/// Root-relative paths under `root`, per `Get-ChildItem -Recurse -Force`.
fn get_child_item(root: &Path) -> BTreeSet<PathBuf> {
    let output = Command::new("pwsh")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false); \
             Get-ChildItem -LiteralPath $env:SL_ROOT -Recurse -Force | \
             ForEach-Object { $_.FullName }",
        ])
        .env("SL_ROOT", root)
        .stdin(Stdio::null())
        .output()
        .expect("run pwsh (PowerShell 7 is preinstalled on GitHub runners)");
    assert!(
        output.status.success(),
        "Get-ChildItem failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8(output.stdout)
        .expect("utf-8 Get-ChildItem output")
        .lines()
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| !line.is_empty())
        .map(|line| {
            Path::new(line)
                .strip_prefix(root)
                .unwrap_or_else(|_| panic!("{line} is not below {}", root.display()))
                .to_path_buf()
        })
        .collect()
}

#[derive(Debug)]
struct OracleEntry {
    kind: EntryKind,
    size_logical: u64,
    /// Files only: what a per-file handle query reports.
    handle: Option<HandleInfo>,
}

#[derive(Debug, Clone, Copy)]
struct HandleInfo {
    alloc: u64,
    file_index: u64,
    links: u32,
}

/// Ask one file's handle for its allocation, index, and link count — the slow
/// per-file path the scanner exists to avoid, used here as the oracle.
fn handle_info(path: &Path) -> HandleInfo {
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;

    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FileStandardInfo, GetFileInformationByHandle, GetFileInformationByHandleEx,
        BY_HANDLE_FILE_INFORMATION, FILE_STANDARD_INFO,
    };

    // Access mode 0 asks for attributes only, so the query works on any file the
    // directory listing could see.
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

    HandleInfo {
        alloc: standard.AllocationSize as u64,
        file_index: (u64::from(by_handle.nFileIndexHigh) << 32)
            | u64::from(by_handle.nFileIndexLow),
        links: by_handle.nNumberOfLinks,
    }
}

/// A `std::fs` walk that never follows links. Allowed here: this is a test, and
/// the fixture lives in a temp dir (AGENTS.md §5.4 applies to application code).
fn std_oracle(root: &Path) -> BTreeMap<PathBuf, OracleEntry> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read_dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).expect("symlink_metadata");
            // std treats junctions and symlinks alike: both are name surrogates.
            let kind = if metadata.file_type().is_symlink() {
                EntryKind::Symlink
            } else if metadata.is_dir() {
                stack.push(path.clone());
                EntryKind::Dir
            } else {
                EntryKind::File
            };
            let size_logical = if kind == EntryKind::File {
                metadata.len()
            } else {
                0
            };
            let handle = (kind == EntryKind::File).then(|| handle_info(&path));
            out.insert(
                path.strip_prefix(root).expect("below root").to_path_buf(),
                OracleEntry {
                    kind,
                    size_logical,
                    handle,
                },
            );
        }
    }
    out
}

fn scan(
    options: ScanOptions,
    root: &Path,
    sink: &mut CollectSink,
    cancel: CancelToken,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<ScanResult> {
    WinScanner::new(options).scan(&[root.to_path_buf()], sink, cancel, progress)
}

#[test]
fn walker_matches_get_child_item_on_the_fixture_tree() {
    let tree = FixtureTree::generate().expect("generate fixture tree");
    let manifest = tree.manifest();
    let root = long_form(tree.root());
    let relative_to_tree = |path: &Path| {
        path.strip_prefix(tree.root())
            .expect("fixture path is below the tree root")
            .to_path_buf()
    };

    let progress_log = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&progress_log);
    let progress = move |event: Progress| log.lock().expect("progress log").push(event);

    let mut sink = CollectSink::new(&root);
    let result = scan(
        ScanOptions::default(),
        &root,
        &mut sink,
        CancelToken::new(),
        &progress,
    )
    .expect("scan the fixture tree");
    let scanned = sink.relative();
    let scanned_paths: BTreeSet<PathBuf> = scanned.keys().cloned().collect();

    // --- Paths: Get-ChildItem is the acceptance oracle -----------------------
    assert_eq!(
        scanned_paths,
        get_child_item(&root),
        "walker and Get-ChildItem disagree on which paths exist"
    );

    // --- Kinds and sizes: the std walk ----------------------------------------
    let oracle = std_oracle(&root);
    assert_eq!(
        scanned_paths,
        oracle.keys().cloned().collect::<BTreeSet<_>>(),
        "walker and read_dir disagree on which paths exist"
    );
    for (path, want) in &oracle {
        let entry = scanned[path];
        assert_eq!(entry.kind, want.kind, "kind of {}", path.display());
        assert_eq!(
            entry.size_logical,
            want.size_logical,
            "logical size of {}",
            path.display()
        );
        match want.handle {
            Some(handle) => {
                assert_eq!(
                    entry.size_alloc,
                    handle.alloc,
                    "allocated size of {}",
                    path.display()
                );
                assert_eq!(
                    entry.file_id,
                    handle.file_index,
                    "file id of {}",
                    path.display()
                );
                if handle.links <= 1 {
                    assert!(!entry.hardlink_dup, "{} is not a duplicate", path.display());
                }
            }
            None => {
                assert_eq!(entry.size_alloc, 0, "only files have bytes");
                assert!(!entry.hardlink_dup);
            }
        }
    }

    // --- Hard links: one owner per file index ---------------------------------
    let mut by_index: BTreeMap<u64, Vec<&Entry>> = BTreeMap::new();
    for (path, want) in &oracle {
        if let Some(handle) = want.handle.filter(|handle| handle.links > 1) {
            by_index
                .entry(handle.file_index)
                .or_default()
                .push(scanned[path]);
        }
    }
    assert!(!by_index.is_empty(), "the fixture has a hard-linked file");
    for (index, names) in &by_index {
        let owners = names.iter().filter(|entry| !entry.hardlink_dup).count();
        assert_eq!(
            owners, 1,
            "file {index} must be counted at exactly one name"
        );
        for duplicate in names.iter().filter(|entry| entry.hardlink_dup) {
            assert!(duplicate.nlink >= 2, "a duplicate has at least two links");
        }
    }

    // --- The junction loop terminates immediately ------------------------------
    let junction = manifest
        .paths
        .junction
        .as_deref()
        .map(relative_to_tree)
        .expect("mklink /J needs no privilege, so the fixture has a junction");
    assert_eq!(scanned[&junction].kind, EntryKind::Symlink);
    assert!(
        !scanned_paths
            .iter()
            .any(|path| path != &junction && path.starts_with(&junction)),
        "nothing below the junction may be scanned"
    );
    if let Some(loop_links) = &manifest.paths.symlink_loop {
        for link in loop_links {
            assert_eq!(scanned[&relative_to_tree(link)].kind, EntryKind::Symlink);
        }
    }

    // --- Paths over MAX_PATH --------------------------------------------------
    let deep = relative_to_tree(&manifest.paths.deep_path);
    assert!(
        root.join(&deep).as_os_str().len() > 1_024,
        "the fixture's deep path exceeds 1,024 characters on Windows"
    );
    assert_eq!(scanned[&deep].kind, EntryKind::File);
    assert!(scanned.contains_key(&relative_to_tree(&manifest.paths.long_name)));
    for name in [
        &manifest.paths.unicode,
        &manifest.paths.emoji,
        &manifest.paths.rtl,
    ] {
        assert!(
            scanned.contains_key(&relative_to_tree(name)),
            "{}",
            name.display()
        );
    }

    // --- Totals ----------------------------------------------------------------
    // The manifest folds hard links into one file, and so must the scan.
    assert_eq!(result.files, manifest.files, "file count");
    assert_eq!(result.dirs, manifest.dirs, "directory count");
    assert_eq!(
        result.bytes_logical.get(),
        manifest.bytes_logical,
        "logical bytes"
    );
    // The manifest's Windows allocation is the logical size (std has no
    // allocation query), so the handle oracle supplies the expected total, each
    // file index counted once.
    let mut counted = BTreeSet::new();
    let expected_alloc: u64 = oracle
        .values()
        .filter_map(|entry| entry.handle)
        .filter(|handle| counted.insert(handle.file_index))
        .map(|handle| handle.alloc)
        .sum();
    assert_eq!(result.bytes_alloc.get(), expected_alloc, "allocated bytes");
    let links = scanned
        .values()
        .filter(|entry| entry.kind == EntryKind::Symlink)
        .count() as u64;
    assert_eq!(links, manifest.symlinks + manifest.junctions, "links");

    // --- Status, flush, progress ----------------------------------------------
    assert_eq!(result.status, ScanStatus::Complete, "{:?}", result.skipped);
    assert!(result.skipped.is_empty());
    assert_eq!(sink.flush_count, 1, "flush runs exactly once");

    let events = progress_log.lock().expect("progress log");
    let last = events
        .last()
        .expect("a final progress event is always sent");
    assert_eq!(last.files, result.files);
    assert_eq!(last.dirs, result.dirs);
    assert_eq!(last.bytes_alloc, result.bytes_alloc);
}

/// Removes the deny ACE before the temp dir is deleted, even if the test fails.
struct DenyGuard(PathBuf);

impl DenyGuard {
    /// Deny Everyone (`S-1-1-0`) the right to list `dir`. Administrators are not
    /// exempt from a deny ACE, so this holds on an elevated CI runner too.
    fn deny_listing(dir: &Path) -> Self {
        let status = Command::new("icacls")
            .arg(dir)
            .args(["/deny", "*S-1-1-0:(RD)"])
            .stdout(Stdio::null())
            .status()
            .expect("run icacls");
        assert!(status.success(), "icacls /deny failed");
        Self(dir.to_path_buf())
    }
}

impl Drop for DenyGuard {
    fn drop(&mut self) {
        let _ = Command::new("icacls")
            .arg(&self.0)
            .args(["/remove:d", "*S-1-1-0"])
            .stdout(Stdio::null())
            .status();
    }
}

#[test]
fn an_unreadable_directory_is_reported_not_fatal() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = long_form(temp.path());
    let denied = root.join("denied");
    fs::create_dir(&denied).expect("denied dir");
    fs::write(denied.join("secret.txt"), b"secret\n").expect("secret");
    fs::write(root.join("visible.txt"), b"visible\n").expect("visible");
    let _guard = DenyGuard::deny_listing(&denied);

    let mut sink = CollectSink::new(&root);
    let result = scan(
        ScanOptions::default(),
        &root,
        &mut sink,
        CancelToken::new(),
        &|_| {},
    )
    .expect("an access-denied directory is a skip, not an error");

    assert_eq!(result.status, ScanStatus::Partial);
    // The gap is labelled with the scan's elevation, so the UI can offer an
    // elevated relaunch only when it might help.
    assert_eq!(
        result.elevated,
        Some(storagelens_scan_windows::elevation::is_elevated())
    );
    assert_eq!(
        result.skipped,
        vec![SkippedDir {
            path: denied,
            reason: SkipReason::PermissionDenied,
        }]
    );
    let scanned = sink.relative();
    assert!(scanned.contains_key(Path::new("visible.txt")));
    assert!(
        scanned.contains_key(Path::new("denied")),
        "the denied directory keeps its own row"
    );
    assert!(!scanned.contains_key(Path::new(r"denied\secret.txt")));
    assert_eq!(result.files, 1);
}

#[test]
fn excluded_and_too_deep_directories_are_reported() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = long_form(temp.path());
    fs::create_dir_all(root.join("skip")).expect("skip dir");
    fs::create_dir_all(root.join(r"level1\level2")).expect("deep dirs");
    fs::write(root.join("keep.txt"), b"keep\n").expect("keep");
    fs::write(root.join(r"skip\secret.txt"), b"secret\n").expect("secret");
    fs::write(root.join(r"level1\level2\deep.txt"), b"deep\n").expect("deep");

    let options = ScanOptions {
        // Different case on purpose: NTFS matching is case-insensitive.
        exclude: vec![root.join("SKIP")],
        max_depth: Some(1),
        ..ScanOptions::default()
    };

    let mut sink = CollectSink::new(&root);
    let result = scan(options, &root, &mut sink, CancelToken::new(), &|_| {}).expect("scan");

    let scanned = sink.relative();
    for present in ["keep.txt", "skip", "level1", r"level1\level2"] {
        assert!(
            scanned.contains_key(Path::new(present)),
            "{present} is a row"
        );
    }
    for absent in [r"skip\secret.txt", r"level1\level2\deep.txt"] {
        assert!(
            !scanned.contains_key(Path::new(absent)),
            "{absent} is not scanned"
        );
    }

    assert_eq!(result.status, ScanStatus::Partial);
    assert_eq!(result.skipped_by(SkipReason::Excluded), 1);
    assert_eq!(result.skipped_by(SkipReason::Other), 1);
    assert_eq!(result.files, 1);
    assert_eq!(result.dirs, 3);
}

#[test]
fn a_verbatim_root_is_reported_in_its_plain_form() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = long_form(temp.path());
    fs::write(root.join("a.txt"), b"a\n").expect("file");

    let verbatim = PathBuf::from(format!(r"\\?\{}", root.display()));
    let progress_log = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&progress_log);
    let progress = move |event: Progress| log.lock().expect("progress log").push(event);

    let mut sink = CollectSink::new(&root);
    let result = scan(
        ScanOptions::default(),
        &verbatim,
        &mut sink,
        CancelToken::new(),
        &progress,
    )
    .expect("scan");

    assert_eq!(result.files, 1);
    let last = progress_log
        .lock()
        .expect("progress log")
        .last()
        .cloned()
        .expect("final progress");
    assert_eq!(last.current_path.as_deref(), Some(root.as_path()));
}

#[test]
fn cancellation_returns_a_consistent_partial_tree() {
    let tree = FixtureTree::generate().expect("generate fixture tree");
    let root = long_form(tree.root());

    let cancel = CancelToken::new();
    let mut sink = CollectSink::cancelling_at(&root, cancel.clone(), 5);
    let result = scan(ScanOptions::default(), &root, &mut sink, cancel, &|_| {})
        .expect("cancellation is a status, not an error");

    assert_eq!(result.status, ScanStatus::Cancelled);
    assert_eq!(sink.flush_count, 1, "a cancelled scan still flushes");
    assert!(sink.entries.len() >= 5);

    let files = sink
        .entries
        .iter()
        .skip(1)
        .filter(|e| e.is_file() && !e.hardlink_dup)
        .count() as u64;
    let dirs = sink.entries.iter().skip(1).filter(|e| e.is_dir()).count() as u64;
    let bytes: u64 = sink
        .entries
        .iter()
        .filter(|e| e.is_file() && !e.hardlink_dup)
        .map(|e| e.size_alloc)
        .sum();
    assert_eq!(result.files, files);
    assert_eq!(result.dirs, dirs);
    assert_eq!(result.bytes_alloc.get(), bytes);
}

#[test]
fn invalid_roots_are_rejected_before_the_sink_is_touched() {
    let scanner = WinScanner::new(ScanOptions::default());
    let mut sink = CollectSink::new(Path::new(r"C:\"));

    let error = scanner
        .scan(&[PathBuf::new()], &mut sink, CancelToken::new(), &|_| {})
        .expect_err("empty root");
    assert!(matches!(&error, ScanError::InvalidPath(path) if path.as_os_str().is_empty()));

    let relative = PathBuf::from(r"relative\dir");
    let error = scanner
        .scan(
            std::slice::from_ref(&relative),
            &mut sink,
            CancelToken::new(),
            &|_| {},
        )
        .expect_err("relative root");
    assert!(matches!(&error, ScanError::InvalidPath(path) if path == &relative));

    let roots = [PathBuf::from(r"C:\a"), PathBuf::from(r"C:\b")];
    let error = scanner
        .scan(&roots, &mut sink, CancelToken::new(), &|_| {})
        .expect_err("two roots");
    assert!(matches!(&error, ScanError::Unsupported(_)));

    let error = scanner
        .scan(&[], &mut sink, CancelToken::new(), &|_| {})
        .expect_err("no roots");
    assert!(matches!(&error, ScanError::InvalidPath(_)));

    assert!(sink.entries.is_empty(), "no entry before the root is valid");
    assert_eq!(sink.flush_count, 0, "an early error never flushes");
}

#[test]
fn sparse_and_compressed_files_report_bytes_on_disk() {
    use std::io::Write;
    use std::os::windows::fs::MetadataExt;

    const COMPRESSED: u32 = 0x800;
    const SPARSE: u32 = 0x200;

    let temp = tempfile::tempdir().expect("tempdir");
    let root = long_form(temp.path());

    // 64 MiB apparent, 4 KiB written. `fsutil sparse setflag` marks it sparse;
    // extending it afterwards allocates nothing.
    let sparse = root.join("sparse.img");
    fs::write(&sparse, [7u8; 4096]).expect("sparse seed");
    let status = Command::new("fsutil")
        .args(["sparse", "setflag"])
        .arg(&sparse)
        .stdout(Stdio::null())
        .status()
        .expect("run fsutil");
    assert!(status.success(), "fsutil sparse setflag failed");
    fs::OpenOptions::new()
        .write(true)
        .open(&sparse)
        .and_then(|file| file.set_len(64 * 1024 * 1024))
        .expect("extend sparse file");

    // 4 MiB of a repeating pattern compresses to a small fraction.
    let compressed = root.join("compressed.txt");
    let mut file = fs::File::create(&compressed).expect("compressed file");
    for _ in 0..(4 * 1024 * 1024 / 16) {
        file.write_all(b"storagelens-0123").expect("write");
    }
    drop(file);
    let status = Command::new("compact")
        .args(["/c", "/q"])
        .arg(&compressed)
        .stdout(Stdio::null())
        .status()
        .expect("run compact");
    assert!(status.success(), "compact /c failed");

    for (path, flag) in [(&sparse, SPARSE), (&compressed, COMPRESSED)] {
        let attributes = fs::metadata(path).expect("metadata").file_attributes();
        assert_ne!(
            attributes & flag,
            0,
            "{} lacks its attribute",
            path.display()
        );
    }

    let mut sink = CollectSink::new(&root);
    let result = scan(
        ScanOptions::default(),
        &root,
        &mut sink,
        CancelToken::new(),
        &|_| {},
    )
    .expect("scan");
    let scanned = sink.relative();

    let sparse_entry = scanned[Path::new("sparse.img")];
    assert_eq!(sparse_entry.size_logical, 64 * 1024 * 1024);
    assert!(
        sparse_entry.size_alloc <= 1024 * 1024,
        "sparse file allocates {} bytes, expected far less than its 64 MiB",
        sparse_entry.size_alloc
    );

    let compressed_entry = scanned[Path::new("compressed.txt")];
    assert_eq!(compressed_entry.size_logical, 4 * 1024 * 1024);
    assert!(
        compressed_entry.size_alloc < compressed_entry.size_logical / 4,
        "compressed file reports {} bytes on disk for {} logical",
        compressed_entry.size_alloc,
        compressed_entry.size_logical
    );

    assert_eq!(
        result.bytes_alloc.get(),
        sparse_entry.size_alloc + compressed_entry.size_alloc
    );
}

/// The safety preflight (#29) re-stats a path right before deleting it and
/// skips it unless the stamp still matches the scan, so `stamp` must read what
/// the walker recorded. Directory times are not compared: NTFS updates a
/// directory's entry in its parent's index lazily, so a listing can lag the
/// directory's own record (the preflight then skips it, which is safe).
#[test]
fn stamp_matches_what_the_walker_recorded() {
    let tree = FixtureTree::generate().expect("generate fixture tree");
    let root = tree.root();
    let mut sink = CollectSink::new(root);
    scan(
        ScanOptions::default(),
        root,
        &mut sink,
        CancelToken::new(),
        &|_| {},
    )
    .expect("scan");

    let mut checked = 0;
    for (path, entry) in sink.paths.iter().zip(&sink.entries).skip(1) {
        let now = storagelens_scan_windows::stamp(path).expect("stamp");
        let recorded = FileStamp::of(entry);
        assert_eq!(now.kind, recorded.kind, "{path:?}");
        assert_eq!(now.file_id, recorded.file_id, "{path:?}");
        assert_eq!(now.dev, recorded.dev, "{path:?}");
        if entry.kind == EntryKind::File {
            assert_eq!(now.size_logical, recorded.size_logical, "{path:?}");
            assert_eq!(now.modified, recorded.modified, "{path:?}");
        }
        checked += 1;
    }
    assert!(checked > 100, "only {checked} entries checked");
}
