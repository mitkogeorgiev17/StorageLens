//! Data crossing the IPC boundary.
//!
//! Every type is `camelCase` on the wire and has a hand-written twin in
//! `ui/src/lib/api.ts`; change both together. Byte counts are JSON numbers, exact
//! up to 2^53 bytes (8 PiB), far beyond any single volume.
//!
//! Types for features that do not exist yet (rules, deletion, apps,
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
    /// Space the OS reports as available. Includes `purgeable`.
    pub available: u64,
    /// `total − available`.
    pub used: u64,
    /// macOS purgeable space (APFS snapshots, caches the OS may evict), part of
    /// `available`. 0 on Windows, and on macOS until the Foundation bridge lands.
    pub purgeable: u64,
}

/// Something the scan picker offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScanTargetKind {
    Volume,
    Home,
}

/// One entry of the scan picker: a mounted volume or the home folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanTargetDto {
    pub kind: ScanTargetKind,
    pub label: String,
    /// Absolute path to pass to `start_scan`.
    pub path: String,
    /// Capacity, for volumes.
    pub total: Option<u64>,
    pub available: Option<u64>,
    pub removable: bool,
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

/// A directory's subtree, a few levels deep, for the treemap.
///
/// Sizes add up at every level: a node's `size_alloc` is its loaded children
/// plus `remaining_bytes` (hard-link duplicates are listed nowhere, counted
/// once already). A directory with `children: None` was not expanded — the
/// node budget ran out — and is drawn as one block until it is zoomed into.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TreeNodeDto {
    pub id: i64,
    pub name: String,
    pub kind: EntryKind,
    pub category: Category,
    pub size_alloc: u64,
    pub file_count: u64,
    pub has_children: bool,
    pub children: Option<Vec<TreeNodeDto>>,
    /// Children not sent, aggregated into one "smaller items" block.
    pub remaining_count: u64,
    pub remaining_bytes: u64,
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
    /// `File` everywhere but search, which also finds folders.
    pub kind: EntryKind,
    /// Full path. Relative to the root's name only for scans recorded before
    /// schema v2, which stored no root.
    pub path: String,
    pub category: Category,
    pub size_alloc: u64,
    /// Apparent size — show only labelled as such (AGENTS.md invariant 1).
    pub size_logical: u64,
    pub modified: Option<i64>,
}

/// Sort keys of the Large Files table. Each has a natural direction (size:
/// largest first, modified: oldest first, type: by category then largest);
/// `reversed` reads it backwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileSortDto {
    Size,
    Modified,
    Type,
}

/// Rows `start ..` of a file listing, and how many rows it has in all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRangeDto {
    pub total: u64,
    pub start: u64,
    pub items: Vec<FileRowDto>,
}

/// Keyset cursor for search results: biggest, then most recent, then id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchCursorDto {
    pub size_alloc: u64,
    pub modified: i64,
    pub id: i64,
}

/// One step of a breadcrumb.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CrumbDto {
    pub id: i64,
    pub name: String,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Grouping {
    Folder,
    Type,
    App,
    Age,
    Size,
    Cleanliness,
}

/// A smart collection (#28). `query` is `None` when the stored query cannot be
/// read by this build; `problem` then says why, and the collection can still be
/// renamed or deleted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionDto {
    pub id: i64,
    pub name: String,
    pub query: Option<storagelens_catalog::CollectionQuery>,
    pub problem: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A collection evaluated against one scan. Always names the scan, so the UI
/// can drop a result that belongs to an older one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectionResultDto {
    pub collection_id: i64,
    pub scan_id: i64,
    pub bytes: u64,
    pub files: u64,
    /// The folder limit, resolved in this scan.
    pub within: Option<CrumbDto>,
    /// Why the collection shows nothing for this scan (unreadable query, or a
    /// folder this scan does not contain); totals are then zero.
    pub problem: Option<String>,
}

/// Cross-filters applied to a grouping (PLAN §9.7).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FilterDto {
    pub categories: Vec<Category>,
    pub min_size: Option<u64>,
    pub older_than_days: Option<u32>,
    /// Only files inside this folder (an entry id); `None` means the whole scan.
    pub within: Option<i64>,
}

