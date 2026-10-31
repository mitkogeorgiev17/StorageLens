//! macOS storage scanner.
//!
//! The kernel-facing implementation is `#[cfg(target_os = "macos")]`; on other
//! platforms the crate still compiles so that `cargo build --workspace` stays
//! honest on a Windows CI runner.
//!
//! # Layout
//!
//! - [`attrlist`] — attribute bitmaps and the request the scanner makes.
//! - [`parse`] — byte-exact parser for the `getattrlistbulk` buffer. Deliberately
//!   free of `unsafe` and of platform dependencies, so it is unit-tested on every
//!   CI runner.
//! - [`DirReader`] — the syscall loop that fills a buffer and yields one parsed
//!   entry at a time (macOS only).
//! - [`volumes`] — mounted volumes, capacity, and the Full Disk Access probe
//!   (macOS only).
//! - [`walk`] / `writer` — the parallel walker behind [`MacScanner`] (macOS only).
//!
//! Later issues add dedupe and the system-path skip list (#9), and the accuracy
//! gate (#11).

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(clippy::undocumented_unsafe_blocks)]

pub mod attrlist;
pub mod parse;

#[cfg(target_os = "macos")]
mod ffi;
#[cfg(target_os = "macos")]
pub mod volumes;
#[cfg(target_os = "macos")]
pub mod walk;
#[cfg(target_os = "macos")]
mod writer;

pub use attrlist::{AttrSet, Request};
pub use parse::{ObjType, ParseError, RawEntry};

#[cfg(target_os = "macos")]
pub use ffi::{DirReader, BUFFER_BYTES};
#[cfg(target_os = "macos")]
pub use volumes::{full_disk_access, open_full_disk_access_settings, volumes};
#[cfg(target_os = "macos")]
pub use walk::{MacScanner, MAX_THREADS};

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
