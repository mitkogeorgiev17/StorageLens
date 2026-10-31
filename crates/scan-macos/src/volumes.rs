//! Mounted volume enumeration and Full Disk Access probing.
//!
//! Volumes come from `getmntinfo(3)`, names from `getattrlist(2)` on the mount
//! point. Full Disk Access has no query API: the only honest test is to touch a
//! TCC-protected directory and look at the error.

use std::ffi::{CString, OsStr};
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use storagelens_core::{Bytes, Volume};

use crate::{FullDiskAccess, FULL_DISK_ACCESS_URL};

/// Mount flags that tell Finder to hide a volume.
const HIDDEN_MOUNT_FLAGS: u32 = (libc::MNT_DONTBROWSE | libc::MNT_AUTOMOUNTED) as u32;

/// Big enough for the length word, the fstype word, one `attrreference_t`, and a
/// 255-byte volume name. The kernel writes a short buffer into this size.
const VOLUME_ATTRIBUTE_BUFFER_SIZE: usize = 1024;

/// Enumerate the mounted filesystems Finder would show.
///
/// `purgeable` is deliberately always [`Bytes::ZERO`]. The real value comes from
/// Foundation's `NSURLVolumeAvailableCapacityForImportantUsageKey`, which is
/// unreachable from this pure-Rust crate; the Foundation bridge in the app layer
/// fills it in. `available` comes from `statfs` and already includes purgeable
/// space as the OS reports it.
pub fn volumes() -> Vec<Volume> {
    let mut mounts: *mut libc::statfs = std::ptr::null_mut();

    // SAFETY: `getmntinfo` writes a pointer to an array of `count` `statfs`
    // records into `mounts`. We keep that pointer only for the duration of the
    // slice below, and the array stays valid until the next `getmntinfo` call.
    let count = unsafe { libc::getmntinfo(&mut mounts, libc::MNT_NOWAIT) };
    if count <= 0 || mounts.is_null() {
        return Vec::new();
    }

    // SAFETY: on a positive return value `getmntinfo` guarantees `mounts` points
    // to at least `count` initialised `statfs` records owned by libc. We never
    // mutate through the slice and drop it before the next FFI call.
    let mounts = unsafe { std::slice::from_raw_parts(mounts, count as usize) };

    mounts.iter().filter_map(volume_from_mount).collect()
}

/// Whether the process can read TCC-protected locations.
///
/// The probe is `~/Library/Mail`: `EPERM` there is what a denied Full Disk
/// Access grant looks like. A missing Mail directory is inconclusive, not a
/// denial, so it reports [`FullDiskAccess::Unknown`].
pub fn full_disk_access() -> FullDiskAccess {
    let Some(home) = std::env::var_os("HOME") else {
        return FullDiskAccess::Unknown;
    };

    match fs::read_dir(Path::new(&home).join("Library").join("Mail")) {
        Ok(_) => FullDiskAccess::Granted,
        Err(error) => match error.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::NotADirectory => FullDiskAccess::Unknown,
            io::ErrorKind::PermissionDenied => FullDiskAccess::Denied,
            _ => FullDiskAccess::Unknown,
        },
    }
}

/// Open System Settings on the Full Disk Access pane.
///
/// There is no API to request the permission; the user has to flip the switch,
/// so this only deep-links and returns. Callers should re-probe afterwards.
pub fn open_full_disk_access_settings() -> io::Result<()> {
    let status = Command::new("open").arg(FULL_DISK_ACCESS_URL).status()?;

    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "`open` exited unsuccessfully: {status}"
        )))
    }
}

