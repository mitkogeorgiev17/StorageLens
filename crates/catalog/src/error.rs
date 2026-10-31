//! Errors from the catalog layer.

use thiserror::Error;

/// Convenience alias for catalog operations.
pub type Result<T> = std::result::Result<T, CatalogError>;

/// Everything that can go wrong while reading or writing the catalog.
#[derive(Debug, Error)]
pub enum CatalogError {
    /// SQLite returned an error.
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// Filesystem error while creating the database or its directory.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),

    /// The database was written by a newer build than this one.
    #[error("schema version {found} is newer than this build supports ({supported})")]
    SchemaTooNew {
        /// Version found on disk.
        found: u32,
        /// Highest version this build knows how to apply.
        supported: u32,
    },

    /// A scan id was referenced that does not exist.
    #[error("scan session {0} not found")]
    SessionNotFound(i64),

    /// A stored value was not one we recognise.
    #[error("corrupt catalog value: {0}")]
    Corrupt(String),

    /// Anything else, with context.
    #[error("{0}")]
    Other(String),
}
