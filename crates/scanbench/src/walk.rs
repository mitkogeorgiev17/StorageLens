//! Portable reference walker.
//!
//! `scanbench` needs a scanner before the fast platform backends exist (M1/M2),
//! and afterwards it needs a slow, obviously-correct baseline to compare them
//! against. This is that baseline: a single-threaded `std::fs` depth-first walk
//! that streams into an [`EntrySink`] in pre-order.
//!
//! It is deliberately **not** the app's scanner. The platform crates replace it
//! for speed; `scanbench` keeps it as the accuracy reference.
//!
//! # What it gets right
//!
//! - Never follows symlinks or Windows reparse points.
//! - Dedupes hard links by inode on Unix and counts the inode once.
//! - Reports unreadable directories instead of aborting.
//! - Applies `skip_system`, `exclude`, `max_depth`, and (Unix) the
//!   cross-filesystem check.
//! - Streams pre-order, as [`EntrySink`] requires.
//!
//! # Known Windows limitation
//!
//! Stable `std` exposes neither the file index nor the link count on Windows
//! (`file_index`/`number_of_links` are nightly-only), so the reference walker
//! cannot dedupe hard links there. Every name is counted. The real Windows
//! scanner (#12) uses `FindFirstFileExW`'s file index and does dedupe; the
//! fixture check compensates for the difference explicitly.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use storagelens_core::{
    Bytes, CancelToken, Entry, EntryKind, EntrySink, Result, ScanError, ScanOptions, ScanResult,
    ScanStatus, SkipReason, SkippedDir,
};

/// How often to sample resident memory, in accepted entries.
const RSS_SAMPLE_EVERY: u64 = 4_096;

/// Everything a reference walk produces.
#[derive(Debug, Clone)]
pub struct ScanOutcome {
    /// Counts, byte totals, elapsed time, and skipped directories.
    pub result: ScanResult,
    /// Symlinks and Windows reparse points. Never followed.
    pub links: u64,
    /// Entries handed to the sink, including the root.
    pub entries: u64,
    /// Highest sampled resident set size, in bytes. `0` if unavailable.
    pub peak_rss_bytes: u64,
}

/// Walk `root` into `sink` and return the totals.
///
/// `root` may be a file or a directory. It is the first entry accepted, with
/// `parent == 0` (arena index `0`). Cancellation is checked between
/// directories.
pub fn scan<S: EntrySink>(
    root: &Path,
    options: &ScanOptions,
    cancel: &CancelToken,
    sink: &mut S,
) -> Result<ScanOutcome> {
    let started = Instant::now();

    // Follow a symlinked root: the caller asked to scan its target. Children are
    // still enumerated with `symlink_metadata`, so links below the root are not
    // followed.
    let metadata = fs::metadata(root).map_err(|source| ScanError::Io {
        path: root.to_path_buf(),
        source,
    })?;

    let root_name = root
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.display().to_string());

    let mut walker = Walker {
        sink,
        options,
        cancel,
        next_index: 0,
        files: 0,
        dirs: 0,
        bytes_alloc: 0,
        bytes_logical: 0,
        links: 0,
        entries: 0,
        skipped: Vec::new(),
        seen: HashSet::new(),
        peak_rss_bytes: 0,
        root_dev: dev_of(&metadata),
    };
    walker.sample_rss();

    let kind = if metadata.is_dir() {
        EntryKind::Dir
    } else if metadata.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    };

    // The root occupies arena index 0 and is not counted as a child directory.
    let root_index = walker.next_index;
    walker.accept(root_name, kind, &metadata, 0)?;

    match kind {
        EntryKind::Dir => walker.walk_dir(root, root_index, 0)?,
        EntryKind::File => walker.count_file(&metadata),
        EntryKind::Symlink | EntryKind::Other => {}
    }

    walker.sample_rss();
    Ok(walker.finish(started.elapsed().as_millis() as u64))
}

struct Walker<'a, S: EntrySink> {
    sink: &'a mut S,
    options: &'a ScanOptions,
    cancel: &'a CancelToken,
    next_index: u32,
    files: u64,
    dirs: u64,
    bytes_alloc: u64,
    bytes_logical: u64,
    links: u64,
    entries: u64,
    skipped: Vec<SkippedDir>,
    seen: HashSet<FileKey>,
    peak_rss_bytes: u64,
    root_dev: Option<u64>,
}

