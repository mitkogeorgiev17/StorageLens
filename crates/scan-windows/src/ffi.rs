//! `FindFirstFileExW` directory enumeration and the pure rules around it.
//!
//! Everything above the `#[cfg(windows)]` line is plain Rust over `u16` slices
//! and integers, so the path, time, and classification rules are unit-tested on
//! every CI runner. Only [`DirReader`] and the two small helpers below it touch
//! Win32.
//!
//! # One call per directory, no per-file syscalls
//!
//! `FindFirstFileExW` with `FindExInfoBasic` (no 8.3 short names) and
//! `FIND_FIRST_EX_LARGE_FETCH` (a larger kernel buffer) returns name, attributes,
//! logical size, the three timestamps, and — for reparse points — the reparse tag
//! in `dwReserved0`. That is everything the walker needs to classify an entry and
//! decide whether to descend, so AGENTS.md invariant 5 holds.
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

use std::ffi::OsString;

use storagelens_core::EntryKind;

/// `FILE_ATTRIBUTE_DIRECTORY`.
pub const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
/// `FILE_ATTRIBUTE_REPARSE_POINT`.
pub const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
/// `IO_REPARSE_TAG_MOUNT_POINT`: junctions and volume mount points.
pub const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA000_0003;
/// `IO_REPARSE_TAG_SYMLINK`: file and directory symbolic links.
pub const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000_000C;

/// `FILETIME` of 1970-01-01T00:00:00Z: 100 ns ticks since 1601-01-01.
const UNIX_EPOCH_TICKS: i64 = 116_444_736_000_000_000;
/// `FILETIME` resolution.
const TICKS_PER_SECOND: i64 = 10_000_000;

const BACKSLASH: u16 = b'\\' as u16;
const QUESTION: u16 = b'?' as u16;
const DOT: u16 = b'.' as u16;

/// One entry as `FindFirstFileExW` reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawEntry {
    /// The name, exactly as stored. Not necessarily valid Unicode.
    pub name: OsString,
    /// `dwFileAttributes`.
    pub attributes: u32,
    /// `dwReserved0` when the entry is a reparse point, otherwise `0`.
    pub reparse_tag: u32,
    /// `nFileSizeHigh:nFileSizeLow` — the logical size.
    pub size: u64,
    /// `ftCreationTime` in `FILETIME` ticks.
    pub created: u64,
    /// `ftLastWriteTime` in `FILETIME` ticks.
    pub modified: u64,
    /// `ftLastAccessTime` in `FILETIME` ticks. Often stale: Windows disables
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
/// The `FindExInfoBasic` listing cannot distinguish devices, pipes, or sockets
/// from files, and NTFS does not store them in directories anyway, so there is no
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

