//! Windows storage scanner.
//!
//! The kernel-facing implementation is `#[cfg(windows)]`; on other platforms the
//! crate still compiles so that `cargo build --workspace` stays honest on a macOS
//! CI runner, and the pure helpers are unit-tested everywhere.
//!
//! # Layout
//!
//! - [`ffi`] — `FindFirstFileExW` directory reader (Windows only) and the pure
//!   helpers around it: `\\?\` path prefixes, `FILETIME` conversion, and the
//!   attribute/reparse-tag rules that decide what an entry is.
//! - `skip` — which directories the walker refuses to enter. Pure, so it is
//!   tested on every runner.
//! - [`walk`] / `writer` — the parallel walker behind [`WinScanner`] (Windows
//!   only). Same single-writer design as the macOS scanner, deliberately not
//!   shared code (AGENTS.md §2.4).
//!
//! Allocated size, file identity, and hard-link dedupe arrive with #13.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(clippy::undocumented_unsafe_blocks)]

// The pure helpers are only called from the Windows walker, but they are compiled
// and unit-tested on every platform.
#[cfg_attr(not(windows), allow(dead_code))]
pub mod ffi;
#[cfg_attr(not(windows), allow(dead_code))]
mod skip;

#[cfg(windows)]
pub mod walk;
#[cfg(windows)]
mod writer;

#[cfg(windows)]
pub use walk::{WinScanner, MAX_THREADS};
