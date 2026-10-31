//! Bulk directory enumeration and the pure rules around it.
//!
//! Everything above the `#[cfg(windows)]` line is plain Rust over bytes, `u16`
//! slices, and integers, so the record parser, path, time, and classification
//! rules are unit-tested on every CI runner. Only [`DirReader`] and the small
//! helpers below it touch Win32.
//!
//! # One call per directory batch, no per-file syscalls
//!
//! The reader opens each directory once and calls
//! `GetFileInformationByHandleEx(FileIdBothDirectoryInfo)` until it reports
//! `ERROR_NO_MORE_FILES`. Each call fills a 64 KiB buffer with
//! `FILE_ID_BOTH_DIR_INFO` records carrying the name, attributes, the three
//! timestamps, logical size (`EndOfFile`), **allocated size**
//! (`AllocationSize`), the **64-bit file id**, and — for reparse points — the
//! reparse tag (in `EaSize`, per the Win32 docs). That is everything the walker
//! needs for sizes and hard-link dedupe, so AGENTS.md invariant 5 holds. PLAN §7.1
//! records why this replaced `FindFirstFileExW`, which has no file id.
//!
//! The one exception is compressed and sparse files. A directory record's
//! `AllocationSize` describes the stream as if uncompressed and fully allocated,
//! so for exactly those files — flagged by `FILE_ATTRIBUTE_COMPRESSED` /
//! `FILE_ATTRIBUTE_SPARSE_FILE` in the same record — the walker asks
//! `GetCompressedFileSizeW`. They are rare outside system folders, so the cost
//! stays proportional to the files that need it.
//!
//! # Reparse points
//!
//! Only two reparse tags are links: `IO_REPARSE_TAG_SYMLINK` and
//! `IO_REPARSE_TAG_MOUNT_POINT` (junctions and volume mount points share it).
//! Those become [`EntryKind::Symlink`] and are never entered, which is what makes
//! the fixture's junction loop terminate immediately and keeps a mounted volume
//! from being counted inside another one.
//!
//! Every other tag — OneDrive/cloud placeholders, dedup, WCI container layers,
//! `AppExecLink` stubs — sits on a real file or directory with real contents, so
//! it is classified by its directory bit like any other entry. Skipping all
//! reparse points would hide an entire OneDrive folder.
//!
//! # Long paths
//!
//! Every path handed to Win32 goes through [`verbatim`], which adds the `\\?\`
//! (or `\\?\UNC\`) prefix. That lifts the 260-character `MAX_PATH` limit and turns
//! off Win32 path normalisation, so the walker only ever passes absolute,
//! already-normalised paths built by joining names it enumerated itself.

use std::io;

use storagelens_core::EntryKind;

/// `FILE_ATTRIBUTE_DIRECTORY`.
pub const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
/// `FILE_ATTRIBUTE_SPARSE_FILE`.
pub const FILE_ATTRIBUTE_SPARSE_FILE: u32 = 0x0000_0200;
/// `FILE_ATTRIBUTE_REPARSE_POINT`.
pub const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
/// `FILE_ATTRIBUTE_COMPRESSED`.
pub const FILE_ATTRIBUTE_COMPRESSED: u32 = 0x0000_0800;
/// `IO_REPARSE_TAG_MOUNT_POINT`: junctions and volume mount points.
pub const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
/// `IO_REPARSE_TAG_SYMLINK`: file and directory symbolic links.
pub const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;

/// Bytes per `GetFileInformationByHandleEx` call. 64 KiB is also the largest
/// buffer SMB servers accept for a directory query.
pub const DIR_INFO_BUFFER_BYTES: usize = 64 * 1024;

/// `FILETIME` of 1970-01-01T00:00:00Z: 100 ns ticks since 1601-01-01.
const UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;
/// `FILETIME` resolution.
const TICKS_PER_SECOND: i64 = 10_000_000;

const BACKSLASH: u16 = b'\\' as u16;
const QUESTION: u16 = b'?' as u16;
const DOT: u16 = b'.' as u16;

/// Byte offsets inside one `FILE_ID_BOTH_DIR_INFO` record.
///
/// The record is `#[repr(C)]` with 8-byte alignment; `ShortName` (12 × `u16`)
/// starts at 70 after the `i8` length and one byte of padding, and `FileId` is
/// padded up to 96. A wrong offset here silently corrupts every size, so a
/// Windows-only test checks each one against `core::mem::offset_of!` on the
/// `windows` crate's definition.
pub mod layout {
    /// `NextEntryOffset: u32`.
    pub const NEXT_ENTRY_OFFSET: usize = 0;
    /// `CreationTime: i64`.
    pub const CREATION_TIME: usize = 8;
    /// `LastAccessTime: i64`.
    pub const LAST_ACCESS_TIME: usize = 16;
    /// `LastWriteTime: i64`.
    pub const LAST_WRITE_TIME: usize = 24;
    /// `EndOfFile: i64` — the logical size.
    pub const END_OF_FILE: usize = 40;
    /// `AllocationSize: i64`.
    pub const ALLOCATION_SIZE: usize = 48;
    /// `FileAttributes: u32`.
    pub const FILE_ATTRIBUTES: usize = 56;
    /// `FileNameLength: u32`, in bytes.
    pub const FILE_NAME_LENGTH: usize = 60;
    /// `EaSize: u32`; the reparse tag when the entry is a reparse point.
    pub const EA_SIZE: usize = 64;
    /// `FileId: i64`.
    pub const FILE_ID: usize = 96;
    /// `FileName: [u16]`, not NUL-terminated.
    pub const FILE_NAME: usize = 104;
}