impl<S: EntrySink> Walker<'_, S> {
    fn finish(self, elapsed_ms: u64) -> ScanOutcome {
        let status = if self.skipped.is_empty() {
            ScanStatus::Complete
        } else {
            ScanStatus::Partial
        };

        ScanOutcome {
            result: ScanResult {
                status,
                files: self.files,
                dirs: self.dirs,
                bytes_alloc: Bytes::new(self.bytes_alloc),
                bytes_logical: Bytes::new(self.bytes_logical),
                elapsed_ms,
                skipped: self.skipped,
            },
            links: self.links,
            entries: self.entries,
            peak_rss_bytes: self.peak_rss_bytes,
        }
    }

    fn walk_dir(&mut self, dir: &Path, parent: u32, depth: u32) -> Result<()> {
        if self.cancel.is_cancelled() {
            return Err(ScanError::Cancelled);
        }

        let read_dir = match fs::read_dir(dir) {
            Ok(read_dir) => read_dir,
            Err(source) => {
                self.record_skip(dir, skip_reason(&source));
                return Ok(());
            }
        };

        // Sort so that a run is reproducible and the fixture totals are stable.
        let mut children: Vec<(std::ffi::OsString, PathBuf)> = Vec::new();
        for entry in read_dir {
            match entry {
                Ok(entry) => children.push((entry.file_name(), entry.path())),
                Err(source) => self.record_skip(dir, skip_reason(&source)),
            }
        }
        children.sort_by(|left, right| left.0.cmp(&right.0));

        for (name, path) in children {
            if self.cancel.is_cancelled() {
                return Err(ScanError::Cancelled);
            }

            let metadata = match fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(source) => {
                    self.record_skip(&path, skip_reason(&source));
                    continue;
                }
            };
            let name = name.to_string_lossy().into_owned();

            if is_link(&metadata) {
                self.accept(name, EntryKind::Symlink, &metadata, parent)?;
                self.links = self.links.saturating_add(1);
                continue;
            }

            if metadata.is_dir() {
                let index = self.next_index;
                self.accept(name, EntryKind::Dir, &metadata, parent)?;
                self.dirs = self.dirs.saturating_add(1);

                if let Some(reason) = self.skip_reason_for(&path, &metadata, depth + 1) {
                    self.skipped.push(SkippedDir { path, reason });
                    continue;
                }
                self.walk_dir(&path, index, depth + 1)?;
                continue;
            }

            if metadata.is_file() {
                self.accept(name, EntryKind::File, &metadata, parent)?;
                self.count_file(&metadata);
                continue;
            }

            // Sockets, FIFOs, devices: recorded, never counted as files.
            self.accept(name, EntryKind::Other, &metadata, parent)?;
        }

        Ok(())
    }

    /// Count a regular file once, unless a hard link to it was already counted.
    fn count_file(&mut self, metadata: &fs::Metadata) {
        if nlink_of(metadata) > 1 && !self.seen.insert(file_key(metadata)) {
            return;
        }

        self.files = self.files.saturating_add(1);
        let (alloc, logical) = sizes(metadata);
        self.bytes_alloc = self.bytes_alloc.saturating_add(alloc);
        self.bytes_logical = self.bytes_logical.saturating_add(logical);
    }

    fn skip_reason_for(
        &self,
        path: &Path,
        metadata: &fs::Metadata,
        depth: u32,
    ) -> Option<SkipReason> {
        if self.options.skip_system && is_system_path(path) {
            return Some(SkipReason::SystemPath);
        }

        if !self.options.cross_filesystems {
            if let (Some(root_dev), Some(child_dev)) = (self.root_dev, dev_of(metadata)) {
                if root_dev != child_dev {
                    return Some(SkipReason::MountPoint);
                }
            }
        }

        if self
            .options
            .exclude
            .iter()
            .any(|excluded| path.starts_with(excluded))
        {
            return Some(SkipReason::Excluded);
        }

        if let Some(max_depth) = self.options.max_depth {
            if depth > max_depth {
                return Some(SkipReason::Other);
            }
        }

        None
    }

    fn accept(
        &mut self,
        name: String,
        kind: EntryKind,
        metadata: &fs::Metadata,
        parent: u32,
    ) -> Result<()> {
        let (size_alloc, size_logical) = sizes(metadata);
        let entry = Entry {
            parent,
            name,
            kind,
            size_alloc,
            size_logical,
            dev: dev_of(metadata).unwrap_or(0) as u32,
            file_id: file_id_of(metadata),
            nlink: nlink_of(metadata),
            attrs: attrs_of(metadata),
            created: created_of(metadata),
            modified: modified_of(metadata),
            accessed: accessed_of(metadata),
        };

        // The sink assigns arena indices in acceptance order; mirror that here so
        // children get the index their parent was accepted at.
        self.next_index = self.next_index.saturating_add(1);
        self.entries = self.entries.saturating_add(1);

        self.sink.accept(entry)?;

        if self.entries % RSS_SAMPLE_EVERY == 0 {
            self.sample_rss();
        }
        Ok(())
    }

    fn record_skip(&mut self, path: &Path, reason: SkipReason) {
        self.skipped.push(SkippedDir {
            path: path.to_path_buf(),
            reason,
        });
    }

    fn sample_rss(&mut self) {
        if let Some(stats) = memory_stats::memory_stats() {
            self.peak_rss_bytes = self.peak_rss_bytes.max(stats.physical_mem as u64);
        }
    }
}

