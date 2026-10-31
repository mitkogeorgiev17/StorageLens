//! Byte-exact parser for the `getattrlistbulk(2)` attribute buffer.
//!
//! The buffer is a sequence of records. Each record starts with a `u32` length that
//! includes the length itself, followed by the returned-attribute bitmap and then
//! the attribute values **in ascending bitmap order**, group by group (common, then
//! directory, then file; the scanner requests no volume or fork attributes).
//!
//! Two details are easy to get wrong and are central to this module:
//!
//! 1. `ATTR_CMN_RETURNED_ATTRS` is always written first. A group whose returned
//!    bitmap is zero is **omitted entirely** — a directory record has no file
//!    attributes at all, not zeroed ones. The parser therefore looks at the
//!    returned bitmap before consuming each group.
//! 2. The buffer is only 4-byte aligned. In practice `ATTR_CMN_FILEID` (a `u64`)
//!    arrives at an offset congruent to 4 mod 8. Every value here is decoded with
//!    `from_ne_bytes`; no reference to a `u64` or `timespec` is ever created.
//!
//! Every malformed buffer produces a [`ParseError`]; the parser never panics and
//! never reads outside the record it was handed.

use thiserror::Error;

use crate::attrlist::{
    AttrGroup, AttrSet, Group, Request, ATTR_CMN_ACCTIME, ATTR_CMN_CRTIME, ATTR_CMN_DEVID,
    ATTR_CMN_FILEID, ATTR_CMN_FLAGS, ATTR_CMN_MODTIME, ATTR_CMN_NAME, ATTR_CMN_OBJTYPE,
    ATTR_CMN_RETURNED_ATTRS, ATTR_DIR_ENTRYCOUNT, ATTR_FILE_ALLOCSIZE, ATTR_FILE_IOBLOCKSIZE,
    ATTR_FILE_LINKCOUNT, ATTR_FILE_TOTALSIZE,
};

/// Bytes of the per-record length prefix.
pub const RECORD_HEADER_BYTES: usize = 4;

/// Bytes of `struct attribute_set_t` (five bitmaps).
pub const RETURNED_ATTRS_BYTES: usize = 20;

/// Smallest possible record: the length and the returned bitmap.
pub const MIN_RECORD_BYTES: usize = RECORD_HEADER_BYTES + RETURNED_ATTRS_BYTES;

const TIMESPEC_BYTES: usize = 16;

/// Object type reported by `ATTR_CMN_OBJTYPE` (`fsobj_type_t`).
///
/// The values are the BSD `vtype` constants. `libc` does not expose them, so they
/// are defined here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjType {
    /// Regular file.
    Regular,
    /// Directory.
    Directory,
    /// Block device.
    BlockDevice,
    /// Character device.
    CharDevice,
    /// Symbolic link. Never followed by the scanner.
    Symlink,
    /// Unix domain socket.
    Socket,
    /// FIFO.
    Fifo,
    /// Bad inode.
    Bad,
    /// Whiteout.
    Whiteout,
    /// Complex object (for example a compressed bundle).
    Complex,
    /// A value this parser does not know. Kept so a newer filesystem cannot make
    /// the scan fail.
    Unknown(u32),
}

impl ObjType {
    /// Decode a raw `fsobj_type_t`.
    pub const fn from_raw(raw: u32) -> Self {
        match raw {
            1 => Self::Regular,
            2 => Self::Directory,
            3 => Self::BlockDevice,
            4 => Self::CharDevice,
            5 => Self::Symlink,
            6 => Self::Socket,
            7 => Self::Fifo,
            8 => Self::Bad,
            9 => Self::Whiteout,
            10 => Self::Complex,
            other => Self::Unknown(other),
        }
    }

    /// True for directories.
    pub const fn is_dir(self) -> bool {
        matches!(self, Self::Directory)
    }

    /// True for regular files.
    pub const fn is_file(self) -> bool {
        matches!(self, Self::Regular)
    }