/// One entry from a `FILE_ID_BOTH_DIR_INFO` record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEntry {
    /// The name as UTF-16 code units, exactly as stored. Not necessarily valid
    /// Unicode: NTFS allows unpaired surrogates.
    pub name: Vec<u16>,
    /// `FileAttributes`.
    pub attributes: u32,
    /// `EaSize` when the entry is a reparse point, otherwise `0`.
    pub reparse_tag: u32,
    /// `EndOfFile` — the logical size.
    pub size_logical: u64,
    /// `AllocationSize` — bytes on disk, except for compressed and sparse files
    /// (see [`needs_compressed_size`]).
    pub size_alloc: u64,
    /// `FileId` — the NTFS file reference number.
    pub file_id: u64,
    /// `CreationTime` in `FILETIME` ticks.
    pub created: u64,
    /// `LastWriteTime` in `FILETIME` ticks.
    pub modified: u64,
    /// `LastAccessTime` in `FILETIME` ticks. Often stale: Windows disables
    /// last-access updates by default.
    pub accessed: u64,
}

impl RawEntry {
    /// What this entry is, per the module-level reparse rules.
    pub fn kind(&self) -> EntryKind {
        kind_of(self.attributes, self.reparse_tag)
    }
}

/// Classify an entry from its attributes and reparse tag.
///
/// NTFS does not store devices, pipes, or sockets in directories, so there is no
/// [`EntryKind::Other`] here.
pub fn kind_of(attributes: u32, reparse_tag: u32) -> EntryKind {
    if is_link(attributes, reparse_tag) {
        EntryKind::Symlink
    } else if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
        EntryKind::Dir
    } else {
        EntryKind::File
    }
}

/// Whether an entry is a link the walker must never follow.
pub fn is_link(attributes: u32, reparse_tag: u32) -> bool {
    attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        && matches!(
            reparse_tag,
            IO_REPARSE_TAG_SYMLINK | IO_REPARSE_TAG_MOUNT_POINT
        )
}

/// Whether a file's directory-record `AllocationSize` is not its size on disk,
/// so `GetCompressedFileSizeW` must be asked instead.
pub fn needs_compressed_size(attributes: u32) -> bool {
    attributes & (FILE_ATTRIBUTE_COMPRESSED | FILE_ATTRIBUTE_SPARSE_FILE) != 0
}

/// Join the two halves of a 64-bit Win32 value.
pub const fn join_u32(high: u32, low: u32) -> u64 {
    ((high as u64) << 32) | low as u64
}

/// `FILETIME` ticks → Unix seconds, rounded toward negative infinity.
///
/// Times before 1970 stay negative rather than wrapping, matching `st_mtime`.
pub fn filetime_to_unix(ticks: u64) -> i64 {
    // A FILETIME above i64::MAX is not a time any filesystem stores; saturate
    // rather than wrap.
    let ticks = i64::try_from(ticks).unwrap_or(i64::MAX);
    (ticks - UNIX_EPOCH_TICKS).div_euclid(TICKS_PER_SECOND)
}

/// Whether `name` is the `.` or `..` pseudo-entry.
pub fn is_dot_entry(name: &[u16]) -> bool {
    matches!(name, [DOT] | [DOT, DOT])
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn read_u32(buffer: &[u8], at: usize) -> u32 {
    let mut bytes = [0u8; 4];
    bytes.copy_from_slice(&buffer[at..at + 4]);
    u32::from_le_bytes(bytes)
}

fn read_u64(buffer: &[u8], at: usize) -> u64 {
    let mut bytes = [0u8; 8];
    bytes.copy_from_slice(&buffer[at..at + 8]);
    u64::from_le_bytes(bytes)
}

/// Parse the chain of `FILE_ID_BOTH_DIR_INFO` records at the start of `buffer`,
/// appending every entry except `.` and `..` to `out`.
///
/// The API does not report how many bytes it wrote, so the chain is followed by
/// `NextEntryOffset` until a record whose offset is `0`. Every offset and length
/// is bounds-checked before it is read: a malformed buffer is an error, never a
/// panic and never a wrong size.
pub fn parse_dir_info(buffer: &[u8], out: &mut Vec<RawEntry>) -> io::Result<()> {
    let mut offset = 0usize;
    loop {
        let record = buffer
            .get(offset..)
            .filter(|record| record.len() >= layout::FILE_NAME)
            .ok_or_else(|| invalid("directory record header runs past the buffer"))?;

        let next = read_u32(record, layout::NEXT_ENTRY_OFFSET) as usize;
        let name_bytes = read_u32(record, layout::FILE_NAME_LENGTH) as usize;
        if name_bytes % 2 != 0 {
            return Err(invalid("directory record name has an odd byte length"));
        }
        let name_end = layout::FILE_NAME
            .checked_add(name_bytes)
            .filter(|&end| end <= record.len())
            .ok_or_else(|| invalid("directory record name runs past the buffer"))?;
        if next != 0 && next < name_end {
            return Err(invalid("directory records overlap"));
        }

        let name: Vec<u16> = record[layout::FILE_NAME..name_end]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();

        if !is_dot_entry(&name) {
            let attributes = read_u32(record, layout::FILE_ATTRIBUTES);
            let reparse_tag = if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                read_u32(record, layout::EA_SIZE)
            } else {
                0
            };
            out.push(RawEntry {
                name,
                attributes,
                reparse_tag,
                size_logical: read_u64(record, layout::END_OF_FILE),
                size_alloc: read_u64(record, layout::ALLOCATION_SIZE),
                file_id: read_u64(record, layout::FILE_ID),
                created: read_u64(record, layout::CREATION_TIME),
                modified: read_u64(record, layout::LAST_WRITE_TIME),
                accessed: read_u64(record, layout::LAST_ACCESS_TIME),
            });
        }

        if next == 0 {
            return Ok(());
        }
        offset += next;
    }
}