#[cfg(unix)]
type FileKey = (u64, u64);

/// Stable `std` cannot identify a file on Windows, so nothing is deduped there.
/// The placeholder keeps the `seen` set well-typed without a unit argument;
/// `nlink_of` is always `1` on Windows, so it is never populated.
#[cfg(not(unix))]
type FileKey = u64;

#[cfg(unix)]
fn file_key(metadata: &fs::Metadata) -> FileKey {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}

#[cfg(not(unix))]
fn file_key(_metadata: &fs::Metadata) -> FileKey {
    0
}

#[cfg(unix)]
fn sizes(metadata: &fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.blocks().saturating_mul(512), metadata.len())
}

#[cfg(not(unix))]
fn sizes(metadata: &fs::Metadata) -> (u64, u64) {
    // `std` exposes no allocation size on Windows; the real scanner uses
    // `GetCompressedFileSizeW`/`AllocationSize` (#13).
    let size = metadata.len();
    (size, size)
}

#[cfg(unix)]
fn dev_of(metadata: &fs::Metadata) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    Some(metadata.dev())
}

#[cfg(not(unix))]
fn dev_of(_metadata: &fs::Metadata) -> Option<u64> {
    None
}

#[cfg(unix)]
fn file_id_of(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.ino()
}

#[cfg(not(unix))]
fn file_id_of(_metadata: &fs::Metadata) -> u64 {
    0
}

#[cfg(unix)]
fn nlink_of(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::MetadataExt;
    metadata.nlink() as u32
}

#[cfg(not(unix))]
fn nlink_of(_metadata: &fs::Metadata) -> u32 {
    // `MetadataExt::number_of_links` is nightly-only.
    1
}

#[cfg(unix)]
fn attrs_of(metadata: &fs::Metadata) -> u32 {
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        metadata.st_flags()
    }
    #[cfg(not(target_os = "macos"))]
    {
        use std::os::unix::fs::MetadataExt;
        metadata.mode()
    }
}

#[cfg(windows)]
fn attrs_of(metadata: &fs::Metadata) -> u32 {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes()
}

#[cfg(not(any(unix, windows)))]
fn attrs_of(_metadata: &fs::Metadata) -> u32 {
    0
}

#[cfg(unix)]
fn created_of(metadata: &fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt;
    metadata.ctime()
}

#[cfg(unix)]
fn modified_of(metadata: &fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt;
    metadata.mtime()
}

#[cfg(unix)]
fn accessed_of(metadata: &fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt;
    metadata.atime()
}

/// Seconds between 1601-01-01 and 1970-01-01.
#[cfg(windows)]
const FILETIME_UNIX_EPOCH_SECONDS: u64 = 11_644_473_600;

#[cfg(windows)]
fn filetime_to_unix(filetime: u64) -> i64 {
    if filetime == 0 {
        return 0;
    }
    (filetime / 10_000_000).saturating_sub(FILETIME_UNIX_EPOCH_SECONDS) as i64
}

#[cfg(windows)]
fn created_of(metadata: &fs::Metadata) -> i64 {
    use std::os::windows::fs::MetadataExt;
    filetime_to_unix(metadata.creation_time())
}

