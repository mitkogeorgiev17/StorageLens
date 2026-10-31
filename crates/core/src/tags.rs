//! Property tags as a `u64` bitmask.
//!
//! A file has one *primary type* (the `category` column) but many *properties*:
//! "a 4 GB `.mp4` in Downloads, untouched for two years" is Video + Huge + Stale at
//! once. Tags carry the second axis. See PLAN §9.2.
//!
//! Why a bitmask and not SQL columns: 1M files × 8 bytes = 8 MB in the arena, and
//! filtering is a single `AND`. A `WHERE tags & ?` predicate cannot use a B-tree, so
//! the primary type stays in its own indexed column.

use std::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign};

use serde::{Deserialize, Serialize};

use crate::categories::Category;

/// A set of property bits. Empty is a valid set.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Tags(u64);

impl Tags {
    /// No tags.
    pub const NONE: Self = Self(0);

    // Type-derived bits (low half, mirror the primary category).
    /// Image.
    pub const IMAGE: Self = Self(1 << 0);
    /// Video.
    pub const VIDEO: Self = Self(1 << 1);
    /// Audio.
    pub const AUDIO: Self = Self(1 << 2);
    /// Document.
    pub const DOCUMENT: Self = Self(1 << 3);
    /// Archive.
    pub const ARCHIVE: Self = Self(1 << 4);
    /// Source code or configuration.
    pub const CODE: Self = Self(1 << 5);
    /// Application or executable.
    pub const APP: Self = Self(1 << 6);

    // Path- or analysis-derived bits (high half, bits 32–40).
    /// Lives in a cache directory.
    pub const CACHE: Self = Self(1 << 32);
    /// Build output: `target/`, `node_modules/`, DerivedData, …
    pub const DEV_ARTIFACT: Self = Self(1 << 33);
    /// Downloaded installer or disk image.
    pub const INSTALLER: Self = Self(1 << 34);
    /// Backup: Time Machine, iOS backups, `*.bak`.
    pub const BACKUP: Self = Self(1 << 35);
    /// Belongs to the operating system. Never deletable.
    pub const SYSTEM: Self = Self(1 << 36);
    /// Support data whose owning app is gone.
    pub const ORPHAN: Self = Self(1 << 37);
    /// Large enough to be worth surfacing.
    pub const HUGE: Self = Self(1 << 38);
    /// Not modified within the staleness window.
    pub const STALE: Self = Self(1 << 39);
    /// Byte-identical to another file.
    pub const DUPLICATE: Self = Self(1 << 40);

    /// All bits set.
    pub const ALL: Self = Self(u64::MAX);

    /// The raw bitmask, for the `tags` column.
    pub const fn bits(self) -> u64 {
        self.0
    }

    /// Reconstruct from a stored bitmask.
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    /// True when no tags are set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// True when **every** bit in `tag` is present.
    ///
    /// `contains(Tags::NONE)` is trivially true, which is the standard mask
    /// semantics; check [`is_empty`](Self::is_empty) first if that matters.
    pub const fn contains(self, tag: Self) -> bool {
        self.0 & tag.0 == tag.0
    }

    /// True when **any** bit in `other` is present.
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    /// Union, without mutating either side.
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Set the bits in `tag`.
    pub fn insert(&mut self, tag: Self) {
        self.0 |= tag.0;
    }

    /// Clear the bits in `tag`.
    pub fn remove(&mut self, tag: Self) {
        self.0 &= !tag.0;
    }

    /// The type-derived tag for a primary category, if one exists.
    ///
    /// `Data`, `Fonts`, `3D`, `DiskImages`, and `Other` have no dedicated type bit;
    /// they are still fully represented by the `category` column.
    pub const fn from_category(category: Category) -> Self {
        match category {
            Category::Images => Self::IMAGE,
            Category::Video => Self::VIDEO,
            Category::Audio => Self::AUDIO,
            Category::Documents | Category::Spreadsheets | Category::Presentations => {
                Self::DOCUMENT
            }
            Category::Archives => Self::ARCHIVE,
            Category::Code => Self::CODE,
            Category::Apps => Self::APP,
            Category::Data
            | Category::Fonts
            | Category::ThreeD
            | Category::DiskImages
            | Category::Other => Self::NONE,
        }
    }
}