    /// True for symlinks.
    pub const fn is_symlink(self) -> bool {
        matches!(self, Self::Symlink)
    }
}

impl Default for ObjType {
    fn default() -> Self {
        Self::Unknown(0)
    }
}

/// One parsed directory entry, before it becomes a [`storagelens_core::Entry`].
///
/// Directory records carry no file group on macOS, so `size_logical`,
/// `size_alloc`, and `nlink` stay `0` for them. The walker fills directory sizes
/// with a single `lstat` per directory when it needs the reference-equivalent
/// total.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RawEntry {
    /// Final path component, without a trailing NUL. Non-UTF-8 names are decoded
    /// lossily; macOS permits such names on some volumes, and dropping an entire
    /// directory because of one name would be worse than a replacement character.
    pub name: String,
    /// Object type.
    pub obj_type: ObjType,
    /// Device id, matching `stat(2)`'s `st_dev`.
    pub dev: u32,
    /// Inode number, matching `stat(2)`'s `st_ino`.
    pub file_id: u64,
    /// Raw `st_flags`.
    pub flags: u32,
    /// Creation time in Unix seconds.
    pub created: i64,
    /// Modification time in Unix seconds.
    pub modified: i64,
    /// Access time in Unix seconds.
    pub accessed: i64,
    /// Apparent size in bytes (`ATTR_FILE_TOTALSIZE`), clamped at zero.
    pub size_logical: u64,
    /// Allocated size in bytes (`ATTR_FILE_ALLOCSIZE`), clamped at zero.
    pub size_alloc: u64,
    /// Hard-link count. `0` when the file group was not returned.
    pub nlink: u32,
    /// Number of entries in the directory. `0` when not returned.
    pub dir_entry_count: u32,
    /// Preferred I/O block size. `0` when not returned.
    pub io_block_size: u32,
    /// Which of the requested attributes are valid for this entry.
    pub returned: AttrSet,
}

/// Everything that can go wrong while parsing a record.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParseError {
    /// A field or record runs past the end of the buffer.
    #[error("truncated data at offset {offset}: need {needed} bytes, {available} available")]
    Truncated {
        /// Absolute offset of the field or record.
        offset: usize,
        /// Bytes required.
        needed: usize,
        /// Bytes available.
        available: usize,
    },

    /// The record's length prefix cannot describe a record.
    #[error("record at offset {offset} has invalid length {length}")]
    InvalidLength {
        /// Absolute offset of the record.
        offset: usize,
        /// Value of the length prefix.
        length: usize,
    },

    /// A requested attribute has no known size, so the rest of the record cannot
    /// be located.
    #[error("attribute {bit:#010x} in group {group:?} is not supported by this parser")]
    UnsupportedAttribute {
        /// Group containing the attribute.
        group: Group,
        /// The single-bit mask.
        bit: AttrGroup,
    },

    /// The name reference does not point at `length` bytes inside the record.
    #[error("name reference at offset {offset} points outside the record (length {length})")]
    NameOutOfBounds {
        /// Absolute offset of the `attrreference_t`.
        offset: usize,
        /// Declared name length, including the NUL terminator.
        length: usize,
    },

    /// The name is not NUL terminated.
    #[error("name at offset {offset} is not NUL terminated")]
    NameNotTerminated {
        /// Absolute offset of the name bytes.
        offset: usize,
    },

    /// The request omitted `ATTR_CMN_RETURNED_ATTRS`, so group presence is
    /// unknowable.
    #[error("ATTR_CMN_RETURNED_ATTRS is required to parse the buffer")]
    MissingReturnedAttrs,
}

