//! Positional file listings for the Large Files table (#23).
//!
//! The table scrolls over every file of a scan, a million rows or more, but
//! holds only a couple of hundred. Keyset pagination can step from a known row
//! but cannot answer "rows 600,000–600,099" when the scrollbar is dragged, and
//! `OFFSET` degrades linearly. So the order lives here, in memory: one
//! [`NodeIndex`] per counted file (4 bytes each, ~4 MB per million files),
//! sorted once per sort key and filter, then sliced by position in O(1).
//!
//! Row data comes from the [`Arena`] (names, sizes, category, path) plus a
//! modified-time column loaded once per scan (8 bytes per node).

use crate::arena::{Arena, NodeIndex};
use crate::error::Result;
use rusqlite::{params, Connection};

/// How the file listing is ordered. Each has a natural direction; a
/// "reversed" listing reads the same order backwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileSort {
    /// Largest first.
    Size,
    /// Oldest first — the stalest files are the cleanup candidates. Files
    /// with no recorded time sort last.
    Modified,
    /// By category (in `Category::ALL` order), largest first within each.
    Type,
}

/// "No recorded modification time" in a [`modified_times`] column.
pub const UNKNOWN_TIME: i64 = i64::MIN;

/// Modification time of every node, by [`NodeIndex`]; [`UNKNOWN_TIME`] where
/// the platform reported none (0 or NULL). One query, once per scan.
pub fn modified_times(conn: &Connection, arena: &Arena) -> Result<Vec<i64>> {
    let mut times = vec![UNKNOWN_TIME; arena.len()];
    let mut statement = conn.prepare(
        "SELECT id, modified FROM entries WHERE scan_id = ?1 AND kind = 1 AND modified IS NOT NULL",
    )?;
    let mut rows = statement.query(params![arena.scan_id()])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let modified: i64 = row.get(1)?;
        if let Some(slot) = arena
            .index_of(id)
            .and_then(|index| times.get_mut(index as usize))
        {
            *slot = if modified == 0 {
                UNKNOWN_TIME
            } else {
                modified
            };
        }
    }
    Ok(times)
}

/// Every counted file (regular, not a hard-link duplicate) of at least
/// `min_size` allocated bytes, in `sort` order. Ties fall back to size, then
/// to index, so the order is total and stable across calls.
///
/// `modified` is required for [`FileSort::Modified`] and ignored otherwise;
/// without it, every time counts as unknown.
pub fn file_order(
    arena: &Arena,
    sort: FileSort,
    min_size: u64,
    modified: Option<&[i64]>,
) -> Vec<NodeIndex> {
    let nodes = arena.nodes();
    let mut order: Vec<NodeIndex> = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.counts() && node.size_alloc >= min_size)
        .map(|(index, _)| index as NodeIndex)
        .collect();

    let size = |index: NodeIndex| nodes[index as usize].size_alloc;
    match sort {
        FileSort::Size => {
            order.sort_unstable_by(|&a, &b| size(b).cmp(&size(a)).then(a.cmp(&b)));
        }
        FileSort::Type => {
            let category = |index: NodeIndex| nodes[index as usize].category;
            order.sort_unstable_by(|&a, &b| {
                category(a)
                    .cmp(&category(b))
                    .then(size(b).cmp(&size(a)))
                    .then(a.cmp(&b))
            });
        }
        FileSort::Modified => {
            let time = |index: NodeIndex| {
                modified
                    .and_then(|times| times.get(index as usize).copied())
                    .unwrap_or(UNKNOWN_TIME)
            };
            // Oldest first; unknown (i64::MIN) last, not first.
            let key = |index: NodeIndex| match time(index) {
                UNKNOWN_TIME => (1u8, 0i64),
                known => (0u8, known),
            };
            order.sort_unstable_by(|&a, &b| {
                key(a)
                    .cmp(&key(b))
                    .then(size(b).cmp(&size(a)))
                    .then(a.cmp(&b))
            });
        }
    }
    order
}

/// Rows `start .. start + count` of `order`, read backwards when `reversed`.
/// Out-of-range positions are simply not returned.
pub fn slice(order: &[NodeIndex], start: usize, count: usize, reversed: bool) -> Vec<NodeIndex> {
    let len = order.len();
    let end = start.saturating_add(count).min(len);
    if start >= end {
        return Vec::new();
    }
    if reversed {
        (start..end)
            .map(|position| order[len - 1 - position])
            .collect()
    } else {
        order[start..end].to_vec()
    }
}
