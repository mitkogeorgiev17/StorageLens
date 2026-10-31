//! macOS storage scanner.
//!
//! The real implementation is `#[cfg(target_os = "macos")]`; on other platforms the
//! crate still compiles and reports that the scanner is unsupported. This keeps
//! `cargo build --workspace` honest on a Windows CI runner.
//!
//! Filled in by issues #7–#11.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(clippy::undocumented_unsafe_blocks)]

#[cfg(target_os = "macos")]
pub mod volumes;

/// Deep link to the Full Disk Access pane in System Settings.
pub const FULL_DISK_ACCESS_URL: &str =
    "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles";

/// What the Full Disk Access probe found.
///
/// `Unknown` is not a synonym for "denied": it means the probe could not tell,
/// usually because `~/Library/Mail` does not exist on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullDiskAccess {
    /// The process can read TCC-protected locations.
    Granted,
    /// The OS refused access (`EPERM`).
    Denied,
    /// The probe was inconclusive.
    Unknown,
}

#[cfg(target_os = "macos")]
pub use volumes::{full_disk_access, open_full_disk_access_settings, volumes};

/// Enumerate mounted volumes. Always empty off macOS.
#[cfg(not(target_os = "macos"))]
pub fn volumes() -> Vec<storagelens_core::Volume> {
    Vec::new()
}

/// Full Disk Access does not exist off macOS.
#[cfg(not(target_os = "macos"))]
pub fn full_disk_access() -> FullDiskAccess {
    FullDiskAccess::Unknown
}

/// Full Disk Access settings do not exist off macOS.
#[cfg(not(target_os = "macos"))]
pub fn open_full_disk_access_settings() -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Full Disk Access is a macOS-only setting",
    ))
}