impl BitOr for Tags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self::Output {
        self.union(rhs)
    }
}

impl BitOrAssign for Tags {
    fn bitor_assign(&mut self, rhs: Self) {
        self.insert(rhs);
    }
}

impl BitAnd for Tags {
    type Output = Self;

    fn bitand(self, rhs: Self) -> Self::Output {
        Self(self.0 & rhs.0)
    }
}

impl BitAndAssign for Tags {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bits_round_trip() {
        let tags = Tags::VIDEO | Tags::STALE | Tags::HUGE;
        assert_eq!(Tags::from_bits(tags.bits()), tags);
        assert_eq!(tags.bits(), (1 << 1) | (1 << 38) | (1 << 39));
    }

    #[test]
    fn contains_and_intersects() {
        let tags = Tags::VIDEO | Tags::STALE;

        assert!(tags.contains(Tags::VIDEO));
        assert!(tags.contains(Tags::VIDEO | Tags::STALE));
        assert!(!tags.contains(Tags::VIDEO | Tags::HUGE));
        assert!(tags.intersects(Tags::STALE | Tags::HUGE));
        assert!(!tags.intersects(Tags::HUGE | Tags::CACHE));
    }

    #[test]
    fn insert_and_remove() {
        let mut tags = Tags::NONE;
        assert!(tags.is_empty());

        tags.insert(Tags::CACHE);
        tags |= Tags::STALE;
        assert!(tags.contains(Tags::CACHE | Tags::STALE));

        tags.remove(Tags::CACHE);
        assert!(!tags.intersects(Tags::CACHE));
        assert!(tags.contains(Tags::STALE));

        tags &= Tags::NONE;
        assert!(tags.is_empty());
    }

    #[test]
    fn union_does_not_mutate() {
        let a = Tags::IMAGE;
        let b = Tags::STALE;
        let both = a.union(b);

        assert_eq!(a, Tags::IMAGE);
        assert_eq!(b, Tags::STALE);
        assert_eq!(both, Tags::IMAGE | Tags::STALE);
    }

    #[test]
    fn empty_is_contained_but_does_not_intersect() {
        assert!(Tags::IMAGE.contains(Tags::NONE));
        assert!(!Tags::IMAGE.intersects(Tags::NONE));
        assert!(Tags::NONE.is_empty());
    }

    #[test]
    fn category_mapping_is_sane() {
        assert_eq!(Tags::from_category(Category::Images), Tags::IMAGE);
        assert_eq!(Tags::from_category(Category::Video), Tags::VIDEO);
        assert_eq!(Tags::from_category(Category::Audio), Tags::AUDIO);
        assert_eq!(Tags::from_category(Category::Documents), Tags::DOCUMENT);
        assert_eq!(Tags::from_category(Category::Spreadsheets), Tags::DOCUMENT);
        assert_eq!(Tags::from_category(Category::Presentations), Tags::DOCUMENT);
        assert_eq!(Tags::from_category(Category::Archives), Tags::ARCHIVE);
        assert_eq!(Tags::from_category(Category::Code), Tags::CODE);
        assert_eq!(Tags::from_category(Category::Apps), Tags::APP);
        assert_eq!(Tags::from_category(Category::Other), Tags::NONE);
    }

    #[test]
    fn type_and_property_bits_do_not_collide() {
        let type_bits = Tags::IMAGE
            | Tags::VIDEO
            | Tags::AUDIO
            | Tags::DOCUMENT
            | Tags::ARCHIVE
            | Tags::CODE
            | Tags::APP;
        let property_bits = Tags::CACHE
            | Tags::DEV_ARTIFACT
            | Tags::INSTALLER
            | Tags::BACKUP
            | Tags::SYSTEM
            | Tags::ORPHAN
            | Tags::HUGE
            | Tags::STALE
            | Tags::DUPLICATE;

        assert!(!type_bits.intersects(property_bits));
    }
}
