//! Hard-link accounting.
//!
//! A hard link is another name for the same inode, and the blocks belong to the
//! inode, not to any one name. `du` counts the inode once; so does this module.
//!
//! Only regular files with `nlink > 1` are tracked. That keeps the set tiny on a
//! normal disk (a handful of entries), and it avoids treating the `nlink == 2`
//! that every directory carries — its own `.` entry plus one child — as a
//! duplicate.
//!
//! The rule is "attribute to the first path seen": the first name accepted for an
//! inode owns the bytes, and every later name is marked
//! [`Entry::hardlink_dup`](storagelens_core::Entry::hardlink_dup). Rows are never
//! dropped, because every name is a real path the user may browse to or have
//! referenced from an app bundle.

use std::collections::HashSet;

use storagelens_core::{Entry, EntryKind};

/// Tracks which `(device, inode)` pairs have already been counted.
#[derive(Debug, Default)]
pub(crate) struct Dedupe {
    seen: HashSet<(u32, u64)>,
}

impl Dedupe {
    /// An empty tracker.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Whether `entry` is an additional name for an already-counted inode.
    ///
    /// Calling this is what records the inode, so it must be called once per entry,
    /// in acceptance order. It is deliberately cheap for the common case: a file
    /// with a single link returns before touching the set.
    pub(crate) fn is_duplicate(&mut self, entry: &Entry) -> bool {
        if entry.kind != EntryKind::File || entry.nlink <= 1 {
            return false;
        }

        !self.seen.insert((entry.dev, entry.file_id))
    }

    /// Number of distinct multi-link inodes counted so far.
    ///
    /// Exposed for tests and diagnostics; the scan result keeps only totals.
    #[cfg(test)]
    pub(crate) fn tracked(&self) -> usize {
        self.seen.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, dev: u32, file_id: u64, nlink: u32) -> Entry {
        Entry {
            parent: 0,
            name: name.into(),
            kind: EntryKind::File,
            size_alloc: 4_096,
            size_logical: 4_096,
            dev,
            file_id,
            nlink,
            attrs: 0,
            created: 0,
            modified: 0,
            accessed: 0,
            hardlink_dup: false,
        }
    }

    #[test]
    fn the_first_name_owns_the_inode() {
        let mut dedupe = Dedupe::new();
        let first = entry("a.txt", 7, 42, 3);
        let second = entry("b.txt", 7, 42, 3);
        let third = entry("c.txt", 7, 42, 3);

        assert!(!dedupe.is_duplicate(&first));
        assert!(dedupe.is_duplicate(&second));
        assert!(dedupe.is_duplicate(&third));
        assert_eq!(dedupe.tracked(), 1);
    }

    #[test]
    fn single_link_files_are_never_tracked() {
        let mut dedupe = Dedupe::new();

        assert!(!dedupe.is_duplicate(&entry("a", 7, 1, 1)));
        assert!(!dedupe.is_duplicate(&entry("b", 7, 2, 1)));
        assert_eq!(dedupe.tracked(), 0);
    }

    #[test]
    fn the_same_inode_on_another_device_is_a_different_file() {
        let mut dedupe = Dedupe::new();

        assert!(!dedupe.is_duplicate(&entry("a", 7, 42, 2)));
        assert!(!dedupe.is_duplicate(&entry("b", 8, 42, 2)));
        assert_eq!(dedupe.tracked(), 2);
    }

    #[test]
    fn directories_are_not_deduped() {
        let mut dedupe = Dedupe::new();
        let mut dir = entry("Library", 7, 99, 12);
        dir.kind = EntryKind::Dir;

        assert!(!dedupe.is_duplicate(&dir));
        assert!(!dedupe.is_duplicate(&dir));
        assert_eq!(dedupe.tracked(), 0);
    }

    #[test]
    fn a_file_with_no_returned_link_count_is_not_deduped() {
        let mut dedupe = Dedupe::new();

        // `nlink == 0` means the file group was absent, not "no links".
        assert!(!dedupe.is_duplicate(&entry("a", 7, 42, 0)));
        assert!(!dedupe.is_duplicate(&entry("b", 7, 42, 0)));
        assert_eq!(dedupe.tracked(), 0);
    }
}