/// Parse one record starting at `*cursor`, advancing it past the record.
///
/// The caller normally passes a whole buffer and loops until the cursor reaches
/// the end. A record that declares a length beyond `buffer` is rejected.
pub fn parse_record(
    buffer: &[u8],
    cursor: &mut usize,
    request: &Request,
) -> Result<RawEntry, ParseError> {
    let start = *cursor;
    let available = buffer.len().saturating_sub(start);
    if available < RECORD_HEADER_BYTES {
        return Err(ParseError::Truncated {
            offset: start,
            needed: RECORD_HEADER_BYTES,
            available,
        });
    }

    let length = u32::from_ne_bytes([
        buffer[start],
        buffer[start + 1],
        buffer[start + 2],
        buffer[start + 3],
    ]) as usize;
    if length < MIN_RECORD_BYTES {
        return Err(ParseError::InvalidLength {
            offset: start,
            length,
        });
    }
    if length > available {
        return Err(ParseError::Truncated {
            offset: start,
            needed: length,
            available,
        });
    }
    if !request.has_returned_attrs() {
        return Err(ParseError::MissingReturnedAttrs);
    }

    let record = &buffer[start..start + length];
    let mut reader = Cursor::new(record, start);
    reader.position = RECORD_HEADER_BYTES;

    let returned_common = reader.u32()?;
    let _returned_volume = reader.u32()?;
    let returned = AttrSet {
        common: returned_common,
        dir: reader.u32()?,
        file: reader.u32()?,
    };
    let _returned_fork = reader.u32()?;

    let mut entry = RawEntry {
        returned,
        ..RawEntry::default()
    };

    // A zero group bitmap means the group is absent from the buffer, not zeroed.
    if returned.common != 0 {
        parse_common(&mut reader, request, &mut entry)?;
    }
    if returned.dir != 0 {
        parse_directory(&mut reader, request, &mut entry)?;
    }
    if returned.file != 0 {
        parse_file(&mut reader, request, &mut entry)?;
    }

    *cursor = start + length;
    Ok(entry)
}

/// Parse every record in `buffer`.
///
/// Convenience for tests and for callers that already hold a complete buffer; the
/// scanner uses [`parse_record`] directly so it can yield one entry at a time.
pub fn parse_batch(buffer: &[u8], request: &Request) -> Result<Vec<RawEntry>, ParseError> {
    let mut entries = Vec::new();
    let mut cursor = 0;
    while cursor < buffer.len() {
        entries.push(parse_record(buffer, &mut cursor, request)?);
    }
    Ok(entries)
}

fn parse_common(
    reader: &mut Cursor<'_>,
    request: &Request,
    entry: &mut RawEntry,
) -> Result<(), ParseError> {
    for index in 0..AttrGroup::BITS {
        let bit = 1 << index;
        if request.common & bit == 0 {
            continue;
        }
        match bit {
            // Already consumed before any group was read.
            ATTR_CMN_RETURNED_ATTRS => {}
            ATTR_CMN_NAME => {
                let reference_offset = reader.position;
                let data_offset = reader.i32()?;
                let data_length = reader.u32()? as usize;
                if data_length == 0 {
                    // FSOPT_PACK_INVAL_ATTRS writes a zero-length reference when
                    // the filesystem cannot supply a name.
                    continue;
                }
                let start = i64::try_from(reference_offset)
                    .ok()
                    .and_then(|offset| offset.checked_add(i64::from(data_offset)))
                    .filter(|offset| *offset >= 0 && *offset <= i64::from(i32::MAX))
                    .map(|offset| offset as usize)
                    .ok_or(ParseError::NameOutOfBounds {
                        offset: reader.absolute(reference_offset),
                        length: data_length,
                    })?;
                let end = start
                    .checked_add(data_length)
                    .filter(|end| *end <= reader.data.len())
                    .ok_or(ParseError::NameOutOfBounds {
                        offset: reader.absolute(reference_offset),
                        length: data_length,
                    })?;
                let raw = &reader.data[start..end];
                if raw.last() != Some(&0) {
                    return Err(ParseError::NameNotTerminated {
                        offset: reader.absolute(start),
                    });
                }
                entry.name = String::from_utf8_lossy(&raw[..raw.len() - 1]).into_owned();
            }
            ATTR_CMN_DEVID => {
                entry.dev = reader.i32()? as u32;
            }
            ATTR_CMN_OBJTYPE => {
                entry.obj_type = ObjType::from_raw(reader.u32()?);
            }
            ATTR_CMN_CRTIME => {
                entry.created = reader.timespec()?;
            }
            ATTR_CMN_MODTIME => {
                entry.modified = reader.timespec()?;
            }
            ATTR_CMN_ACCTIME => {
                entry.accessed = reader.timespec()?;
            }
            ATTR_CMN_FLAGS => {
                entry.flags = reader.u32()?;
            }
            ATTR_CMN_FILEID => {
                entry.file_id = reader.u64()?;
            }
            _ => {
                return Err(ParseError::UnsupportedAttribute {
                    group: Group::Common,
                    bit,
                });
            }
        }
    }
    Ok(())
}

