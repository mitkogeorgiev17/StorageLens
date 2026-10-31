//! Analysis passes over a completed scan: cleanup rules, duplicates, app inventory.
//!
//! Grouping views (#25) live in [`grouping`]; rules, duplicates and the app
//! inventory are filled in by issues #31–#33 and #36–#40. The rules engine
//! (#31) is [`rules`]; macOS app classification (#36) is [`apps_macos`].

#![forbid(unsafe_code)]

pub mod apps_macos;
pub mod grouping;
pub mod rules;

pub use grouping::{build, FileFilter, Group, Grouping, GroupingInput, Risk, SyntheticTree};