/// Build a [`Volume`] from one `getmntinfo` record, or `None` when Finder hides it.
fn volume_from_mount(fs: &libc::statfs) -> Option<Volume> {
    let mount_point_bytes = c_string_bytes(&fs.f_mntonname);
    let mount_point = PathBuf::from(OsStr::from_bytes(&mount_point_bytes));
    if mount_point.as_os_str().is_empty() {
        return None;
    }

    let is_root = mount_point == Path::new("/");
    if fs.f_flags & HIDDEN_MOUNT_FLAGS != 0 && !is_root {
        return None;
    }

    let name = volume_name(&mount_point).unwrap_or_else(|| fallback_name(&mount_point));

    // `st_dev` is what `Entry::dev` will hold, so a volume can be matched to its
    // entries. `f_fsid` is the fallback when the mount point cannot be stat'ed.
    let id = fs::metadata(&mount_point)
        .ok()
        .and_then(|metadata| u32::try_from(metadata.dev()).ok())
        .unwrap_or_else(|| fsid_first_word(&fs.f_fsid));

    // `statfs` on macOS has no removable flag; the mount convention is the only
    // signal available.
    let removable = mount_point.as_os_str().as_bytes().starts_with(b"/Volumes/");

    Some(Volume {
        id,
        name,
        mount_point,
        fs_type: String::from_utf8_lossy(&c_string_bytes(&fs.f_fstypename)).into_owned(),
        total: Bytes::new(u64::from(fs.f_bsize).saturating_mul(fs.f_blocks)),
        available: Bytes::new(u64::from(fs.f_bsize).saturating_mul(fs.f_bavail)),
        purgeable: Bytes::ZERO,
        removable,
    })
}

