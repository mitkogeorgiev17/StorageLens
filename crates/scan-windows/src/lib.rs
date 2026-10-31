//! Windows storage scanner.
//!
//! The real implementation is `#[cfg(windows)]`; on other platforms the crate still
//! compiles and reports that the scanner is unsupported.
//!
//! Filled in by issues #12–#16.

#![deny(unsafe_op_in_unsafe_fn)]
#![warn(clippy::undocumented_unsafe_blocks)]
