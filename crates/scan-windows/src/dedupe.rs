//! Hard-link accounting for the Windows scanner.
//!
//! A hard link is another name for the same MFT record, and the clusters belong
//! to the record, not to any one name. PLAN §8 dedupes on
//! `(volume serial, file index)`; the walker never leaves its root volume, so the
//! serial is constant and the set only stores the 64-bit id.
//!
//! The bulk listing has no link count, so unlike the macOS scanner (which only
//! tracks `nlink > 1`) every regular file's id is recorded. That costs about
//! 16 bytes per file — roughly 16 MB for a million files, well inside the
//! 400 MB scan budget — and one hash lookup.
//!
//! The rule is "attribute to the first path seen", matching macOS: the first name
//! accepted owns the bytes, and every later name keeps its row and true size but
//! is marked [`Entry::hardlink_dup`](storagelens_core::Entry::hardlink_dup).

use std::collections::HashSet;

use storagelens_core::{Entry, EntryKind};

/// Tracks which file ids have already been counted.
#[derive(Debug, Default)]
pub(crate) struct Dedupe {
    seen: HashSet<u64>,
}

impl Dedupe {
    /// An empty tracker.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Whether `entry` is an additional name for an already-counted file.
    ///
    /// Calling this records the id, so it must be called once per entry, in
    /// acceptance order. Ids `0` and `u64::MAX` mean "no id" and are never
    /// deduped.
    pub(crate) fn is_duplicate(&mut self, entry: &Entry) -> bool {
        if entry.kind != EntryKind::File || entry.file_id == 0 || entry.file_id == u64::MAX {
            return false;
        }
        !self.seen.insert(entry.file_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(id: u64) -> Entry {
        Entry {
            parent: 0,
            name: "f".to_owned(),
            kind: EntryKind::File,
            size_alloc: 4096,
            size_logical: 10,
            dev: 7,
            file_id: id,
            nlink: 1,
            attrs: 0,
            created: 0,
            modified: 0,
            accessed: 0,
            hardlink_dup: false,
        }
    }

    #[test]
    fn the_first_name_owns_the_file() {
        let mut dedupe = Dedupe::new();
        assert!(!dedupe.is_duplicate(&file(5)));
        assert!(dedupe.is_duplicate(&file(5)));
        assert!(dedupe.is_duplicate(&file(5)));
        assert!(!dedupe.is_duplicate(&file(6)));
    }

    #[test]
    fn directories_links_and_missing_ids_are_never_duplicates() {
        let mut dedupe = Dedupe::new();
        for kind in [EntryKind::Dir, EntryKind::Symlink] {
            let entry = Entry { kind, ..file(9) };
            assert!(!dedupe.is_duplicate(&entry));
            assert!(!dedupe.is_duplicate(&entry));
        }
        for id in [0, u64::MAX] {
            assert!(!dedupe.is_duplicate(&file(id)));
            assert!(!dedupe.is_duplicate(&file(id)));
        }
    }
}
