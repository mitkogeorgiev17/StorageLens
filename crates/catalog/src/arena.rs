//! The in-memory tree the treemap lays out against (PLAN §5).
//!
//! SQLite is the record; the arena is the working copy. After a scan, the whole
//! tree is loaded once into one contiguous `Vec<Node>` plus one string blob, so
//! zooming, hit-testing, and layout never touch the database.
//!
//! # Layout
//!
//! A [`Node`] is 40 bytes: three links (`parent`, `first_child`,
//! `next_sibling`), an offset into the shared name blob, the node's own allocated
//! size, its subtree total, and three bytes of kind / category / flags. 1M nodes
//! is 40 MB of nodes plus the names, about 70 MB in total — PLAN's estimate.
//!
//! # Indices and SQL ids
//!
//! The catalog writer assigns row ids in acceptance order, and scanners accept in
//! pre-order, so one scan's ids are contiguous and every parent's id is below its
//! children's. The arena relies on both: node `i` is row `base_id + i`, and a
//! single reverse pass is a valid post-order (see `rollup.rs`). [`Arena::load`]
//! verifies the two properties instead of assuming them, so a damaged catalog is
//! an error, not a wrong tree.
//!
//! # Child order
//!
//! Children are linked by prepending, so a directory lists its children in
//! reverse id order. Nothing depends on the order — the treemap sorts by size —
//! and prepending needs no per-node "last child" scratch space.

use rusqlite::{params, Connection};
use storagelens_core::{Category, EntryKind};

use crate::error::{CatalogError, Result};
use crate::util::to_u64;

/// Index of a node in the arena.
pub type NodeIndex = u32;

/// "No node": the root's parent, a leaf's first child, a last sibling.
pub const NIL: NodeIndex = u32::MAX;

/// [`Node::flags`] bit: an extra name for an already-counted hard link. The row
/// is kept, but its bytes are not counted (AGENTS.md invariant 2).
pub const FLAG_HARDLINK_DUP: u8 = 1 << 0;

/// One entry of the tree. See the module docs for the layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct Node {
    /// Parent index, [`NIL`] for the root.
    pub parent: NodeIndex,
    /// First child, [`NIL`] for a leaf.
    pub first_child: NodeIndex,
    /// Next sibling, [`NIL`] for the last child.
    pub next_sibling: NodeIndex,
    /// Start of this node's name in the blob. It ends where the next node's
    /// name starts.
    pub name_offset: u32,
    /// The node's own allocated bytes, as scanned.
    pub size_alloc: u64,
    /// Allocated bytes of the whole subtree, as counted (regular files only,
    /// hard-link duplicates excluded). Filled by the rollup; for a counted file
    /// it equals `size_alloc`.
    pub total_alloc: u64,
    /// [`EntryKind`] as stored (`EntryKind::as_u8`).
    pub kind: u8,
    /// Position of the category in [`Category::ALL`].
    pub category: u8,
    /// `FLAG_*` bits.
    pub flags: u8,
    _reserved: [u8; 5],
}

// The arena's memory budget is built on this size; keep it from drifting.
const _: () = assert!(std::mem::size_of::<Node>() == 40);

impl Node {
    /// The entry kind. Unknown stored values read as [`EntryKind::Other`].
    pub fn kind(&self) -> EntryKind {
        EntryKind::from_u8(self.kind).unwrap_or(EntryKind::Other)
    }

    /// The node's category.
    pub fn category(&self) -> Category {
        Category::ALL
            .get(usize::from(self.category))
            .copied()
            .unwrap_or(Category::Other)
    }

    /// Whether this is an extra name for an already-counted hard link.
    pub fn is_hardlink_dup(&self) -> bool {
        self.flags & FLAG_HARDLINK_DUP != 0
    }

    /// Whether this node's own bytes count toward totals: a regular file that is
    /// not a hard-link duplicate. The same rule the scanners use for
    /// `ScanResult`, so arena and scan totals agree.
    pub fn counts(&self) -> bool {
        self.kind() == EntryKind::File && !self.is_hardlink_dup()
    }
}

/// The whole tree of one scan.
#[derive(Debug, Clone)]
pub struct Arena {
    scan_id: i64,
    /// SQL id of node 0.
    base_id: i64,
    pub(crate) nodes: Vec<Node>,
    /// Logical bytes per node, own size. Kept beside the nodes rather than in
    /// them: only the rollup reads it, and the hot layout loop should not pay
    /// for it in cache lines.
    pub(crate) size_logical: Vec<u64>,
    /// Subtree logical bytes, filled by the rollup. Same counting rule as
    /// [`Node::total_alloc`].
    pub(crate) total_logical: Vec<u64>,
    /// Subtree count of counted files, filled by the rollup.
    pub(crate) file_count: Vec<u64>,
    names: String,
}