/// What the Cleanup screen shows for one scan (#34).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupDto {
    pub scan_id: i64,
    /// By kind (regenerable, stale, duplicate, leftover, valuable, system),
    /// biggest first within a kind.
    pub groups: Vec<RuleGroupDto>,
    /// Matches the path guard refused; they are never offered.
    pub refused: u64,
    /// Σ bytes × confidence over safe suggestions, each byte once (PLAN §10),
    /// as of the scan.
    pub reclaim_safe: u64,
    /// The same over suggestions that need review, before the score's 0.5.
    pub reclaim_review: u64,
    /// Catalog files or rules that were skipped, and why.
    pub problems: Vec<String>,
}

/// One rule card.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleGroupDto {
    pub rule_id: String,
    pub title: String,
    /// `regenerable`, `stale`, `duplicate`, `leftover`, `valuable`, `system`.
    pub kind: String,
    /// `safe`, `review`, `keep`, `never`.
    pub risk: String,
    pub explain: String,
    /// For report-only cards: what to run instead.
    pub command: Option<String>,
    /// Whether its items can be selected at all.
    pub deletable: bool,
    pub bytes: u64,
    pub items: Vec<RuleItemDto>,
}

/// One match on a rule card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuleItemDto {
    /// Absolute path (or, for a snapshot, its name).
    pub path: String,
    pub entry_id: Option<i64>,
    pub bytes: u64,
    /// Starts selected.
    pub selected: bool,
}

/// The dry-run receipt `execute_deletion` requires (AGENTS.md invariant 10):
/// exactly what will be moved, in the order the manifest will list it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletionPlanDto {
    pub plan_id: String,
    pub scan_id: i64,
    pub items: Vec<PlanItemDto>,
    pub total_bytes: u64,
    /// Over 5 GB: execution needs a second confirmation (PLAN §15.7).
    pub needs_confirmation: bool,
    pub created_at: i64,
}

/// One receipt line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanItemDto {
    pub path: String,
    pub bytes: u64,
    pub rule_id: String,
    pub title: String,
    pub risk: String,
}

/// What an executed plan did, item by item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletionResultDto {
    /// The manifest's file name; `undo_deletion` takes it.
    pub manifest_id: String,
    pub items: Vec<ResultItemDto>,
    pub trashed: u64,
    pub trashed_bytes: u64,
    /// Set when the final manifest write failed (undo still works).
    pub warning: Option<String>,
}

/// One item's outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultItemDto {
    pub path: String,
    pub bytes: u64,
    /// `trashed`, `refused`, `skipped`, `failed`, `cancelled`.
    pub status: String,
    pub reason: Option<String>,
}

/// What an undo did, item by item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoResultDto {
    pub items: Vec<UndoItemDto>,
    pub restored: u64,
    pub warning: Option<String>,
}

/// One item's undo outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UndoItemDto {
    pub path: String,
    /// `restored`, `already-restored`, `not-trashed`, `not-in-trash`,
    /// `occupied`, `failed`.
    pub result: String,
    pub reason: Option<String>,
}

/// Deletion progress, sent before each item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupProgressDto {
    pub plan_id: String,
    pub done: u64,
    pub total: u64,
}

/// One installed app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppRowDto {
    /// Bundle id (macOS) or uninstall key (Windows).
    pub id: String,
    pub name: String,
    pub version: Option<String>,
    /// Empty when Windows recorded no install directory.
    pub path: String,
    pub app_bytes: u64,
    /// Associated data: a heuristic, always shown as "estimated".
    pub satellite_bytes: u64,
    pub total_bytes: u64,
    pub last_used: Option<i64>,
    pub use_count: Option<u64>,
    /// `in_use` | `likely_unused` | `unused` | `unknown`.
    pub status: storagelens_core::AppStatus,
    pub running: bool,
    pub autostart: bool,
    pub default_handler: bool,
    pub satellites: Vec<SatelliteDto>,
}

/// One associated-data path of an app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SatelliteDto {
    pub path: String,
    pub bytes: u64,
}

/// A set of identical files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DupGroupDto {
    pub hash: String,
    pub size_alloc: u64,
    pub paths: Vec<String>,
}