#[cfg(windows)]
fn modified_of(metadata: &fs::Metadata) -> i64 {
    use std::os::windows::fs::MetadataExt;
    filetime_to_unix(metadata.last_write_time())
}

#[cfg(windows)]
fn accessed_of(metadata: &fs::Metadata) -> i64 {
    use std::os::windows::fs::MetadataExt;
    filetime_to_unix(metadata.last_access_time())
}

#[cfg(not(any(unix, windows)))]
fn created_of(_metadata: &fs::Metadata) -> i64 {
    0
}

#[cfg(not(any(unix, windows)))]
fn modified_of(_metadata: &fs::Metadata) -> i64 {
    0
}

#[cfg(not(any(unix, windows)))]
fn accessed_of(_metadata: &fs::Metadata) -> i64 {
    0
}

#[cfg(unix)]
fn is_link(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn is_link(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
    metadata.file_type().is_symlink()
        || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(any(unix, windows)))]
fn is_link(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

fn skip_reason(source: &std::io::Error) -> SkipReason {
    match source.kind() {
        std::io::ErrorKind::PermissionDenied => SkipReason::PermissionDenied,
        _ => SkipReason::IoError,
    }
}

/// The PLAN §6.4 default skip list, plus the Windows equivalents.
#[cfg(target_os = "macos")]
fn is_system_path(path: &Path) -> bool {
    const PREFIXES: &[&str] = &[
        "/System/Volumes",
        "/dev",
        "/Volumes",
        "/.Spotlight-V100",
        "/.fseventsd",
        "/.DocumentRevisions-V100",
        "/.Trashes",
        "/private/var/vm",
    ];
    PREFIXES.iter().any(|prefix| path.starts_with(prefix))
}

#[cfg(windows)]
fn is_system_path(path: &Path) -> bool {
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_owned());
    let system = format!("{drive}\\Windows");
    let recycle = format!("{drive}\\$Recycle.Bin");
    let vss = format!("{drive}\\System Volume Information");

    let lowered = path.to_string_lossy().to_ascii_lowercase();
    [system, recycle, vss]
        .iter()
        .any(|prefix| lowered.starts_with(&prefix.to_ascii_lowercase()))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn is_system_path(_path: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use storagelens_core::FixtureTree;

    #[derive(Default)]
    struct CollectingSink {
        entries: Vec<Entry>,
    }

    impl EntrySink for CollectingSink {
        fn accept(&mut self, entry: Entry) -> Result<()> {
            self.entries.push(entry);
            Ok(())
        }

        fn flush(&mut self) -> Result<()> {
            Ok(())
        }
    }

    fn walk_fixture() -> (FixtureTree, ScanOutcome, Vec<Entry>) {
        let tree = FixtureTree::generate().expect("generate fixture tree");
        let options = ScanOptions::default();
        let cancel = CancelToken::new();
        let mut sink = CollectingSink::default();
        let outcome = scan(tree.root(), &options, &cancel, &mut sink).expect("scan");
        (tree, outcome, sink.entries)
    }

    /// Hard-link names the walker counts but the manifest folds into one file.
    #[cfg(unix)]
    fn hardlink_extras(_manifest: &storagelens_core::Manifest) -> (u64, u64) {
        (0, 0)
    }

    #[cfg(not(unix))]
    fn hardlink_extras(manifest: &storagelens_core::Manifest) -> (u64, u64) {
        let extras = manifest.hardlink_names.saturating_sub(1);
        let size = fs::metadata(&manifest.paths.hardlinks[0])
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        (extras, extras.saturating_mul(size))
    }

    #[test]
    fn matches_the_fixture_manifest() {
        let (tree, outcome, _) = walk_fixture();
        let manifest = tree.manifest();
        let (extra_files, extra_bytes) = hardlink_extras(manifest);

        assert_eq!(outcome.result.files + extra_files, manifest.files, "files");
        assert_eq!(outcome.result.dirs, manifest.dirs, "dirs");
        assert_eq!(
            outcome.links,
            manifest.symlinks + manifest.junctions,
            "links"
        );
        assert_eq!(
            outcome.result.bytes_alloc.get() + extra_bytes,
            manifest.bytes_alloc,
            "allocated"
        );
        assert_eq!(
            outcome.result.bytes_logical.get() + extra_bytes,
            manifest.bytes_logical,
            "logical"
        );
    }

    #[test]
    fn entries_arrive_in_preorder() {
        let (_tree, _outcome, entries) = walk_fixture();

        assert_eq!(entries[0].parent, 0, "root has parent 0");
        for (index, entry) in entries.iter().enumerate().skip(1) {
            assert!(
                (entry.parent as usize) < index,
                "entry {index} ({}) has parent {} — not pre-order",
                entry.name,
                entry.parent
            );
        }
    }

    /// Windows has no inode, so the reference walker deliberately counts every
    /// hard-link name there; `fixtures_check` compensates for that.
    #[cfg(unix)]
    #[test]
    fn hard_links_are_counted_once_on_unix() {
        let (tree, outcome, _) = walk_fixture();
        let manifest = tree.manifest();

        assert_eq!(outcome.result.files, manifest.files);
        assert_eq!(manifest.hardlink_names, 3);
    }

    #[test]
    fn children_point_at_their_parents_arena_index() {
        let (_tree, _outcome, entries) = walk_fixture();

        let files_index = entries
            .iter()
            .position(|entry| entry.name == "files" && entry.kind == EntryKind::Dir)
            .expect("files dir") as u32;
        let plain = entries
            .iter()
            .find(|entry| entry.name == "plain.txt")
            .expect("plain file");

        assert_eq!(plain.kind, EntryKind::File);
        assert_eq!(plain.parent, files_index);
    }

    #[cfg(unix)]
    #[test]
    fn the_unreadable_directory_is_skipped_not_fatal() {
        let (tree, outcome, _) = walk_fixture();
        let denied = tree
            .manifest()
            .paths
            .denied_dir
            .as_ref()
            .expect("denied dir");

        assert!(outcome.result.skipped.iter().any(|skipped| {
            skipped.path == *denied && skipped.reason == SkipReason::PermissionDenied
        }));
        assert_eq!(outcome.result.status, ScanStatus::Partial);
    }

    #[test]
    fn excluded_paths_are_skipped() {
        let tree = FixtureTree::generate().expect("generate fixture tree");
        let options = ScanOptions {
            exclude: vec![tree.root().join("categories")],
            ..ScanOptions::default()
        };
        let cancel = CancelToken::new();
        let mut sink = CollectingSink::default();
        let outcome = scan(tree.root(), &options, &cancel, &mut sink).expect("scan");

        assert!(outcome
            .result
            .skipped
            .iter()
            .any(|skipped| skipped.reason == SkipReason::Excluded));
        assert!(!sink
            .entries
            .iter()
            .any(|entry| entry.name.starts_with("sample.")));
    }

    #[test]
    fn max_depth_stops_descending() {
        let tree = FixtureTree::generate().expect("generate fixture tree");
        let options = ScanOptions {
            max_depth: Some(1),
            ..ScanOptions::default()
        };
        let cancel = CancelToken::new();
        let mut sink = CollectingSink::default();
        let outcome = scan(tree.root(), &options, &cancel, &mut sink).expect("scan");

        // Top-level directories are entered; their subdirectories are not.
        assert!(outcome.result.dirs < tree.manifest().dirs);
        assert!(outcome
            .result
            .skipped
            .iter()
            .any(|skipped| skipped.reason == SkipReason::Other));
        // `files/plain.txt` is a file one level down and is still seen.
        assert!(sink.entries.iter().any(|entry| entry.name == "plain.txt"));
        // The 200-level tree is not entered, so its leaf is never seen.
        assert!(!sink.entries.iter().any(|entry| entry.name == "leaf.txt"));
    }

    #[test]
    fn a_file_root_is_scanned_as_one_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("single.txt");
        fs::write(&path, b"hello").expect("write");

        let cancel = CancelToken::new();
        let mut sink = CollectingSink::default();
        let outcome = scan(&path, &ScanOptions::default(), &cancel, &mut sink).expect("scan");

        assert_eq!(outcome.result.files, 1);
        assert_eq!(outcome.result.dirs, 0);
        assert_eq!(outcome.result.bytes_logical.get(), 5);
        assert_eq!(outcome.entries, 1);
    }

    #[test]
    fn a_missing_root_is_an_error() {
        let cancel = CancelToken::new();
        let mut sink = CollectingSink::default();
        let error = scan(
            Path::new("/definitely/not/here"),
            &ScanOptions::default(),
            &cancel,
            &mut sink,
        )
        .expect_err("must fail");

        assert!(matches!(error, ScanError::Io { .. }));
    }
}