fn category_index(category: Category) -> u8 {
    Category::ALL
        .iter()
        .position(|candidate| *candidate == category)
        .unwrap_or(Category::ALL.len() - 1) as u8
}

impl Arena {
    /// Load the tree of `scan_id` from the catalog.
    ///
    /// One sequential `ORDER BY id` read; no per-node query. Fails with
    /// [`CatalogError::Corrupt`] if the scan has no rows, if its ids are not
    /// contiguous, or if a parent does not precede its child.
    pub fn load(conn: &Connection, scan_id: i64) -> Result<Self> {
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM entries WHERE scan_id = ?1",
            params![scan_id],
            |row| row.get(0),
        )?;
        let count = usize::try_from(count).unwrap_or(0);
        if count == 0 {
            return Err(CatalogError::Corrupt(format!(
                "scan {scan_id} has no entries"
            )));
        }
        if count >= NIL as usize {
            return Err(CatalogError::Corrupt(format!(
                "scan {scan_id} has {count} entries, more than an arena can index"
            )));
        }

        let mut arena = Self {
            scan_id,
            base_id: 0,
            nodes: Vec::with_capacity(count),
            size_logical: Vec::with_capacity(count),
            total_logical: Vec::new(),
            file_count: Vec::new(),
            names: String::with_capacity(count * 16),
        };

        let mut statement = conn.prepare(
            "SELECT id, parent_id, name, kind, size_alloc, size_logical, category, hardlink_dup \
             FROM entries WHERE scan_id = ?1 ORDER BY id",
        )?;
        let mut rows = statement.query(params![scan_id])?;
        while let Some(row) = rows.next()? {
            let id: i64 = row.get(0)?;
            let parent_id: i64 = row.get(1)?;
            let name: String = row.get(2)?;
            let kind: i64 = row.get(3)?;
            let size_alloc: i64 = row.get(4)?;
            let size_logical: i64 = row.get(5)?;
            let category: Option<String> = row.get(6)?;
            let hardlink_dup: i64 = row.get(7)?;

            arena.push_row(
                id,
                parent_id,
                &name,
                kind,
                to_u64(size_alloc),
                to_u64(size_logical),
                category.as_deref(),
                hardlink_dup != 0,
            )?;
        }

