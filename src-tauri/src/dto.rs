//! Data crossing the IPC boundary.
//!
//! Every type is `camelCase` on the wire and has a hand-written twin in
//! `ui/src/lib/api.ts`; change both together. Byte counts are JSON numbers, exact
//! up to 2^53 bytes (8 PiB), far beyond any single volume.
//!
//! Types for features that do not exist yet (grouping, rules, deletion, apps,
//! duplicates) are declared with their final shape so the UI can be written
//! against them; the commands that return them say which issue builds them.

use serde::{Deserialize, Serialize};
use storagelens_core::{Category, EntryKind, ScanStatus};

/// Live or final state of one scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanStatusDto {
    pub scan_id: i64,
    pub status: ScanStatus,
    /// The scanned path. `None` only for scans recorded before schema v2.
    pub root: Option<String>,
    pub files: u64,
    pub dirs: u64,
    /// Allocated bytes — the "space used" number (AGENTS.md invariant 1).
    pub bytes_alloc: u64,
    pub elapsed_ms: u64,
    pub skipped_dirs: u64,
    /// `Some(false)` on an unelevated Windows scan; `None` where it does not apply.
    pub elevated: Option<bool>,
}

/// Capacity of the volume holding the scan root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VolumeDto {
    pub name: String,
    pub mount_point: String,
    pub total: u64,
    pub available: u64,
    pub used: u64,
}

/// One category's share of the scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryDto {
    pub category: Category,
    pub label: &'static str,
    pub bytes_alloc: u64,
    pub file_count: u64,
}

/// One entry of a directory listing, sized by its whole subtree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildDto {
    /// Catalog row id; pass it back as `parentId` to descend.
    pub id: i64,
    pub name: String,
    pub kind: EntryKind,
    pub category: Category,
    /// Own bytes for a file, subtree total for a directory.
    pub size_alloc: u64,
    /// Counted files in the subtree (1 for a counted file).
    pub file_count: u64,
    pub has_children: bool,
    /// An extra name for an already-counted hard link: listed, not counted.
    pub hardlink_dup: bool,
}

/// What the Overview screen shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OverviewDto {
    pub scan: ScanStatusDto,
    /// Row id of the scanned root, for `get_children`.
    pub root_id: i64,
    pub root_name: String,
    pub volume: Option<VolumeDto>,
    pub categories: Vec<CategoryDto>,
    /// The root's largest children.
    pub biggest: Vec<ChildDto>,
}

/// One file in a flat list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRowDto {
    pub id: i64,
    pub name: String,
    /// Full path. Relative to the root's name only for scans recorded before
    /// schema v2, which stored no root.
    pub path: String,
    pub category: Category,
    pub size_alloc: u64,
    /// Apparent size — show only labelled as such (AGENTS.md invariant 1).
    pub size_logical: u64,
    pub modified: Option<i64>,
}

/// Keyset cursor for size-ordered lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SizeCursorDto {
    pub size_alloc: u64,
    pub id: i64,
}

/// One page of a keyset-paginated list (AGENTS.md §8: ≤ 200 rows).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T, C> {
    pub items: Vec<T>,
    /// Pass back to get the next page; `None` on the last page.
    pub next_cursor: Option<C>,
}

/// The six grouping views (PLAN §9.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Grouping {
    Folder,
    Type,
    App,
    Age,
    Size,
    Cleanliness,
}

/// Cross-filters applied to a grouping (PLAN §9.7).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FilterDto {
    pub categories: Vec<Category>,
    pub min_size: Option<u64>,
    pub older_than_days: Option<u32>,
}

/// A node of a synthetic grouping tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyntheticNodeDto {
    pub label: String,
    pub size_alloc: u64,
    pub file_count: u64,
    pub children: Vec<SyntheticNodeDto>,
}

/// A cleanup rule's findings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleGroupDto {
    pub rule_id: String,
    pub title: String,
    pub risk: String,
    pub bytes: u64,
    pub paths: Vec<String>,
}

/// The dry-run receipt `execute_deletion` requires (AGENTS.md invariant 10).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletionPlanDto {
    pub plan_id: String,
    pub paths: Vec<String>,
    pub created_at: i64,
}

/// What an executed plan did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletionResultDto {
    pub manifest_id: String,
    pub trashed: Vec<String>,
    pub skipped: Vec<String>,
    pub bytes_freed: u64,
}

/// One installed app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppRowDto {
    pub name: String,
    pub path: String,
    pub bytes_alloc: u64,
    pub last_used: Option<i64>,
}

/// A set of identical files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DupGroupDto {
    pub hash: String,
    pub size_alloc: u64,
    pub paths: Vec<String>,
}