/// The part of a NUL-terminated buffer before its first NUL.
pub fn until_nul(buffer: &[u16]) -> &[u16] {
    let end = buffer
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(buffer.len());
    &buffer[..end]
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

/// The NUL-terminated `dir\*` pattern `FindFirstFileExW` expects, verbatim.
pub fn search_pattern(dir: &[u16]) -> Vec<u16> {
    let mut pattern = verbatim(dir);
    if pattern.last() != Some(&BACKSLASH) {
        pattern.push(BACKSLASH);
    }
    pattern.push(b'*' as u16);
    pattern.push(0);
    pattern
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

#[cfg(windows)]
pub use sys::{throttle_current_thread, volume_serial, DirReader};

#[cfg(windows)]
mod sys {
    use std::ffi::OsString;
    use std::io;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use std::path::Path;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_FILES, HANDLE};
    use windows::Win32::Storage::FileSystem::{
        FindClose, FindExInfoBasic, FindExSearchNameMatch, FindFirstFileExW, FindNextFileW,
        GetVolumeInformationW, GetVolumePathNameW, FIND_FIRST_EX_LARGE_FETCH, WIN32_FIND_DATAW,
    };
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_MODE_BACKGROUND_BEGIN,
    };

    use super::{
        is_dot_entry, join_u32, search_pattern, until_nul, verbatim, win32_code, RawEntry,
        FILE_ATTRIBUTE_REPARSE_POINT,
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

    fn is_code(error: &windows::core::Error, code: u32) -> bool {
        win32_code(error.code().0) == Some(code)
    }

    fn raw_entry(data: &WIN32_FIND_DATAW) -> RawEntry {
        let reparse_tag = if data.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            data.dwReserved0
        } else {
            0
        };
        let ticks = |time: windows::Win32::Foundation::FILETIME| {
            join_u32(time.dwHighDateTime, time.dwLowDateTime)
        };

        RawEntry {
            name: OsString::from_wide(until_nul(&data.cFileName)),
            attributes: data.dwFileAttributes,
            reparse_tag,
            size: join_u32(data.nFileSizeHigh, data.nFileSizeLow),
            created: ticks(data.ftCreationTime),
            modified: ticks(data.ftLastWriteTime),
            accessed: ticks(data.ftLastAccessTime),
        }
    }

    /// Streams the entries of one directory, skipping `.` and `..`.
    ///
    /// Owns the find handle and closes it on drop, so a walker that stops early
    /// (cancellation, an error in a later entry) never leaks it.
    pub struct DirReader {
        handle: HANDLE,
        data: WIN32_FIND_DATAW,
        /// `data` holds an entry not yet yielded.
        pending: bool,
        done: bool,
    }

    impl DirReader {
        /// Open `dir` for enumeration.
        ///
        /// `dir` must be absolute and normalised; see [`verbatim`].
        pub fn open(dir: &Path) -> io::Result<Self> {
            let pattern = search_pattern(&wide(dir));
            let mut data = WIN32_FIND_DATAW::default();

            // SAFETY: `pattern` is NUL-terminated and outlives the call; `data` is a
            // valid, writable WIN32_FIND_DATAW, which is what `FindExInfoBasic`
            // fills. The search filter must be null for `FindExSearchNameMatch`.
            let opened = unsafe {
                FindFirstFileExW(
                    PCWSTR(pattern.as_ptr()),
                    FindExInfoBasic,
                    (&mut data as *mut WIN32_FIND_DATAW).cast(),
                    FindExSearchNameMatch,
                    None,
                    FIND_FIRST_EX_LARGE_FETCH,
                )
            };

            match opened {
                Ok(handle) => Ok(Self {
                    handle,
                    data,
                    pending: true,
                    done: false,
                }),
                // No entry matched `*`. Only a volume root can be this empty (other
                // directories always list `.` and `..`), but it is not an error.
                Err(error) if is_code(&error, ERROR_FILE_NOT_FOUND.0) => Ok(Self {
                    handle: HANDLE::default(),
                    data,
                    pending: false,
                    done: true,
                }),
                Err(error) => Err(io_error(&error)),
            }
        }
    }

    impl Iterator for DirReader {
        type Item = io::Result<RawEntry>;

        fn next(&mut self) -> Option<Self::Item> {
            loop {
                if self.pending {
                    self.pending = false;
                    if !is_dot_entry(until_nul(&self.data.cFileName)) {
                        return Some(Ok(raw_entry(&self.data)));
                    }
                }
                if self.done {
                    return None;
                }

                // SAFETY: `handle` is the live find handle returned by
                // `FindFirstFileExW` (`done` is false, so it was opened and has not
                // been closed), and `data` is a valid, writable WIN32_FIND_DATAW.
                match unsafe { FindNextFileW(self.handle, &mut self.data) } {
                    Ok(()) => self.pending = true,
                    Err(error) => {
                        self.done = true;
                        if !is_code(&error, ERROR_NO_MORE_FILES.0) {
                            return Some(Err(io_error(&error)));
                        }
                    }
                }
            }
        }
    }

    impl Drop for DirReader {
        fn drop(&mut self) {
            // `is_invalid` covers both null (the empty-volume case) and
            // INVALID_HANDLE_VALUE.
            if self.handle.is_invalid() {
                return;
            }
            // SAFETY: the handle came from `FindFirstFileExW`, is closed exactly
            // once (here), and is never used afterwards. A failure leaves nothing
            // to recover.
            let _ = unsafe { FindClose(self.handle) };
        }
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

    /// The serial number of the volume holding `path`, used as the entry `dev`.
    ///
    /// Called once per scan, not per entry. `None` when the volume cannot be
    /// queried (some network redirectors refuse).
    pub fn volume_serial(path: &Path) -> Option<u32> {
        let mut path_wide = verbatim(&wide(path));
        path_wide.push(0);

        // Long enough for any volume GUID path or drive root.
        let mut volume = [0u16; 1024];
        // SAFETY: `path_wide` is NUL-terminated and outlives the call; the output
        // slice carries its own length.
        unsafe { GetVolumePathNameW(PCWSTR(path_wide.as_ptr()), &mut volume) }.ok()?;

        let mut serial = 0u32;
        // SAFETY: `volume` is NUL-terminated by the previous call; `serial` is a
        // valid, writable u32 and every other output is omitted.
        unsafe {
            GetVolumeInformationW(
                PCWSTR(volume.as_ptr()),
                None,
                Some(&mut serial),
                None,
                None,
                None,
            )
        }
        .ok()?;
        Some(serial)
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
        // `dwReserved0` is only meaningful when the attribute is set.
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
    fn names_stop_at_the_first_nul() {
        let mut buffer = [0u16; 8];
        buffer[..3].copy_from_slice(&w("abc"));
        assert_eq!(until_nul(&buffer), w("abc").as_slice());
        assert_eq!(until_nul(&w("full")), w("full").as_slice());
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
    fn the_search_pattern_is_verbatim_wildcarded_and_terminated() {
        let pattern = search_pattern(&w(r"C:\data"));
        assert_eq!(pattern.last(), Some(&0));
        assert_eq!(s(until_nul(&pattern)), r"\\?\C:\data\*");

        // A volume root already ends in a separator.
        assert_eq!(s(until_nul(&search_pattern(&w(r"C:\")))), r"\\?\C:\*");
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
}