fn parse_directory(
    reader: &mut Cursor<'_>,
    request: &Request,
    entry: &mut RawEntry,
) -> Result<(), ParseError> {
    for index in 0..AttrGroup::BITS {
        let bit = 1 << index;
        if request.dir & bit == 0 {
            continue;
        }
        match bit {
            ATTR_DIR_ENTRYCOUNT => {
                entry.dir_entry_count = reader.u32()?;
            }
            _ => {
                return Err(ParseError::UnsupportedAttribute {
                    group: Group::Directory,
                    bit,
                });
            }
        }
    }
    Ok(())
}

fn parse_file(
    reader: &mut Cursor<'_>,
    request: &Request,
    entry: &mut RawEntry,
) -> Result<(), ParseError> {
    for index in 0..AttrGroup::BITS {
        let bit = 1 << index;
        if request.file & bit == 0 {
            continue;
        }
        match bit {
            ATTR_FILE_LINKCOUNT => {
                entry.nlink = reader.u32()?;
            }
            ATTR_FILE_TOTALSIZE => {
                entry.size_logical = clamp_size(reader.i64()?);
            }
            ATTR_FILE_ALLOCSIZE => {
                entry.size_alloc = clamp_size(reader.i64()?);
            }
            ATTR_FILE_IOBLOCKSIZE => {
                entry.io_block_size = reader.u32()?;
            }
            _ => {
                return Err(ParseError::UnsupportedAttribute {
                    group: Group::File,
                    bit,
                });
            }
        }
    }
    Ok(())
}

/// `off_t` is signed. A negative value is not a size; clamping keeps a corrupt
/// value from wrapping to an absurd total, without failing the whole directory.
fn clamp_size(value: i64) -> u64 {
    value.max(0) as u64
}

