//! Attribute bitmaps for `getattrlistbulk(2)`.
//!
//! The numeric values are from `<sys/attr.h>` on macOS. They are duplicated here
//! instead of re-exported from `libc` so that [`crate::parse`] — the part with the
//! real logic — compiles and is unit-tested on every CI runner, including Windows.
//! A macOS-only test in this module asserts that every constant still matches
//! `libc`, so a divergence in the headers is caught instead of becoming a silently
//! misaligned parser.
//!
//! # Packed invalid attributes
//!
//! The scanner always requests [`FSOPT_PACK_INVAL_ATTRS`]. With that option every
//! requested attribute of a *group that applies to the object* is written to the
//! buffer, zeroed when invalid; `ATTR_CMN_RETURNED_ATTRS` then says which values
//! are real. Groups that do not apply (for example the file group on a directory)
//! are omitted entirely, so the parser must consult the returned bitmaps before
//! consuming a group.

/// A bitmap of attributes within one group.
pub type AttrGroup = u32;

/// Attribute group in `struct attrlist`.
///
/// Volume and fork attributes are never requested by [`Request::storage`], so the
/// parser only needs these three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// Common attributes, present for every object type.
    Common,
    /// Directory attributes, present for directories.
    Directory,
    /// File attributes, present for non-directories.
    File,
}

// --- Common attributes ------------------------------------------------------

/// Returned bitmap, always first in the common group.
pub const ATTR_CMN_RETURNED_ATTRS: AttrGroup = 0x8000_0000;
/// Name as an `attrreference_t`, NUL terminated.
pub const ATTR_CMN_NAME: AttrGroup = 0x0000_0001;
/// Device id (`dev_t`, 4 bytes).
pub const ATTR_CMN_DEVID: AttrGroup = 0x0000_0002;
/// Object type (`fsobj_type_t`, 4 bytes).
pub const ATTR_CMN_OBJTYPE: AttrGroup = 0x0000_0008;
/// Creation time (`struct timespec`, 16 bytes).
pub const ATTR_CMN_CRTIME: AttrGroup = 0x0000_0200;
/// Modification time (`struct timespec`, 16 bytes).
pub const ATTR_CMN_MODTIME: AttrGroup = 0x0000_0400;
/// Access time (`struct timespec`, 16 bytes).
pub const ATTR_CMN_ACCTIME: AttrGroup = 0x0000_1000;
/// Raw `st_flags` (4 bytes).
pub const ATTR_CMN_FLAGS: AttrGroup = 0x0004_0000;
/// Inode number (`u64`, 4-byte aligned in the buffer).
pub const ATTR_CMN_FILEID: AttrGroup = 0x0200_0000;

// --- Directory attributes ---------------------------------------------------

/// Number of entries in the directory (4 bytes).
pub const ATTR_DIR_ENTRYCOUNT: AttrGroup = 0x0000_0002;

// --- File attributes --------------------------------------------------------

/// Hard-link count (4 bytes).
pub const ATTR_FILE_LINKCOUNT: AttrGroup = 0x0000_0001;
/// Apparent size (`off_t`, 8 bytes).
pub const ATTR_FILE_TOTALSIZE: AttrGroup = 0x0000_0002;
/// Allocated size (`off_t`, 8 bytes). The number the app displays.
pub const ATTR_FILE_ALLOCSIZE: AttrGroup = 0x0000_0004;
/// Preferred I/O block size (4 bytes).
pub const ATTR_FILE_IOBLOCKSIZE: AttrGroup = 0x0000_0008;

// --- Options ----------------------------------------------------------------

/// Do not follow symlinks. Symlinks are reported, never traversed.
pub const FSOPT_NOFOLLOW: u64 = 0x0000_0001;
/// Write every requested attribute, zeroed when invalid. Requires
/// [`ATTR_CMN_RETURNED_ATTRS`].
pub const FSOPT_PACK_INVAL_ATTRS: u64 = 0x0000_0008;

/// Which attributes the scanner asks for, and the flags to ask with.
///
/// Volume and fork groups are intentionally empty: `getattrlistbulk` rejects a
/// request containing volume attributes, and the scanner has no use for forks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Request {
    /// Common group bitmap.
    pub common: AttrGroup,
    /// Directory group bitmap.
    pub dir: AttrGroup,
    /// File group bitmap.
    pub file: AttrGroup,
}

impl Request {
    /// The one request the scanner uses.
    pub const STORAGE: Self = Self {
        common: ATTR_CMN_RETURNED_ATTRS
            | ATTR_CMN_NAME
            | ATTR_CMN_DEVID
            | ATTR_CMN_OBJTYPE
            | ATTR_CMN_CRTIME
            | ATTR_CMN_MODTIME
            | ATTR_CMN_ACCTIME
            | ATTR_CMN_FLAGS
            | ATTR_CMN_FILEID,
        dir: ATTR_DIR_ENTRYCOUNT,
        file: ATTR_FILE_LINKCOUNT
            | ATTR_FILE_TOTALSIZE
            | ATTR_FILE_ALLOCSIZE
            | ATTR_FILE_IOBLOCKSIZE,
    };