/// Whether `path` already carries a `\\?\` or `\\.\` prefix.
fn is_prefixed(path: &[u16]) -> bool {
    matches!(path, [BACKSLASH, BACKSLASH, QUESTION | DOT, BACKSLASH, ..])
}

/// Add the `\\?\` prefix that lifts `MAX_PATH`.
///
/// `C:\dir` → `\\?\C:\dir`, `\\server\share` → `\\?\UNC\server\share`, and a path
/// that is already prefixed is returned unchanged. The input must be absolute and
/// normalised: a verbatim path is passed to the filesystem as-is, so `/` and `..`
/// would no longer be interpreted.
pub fn verbatim(path: &[u16]) -> Vec<u16> {
    if is_prefixed(path) {
        return path.to_vec();
    }

    let mut out = Vec::with_capacity(path.len() + 8);
    if let [BACKSLASH, BACKSLASH, rest @ ..] = path {
        out.extend("\\\\?\\UNC\\".encode_utf16());
        out.extend_from_slice(rest);
    } else {
        out.extend("\\\\?\\".encode_utf16());
        out.extend_from_slice(path);
    }
    out
}

/// Remove a `\\?\` prefix, the inverse of [`verbatim`].
///
/// Reported paths stay in the form users and `ScanOptions::exclude` use. A
/// `\\.\` device path is returned unchanged: it is not a filesystem path.
pub fn strip_verbatim(path: &[u16]) -> Vec<u16> {
    let unc: Vec<u16> = "\\\\?\\UNC\\".encode_utf16().collect();
    if let Some(rest) = path.strip_prefix(unc.as_slice()) {
        let mut out = Vec::with_capacity(rest.len() + 2);
        out.extend([BACKSLASH, BACKSLASH]);
        out.extend_from_slice(rest);
        return out;
    }

    match path {
        [BACKSLASH, BACKSLASH, QUESTION, BACKSLASH, rest @ ..] => rest.to_vec(),
        _ => path.to_vec(),
    }
}

/// [`verbatim`] plus the terminating NUL Win32 expects.
pub fn verbatim_c(path: &[u16]) -> Vec<u16> {
    let mut out = verbatim(path);
    out.push(0);
    out
}

/// A Win32 error code from an `HRESULT`, when it wraps one.
///
/// `windows::core::Error` stores `HRESULT_FROM_WIN32(code)`; its `io::Error`
/// conversion passes the whole HRESULT through as the OS error, which loses the
/// [`std::io::ErrorKind`] mapping (`ERROR_ACCESS_DENIED` would no longer be
/// `PermissionDenied`). Unwrapping the facility restores it.
pub const fn win32_code(hresult: i32) -> Option<u32> {
    const FACILITY_WIN32: u32 = 0x8007_0000;
    let value = hresult as u32;
    if value & 0xFFFF_0000 == FACILITY_WIN32 {
        Some(value & 0xFFFF)
    } else {
        None
    }
}

/// What the scan needs to know about the root's volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VolumeInfo {
    /// Volume serial number, used as every entry's `dev`.
    pub serial: u32,
    /// The filesystem is NTFS.
    ///
    /// Only NTFS guarantees that the 64-bit `FileId` is unique on the volume.
    /// ReFS ids are 128-bit, so the low 64 bits may collide; FAT and exFAT have
    /// no stable ids and no hard links. Deduping on anything but NTFS could merge
    /// two different files.
    pub ntfs: bool,
}

/// Whether a filesystem name, as `GetVolumeInformationW` reports it, is NTFS.
pub fn is_ntfs(name: &[u16]) -> bool {
    String::from_utf16_lossy(name).eq_ignore_ascii_case("NTFS")
}

// --- Drives -------------------------------------------------------------------------

/// `GetDriveTypeW` results worth listing, from `WindowsProgramming` (a feature
/// not otherwise needed): removable media and fixed disks. Network shares,
/// optical drives, and RAM disks are left out, as the volume picker shows them.
pub const DRIVE_REMOVABLE: u32 = 2;
pub const DRIVE_FIXED: u32 = 3;

/// Drive letters present in a `GetLogicalDrives` bitmask (bit 0 = `A:`).
pub fn drive_letters(mask: u32) -> Vec<char> {
    (0..26u8)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit| char::from(b'A' + bit))
        .collect()
}

/// Display name for a drive: its label, or `Local Disk (C:)` like Explorer.
pub fn drive_name(label: &str, letter: char) -> String {
    let label = label.trim();
    if label.is_empty() {
        format!("Local Disk ({letter}:)")
    } else {
        format!("{label} ({letter}:)")
    }
}

#[cfg(windows)]
pub use sys::{compressed_size, drives, stamp, throttle_current_thread, volume_info, DirReader};