/// Bounds-checked reader over one record.
///
/// The buffer is only 4-byte aligned, so scalars are decoded from byte arrays
/// rather than by reference.
struct Cursor<'a> {
    data: &'a [u8],
    position: usize,
    /// Absolute offset of `data` in the caller's buffer, for error messages.
    base: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8], base: usize) -> Self {
        Self {
            data,
            position: 0,
            base,
        }
    }

    fn absolute(&self, position: usize) -> usize {
        self.base.saturating_add(position)
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], ParseError> {
        let available = self.data.len().saturating_sub(self.position);
        if count > available {
            return Err(ParseError::Truncated {
                offset: self.absolute(self.position),
                needed: count,
                available,
            });
        }
        let slice = &self.data[self.position..self.position + count];
        self.position += count;
        Ok(slice)
    }

    fn u32(&mut self) -> Result<u32, ParseError> {
        let bytes = self.take(4)?;
        Ok(u32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn i32(&mut self) -> Result<i32, ParseError> {
        let bytes = self.take(4)?;
        Ok(i32::from_ne_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> Result<u64, ParseError> {
        let bytes = self.take(8)?;
        Ok(u64::from_ne_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn i64(&mut self) -> Result<i64, ParseError> {
        let bytes = self.take(8)?;
        Ok(i64::from_ne_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    /// Read a `struct timespec` and return its seconds.
    ///
    /// Nanoseconds are consumed but not exposed: [`storagelens_core::Entry`] keeps
    /// whole seconds, and the fixture test compares against `stat(2)` seconds.
    fn timespec(&mut self) -> Result<i64, ParseError> {
        let seconds = self.i64()?;
        let _nanoseconds = self.take(TIMESPEC_BYTES - 8)?;
        Ok(seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000;

    /// Build a record laid out exactly like `getattrlistbulk` writes one for
    /// [`Request::storage`], with fixed sentinel values.
    ///
    /// `returned` selects which groups are present. Within a present group, every
    /// requested attribute is written, because the scanner always sets
    /// `FSOPT_PACK_INVAL_ATTRS`.
    fn record(returned: AttrSet, obj_type: u32, name: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0u8; RECORD_HEADER_BYTES];
        let groups = [returned.common, 0, returned.dir, returned.file, 0];
        for group in groups {
            bytes.extend_from_slice(&group.to_ne_bytes());
        }

        let name_reference = if returned.common & ATTR_CMN_NAME != 0 {
            let position = bytes.len();
            // i32 data offset (patched once the name location is known) followed by
            // the u32 length, which includes the NUL terminator.
            bytes.extend_from_slice(&[0u8; 8]);
            let length = u32::try_from(name.len().saturating_add(1)).unwrap_or(u32::MAX);
            bytes[position + 4..position + 8].copy_from_slice(&length.to_ne_bytes());
            Some(position)
        } else {
            None
        };

        if returned.common & ATTR_CMN_DEVID != 0 {
            bytes.extend_from_slice(&7i32.to_ne_bytes());
        }
        if returned.common & ATTR_CMN_OBJTYPE != 0 {
            bytes.extend_from_slice(&obj_type.to_ne_bytes());
        }
        let samples: [(i64, i64); 3] = [(NOW, 123), (NOW + 100, 456), (NOW + 200, 789)];
        for (seconds, nanos) in samples {
            let bit = match seconds {
                NOW => ATTR_CMN_CRTIME,
                s if s == NOW + 100 => ATTR_CMN_MODTIME,
                _ => ATTR_CMN_ACCTIME,
            };
            if returned.common & bit != 0 {
                bytes.extend_from_slice(&seconds.to_ne_bytes());
                bytes.extend_from_slice(&nanos.to_ne_bytes());
            }
        }
        if returned.common & ATTR_CMN_FLAGS != 0 {
            bytes.extend_from_slice(&0x8000_0000u32.to_ne_bytes());
        }
        if returned.common & ATTR_CMN_FILEID != 0 {
            bytes.extend_from_slice(&42u64.to_ne_bytes());
        }

        if returned.dir & ATTR_DIR_ENTRYCOUNT != 0 {
            bytes.extend_from_slice(&3u32.to_ne_bytes());
        }

        if returned.file != 0 {
            if returned.file & ATTR_FILE_LINKCOUNT != 0 {
                bytes.extend_from_slice(&2u32.to_ne_bytes());
            }
            if returned.file & ATTR_FILE_TOTALSIZE != 0 {
                bytes.extend_from_slice(&4096i64.to_ne_bytes());
            }
            if returned.file & ATTR_FILE_ALLOCSIZE != 0 {
                bytes.extend_from_slice(&8192i64.to_ne_bytes());
            }
            if returned.file & ATTR_FILE_IOBLOCKSIZE != 0 {
                bytes.extend_from_slice(&4096u32.to_ne_bytes());
            }
        }

        let name_start = bytes.len();
        if let Some(reference) = name_reference {
            let delta =
                i64::try_from(name_start).unwrap_or(0) - i64::try_from(reference).unwrap_or(0);
            let delta = i32::try_from(delta).unwrap_or(0);
            bytes[reference..reference + 4].copy_from_slice(&delta.to_ne_bytes());
        }
        bytes.extend_from_slice(name);
        bytes.push(0);
        while bytes.len() % 8 != 0 {
            bytes.push(0);
        }

        let length = u32::try_from(bytes.len()).unwrap_or(u32::MAX);
        bytes[..RECORD_HEADER_BYTES].copy_from_slice(&length.to_ne_bytes());
        bytes
    }

    fn full_common() -> AttrGroup {
        Request::storage().common & !ATTR_CMN_RETURNED_ATTRS
    }

    fn file_record(name: &[u8]) -> Vec<u8> {
        record(
            AttrSet {
                common: full_common(),
                dir: 0,
                file: Request::storage().file,
            },
            1,
            name,
        )
    }

    fn parse_one(bytes: &[u8]) -> Result<RawEntry, ParseError> {
        let mut cursor = 0;
        let entry = parse_record(bytes, &mut cursor, &Request::storage())?;
        assert_eq!(cursor, bytes.len(), "parser must consume the whole record");
        Ok(entry)
    }

    #[test]
    fn parses_a_regular_file() {
        let entry = parse_one(&file_record(b"report.pdf")).expect("parse");

        assert_eq!(entry.name, "report.pdf");
        assert_eq!(entry.obj_type, ObjType::Regular);
        assert_eq!(entry.dev, 7);
        assert_eq!(entry.file_id, 42);
        assert_eq!(entry.flags, 0x8000_0000);
        assert_eq!(entry.created, NOW);
        assert_eq!(entry.modified, NOW + 100);
        assert_eq!(entry.accessed, NOW + 200);
        assert_eq!(entry.size_logical, 4096);
        assert_eq!(entry.size_alloc, 8192);
        assert_eq!(entry.nlink, 2);
        assert_eq!(entry.io_block_size, 4096);
        assert_eq!(entry.dir_entry_count, 0);
        assert!(entry.returned.common_has(ATTR_CMN_NAME));
        assert!(entry.returned.file_has(ATTR_FILE_ALLOCSIZE));
        assert_eq!(entry.returned.dir, 0);
    }

    #[test]
    fn parses_multiple_records() {
        let mut buffer = file_record(b"first");
        buffer.extend_from_slice(&file_record(b"second"));

        let entries = parse_batch(&buffer, &Request::storage()).expect("parse batch");
        let names: Vec<_> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, ["first", "second"]);
    }

    #[test]
    fn directory_records_have_no_file_group() {
        let bytes = record(
            AttrSet {
                common: full_common(),
                dir: ATTR_DIR_ENTRYCOUNT,
                file: 0,
            },
            2,
            b"Library",
        );

        let entry = parse_one(&bytes).expect("parse");
        assert_eq!(entry.obj_type, ObjType::Directory);
        assert_eq!(entry.dir_entry_count, 3);
        assert_eq!(entry.size_alloc, 0);
        assert_eq!(entry.size_logical, 0);
        assert_eq!(entry.nlink, 0);
        assert_eq!(entry.returned.file, 0);
    }

    #[test]
    fn zero_length_attributes_keep_defaults() {
        // FSOPT_PACK_INVAL_ATTRS can return an empty returned bitmap for an
        // object whose metadata is entirely unavailable.
        let bytes = record(AttrSet::EMPTY, 0, b"");

        let entry = parse_one(&bytes).expect("parse");
        assert_eq!(entry.name, "");
        assert_eq!(entry.obj_type, ObjType::Unknown(0));
        assert_eq!(entry.dev, 0);
        assert_eq!(entry.file_id, 0);
        assert_eq!(entry.size_alloc, 0);
        assert!(entry.returned.is_empty());
    }

    #[test]
    fn zero_length_name_is_empty() {
        let mut bytes = file_record(b"ignored");
        // ATTR_CMN_NAME's attrreference_t starts at offset 24: i32 delta, u32 len.
        bytes[28..32].copy_from_slice(&0u32.to_ne_bytes());

        let entry = parse_one(&bytes).expect("parse");
        assert_eq!(entry.name, "");
        assert_eq!(entry.obj_type, ObjType::Regular);
    }

    #[test]
    fn truncated_record_is_an_error() {
        let bytes = file_record(b"cut");
        let truncated = &bytes[..bytes.len() - 1];

        let mut cursor = 0;
        let error = parse_record(truncated, &mut cursor, &Request::storage()).expect_err("error");
        assert_eq!(
            error,
            ParseError::Truncated {
                offset: 0,
                needed: bytes.len(),
                available: truncated.len(),
            }
        );
    }

    #[test]
    fn record_shorter_than_its_header_is_an_error() {
        let mut cursor = 0;
        let error = parse_record(&[0, 0], &mut cursor, &Request::storage()).expect_err("error");
        assert!(matches!(error, ParseError::Truncated { .. }));
    }

    #[test]
    fn impossible_record_length_is_an_error() {
        let mut bytes = file_record(b"short");
        bytes[..4].copy_from_slice(&3u32.to_ne_bytes());

        let mut cursor = 0;
        let error = parse_record(&bytes, &mut cursor, &Request::storage()).expect_err("error");
        assert_eq!(
            error,
            ParseError::InvalidLength {
                offset: 0,
                length: 3
            }
        );
    }

    #[test]
    fn unknown_requested_bits_are_rejected() {
        let bytes = file_record(b"future");
        let mut request = Request::storage();
        request.common |= 0x0000_0010; // ATTR_CMN_FSID: known to the kernel, not this parser.

        let mut cursor = 0;
        let error = parse_record(&bytes, &mut cursor, &request).expect_err("error");
        assert_eq!(
            error,
            ParseError::UnsupportedAttribute {
                group: Group::Common,
                bit: 0x0000_0010,
            }
        );
    }

    #[test]
    fn name_outside_the_record_is_an_error() {
        let mut bytes = file_record(b"escape");
        let length = i32::try_from(bytes.len()).unwrap_or(i32::MAX);
        bytes[24..28].copy_from_slice(&(length + 16).to_ne_bytes());

        let mut cursor = 0;
        let error = parse_record(&bytes, &mut cursor, &Request::storage()).expect_err("error");
        assert!(matches!(error, ParseError::NameOutOfBounds { .. }));
    }

    #[test]
    fn name_without_a_terminator_is_an_error() {
        let mut bytes = file_record(b"unterminated");
        let name_length = u32::try_from("unterminated".len()).unwrap_or(u32::MAX);
        bytes[28..32].copy_from_slice(&name_length.to_ne_bytes());

        let mut cursor = 0;
        let error = parse_record(&bytes, &mut cursor, &Request::storage()).expect_err("error");
        assert!(matches!(error, ParseError::NameNotTerminated { .. }));
    }

    #[test]
    fn non_utf8_names_are_decoded_lossily() {
        let entry = parse_one(&file_record(b"bad\xffname")).expect("parse");
        assert_eq!(entry.name, "bad\u{fffd}name");
    }

    #[test]
    fn missing_returned_attrs_is_an_error() {
        let bytes = file_record(b"no bitmap");
        let request = Request {
            common: Request::storage().common & !ATTR_CMN_RETURNED_ATTRS,
            ..Request::storage()
        };

        let mut cursor = 0;
        let error = parse_record(&bytes, &mut cursor, &request).expect_err("error");
        assert_eq!(error, ParseError::MissingReturnedAttrs);
    }

    #[test]
    fn obj_type_values_match_the_bsd_vtype_constants() {
        assert_eq!(ObjType::from_raw(1), ObjType::Regular);
        assert_eq!(ObjType::from_raw(2), ObjType::Directory);
        assert_eq!(ObjType::from_raw(5), ObjType::Symlink);
        assert_eq!(ObjType::from_raw(7), ObjType::Fifo);
        assert_eq!(ObjType::from_raw(99), ObjType::Unknown(99));
        assert!(ObjType::from_raw(2).is_dir());
        assert!(!ObjType::from_raw(1).is_dir());
    }
}
