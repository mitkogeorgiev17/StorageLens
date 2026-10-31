//! End-to-end tests for the parallel walker.
//!
//! The fixture tree from [`storagelens_core::fixtures`] is the input; a plain
//! `read_dir` walk is the oracle. The two are compared as sets, because sibling
//! subtrees interleave in a parallel scan and arrival order is deliberately not
//! part of the contract.
//!
//! What *is* part of the contract, and is asserted on every accept:
//!
//! - the root arrives first, with `parent == 0`;
//! - a child never arrives before its parent, so `parent < index`;
//! - the arena the sink builds is therefore valid.
//!
//! Totals are checked against the oracle (hard links are counted per name until
//! #9 adds dedupe), and the final progress snapshot must agree with the result.

#![cfg(target_os = "macos")]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use storagelens_core::fixtures::FixtureTree;
use storagelens_core::{
    CancelToken, Entry, EntryKind, EntrySink, Progress, Result, ScanError, ScanOptions, ScanStatus,
    SkipReason, SkippedDir, StorageScanner,
};
use storagelens_scan_macos::MacScanner;

/// Records every entry and enforces the arena contract as it goes.
struct CollectSink {
    root: PathBuf,
    /// Entries in acceptance order. Index in this vector is the arena index.
    entries: Vec<Entry>,
    /// Absolute path of each accepted entry, reconstructed from `parent`.
    paths: Vec<PathBuf>,
    flush_count: usize,
    /// Cancel the scan once this many entries have been accepted.
    cancel: Option<CancelToken>,
    cancel_after: Option<usize>,
}

