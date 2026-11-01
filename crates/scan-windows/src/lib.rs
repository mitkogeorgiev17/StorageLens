//! Windows storage scanner.
//!
//! The kernel-facing implementation is `#[cfg(windows)]`; on other platforms the
//! crate still compiles so that `cargo build --workspace` stays honest on a macOS
//! CI runner, and the pure helpers are unit-tested everywhere.
//!
//! # Layout
//!
//! - [`ffi`] — the bulk directory reader (Windows only) and the pure helpers
//!   around it: the `FILE_ID_BOTH_DIR_INFO` record parser, `\\?\` path prefixes,
//!   `FILETIME` conversion, and the attribute/reparse-tag rules.
//! - `dedupe` — hard-link accounting on the NTFS file id. Pure.
//! - [`registry`] — one startup pass over `HKEY_CLASSES_ROOT` that fills gaps
//!   in the shared category table. Never called during a scan.
//! - [`apps`] — installed-app inventory: uninstall keys, usage evidence,
//!   footprint. Matching is pure; the reads are Windows only.
//! - [`elevation`] — elevation state and the "relaunch elevated" flow.
//! - `skip` — which directories the walker refuses to enter. Pure, so it is
//!   tested on every runner.
//! - [`walk`] / `writer` — the parallel walker behind [`WinScanner`] (Windows
//!   only). Same single-writer design as the macOS scanner, deliberately not
//!   shared code (AGENTS.md §2.4).

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(clippy::undocumented_unsafe_blocks)]

// The pure helpers are only called from the Windows walker, but they are compiled
// and unit-tested on every platform.
#[cfg_attr(not(windows), allow(dead_code))]
pub mod ffi;
#[cfg_attr(not(windows), allow(dead_code))]
mod skip;

pub mod apps;
pub mod background;
#[cfg_attr(not(windows), allow(dead_code))]
mod dedupe;
pub mod elevation;
pub mod leftover_keys;
pub mod registry;
pub mod startup;
pub mod store_apps;
#[cfg(windows)]
pub mod walk;
#[cfg(windows)]
mod writer;

#[cfg(windows)]
pub use ffi::stamp;
#[cfg(windows)]
pub use walk::{WinScanner, MAX_THREADS};
