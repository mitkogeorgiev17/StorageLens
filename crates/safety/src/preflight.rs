//! Preflight re-stat (#29, PLAN §15.3): the TOCTOU guard.
//!
//! Immediately before a path is moved, it is stat'ed again and compared with
//! what the scan recorded. Anything that changed since — a different file
//! now at that path (new file id or volume), a different kind, a new size or
//! modification time for a file, a new modification time for a folder or
//! link — means the user would be deleting something they never saw, so the
//! item is skipped and the reason reported. So is an item that is gone, cannot
//! be read, or is now locked (immutable or append-only on macOS, `System` on
//! Windows).
//!
//! The comparison, [`compare`], is pure and allocation-free. The stat itself
//! comes from the platform crate's `stamp(path)` (`storagelens-scan-macos` /
//! `-windows`), which reads exactly the fields its scanner recorded; it is
//! passed in, so this crate stays free of `unsafe` and of OS APIs.

use std::fmt;
use std::io;
use std::path::Path;

use storagelens_core::{EntryKind, FileStamp};

use crate::Platform;

/// macOS `st_flags` that make a file impossible or unsafe to move:
/// `UF_IMMUTABLE`, `UF_APPEND`, `SF_IMMUTABLE`, `SF_APPEND`, `SF_RESTRICTED`
/// (System Integrity Protection), `SF_NOUNLINK`.
pub const MACOS_LOCKED_FLAGS: u32 = 0x2 | 0x4 | 0x2_0000 | 0x4_0000 | 0x8_0000 | 0x10_0000;
/// Windows `FILE_ATTRIBUTE_SYSTEM`.
pub const WINDOWS_LOCKED_ATTRIBUTES: u32 = 0x4;

/// Why an item was skipped. Every skip is reported, never dropped silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skip {
    /// Nothing is at the path any more.
    Missing,
    /// The path could not be stat'ed.
    Unreadable(io::ErrorKind),
    /// A different kind of entry is there now.
    KindChanged {
        expected: EntryKind,
        found: EntryKind,
    },
    /// A different file is at the path now (new file id or volume).
    Replaced,
    /// The file's size changed.
    Resized { expected: u64, found: u64 },
    /// It was modified after the scan.
    Modified { expected: i64, found: i64 },
    /// It is now immutable, append-only, or a system file.
    Locked,
}

impl fmt::Display for Skip {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing => f.write_str("it is no longer there"),
            Self::Unreadable(kind) => write!(f, "it could not be checked ({kind})"),
            Self::KindChanged { expected, found } => {
                write!(
                    f,
                    "it was a {expected:?} when scanned and is a {found:?} now"
                )
            }
            Self::Replaced => f.write_str("a different file is there now"),
            Self::Resized { expected, found } => {
                write!(f, "its size changed from {expected} to {found} bytes")
            }
            Self::Modified { .. } => f.write_str("it was modified after the scan"),
            Self::Locked => f.write_str("it is locked or a system file"),
        }
    }
}

impl std::error::Error for Skip {}

/// Re-stat `path` with `stamp` and compare it with what the scan recorded.
pub fn preflight(
    path: &Path,
    expected: &FileStamp,
    stamp: impl FnOnce(&Path) -> io::Result<FileStamp>,
) -> Result<(), Skip> {
    compare(Platform::CURRENT, expected, stamp(path))
}

/// Compare a fresh stat with the scan's. Pure: no allocation, no syscalls.
pub fn compare(
    platform: Platform,
    expected: &FileStamp,
    observed: io::Result<FileStamp>,
) -> Result<(), Skip> {
    let found = match observed {
        Ok(found) => found,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(Skip::Missing),
        Err(error) => return Err(Skip::Unreadable(error.kind())),
    };
    if found.kind != expected.kind {
        return Err(Skip::KindChanged {
            expected: expected.kind,
            found: found.kind,
        });
    }
    if found.dev != expected.dev || found.file_id != expected.file_id {
        return Err(Skip::Replaced);
    }
    // A folder's or link's size is not a content measure; its time is.
    if found.kind == EntryKind::File && found.size_logical != expected.size_logical {
        return Err(Skip::Resized {
            expected: expected.size_logical,
            found: found.size_logical,
        });
    }
    if found.modified != expected.modified {
        return Err(Skip::Modified {
            expected: expected.modified,
            found: found.modified,
        });
    }
    let locked = match platform {
        Platform::MacOs => MACOS_LOCKED_FLAGS,
        Platform::Windows => WINDOWS_LOCKED_ATTRIBUTES,
    };
    if found.attrs & locked != 0 {
        return Err(Skip::Locked);
    }
    Ok(())
}
