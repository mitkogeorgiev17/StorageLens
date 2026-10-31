//! Re-stat one path for the safety layer's preflight (#29).
//!
//! Reads the same fields the walker records, from `lstat(2)`: a final symlink
//! is described, not followed. `getattrlistbulk` and `lstat` report the same
//! inode, device, data length, flags, and modification seconds, which
//! `tests/fixture_stat.rs` checks for every entry of the fixture tree.

use std::fs;
use std::io;
use std::os::macos::fs::MetadataExt as _;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use storagelens_core::{EntryKind, FileStamp};

/// What the scanner would record for `path` now.
pub fn stamp(path: &Path) -> io::Result<FileStamp> {
    let metadata = fs::symlink_metadata(path)?;
    let file_type = metadata.file_type();
    let kind = if file_type.is_symlink() {
        EntryKind::Symlink
    } else if file_type.is_dir() {
        EntryKind::Dir
    } else if file_type.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    };
    Ok(FileStamp {
        kind,
        size_logical: metadata.len(),
        modified: metadata.mtime(),
        dev: metadata.dev() as u32,
        file_id: metadata.ino(),
        attrs: metadata.st_flags(),
    })
}
