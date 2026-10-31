//! macOS storage scanner.
//!
//! The real implementation is `#[cfg(target_os = "macos")]`; on other platforms the
//! crate still compiles and reports that the scanner is unsupported. This keeps
//! `cargo build --workspace` honest on a Windows CI runner.
//!
//! Filled in by issues #7–#11.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(clippy::undocumented_unsafe_blocks)]
