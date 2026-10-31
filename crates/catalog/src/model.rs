//! Public data structures returned by catalog queries.

use serde::{Deserialize, Serialize};
use storagelens_core::{Category, EntryKind, ScanStatus, Tags};

/// Which operating system produced a scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    /// macOS.
    Macos,
    /// Windows.
    Windows,
}

impl Platform {
    /// The value stored in `scan_session.platform`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Macos => "macos",
            Self::Windows => "windows",
        }
    }

    /// Parse a stored value.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "macos" => Some(Self::Macos),
            "windows" => Some(Self::Windows),
            _ => None,
        }
    }

    /// The platform this build is running on.
    pub const fn current() -> Self {
        if cfg!(target_os = "windows") {
            Self::Windows
        } else {
            Self::Macos
        }
    }
}

/// One row from `scan_session`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanSession {
    /// Row id.
    pub id: i64,
    /// Operating system that produced the scan.
    pub platform: Platform,
    /// Unix seconds.
    pub started_at: i64,
    /// Unix seconds, `None` while running.
    pub finished_at: Option<i64>,
    /// Lifecycle state.
    pub status: ScanStatus,
    /// Files counted.
    pub files_seen: u64,
    /// Allocated bytes counted.
    pub bytes_seen: u64,
    /// Directories that could not be entered.
    pub dirs_skipped: u64,
    /// Whether the scan ran elevated.
    pub elevated: bool,
}

/// An entry read back from `entries`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredEntry {
    /// Row id. Used as the keyset cursor alongside `size_alloc`.
    pub id: i64,
    /// Parent row id; `0` is a volume root.
    pub parent_id: i64,
    /// Final path component.
    pub name: String,
    /// Lowercased last extension segment.
    pub ext: Option<String>,
    /// File, directory, symlink, or other.
    pub kind: EntryKind,
    /// Allocated bytes.
    pub size_alloc: u64,
    /// Logical bytes.
    pub size_logical: u64,
    /// Volume identity.
    pub dev: u32,
    /// Inode / file index.
    pub file_id: u64,
    /// Hard-link count.
    pub nlink: u32,
    /// Raw platform attributes.
    pub attrs: u32,
    /// Creation time, when the platform reported one.
    pub created: Option<i64>,
    /// Modification time.
    pub modified: Option<i64>,
    /// Access time.
    pub accessed: Option<i64>,
    /// Primary category.
    pub category: Category,
    /// Property bits.
    pub tags: Tags,
    /// True when this path is a hard-link duplicate that was not counted.
    pub hardlink_dup: bool,
}

/// Keyset cursor for [`Catalog::largest`](crate::Catalog::largest).
///
/// Deliberately not an offset: `OFFSET` degrades linearly and the Large Files
/// screen must scroll a million rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SizeCursor {
    /// `size_alloc` of the last row on the previous page.
    pub size_alloc: u64,
    /// `id` of the last row on the previous page.
    pub id: i64,
}

/// One row from the category-totals query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CategoryTotal {
    /// Primary category.
    pub category: Category,
    /// Allocated bytes in this category.
    pub bytes_alloc: u64,
    /// Number of regular files.
    pub file_count: u64,
}
