//! Durable storage for scans: SQLite schema, migrations, batched writes, queries.
//!
//! One database, one schema, both platforms (PLAN §5). The catalog holds the
//! read connection; a scan writes through [`EntryWriter`] on its own connection so
//! that WAL keeps reads unblocked during a batch insert.

#![forbid(unsafe_code)]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod arena;
pub mod db;
pub mod error;
pub mod model;
pub mod query;
pub mod rollup;
pub mod schema;
pub mod util;
pub mod writer;

pub use arena::{Arena, Node, NodeIndex, NIL};
pub use db::Catalog;
pub use error::{CatalogError, Result};
pub use model::{CategoryTotal, Platform, ScanSession, SizeCursor, StoredEntry};
pub use writer::{EntryWriter, DEFAULT_BATCH_SIZE};