/// Read `ATTR_VOL_NAME` for a mount point, `None` when the kernel says no.
fn volume_name(mount_point: &Path) -> Option<String> {
    let path = CString::new(mount_point.as_os_str().as_bytes()).ok()?;
    let mut attr_list = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_FSTYPE | libc::ATTR_VOL_NAME,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut buffer = [0u8; VOLUME_ATTRIBUTE_BUFFER_SIZE];

    // SAFETY: `path` is a NUL-terminated C string, `attr_list` and `buffer` are
    // valid for writes of their full size, `buffer.len()` is that size, and
    // `getattrlist` does not retain either pointer after returning.
    let result = unsafe {
        libc::getattrlist(
            path.as_ptr(),
            (&mut attr_list as *mut libc::attrlist).cast(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            0,
        )
    };
    if result != 0 {
        return None;
    }

    parse_volume_name(&buffer)
}

/// Parse a volume-attribute buffer returned for
/// `ATTR_VOL_INFO | ATTR_VOL_FSTYPE | ATTR_VOL_NAME`.
///
/// Layout: a native-endian `u32` total length, then the attribute values in
/// ascending bit order, each 4-byte aligned. `ATTR_VOL_FSTYPE` (`0x1`) is a
/// `u32`; `ATTR_VOL_NAME` (`0x2000`) is an [`attrreference_t`] whose data offset
/// is relative to the reference itself and whose length includes the NUL.
///
/// [`attrreference_t`]: libc::attrreference_t
fn parse_volume_name(buffer: &[u8]) -> Option<String> {
    let declared_length = usize::try_from(read_u32(buffer, 0)?).ok()?;
    if declared_length < 16 || declared_length > buffer.len() {
        return None;
    }
    let buffer = buffer.get(..declared_length)?;

    // `ATTR_VOL_FSTYPE` comes first in bit order; we only need it to advance.
    read_u32(buffer, 4)?;

    let reference_at = 8usize;
    let name_offset = read_i32(buffer, reference_at)?;
    let name_length = usize::try_from(read_u32(buffer, reference_at + 4)?).ok()?;
    let name_start = usize::try_from(reference_at as i64 + i64::from(name_offset)).ok()?;
    let name_end = name_start.checked_add(name_length)?;
    let raw = buffer.get(name_start..name_end)?;

    // The length includes the terminating NUL, but tolerate a missing one.
    let end = raw.iter().position(|&byte| byte == 0).unwrap_or(raw.len());
    let name = std::str::from_utf8(&raw[..end]).ok()?;
    if name.is_empty() {
        None
    } else {
        Some(name.to_owned())
    }
}

/// Last-resort display name when `getattrlist` cannot answer.
fn fallback_name(mount_point: &Path) -> String {
    if mount_point == Path::new("/") {
        return String::from("Macintosh HD");
    }

    match mount_point.file_name() {
        Some(name) if !name.is_empty() => name.to_string_lossy().into_owned(),
        _ => mount_point.display().to_string(),
    }
}

/// Mirror of the C `fsid_t`, whose fields libc 0.2 keeps private.
#[repr(C)]
struct Fsid {
    val: [i32; 2],
}

/// First word of an `fsid_t`, used when the mount point cannot be `stat`ed.
fn fsid_first_word(fsid: &libc::fsid_t) -> u32 {
    // SAFETY: `libc::fsid_t` is a `#[repr(C)]` struct containing exactly one
    // `[i32; 2]` field, so `Fsid` has the same size, alignment, and layout, and
    // the returned reference borrows the same storage for the same lifetime.
    let fsid = unsafe { &*(fsid as *const libc::fsid_t).cast::<Fsid>() };
    fsid.val[0] as u32
}

/// Copy a fixed-size C string field into bytes, stopping at the first NUL.
fn c_string_bytes(array: &[libc::c_char]) -> Vec<u8> {
    array
        .iter()
        .copied()
        .take_while(|&byte| byte != 0)
        .map(|byte| byte as u8)
        .collect()
}

fn read_u32(buffer: &[u8], offset: usize) -> Option<u32> {
    let bytes: [u8; 4] = buffer
        .get(offset..offset.checked_add(4)?)?
        .try_into()
        .ok()?;
    Some(u32::from_ne_bytes(bytes))
}

fn read_i32(buffer: &[u8], offset: usize) -> Option<i32> {
    let bytes: [u8; 4] = buffer
        .get(offset..offset.checked_add(4)?)?
        .try_into()
        .ok()?;
    Some(i32::from_ne_bytes(bytes))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn at_least_one_volume_is_reported() {
        let volumes = volumes();
        assert!(
            !volumes.is_empty(),
            "getmntinfo should report at least the root volume"
        );
    }

    #[test]
    fn root_volume_is_consistent() {
        let volumes = volumes();
        let root = volumes
            .iter()
            .find(|volume| volume.mount_point == Path::new("/"))
            .expect("root volume missing");

        assert!(root.total.get() > 0, "root total should be positive");
        assert!(root.used() <= root.total, "used space cannot exceed total");
        assert!(!root.fs_type.is_empty(), "root fs_type should be known");
        assert!(
            root.mount_point.is_absolute(),
            "mount points are absolute paths"
        );
    }

    #[test]
    fn purgeable_is_zero_pending_the_foundation_bridge() {
        for volume in volumes() {
            assert_eq!(
                volume.purgeable,
                Bytes::ZERO,
                "{} reports purgeable space without the Foundation bridge",
                volume.mount_point.display()
            );
        }
    }

    #[test]
    fn full_disk_access_returns_a_known_variant() {
        assert!(matches!(
            full_disk_access(),
            FullDiskAccess::Granted | FullDiskAccess::Denied | FullDiskAccess::Unknown
        ));
    }

    #[test]
    fn full_disk_access_url_matches_the_settings_pane() {
        assert_eq!(
            FULL_DISK_ACCESS_URL,
            "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles"
        );
    }

    #[test]
    fn parses_a_synthetic_volume_name_buffer() {
        let name = b"Test Volume\0";
        let buffer_length = 16 + name.len();
        let mut buffer = vec![0u8; buffer_length];

        buffer[0..4].copy_from_slice(&(buffer_length as u32).to_ne_bytes());
        buffer[4..8].copy_from_slice(&1u32.to_ne_bytes()); // fstypenum, unused here

        let reference_at = 8usize;
        let name_at = 16usize;
        buffer[8..12].copy_from_slice(&((name_at - reference_at) as i32).to_ne_bytes());
        buffer[12..16].copy_from_slice(&(name.len() as u32).to_ne_bytes());
        buffer[16..].copy_from_slice(name);

        assert_eq!(parse_volume_name(&buffer).as_deref(), Some("Test Volume"));
    }

    #[test]
    fn rejects_truncated_volume_name_buffers() {
        assert_eq!(parse_volume_name(&[]), None);
        assert_eq!(parse_volume_name(&[16, 0, 0, 0]), None);

        let mut buffer = [0u8; 16];
        buffer[0..4].copy_from_slice(&16u32.to_ne_bytes());
        buffer[8..12].copy_from_slice(&64i32.to_ne_bytes());
        buffer[12..16].copy_from_slice(&8u32.to_ne_bytes());
        assert_eq!(parse_volume_name(&buffer), None);
    }

    #[test]
    fn fallback_names_cover_root_and_mounts() {
        assert_eq!(fallback_name(Path::new("/")), "Macintosh HD");
        assert_eq!(fallback_name(Path::new("/Volumes/Backup")), "Backup");
    }
}