    /// The one request the scanner uses.
    pub const fn storage() -> Self {
        Self::STORAGE
    }

    /// Whether the request asks for the returned bitmap. Without it the parser
    /// cannot tell which groups were written and must reject the buffer.
    pub const fn has_returned_attrs(self) -> bool {
        self.common & ATTR_CMN_RETURNED_ATTRS != 0
    }

    /// Flags passed to `getattrlistbulk(2)`.
    pub const fn options() -> u64 {
        FSOPT_PACK_INVAL_ATTRS | FSOPT_NOFOLLOW
    }
}

/// The `ATTR_CMN_RETURNED_ATTRS` value: which requested attributes are valid.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AttrSet {
    /// Valid common attributes.
    pub common: AttrGroup,
    /// Valid directory attributes.
    pub dir: AttrGroup,
    /// Valid file attributes.
    pub file: AttrGroup,
}

impl AttrSet {
    /// No attributes are valid.
    pub const EMPTY: Self = Self {
        common: 0,
        dir: 0,
        file: 0,
    };

    /// True when the object had none of the requested attributes.
    pub const fn is_empty(self) -> bool {
        self.common == 0 && self.dir == 0 && self.file == 0
    }

    /// Whether `bit` in `group` is valid.
    pub const fn has(self, group: Group, bit: AttrGroup) -> bool {
        match group {
            Group::Common => self.common & bit != 0,
            Group::Directory => self.dir & bit != 0,
            Group::File => self.file & bit != 0,
        }
    }

    /// Whether a common attribute is valid.
    pub const fn common_has(self, bit: AttrGroup) -> bool {
        self.has(Group::Common, bit)
    }

    /// Whether a directory attribute is valid.
    pub const fn dir_has(self, bit: AttrGroup) -> bool {
        self.has(Group::Directory, bit)
    }

    /// Whether a file attribute is valid.
    pub const fn file_has(self, bit: AttrGroup) -> bool {
        self.has(Group::File, bit)
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn the_storage_request_never_follows_symlinks() {
        // #9's guarantee that a symlink loop cannot be walked depends on this flag,
        // so pin it here instead of trusting the request to stay unchanged.
        assert_ne!(Request::options() & FSOPT_NOFOLLOW, 0);
        assert_ne!(Request::options() & FSOPT_PACK_INVAL_ATTRS, 0);
        assert!(Request::storage().has_returned_attrs());
    }

    #[test]
    fn constants_match_the_system_headers() {
        assert_eq!(ATTR_CMN_RETURNED_ATTRS, libc::ATTR_CMN_RETURNED_ATTRS);
        assert_eq!(ATTR_CMN_NAME, libc::ATTR_CMN_NAME);
        assert_eq!(ATTR_CMN_DEVID, libc::ATTR_CMN_DEVID);
        assert_eq!(ATTR_CMN_OBJTYPE, libc::ATTR_CMN_OBJTYPE);
        assert_eq!(ATTR_CMN_CRTIME, libc::ATTR_CMN_CRTIME);
        assert_eq!(ATTR_CMN_MODTIME, libc::ATTR_CMN_MODTIME);
        assert_eq!(ATTR_CMN_ACCTIME, libc::ATTR_CMN_ACCTIME);
        assert_eq!(ATTR_CMN_FLAGS, libc::ATTR_CMN_FLAGS);
        assert_eq!(ATTR_CMN_FILEID, libc::ATTR_CMN_FILEID);
        assert_eq!(ATTR_DIR_ENTRYCOUNT, libc::ATTR_DIR_ENTRYCOUNT);
        assert_eq!(ATTR_FILE_LINKCOUNT, libc::ATTR_FILE_LINKCOUNT);
        assert_eq!(ATTR_FILE_TOTALSIZE, libc::ATTR_FILE_TOTALSIZE);
        assert_eq!(ATTR_FILE_ALLOCSIZE, libc::ATTR_FILE_ALLOCSIZE);
        assert_eq!(ATTR_FILE_IOBLOCKSIZE, libc::ATTR_FILE_IOBLOCKSIZE);
        assert_eq!(FSOPT_NOFOLLOW, u64::from(libc::FSOPT_NOFOLLOW));
        assert_eq!(
            FSOPT_PACK_INVAL_ATTRS,
            u64::from(libc::FSOPT_PACK_INVAL_ATTRS)
        );
    }
}
