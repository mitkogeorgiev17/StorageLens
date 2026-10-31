//! Shared domain types, categories, grouping, and the `StorageScanner` trait.
//!
//! This crate has **zero platform dependencies** — it must compile unchanged on
//! macOS, Windows, and Linux CI runners.
//!
//! Everything above the scanner trait (schema, rules, rollups, dedupe, safety, UI) is
//! written once against these types.

#![forbid(unsafe_code)]

pub mod error;
pub mod scanner;
pub mod types;

pub use error::{Result, ScanError};
pub use scanner::{CancelToken, EntrySink, StorageScanner};
pub use types::{
    AppInfo, AppStatus, Bytes, Entry, EntryKind, NodeId, PlatformTarget, Progress, ScanOptions,
    ScanResult, ScanStatus, SkipReason, SkippedDir, Volume,
};