        crate::rollup::compute(&mut arena);
        Ok(arena)
    }

    #[allow(clippy::too_many_arguments)]
    fn push_row(
        &mut self,
        id: i64,
        parent_id: i64,
        name: &str,
        kind: i64,
        size_alloc: u64,
        size_logical: u64,
        category: Option<&str>,
        hardlink_dup: bool,
    ) -> Result<()> {
        let index = self.nodes.len();
        if index == 0 {
            if parent_id != 0 {
                return Err(CatalogError::Corrupt(format!(
                    "scan {} starts at row {id} with parent {parent_id}, not a root",
                    self.scan_id
                )));
            }
            self.base_id = id;
        } else if id != self.base_id + index as i64 {
            return Err(CatalogError::Corrupt(format!(
                "scan {} ids are not contiguous: expected {}, found {id}",
                self.scan_id,
                self.base_id + index as i64
            )));
        }

        let parent = if index == 0 {
            NIL
        } else {
            let parent = parent_id - self.base_id;
            if parent < 0 || parent >= index as i64 {
                return Err(CatalogError::Corrupt(format!(
                    "row {id} has parent {parent_id}, which does not precede it in scan {}",
                    self.scan_id
                )));
            }
            parent as NodeIndex
        };

        let name_offset = u32::try_from(self.names.len())
            .map_err(|_| CatalogError::Corrupt("names exceed the arena's 4 GiB blob".to_owned()))?;
        self.names.push_str(name);

        let category = category
            .and_then(Category::from_name)
            .unwrap_or(Category::Other);
        let kind = u8::try_from(kind).unwrap_or(0);

        let index = index as NodeIndex;
        let mut node = Node {
            parent,
            first_child: NIL,
            next_sibling: NIL,
            name_offset,
            size_alloc,
            total_alloc: 0,
            kind,
            category: category_index(category),
            flags: if hardlink_dup { FLAG_HARDLINK_DUP } else { 0 },
            _reserved: [0; 5],
        };
        if parent != NIL {
            let parent_node = &mut self.nodes[parent as usize];
            node.next_sibling = parent_node.first_child;
            parent_node.first_child = index;
        }
        self.nodes.push(node);
        self.size_logical.push(size_logical);
        Ok(())
    }

    /// The scan this tree belongs to.
    pub fn scan_id(&self) -> i64 {
        self.scan_id
    }

    /// Number of nodes, root included.
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Never true for a loaded arena: a scan always has its root.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// The root, always node 0.
    pub const fn root(&self) -> NodeIndex {
        0
    }

    /// A node by index.
    pub fn node(&self, index: NodeIndex) -> Option<&Node> {
        self.nodes.get(index as usize)
    }

    /// All nodes, in SQL id order.
    pub fn nodes(&self) -> &[Node] {
        &self.nodes
    }

    /// A node's name.
    pub fn name(&self, index: NodeIndex) -> Option<&str> {
        let start = self.node(index)?.name_offset as usize;
        let end = self
            .nodes
            .get(index as usize + 1)
            .map_or(self.names.len(), |next| next.name_offset as usize);
        self.names.get(start..end)
    }

    /// A node's own logical size.
    pub fn size_logical(&self, index: NodeIndex) -> Option<u64> {
        self.size_logical.get(index as usize).copied()
    }

    /// Subtree logical bytes of a node, after the rollup.
    pub fn total_logical(&self, index: NodeIndex) -> Option<u64> {
        self.total_logical.get(index as usize).copied()
    }

    /// Subtree count of counted regular files, after the rollup.
    pub fn file_count(&self, index: NodeIndex) -> Option<u64> {
        self.file_count.get(index as usize).copied()
    }

    /// The SQL row id of a node.
    pub fn entry_id(&self, index: NodeIndex) -> Option<i64> {
        ((index as usize) < self.nodes.len()).then(|| self.base_id + i64::from(index))
    }

    /// The node for a SQL row id of this scan.
    pub fn index_of(&self, entry_id: i64) -> Option<NodeIndex> {
        let index = entry_id.checked_sub(self.base_id)?;
        (0..self.nodes.len() as i64)
            .contains(&index)
            .then_some(index as NodeIndex)
    }

    /// Whether `index` is `top` or inside its subtree.
    ///
    /// Walks up from `index` and stops as soon as an ancestor's index drops
    /// below `top`: parents always precede their children (see the module
    /// docs), so nothing above that point can be `top`.
    pub fn is_within(&self, index: NodeIndex, top: NodeIndex) -> bool {
        let mut current = index;
        while current != NIL && current > top {
            current = self.node(current).map_or(NIL, |node| node.parent);
        }
        current == top
    }

    /// Children of a node, in arena link order (see the module docs).
    pub fn children(&self, index: NodeIndex) -> Children<'_> {
        Children {
            arena: self,
            next: self.node(index).map_or(NIL, |node| node.first_child),
        }
    }

    /// Ancestors of a node, nearest first, root last. Excludes the node itself.
    pub fn ancestors(&self, index: NodeIndex) -> Ancestors<'_> {
        Ancestors {
            arena: self,
            next: self.node(index).map_or(NIL, |node| node.parent),
        }
    }

    /// Path components from the root's child down to `index`, for breadcrumbs.
    pub fn path_names(&self, index: NodeIndex) -> Vec<&str> {
        let mut names: Vec<&str> = std::iter::once(index)
            .chain(self.ancestors(index))
            .filter(|&ancestor| ancestor != self.root())
            .filter_map(|ancestor| self.name(ancestor))
            .collect();
        names.reverse();
        names
    }

    /// Change a file's sizes in place, e.g. after an incremental refresh. The
    /// caller then recomputes the rollup for the parent with
    /// [`crate::rollup::recompute_dirty`].
    pub fn set_sizes(&mut self, index: NodeIndex, size_alloc: u64, size_logical: u64) -> bool {
        match (
            self.nodes.get_mut(index as usize),
            self.size_logical.get_mut(index as usize),
        ) {
            (Some(node), Some(logical)) => {
                node.size_alloc = size_alloc;
                *logical = size_logical;
                true
            }
            _ => false,
        }
    }

    /// Bytes the arena holds on the heap: nodes, the per-node size and rollup
    /// arrays, and names.
    pub fn heap_bytes(&self) -> usize {
        self.nodes.capacity() * std::mem::size_of::<Node>()
            + (self.size_logical.capacity()
                + self.total_logical.capacity()
                + self.file_count.capacity())
                * std::mem::size_of::<u64>()
            + self.names.capacity()
    }
}

/// Iterator over a node's children.
pub struct Children<'a> {
    arena: &'a Arena,
    next: NodeIndex,
}

impl Iterator for Children<'_> {
    type Item = NodeIndex;

    fn next(&mut self) -> Option<NodeIndex> {
        let current = self.next;
        let node = self.arena.node(current)?;
        self.next = node.next_sibling;
        Some(current)
    }
}

/// Iterator over a node's ancestors.
pub struct Ancestors<'a> {
    arena: &'a Arena,
    next: NodeIndex,
}

impl Iterator for Ancestors<'_> {
    type Item = NodeIndex;

    fn next(&mut self) -> Option<NodeIndex> {
        let current = self.next;
        let node = self.arena.node(current)?;
        self.next = node.parent;
        Some(current)
    }
}