impl CollectSink {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            entries: Vec::new(),
            paths: Vec::new(),
            flush_count: 0,
            cancel: None,
            cancel_after: None,
        }
    }

    fn cancelling_at(root: &Path, cancel: CancelToken, after: usize) -> Self {
        Self {
            cancel: Some(cancel),
            cancel_after: Some(after),
            ..Self::new(root)
        }
    }

    /// Relative path of every entry below the root, keyed the same way as the
    /// oracle. The root itself is dropped: it is the container, not a child.
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
            assert_eq!(entry.kind, EntryKind::Dir, "fixture root is a directory");
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

        if let (Some(cancel), Some(after)) = (&self.cancel, self.cancel_after) {
            if self.entries.len() >= after {
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct OracleEntry {
    kind: EntryKind,
    size_alloc: u64,
    size_logical: u64,
    dev: u32,
    file_id: u64,
    nlink: u32,
}

/// Walk the tree with `std::fs`, the way the reference walker does.
///
/// Returns every entry below `root` and the directories that could not be opened.
/// Iterative: the fixture nests 200 levels deep, which is far past a safe recursion
/// depth on a test thread's default stack.
fn oracle(root: &Path) -> (BTreeMap<PathBuf, OracleEntry>, Vec<PathBuf>) {
    let mut entries = BTreeMap::new();
    let mut skipped = Vec::new();
    let mut pending = vec![(root.to_path_buf(), PathBuf::new())];

    while let Some((dir, relative)) = pending.pop() {
        let reader = match fs::read_dir(&dir) {
            Ok(reader) => reader,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                skipped.push(dir);
                continue;
            }
            Err(error) => panic!("read_dir {}: {error}", dir.display()),
        };

        for item in reader {
            let item = item.unwrap_or_else(|error| panic!("entry of {}: {error}", dir.display()));
            let path = item.path();
            let name = item.file_name().to_string_lossy().into_owned();
            let metadata = fs::symlink_metadata(&path)
                .unwrap_or_else(|error| panic!("lstat {}: {error}", path.display()));

            let kind = kind_of(&metadata);
            let (size_alloc, size_logical) = match kind {
                // Directories report no allocation on APFS and the scanner has no
                // directory-size attribute, so both sides agree on zero.
                EntryKind::Dir => (0, 0),
                EntryKind::File | EntryKind::Symlink | EntryKind::Other => {
                    (metadata.blocks().saturating_mul(512), metadata.len())
                }
            };

            let child = relative.join(&name);
            entries.insert(
                child.clone(),
                OracleEntry {
                    kind,
                    size_alloc,
                    size_logical,
                    dev: metadata.dev() as u32,
                    file_id: metadata.ino(),
                    nlink: metadata.nlink() as u32,
                },
            );

            if kind == EntryKind::Dir {
                pending.push((path, child));
            }
        }
    }

    (entries, skipped)
}

fn kind_of(metadata: &fs::Metadata) -> EntryKind {
    let file_type = metadata.file_type();
    if file_type.is_dir() {
        EntryKind::Dir
    } else if file_type.is_symlink() {
        EntryKind::Symlink
    } else if file_type.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

fn scan(
    scanner: &MacScanner,
    root: &Path,
    sink: &mut CollectSink,
    cancel: CancelToken,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> Result<storagelens_core::ScanResult> {
    scanner.scan(&[root.to_path_buf()], sink, cancel, progress)
}

#[test]
fn walker_matches_the_read_dir_oracle() {
    let tree = FixtureTree::generate().expect("generate fixture tree");
    let root = tree.root();
    let (expected, expected_skipped) = oracle(root);

    let progress_log = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&progress_log);
    // The callback runs on the scanning thread; a mutex keeps the log safe without
    // requiring the sink (which is `!Send`) to leave that thread.
    let progress = move |event: Progress| log.lock().expect("progress log").push(event);

    let mut sink = CollectSink::new(root);
    let result = scan(
        &MacScanner::new(ScanOptions::default()),
        root,
        &mut sink,
        CancelToken::new(),
        &progress,
    )
    .expect("scan the fixture tree");

    // --- The arena ---------------------------------------------------------
    // `CollectSink::accept` has already checked root-first and parent < index.
    assert_eq!(
        sink.entries[0].name,
        root.file_name()
            .expect("tempdir has a name")
            .to_string_lossy(),
        "the root is recorded under its real name"
    );
    let scanned = sink.relative();
    let scanned_paths: BTreeSet<_> = scanned.keys().cloned().collect();
    let expected_paths: BTreeSet<_> = expected.keys().cloned().collect();
    assert_eq!(
        scanned_paths, expected_paths,
        "walker and read_dir disagree on which paths exist"
    );

    for (path, want) in &expected {
        let entry = scanned
            .get(path)
            .unwrap_or_else(|| panic!("{} missing from the scan", path.display()));
        assert_eq!(entry.kind, want.kind, "kind of {}", path.display());
        assert_eq!(
            entry.size_alloc,
            want.size_alloc,
            "allocated size of {}",
            path.display()
        );
        assert_eq!(
            entry.size_logical,
            want.size_logical,
            "logical size of {}",
            path.display()
        );
        assert_eq!(entry.dev, want.dev, "device of {}", path.display());
        assert_eq!(entry.file_id, want.file_id, "inode of {}", path.display());

        if want.kind != EntryKind::File || want.nlink <= 1 {
            assert!(
                !entry.hardlink_dup,
                "{} is not a hard-link duplicate but was flagged",
                path.display()
            );
        }
    }

    // --- Hard links --------------------------------------------------------
    // Every name keeps a row with its true size; exactly one name per inode owns
    // the bytes and the rest set `hardlink_dup`.
    let mut by_inode: BTreeMap<(u32, u64), Vec<(&PathBuf, bool)>> = BTreeMap::new();
    for (path, want) in &expected {
        if want.kind == EntryKind::File && want.nlink > 1 {
            let entry = scanned.get(path).expect("checked above");
            by_inode
                .entry((want.dev, want.file_id))
                .or_default()
                .push((path, entry.hardlink_dup));
        }
    }
    assert!(
        !by_inode.is_empty(),
        "the fixture must contain at least one hard-linked inode"
    );
    for ((dev, inode), names) in &by_inode {
        assert!(
            names.len() > 1,
            "inode {inode} on device {dev} has a link count but one name"
        );
        let owners = names.iter().filter(|(_, duplicate)| !duplicate).count();
        assert_eq!(
            owners, 1,
            "inode {inode} on device {dev} must be counted at exactly one path, got {names:?}"
        );
    }

    // The fixture's three names for one inode are the acceptance criterion: they
    // stay visible, but contribute one file and one allocation.
    let hardlinks = &tree.manifest().paths.hardlinks;
    let mut owners = 0;
    for path in hardlinks {
        let relative = path
            .strip_prefix(root)
            .expect("hard link is below the root");
        let entry = scanned
            .get(relative)
            .unwrap_or_else(|| panic!("{} missing from the scan", path.display()));
        assert_eq!(entry.kind, EntryKind::File);
        assert_eq!(entry.size_alloc, expected[relative].size_alloc);
        if !entry.hardlink_dup {
            owners += 1;
        }
    }
    assert_eq!(owners, 1, "exactly one hard-link name owns the inode");

    // --- Symlinks ----------------------------------------------------------
    // `FSOPT_NOFOLLOW` means the loop is reported as two links and never entered,
    // and the link pointing outside the tree is not followed either.
    if let Some(loop_links) = &tree.manifest().paths.symlink_loop {
        for link in loop_links {
            let relative = link.strip_prefix(root).expect("link is below the root");
            assert_eq!(
                scanned
                    .get(relative)
                    .unwrap_or_else(|| panic!("{} missing", link.display()))
                    .kind,
                EntryKind::Symlink,
                "{} must stay a symlink",
                link.display()
            );
        }
    }
    if let Some(outside) = &tree.manifest().paths.symlink_outside {
        let relative = outside.strip_prefix(root).expect("link is below the root");
        assert_eq!(
            scanned
                .get(relative)
                .unwrap_or_else(|| panic!("{} missing", outside.display()))
                .kind,
            EntryKind::Symlink
        );
    }

    // --- Totals ------------------------------------------------------------
    // Count each inode once, at its first path, matching #9's rule.
    let mut counted = BTreeSet::new();
    let mut expected_files = 0u64;
    let mut expected_alloc = 0u64;
    let mut expected_logical = 0u64;
    for entry in expected.values().filter(|e| e.kind == EntryKind::File) {
        if counted.insert((entry.dev, entry.file_id)) {
            expected_files += 1;
            expected_alloc = expected_alloc.saturating_add(entry.size_alloc);
            expected_logical = expected_logical.saturating_add(entry.size_logical);
        }
    }
    let expected_dirs = expected
        .values()
        .filter(|entry| entry.kind == EntryKind::Dir)
        .count() as u64;

    assert_eq!(result.files, expected_files, "file count");
    assert_eq!(result.dirs, expected_dirs, "directory count");
    assert_eq!(result.bytes_alloc.get(), expected_alloc, "allocated bytes");
    assert_eq!(
        result.bytes_logical.get(),
        expected_logical,
        "logical bytes"
    );
    // Every name is still a row.
    assert_eq!(sink.entries.len() as u64, expected_paths.len() as u64 + 1);

    // The manifest folds hard links into one file; the scan must now agree with it
    // exactly, not merely with the oracle.
    let manifest = tree.manifest();
    assert_eq!(result.files, manifest.files, "manifest file count");
    assert_eq!(result.dirs, manifest.dirs, "manifest directory count");
    assert_eq!(
        result.bytes_alloc.get(),
        manifest.bytes_alloc,
        "manifest allocated bytes"
    );
    assert_eq!(
        result.bytes_logical.get(),
        manifest.bytes_logical,
        "manifest logical bytes"
    );

    // --- Skips and status --------------------------------------------------
    assert_eq!(result.status, ScanStatus::Partial);
    let manifest_denied = tree
        .manifest()
        .paths
        .denied_dir
        .clone()
        .expect("macOS fixture has an unreadable directory");
    assert_eq!(expected_skipped, vec![manifest_denied.clone()]);
    assert_eq!(
        result.skipped,
        vec![SkippedDir {
            path: manifest_denied,
            reason: SkipReason::PermissionDenied,
        }]
    );
    assert_eq!(result.skipped_by(SkipReason::PermissionDenied), 1);
    assert_eq!(result.skipped_by(SkipReason::SystemPath), 0);
    assert_eq!(result.skipped_by(SkipReason::MountPoint), 0);

    // --- Progress and flush ------------------------------------------------
    assert_eq!(sink.flush_count, 1, "flush runs exactly once");

    let events = progress_log.lock().expect("progress log");
    let last = events
        .last()
        .expect("a final progress event is always sent");
    assert_eq!(last.files, result.files);
    assert_eq!(last.dirs, result.dirs);
    assert_eq!(last.bytes_alloc, result.bytes_alloc);
    assert!(
        last.current_path.is_some(),
        "the walk knows its last directory"
    );

    let mut previous = Progress::default();
    for event in events.iter() {
        assert!(
            event.files >= previous.files
                && event.dirs >= previous.dirs
                && event.bytes_alloc >= previous.bytes_alloc,
            "progress went backwards: {previous:?} then {event:?}"
        );
        previous = event.clone();
    }
}

#[test]
fn excluded_and_too_deep_directories_are_reported() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    fs::create_dir_all(root.join("skip")).expect("skip dir");
    fs::create_dir_all(root.join("level1/level2")).expect("deep dirs");
    fs::write(root.join("keep.txt"), b"keep\n").expect("keep");
    fs::write(root.join("skip/secret.txt"), b"secret\n").expect("secret");
    fs::write(root.join("level1/level2/deep.txt"), b"deep\n").expect("deep");

    let options = ScanOptions {
        exclude: vec![root.join("skip")],
        max_depth: Some(1),
        ..ScanOptions::default()
    };

    let mut sink = CollectSink::new(root);
    let result = scan(
        &MacScanner::new(options),
        root,
        &mut sink,
        CancelToken::new(),
        &|_| {},
    )
    .expect("scan");

    let scanned = sink.relative();
    for present in ["keep.txt", "skip", "level1", "level1/level2"] {
        assert!(
            scanned.contains_key(Path::new(present)),
            "{present} should be a row"
        );
    }
    // A skipped directory keeps its own row; nothing below it exists.
    for absent in ["skip/secret.txt", "level1/level2/deep.txt"] {
        assert!(
            !scanned.contains_key(Path::new(absent)),
            "{absent} must not be scanned"
        );
    }

    assert_eq!(result.status, ScanStatus::Partial);
    assert_eq!(result.skipped.len(), 2);
    assert_eq!(result.skipped_by(SkipReason::Excluded), 1);
    assert_eq!(result.skipped_by(SkipReason::Other), 1);
    assert_eq!(result.files, 1);
    assert_eq!(result.dirs, 3);

    let excluded = result
        .skipped
        .iter()
        .find(|skipped| skipped.reason == SkipReason::Excluded)
        .expect("excluded skip");
    assert_eq!(excluded.path, root.join("skip"));
}

#[test]
fn cancellation_returns_a_consistent_partial_tree() {
    let tree = FixtureTree::generate().expect("generate fixture tree");
    let root = tree.root();

    let cancel = CancelToken::new();
    let mut sink = CollectSink::cancelling_at(root, cancel.clone(), 5);

    let result = scan(
        &MacScanner::new(ScanOptions::default()),
        root,
        &mut sink,
        cancel,
        &|_| {},
    )
    .expect("cancellation is a status, not an error");

    assert_eq!(result.status, ScanStatus::Cancelled);
    assert_eq!(sink.flush_count, 1, "a cancelled scan still flushes");
    assert!(
        sink.entries.len() >= 5,
        "the sink cancels on the fifth accept, so at least five rows exist"
    );

    // Every accepted row is in the sink, and the totals count exactly those rows.
    // Consistency is checked by `CollectSink::accept` as each row arrives.
    let files = sink.entries.iter().skip(1).filter(|e| e.is_file()).count() as u64;
    let dirs = sink.entries.iter().skip(1).filter(|e| e.is_dir()).count() as u64;
    let bytes: u64 = sink
        .entries
        .iter()
        .filter(|e| e.is_file())
        .map(|e| e.size_alloc)
        .sum();

    assert_eq!(result.files, files);
    assert_eq!(result.dirs, dirs);
    assert_eq!(result.bytes_alloc.get(), bytes);
}

#[test]
fn invalid_roots_are_rejected_before_the_sink_is_touched() {
    let scanner = MacScanner::new(ScanOptions::default());

    let mut sink = CollectSink::new(Path::new("/"));
    let error = scan(
        &scanner,
        Path::new(""),
        &mut sink,
        CancelToken::new(),
        &|_| {},
    )
    .expect_err("empty root");
    assert!(
        matches!(&error, ScanError::InvalidPath(path) if path.as_os_str().is_empty()),
        "unexpected error: {error}"
    );

    let relative = PathBuf::from("relative/dir");
    let error = scan(&scanner, &relative, &mut sink, CancelToken::new(), &|_| {})
        .expect_err("relative root");
    assert!(
        matches!(&error, ScanError::InvalidPath(path) if path == &relative),
        "unexpected error: {error}"
    );

    let mut sink = CollectSink::new(Path::new("/"));
    let roots = [PathBuf::from("/tmp"), PathBuf::from("/usr")];
    let error = scanner
        .scan(&roots, &mut sink, CancelToken::new(), &|_| {})
        .expect_err("two roots in one pass");
    assert!(
        matches!(&error, ScanError::Unsupported(_)),
        "unexpected error: {error}"
    );

    let mut empty = CollectSink::new(Path::new("/"));
    let error = scanner
        .scan(&[], &mut empty, CancelToken::new(), &|_| {})
        .expect_err("no roots");
    assert!(
        matches!(&error, ScanError::InvalidPath(path) if path.as_os_str().is_empty()),
        "unexpected error: {error}"
    );

    assert!(sink.entries.is_empty(), "no entry before the root is valid");
    assert!(
        empty.entries.is_empty(),
        "no entry before the root is valid"
    );
    assert_eq!(sink.flush_count, 0, "an early error never flushes");
}