#[cfg(windows)]
mod sys {
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use storagelens_core::{Bytes, FileStamp, Volume};
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{CloseHandle, ERROR_NO_MORE_FILES, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, FileAttributeTagInfo, FileIdBothDirectoryInfo, GetCompressedFileSizeW,
        GetDiskFreeSpaceExW, GetDriveTypeW, GetFileInformationByHandle,
        GetFileInformationByHandleEx, GetLogicalDrives, GetVolumeInformationW, GetVolumePathNameW,
        BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_TAG_INFO, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_LIST_DIRECTORY, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
        FILE_SHARE_READ, FILE_SHARE_WRITE, INVALID_FILE_SIZE, OPEN_EXISTING,
    };
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
    };

    use super::{
        drive_letters, drive_name, filetime_to_unix, is_ntfs, join_u32, kind_of, parse_dir_info,
        verbatim_c, win32_code, RawEntry, VolumeInfo, DIR_INFO_BUFFER_BYTES, DRIVE_FIXED,
        DRIVE_REMOVABLE,
    };

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().collect()
    }

    fn io_error(error: &windows::core::Error) -> io::Error {
        match win32_code(error.code().0) {
            Some(code) => io::Error::from_raw_os_error(code as i32),
            None => io::Error::other(error.message()),
        }
    }

    /// Streams the entries of one directory, skipping `.` and `..`.
    ///
    /// Owns the directory handle and closes it on drop, so a walker that stops
    /// early (cancellation, a later error) never leaks it.
    pub struct DirReader {
        handle: HANDLE,
        /// 8-byte aligned, as `FILE_ID_BOTH_DIR_INFO` requires.
        buffer: Vec<u64>,
        /// Parsed entries not yet yielded, in reverse order so `pop` is cheap.
        pending: Vec<RawEntry>,
        done: bool,
    }

    impl DirReader {
        /// Open `dir` for enumeration.
        ///
        /// `dir` must be absolute and normalised; see [`super::verbatim`].
        pub fn open(dir: &Path) -> io::Result<Self> {
            let path = verbatim_c(&wide(dir));

            // SAFETY: `path` is NUL-terminated and outlives the call. Opening a
            // directory requires FILE_FLAG_BACKUP_SEMANTICS; FILE_LIST_DIRECTORY is
            // the only right the query needs, and full sharing never blocks other
            // processes.
            let handle = unsafe {
                CreateFileW(
                    PCWSTR(path.as_ptr()),
                    FILE_LIST_DIRECTORY.0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS,
                    None,
                )
            }
            .map_err(|error| io_error(&error))?;

            Ok(Self {
                handle,
                buffer: vec![0u64; DIR_INFO_BUFFER_BYTES / 8],
                pending: Vec::new(),
                done: false,
            })
        }

        /// Fetch and parse the next batch of records.
        fn refill(&mut self) -> io::Result<()> {
            let bytes = (self.buffer.len() * 8) as u32;
            // SAFETY: `handle` is the live directory handle opened above, and
            // `buffer` is a writable, 8-byte aligned allocation of `bytes` bytes,
            // which is the size passed. The kernel writes at most that many.
            let filled = unsafe {
                GetFileInformationByHandleEx(
                    self.handle,
                    FileIdBothDirectoryInfo,
                    self.buffer.as_mut_ptr().cast(),
                    bytes,
                )
            };

            match filled {
                Ok(()) => {
                    // SAFETY: reinterpreting the initialised u64 buffer as bytes;
                    // u8 has no alignment or validity requirements.
                    let bytes = unsafe {
                        std::slice::from_raw_parts(
                            self.buffer.as_ptr().cast::<u8>(),
                            self.buffer.len() * 8,
                        )
                    };
                    parse_dir_info(bytes, &mut self.pending)?;
                    self.pending.reverse();
                    Ok(())
                }
                Err(error) if win32_code(error.code().0) == Some(ERROR_NO_MORE_FILES.0) => {
                    self.done = true;
                    Ok(())
                }
                Err(error) => Err(io_error(&error)),
            }
        }
    }

    impl Iterator for DirReader {
        type Item = io::Result<RawEntry>;

        fn next(&mut self) -> Option<Self::Item> {
            loop {
                if let Some(entry) = self.pending.pop() {
                    return Some(Ok(entry));
                }
                if self.done {
                    return None;
                }
                if let Err(error) = self.refill() {
                    // The handle's position is unknown after a failure.
                    self.done = true;
                    return Some(Err(error));
                }
            }
        }
    }

    impl Drop for DirReader {
        fn drop(&mut self) {
            // SAFETY: the handle came from `CreateFileW`, is closed exactly once
            // (here), and is never used afterwards. A failure leaves nothing to
            // recover.
            let _ = unsafe { CloseHandle(self.handle) };
        }
    }

    /// What the walker would record for `path` now, for the safety layer's
    /// preflight re-stat (#29). A final symlink or junction is described, not
    /// followed. The file id is the same 64-bit NTFS reference number the
    /// directory listing reports.
    pub fn stamp(path: &Path) -> io::Result<FileStamp> {
        let path = verbatim_c(&wide(path));
        // SAFETY: `path` is NUL-terminated and outlives the call. Reading
        // attributes needs only FILE_READ_ATTRIBUTES; BACKUP_SEMANTICS allows
        // directories and OPEN_REPARSE_POINT opens a link rather than its target.
        // Full sharing never blocks other processes.
        let handle = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                FILE_READ_ATTRIBUTES.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
                None,
            )
        }
        .map_err(|error| io_error(&error))?;
        let result = stamp_handle(handle);
        // SAFETY: the handle came from `CreateFileW` above, is closed exactly
        // once, and is not used afterwards.
        let _ = unsafe { CloseHandle(handle) };
        result
    }

    fn stamp_handle(handle: HANDLE) -> io::Result<FileStamp> {
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: `handle` is live and `info` is a writable struct of the type
        // the call fills.
        unsafe { GetFileInformationByHandle(handle, &mut info) }
            .map_err(|error| io_error(&error))?;
        let mut tag = FILE_ATTRIBUTE_TAG_INFO::default();
        // SAFETY: `tag` is a writable FILE_ATTRIBUTE_TAG_INFO and its exact
        // size is passed, as FileAttributeTagInfo requires.
        unsafe {
            GetFileInformationByHandleEx(
                handle,
                FileAttributeTagInfo,
                (&mut tag as *mut FILE_ATTRIBUTE_TAG_INFO).cast(),
                std::mem::size_of::<FILE_ATTRIBUTE_TAG_INFO>() as u32,
            )
        }
        .map_err(|error| io_error(&error))?;
        let written = join_u32(
            info.ftLastWriteTime.dwHighDateTime,
            info.ftLastWriteTime.dwLowDateTime,
        );
        Ok(FileStamp {
            kind: kind_of(info.dwFileAttributes, tag.ReparseTag),
            size_logical: join_u32(info.nFileSizeHigh, info.nFileSizeLow),
            modified: filetime_to_unix(written),
            dev: info.dwVolumeSerialNumber,
            file_id: join_u32(info.nFileIndexHigh, info.nFileIndexLow),
            attrs: info.dwFileAttributes,
        })
    }

    /// Bytes on disk for a compressed or sparse file, or `None` if the query fails.
    pub fn compressed_size(path: &Path) -> Option<u64> {
        let path = verbatim_c(&wide(path));
        let mut high = 0u32;
        // SAFETY: `path` is NUL-terminated and outlives the call; `high` is a
        // valid, writable u32.
        let low = unsafe { GetCompressedFileSizeW(PCWSTR(path.as_ptr()), Some(&mut high)) };
        // INVALID_FILE_SIZE is also a valid low half; only the thread's last error
        // tells them apart, so read it before anything else can overwrite it.
        if low == INVALID_FILE_SIZE && io::Error::last_os_error().raw_os_error() != Some(0) {
            return None;
        }
        Some(join_u32(high, low))
    }

    /// Lower this thread's CPU and I/O priority for the rest of its life.
    ///
    /// `THREAD_MODE_BACKGROUND_BEGIN` is the Windows equivalent of macOS
    /// `setiopolicy_np(IOPOL_THROTTLE)` (AGENTS.md §6): the scan yields to
    /// foreground I/O instead of competing with it. Best effort — a failure only
    /// means the scan runs at normal priority.
    pub fn throttle_current_thread() {
        // SAFETY: `GetCurrentThread` returns a pseudo-handle that is always valid
        // for the calling thread and needs no closing.
        let _ = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_MODE_BACKGROUND_BEGIN) };
    }

    /// Serial number and filesystem of the volume holding `path`.
    ///
    /// Called once per scan, not per entry. `None` when the volume cannot be
    /// queried (some network redirectors refuse).
    pub fn volume_info(path: &Path) -> Option<VolumeInfo> {
        let path = verbatim_c(&wide(path));

        // Long enough for any volume GUID path or drive root.
        let mut volume = [0u16; 1024];
        // SAFETY: `path` is NUL-terminated and outlives the call; the output slice
        // carries its own length.
        unsafe { GetVolumePathNameW(PCWSTR(path.as_ptr()), &mut volume) }.ok()?;

        let mut serial = 0u32;
        let mut filesystem = [0u16; 64];
        // SAFETY: `volume` was NUL-terminated by the previous call; `serial` is a
        // valid, writable u32 and the name slice carries its own length.
        unsafe {
            GetVolumeInformationW(
                PCWSTR(volume.as_ptr()),
                None,
                Some(&mut serial),
                None,
                None,
                Some(&mut filesystem),
            )
        }
        .ok()?;

        let end = filesystem
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(filesystem.len());
        Some(VolumeInfo {
            serial,
            ntfs: is_ntfs(&filesystem[..end]),
        })
    }

    fn until_nul(units: &[u16]) -> String {
        let end = units
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(units.len());
        String::from_utf16_lossy(&units[..end])
    }

    /// Fixed and removable drives with their capacity. A drive that cannot be
    /// queried (an empty card reader) is skipped, never an error.
    pub fn drives() -> Vec<Volume> {
        // SAFETY: no arguments; returns a bitmask.
        let mask = unsafe { GetLogicalDrives() };
        drive_letters(mask)
            .into_iter()
            .filter_map(|letter| {
                let root = format!("{letter}:\\");
                let wide_root: Vec<u16> = root.encode_utf16().chain(Some(0)).collect();
                let root_ptr = PCWSTR(wide_root.as_ptr());
                // SAFETY: `wide_root` is NUL-terminated and outlives each call
                // below; every output is a valid, writable local.
                let kind = unsafe { GetDriveTypeW(root_ptr) };
                if kind != DRIVE_FIXED && kind != DRIVE_REMOVABLE {
                    return None;
                }
                let mut label = [0u16; 261];
                let mut filesystem = [0u16; 64];
                let mut serial = 0u32;
                // SAFETY: as above; the name slices carry their own lengths.
                unsafe {
                    GetVolumeInformationW(
                        root_ptr,
                        Some(&mut label),
                        Some(&mut serial),
                        None,
                        None,
                        Some(&mut filesystem),
                    )
                }
                .ok()?;
                let mut available = 0u64;
                let mut total = 0u64;
                // SAFETY: as above; both outputs are writable u64 locals.
                unsafe {
                    GetDiskFreeSpaceExW(root_ptr, Some(&mut available), Some(&mut total), None)
                }
                .ok()?;
                Some(Volume {
                    id: serial,
                    name: drive_name(&until_nul(&label), letter),
                    mount_point: root.into(),
                    fs_type: until_nul(&filesystem),
                    total: Bytes::new(total),
                    available: Bytes::new(available),
                    purgeable: Bytes::ZERO,
                    removable: kind == DRIVE_REMOVABLE,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod drive_tests {
    use super::*;

    #[test]
    fn drive_letters_follow_the_bitmask() {
        assert_eq!(drive_letters(0), Vec::<char>::new());
        assert_eq!(drive_letters(0b1100), vec!['C', 'D']);
        assert_eq!(drive_letters(1 << 25 | 1), vec!['A', 'Z']);
        assert_eq!(
            drive_letters(u32::MAX).len(),
            26,
            "bits above Z are ignored"
        );
    }

    #[test]
    fn drives_are_named_like_explorer() {
        assert_eq!(drive_name("", 'C'), "Local Disk (C:)");
        assert_eq!(drive_name("  Data ", 'D'), "Data (D:)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    fn s(units: &[u16]) -> String {
        String::from_utf16(units).expect("valid utf-16")
    }

    /// One record's worth of fields, for building test buffers.
    #[derive(Clone)]
    struct Record {
        name: Vec<u16>,
        attributes: u32,
        ea_size: u32,
        size_logical: u64,
        size_alloc: u64,
        file_id: u64,
        created: u64,
        modified: u64,
        accessed: u64,
    }

    fn record(name: &str) -> Record {
        Record {
            name: w(name),
            attributes: 0x20,
            ea_size: 0,
            size_logical: 5,
            size_alloc: 8,
            file_id: 42,
            created: 1,
            modified: 2,
            accessed: 3,
        }
    }

    /// Lay records out the way the kernel does: 8-byte aligned, chained by
    /// `NextEntryOffset`, the last one `0`, trailing garbage after it.
    fn buffer(records: &[Record]) -> Vec<u8> {
        let mut out = Vec::new();
        for (index, record) in records.iter().enumerate() {
            let start = out.len();
            let length = layout::FILE_NAME + record.name.len() * 2;
            let padded = length.next_multiple_of(8);
            out.resize(start + padded, 0);

            let put32 = |out: &mut Vec<u8>, at: usize, value: u32| {
                out[start + at..start + at + 4].copy_from_slice(&value.to_le_bytes());
            };
            let put64 = |out: &mut Vec<u8>, at: usize, value: u64| {
                out[start + at..start + at + 8].copy_from_slice(&value.to_le_bytes());
            };

            let next = if index + 1 == records.len() {
                0
            } else {
                padded
            };
            put32(&mut out, layout::NEXT_ENTRY_OFFSET, next as u32);
            put64(&mut out, layout::CREATION_TIME, record.created);
            put64(&mut out, layout::LAST_ACCESS_TIME, record.accessed);
            put64(&mut out, layout::LAST_WRITE_TIME, record.modified);
            put64(&mut out, layout::END_OF_FILE, record.size_logical);
            put64(&mut out, layout::ALLOCATION_SIZE, record.size_alloc);
            put32(&mut out, layout::FILE_ATTRIBUTES, record.attributes);
            put32(
                &mut out,
                layout::FILE_NAME_LENGTH,
                (record.name.len() * 2) as u32,
            );
            put32(&mut out, layout::EA_SIZE, record.ea_size);
            put64(&mut out, layout::FILE_ID, record.file_id);
            for (offset, unit) in record.name.iter().enumerate() {
                let at = start + layout::FILE_NAME + offset * 2;
                out[at..at + 2].copy_from_slice(&unit.to_le_bytes());
            }
        }
        out.extend([0xAB; 64]);
        out
    }

    fn parse(bytes: &[u8]) -> io::Result<Vec<RawEntry>> {
        let mut out = Vec::new();
        parse_dir_info(bytes, &mut out).map(|()| out)
    }

    #[test]
    fn records_are_parsed_field_by_field() {
        let entries = parse(&buffer(&[record("a.txt")])).expect("parse");
        assert_eq!(
            entries,
            vec![RawEntry {
                name: w("a.txt"),
                attributes: 0x20,
                reparse_tag: 0,
                size_logical: 5,
                size_alloc: 8,
                file_id: 42,
                created: 1,
                modified: 2,
                accessed: 3,
            }]
        );
    }

    #[test]
    fn the_chain_is_followed_and_dot_entries_are_dropped() {
        let mut second = record("Фото-📷.jpg");
        second.file_id = 7;
        let entries =
            parse(&buffer(&[record("."), record(".."), record("a"), second])).expect("parse");
        let names: Vec<String> = entries.iter().map(|entry| s(&entry.name)).collect();
        assert_eq!(names, ["a", "Фото-📷.jpg"]);
        assert_eq!(entries[1].file_id, 7);
    }

    #[test]
    fn the_reparse_tag_is_read_only_for_reparse_points() {
        let mut junction = record("loop");
        junction.attributes = FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT;
        junction.ea_size = IO_REPARSE_TAG_MOUNT_POINT;
        let mut plain = record("plain");
        plain.ea_size = 99; // A real EA size, not a tag.

        let entries = parse(&buffer(&[junction, plain])).expect("parse");
        assert_eq!(entries[0].reparse_tag, IO_REPARSE_TAG_MOUNT_POINT);
        assert_eq!(entries[0].kind(), EntryKind::Symlink);
        assert_eq!(entries[1].reparse_tag, 0);
        assert_eq!(entries[1].kind(), EntryKind::File);
    }

    #[test]
    fn a_255_unit_name_and_large_values_round_trip() {
        let mut long = record(&"n".repeat(255));
        long.size_logical = u64::MAX - 1;
        long.size_alloc = 1 << 40;
        long.file_id = 0x0001_0000_0000_0005; // sequence number in the high bits
        let entries = parse(&buffer(&[long.clone()])).expect("parse");
        assert_eq!(entries[0].name.len(), 255);
        assert_eq!(entries[0].size_logical, long.size_logical);
        assert_eq!(entries[0].size_alloc, long.size_alloc);
        assert_eq!(entries[0].file_id, long.file_id);
    }

    #[test]
    fn malformed_buffers_are_errors_not_panics() {
        // Too short for a header.
        assert!(parse(&[0u8; layout::FILE_NAME - 1]).is_err());

        // Name length past the end.
        let mut bytes = buffer(&[record("abc")]);
        bytes[layout::FILE_NAME_LENGTH..layout::FILE_NAME_LENGTH + 4]
            .copy_from_slice(&10_000u32.to_le_bytes());
        assert!(parse(&bytes).is_err());

        // Odd name length.
        let mut bytes = buffer(&[record("abc")]);
        bytes[layout::FILE_NAME_LENGTH..layout::FILE_NAME_LENGTH + 4]
            .copy_from_slice(&5u32.to_le_bytes());
        assert!(parse(&bytes).is_err());

        // Next record past the end.
        let mut bytes = buffer(&[record("a"), record("b")]);
        bytes[0..4].copy_from_slice(&1_000_000u32.to_le_bytes());
        assert!(parse(&bytes).is_err());

        // Next offset pointing back into the current record's name.
        let mut bytes = buffer(&[record("abcdef"), record("b")]);
        bytes[0..4].copy_from_slice(&(layout::FILE_NAME as u32).to_le_bytes());
        assert!(parse(&bytes).is_err());
    }

    #[test]
    fn only_compressed_and_sparse_files_need_the_extra_query() {
        assert!(!needs_compressed_size(0x20));
        assert!(needs_compressed_size(FILE_ATTRIBUTE_COMPRESSED));
        assert!(needs_compressed_size(FILE_ATTRIBUTE_SPARSE_FILE | 0x20));
    }

    #[test]
    fn only_ntfs_ids_are_trusted() {
        assert!(is_ntfs(&w("NTFS")));
        assert!(is_ntfs(&w("ntfs")));
        assert!(!is_ntfs(&w("ReFS")));
        assert!(!is_ntfs(&w("FAT32")));
        assert!(!is_ntfs(&w("exFAT")));
    }

    #[test]
    fn plain_entries_are_classified_by_the_directory_bit() {
        assert_eq!(kind_of(0x20, 0), EntryKind::File); // ARCHIVE
        assert_eq!(kind_of(FILE_ATTRIBUTE_DIRECTORY, 0), EntryKind::Dir);
    }

    #[test]
    fn junctions_and_symlinks_are_links_and_never_directories() {
        let dir_reparse = FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT;
        assert_eq!(
            kind_of(dir_reparse, IO_REPARSE_TAG_MOUNT_POINT),
            EntryKind::Symlink
        );
        assert_eq!(
            kind_of(dir_reparse, IO_REPARSE_TAG_SYMLINK),
            EntryKind::Symlink
        );
        assert_eq!(
            kind_of(FILE_ATTRIBUTE_REPARSE_POINT, IO_REPARSE_TAG_SYMLINK),
            EntryKind::Symlink
        );
    }

    #[test]
    fn other_reparse_points_keep_their_real_kind() {
        const IO_REPARSE_TAG_CLOUD_6: u32 = 0x9000_601A;
        const IO_REPARSE_TAG_DEDUP: u32 = 0x8000_0013;
        const IO_REPARSE_TAG_APPEXECLINK: u32 = 0x8000_001B;

        let dir_reparse = FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_REPARSE_POINT;
        assert_eq!(kind_of(dir_reparse, IO_REPARSE_TAG_CLOUD_6), EntryKind::Dir);
        assert_eq!(
            kind_of(FILE_ATTRIBUTE_REPARSE_POINT, IO_REPARSE_TAG_DEDUP),
            EntryKind::File
        );
        assert_eq!(
            kind_of(FILE_ATTRIBUTE_REPARSE_POINT, IO_REPARSE_TAG_APPEXECLINK),
            EntryKind::File
        );
    }

    #[test]
    fn a_link_tag_without_the_reparse_attribute_is_ignored() {
        assert_eq!(
            kind_of(FILE_ATTRIBUTE_DIRECTORY, IO_REPARSE_TAG_MOUNT_POINT),
            EntryKind::Dir
        );
    }

    #[test]
    fn filetime_converts_to_unix_seconds() {
        assert_eq!(filetime_to_unix(UNIX_EPOCH_TICKS as u64), 0);
        // 2024-01-01T00:00:00Z.
        assert_eq!(
            filetime_to_unix(UNIX_EPOCH_TICKS as u64 + 1_704_067_200 * 10_000_000),
            1_704_067_200
        );
        // Sub-second ticks round down, also before 1970.
        assert_eq!(filetime_to_unix(UNIX_EPOCH_TICKS as u64 + 9_999_999), 0);
        assert_eq!(filetime_to_unix(UNIX_EPOCH_TICKS as u64 - 1), -1);
        // 1601-01-01, the zero FILETIME.
        assert_eq!(filetime_to_unix(0), -11_644_473_600);
        assert_eq!(
            filetime_to_unix(u64::MAX),
            filetime_to_unix(i64::MAX as u64)
        );
    }

    #[test]
    fn halves_join_high_first() {
        assert_eq!(join_u32(0, 7), 7);
        assert_eq!(join_u32(1, 0), 1 << 32);
        assert_eq!(join_u32(u32::MAX, u32::MAX), u64::MAX);
    }

    #[test]
    fn only_dot_and_dot_dot_are_pseudo_entries() {
        assert!(is_dot_entry(&w(".")));
        assert!(is_dot_entry(&w("..")));
        assert!(!is_dot_entry(&w("...")));
        assert!(!is_dot_entry(&w(".git")));
        assert!(!is_dot_entry(&[]));
    }

    #[test]
    fn drive_and_unc_paths_gain_the_right_prefix() {
        assert_eq!(s(&verbatim(&w(r"C:\Users\me"))), r"\\?\C:\Users\me");
        assert_eq!(s(&verbatim(&w(r"C:\"))), r"\\?\C:\");
        assert_eq!(
            s(&verbatim(&w(r"\\server\share\dir"))),
            r"\\?\UNC\server\share\dir"
        );
        assert_eq!(s(&verbatim(&w(r"\\?\C:\x"))), r"\\?\C:\x");
        assert_eq!(
            s(&verbatim(&w(r"\\.\PhysicalDrive0"))),
            r"\\.\PhysicalDrive0"
        );
        let terminated = verbatim_c(&w(r"C:\x"));
        assert_eq!(terminated.last(), Some(&0));
        assert_eq!(s(&terminated[..terminated.len() - 1]), r"\\?\C:\x");
    }

    #[test]
    fn stripping_inverts_the_prefix() {
        for path in [r"C:\Users\me", r"C:\", r"\\server\share\dir"] {
            assert_eq!(s(&strip_verbatim(&verbatim(&w(path)))), path);
        }
        assert_eq!(s(&strip_verbatim(&w(r"D:\plain"))), r"D:\plain");
        assert_eq!(s(&strip_verbatim(&w(r"\\.\C:"))), r"\\.\C:");
    }

    #[test]
    fn a_path_longer_than_max_path_survives_the_round_trip() {
        let long = format!(r"C:\{}", ["deep"; 250].join(r"\"));
        assert!(long.len() > 1_024);
        assert_eq!(s(&strip_verbatim(&verbatim(&w(&long)))), long);
    }

    #[test]
    fn win32_errors_are_unwrapped_from_hresults() {
        // HRESULT_FROM_WIN32(ERROR_ACCESS_DENIED).
        assert_eq!(win32_code(0x8007_0005_u32 as i32), Some(5));
        // HRESULT_FROM_WIN32(ERROR_NO_MORE_FILES).
        assert_eq!(win32_code(0x8007_0012_u32 as i32), Some(18));
        // E_FAIL is not a Win32 code.
        assert_eq!(win32_code(0x8000_4005_u32 as i32), None);
    }

    /// The hand-written offsets must match the real struct, or every size is
    /// silently wrong (AGENTS.md §10).
    #[cfg(windows)]
    #[test]
    fn layout_matches_the_windows_definition() {
        use core::mem::offset_of;
        use windows::Win32::Storage::FileSystem::FILE_ID_BOTH_DIR_INFO as Info;

        assert_eq!(offset_of!(Info, NextEntryOffset), layout::NEXT_ENTRY_OFFSET);
        assert_eq!(offset_of!(Info, CreationTime), layout::CREATION_TIME);
        assert_eq!(offset_of!(Info, LastAccessTime), layout::LAST_ACCESS_TIME);
        assert_eq!(offset_of!(Info, LastWriteTime), layout::LAST_WRITE_TIME);
        assert_eq!(offset_of!(Info, EndOfFile), layout::END_OF_FILE);
        assert_eq!(offset_of!(Info, AllocationSize), layout::ALLOCATION_SIZE);
        assert_eq!(offset_of!(Info, FileAttributes), layout::FILE_ATTRIBUTES);
        assert_eq!(offset_of!(Info, FileNameLength), layout::FILE_NAME_LENGTH);
        assert_eq!(offset_of!(Info, EaSize), layout::EA_SIZE);
        assert_eq!(offset_of!(Info, FileId), layout::FILE_ID);
        assert_eq!(offset_of!(Info, FileName), layout::FILE_NAME);
    }
}
