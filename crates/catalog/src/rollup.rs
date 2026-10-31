//! Directory rollups: subtree totals in the arena and in `dir_rollup`.
//!
//! # One reverse pass is a post-order
//!
//! Arena indices follow SQL ids, which follow pre-order acceptance, so every
//! child has a higher index than its parent (verified by [`Arena::load`]).
//! Visiting nodes from the highest index down therefore finishes every child
//! before its parent — exactly the guarantee a post-order DFS gives — with no
//! recursion and no stack, even through the fixture's 200-level nesting.
//!
//! # What counts
//!
//! A node's own bytes count only if [`Node::counts`]: a regular file that is not
//! a hard-link duplicate. That is the rule both scanners use for `ScanResult`,
//! so the root's rollup equals the scan's own totals (AGENTS.md invariants 2
//! and 11).
//!
//! # Incremental
//!
//! [`recompute_dirty`] recomputes only the subtrees under the given directories,
//! then applies each one's change to its ancestors. Its result is identical to a
//! full [`compute`]; the tests check that on random trees.

use std::collections::BTreeSet;

use rusqlite::{params, Connection};
use storagelens_core::EntryKind;

use crate::arena::{Arena, Node, NodeIndex, NIL};
use crate::error::Result;
use crate::util::to_i64;

/// A node's own contribution: `(alloc, logical, files)`.
fn own(node: &Node, logical: u64) -> (u64, u64, u64) {
    if node.counts() {
        (node.size_alloc, logical, 1)
    } else {
        (0, 0, 0)
    }
}

/// Fill every node's subtree totals. Called by [`Arena::load`].
pub fn compute(arena: &mut Arena) {
    let count = arena.nodes.len();
    arena.total_logical.clear();
    arena.total_logical.resize(count, 0);
    arena.file_count.clear();
    arena.file_count.resize(count, 0);

    for index in 0..count {
        let (alloc, logical, files) = own(&arena.nodes[index], arena.size_logical[index]);
        arena.nodes[index].total_alloc = alloc;
        arena.total_logical[index] = logical;
        arena.file_count[index] = files;
    }

    for index in (1..count).rev() {
        add_to_parent(arena, index);
    }
}

/// Add a finished node's totals to its parent.
fn add_to_parent(arena: &mut Arena, index: usize) {
    let parent = arena.nodes[index].parent;
    if parent == NIL {
        return;
    }
    let parent = parent as usize;
    let (alloc, logical, files) = (
        arena.nodes[index].total_alloc,
        arena.total_logical[index],
        arena.file_count[index],
    );
    arena.nodes[parent].total_alloc = arena.nodes[parent].total_alloc.saturating_add(alloc);
    arena.total_logical[parent] = arena.total_logical[parent].saturating_add(logical);
    arena.file_count[parent] = arena.file_count[parent].saturating_add(files);
}

/// Every node in the subtree rooted at `top`, `top` included, in descending
/// index order — the order that makes the reverse pass valid for a subtree.
fn subtree(arena: &Arena, top: NodeIndex) -> Vec<NodeIndex> {
    let mut out = Vec::new();
    let mut stack = vec![top];
    while let Some(index) = stack.pop() {
        out.push(index);
        stack.extend(arena.children(index));
    }
    out.sort_unstable_by(|left, right| right.cmp(left));
    out
}

/// Recompute the subtrees under `dirty` and propagate the change to their
/// ancestors.
///
/// Returns every directory whose totals may have changed — the dirty subtrees'
/// directories and all their ancestors — sorted, for [`write_dirs`]. Indices out
/// of range are ignored. Nested dirty directories are fine: the inner one is
/// already correct after the outer one's pass, so its own change is zero.
pub fn recompute_dirty(arena: &mut Arena, dirty: &[NodeIndex]) -> Vec<NodeIndex> {
    let mut touched = BTreeSet::new();

    for &top in dirty {
        if arena.node(top).is_none() {
            continue;
        }
        let top_index = top as usize;
        let before = (
            arena.nodes[top_index].total_alloc,
            arena.total_logical[top_index],
            arena.file_count[top_index],
        );

        let nodes = subtree(arena, top);
        for &index in &nodes {
            let index = index as usize;
            let (alloc, logical, files) = own(&arena.nodes[index], arena.size_logical[index]);
            arena.nodes[index].total_alloc = alloc;
            arena.total_logical[index] = logical;
            arena.file_count[index] = files;
        }
        for &index in &nodes {
            if index != top {
                add_to_parent(arena, index as usize);
            }
            if arena.nodes[index as usize].kind() == EntryKind::Dir {
                touched.insert(index);
            }
        }

        let after = (
            arena.nodes[top_index].total_alloc,
            arena.total_logical[top_index],
            arena.file_count[top_index],
        );
        let ancestors: Vec<NodeIndex> = arena.ancestors(top).collect();
        for ancestor in ancestors {
            let index = ancestor as usize;
            // An ancestor's total always includes `before`, so subtracting after
            // adding cannot underflow.
            arena.nodes[index].total_alloc = (arena.nodes[index].total_alloc + after.0) - before.0;
            arena.total_logical[index] = (arena.total_logical[index] + after.1) - before.1;
            arena.file_count[index] = (arena.file_count[index] + after.2) - before.2;
            touched.insert(ancestor);
        }
    }

    touched.into_iter().collect()
}

const INSERT_SQL: &str = "INSERT OR REPLACE INTO dir_rollup \
     (scan_id, dir_id, total_alloc, total_logical, file_count) VALUES (?1, ?2, ?3, ?4, ?5)";

fn insert_dir(
    statement: &mut rusqlite::CachedStatement<'_>,
    arena: &Arena,
    index: NodeIndex,
) -> Result<()> {
    let Some(entry_id) = arena.entry_id(index) else {
        return Ok(());
    };
    let position = index as usize;
    statement.execute(params![
        arena.scan_id(),
        entry_id,
        to_i64(arena.nodes[position].total_alloc),
        to_i64(arena.total_logical[position]),
        to_i64(arena.file_count[position]),
    ])?;
    Ok(())
}

/// Replace the scan's `dir_rollup` rows with the arena's, in one transaction.
///
/// Every directory gets a row, the root included. 1M directories is one
/// transaction of 1M prepared inserts — PLAN's "~2–4 seconds".
pub fn write(conn: &Connection, arena: &Arena) -> Result<()> {
    let transaction = conn.unchecked_transaction()?;
    transaction.execute(
        "DELETE FROM dir_rollup WHERE scan_id = ?1",
        params![arena.scan_id()],
    )?;
    {
        let mut statement = transaction.prepare_cached(INSERT_SQL)?;
        for (index, node) in arena.nodes().iter().enumerate() {
            if node.kind() == EntryKind::Dir {
                insert_dir(&mut statement, arena, index as NodeIndex)?;
            }
        }
    }
    transaction.commit()?;
    Ok(())
}

/// Upsert just the given directories' rows, e.g. the result of
/// [`recompute_dirty`]. Non-directories are skipped.
pub fn write_dirs(conn: &Connection, arena: &Arena, dirs: &[NodeIndex]) -> Result<()> {
    let transaction = conn.unchecked_transaction()?;
    {
        let mut statement = transaction.prepare_cached(INSERT_SQL)?;
        for &index in dirs {
            if arena
                .node(index)
                .is_some_and(|node| node.kind() == EntryKind::Dir)
            {
                insert_dir(&mut statement, arena, index)?;
            }
        }
    }
    transaction.commit()?;
    Ok(())
}
