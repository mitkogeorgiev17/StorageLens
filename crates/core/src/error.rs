//! Errors shared by every scanner and by the code that drives them.

use std::path::PathBuf;

use thiserror::Error;

/// Convenience alias used throughout the workspace.
pub type Result<T> = std::result::Result<T, ScanError>;

/// Everything that can go wrong while enumerating a filesystem.
///
/// Scanners are expected to *collect* per-directory failures into
/// [`ScanResult::skipped`](crate::ScanResult::skipped) and keep going. A `ScanError`
/// is reserved for conditions that make the whole scan meaningless: cancellation,
/// an invalid root, or a broken catalog.
#[derive(Debug, Error)]
pub enum ScanError {
    /// The caller asked the scan to stop.
    #[error("scan cancelled")]
    Cancelled,

    /// A directory or file could not be read.
    #[error("permission denied: {}", path.display())]
    PermissionDenied {
        /// Path that was refused.
        path: PathBuf,
    },

    /// The operation exists on the other platform but not this one.
    #[error("unsupported on this platform: {0}")]
    Unsupported(&'static str),

    /// An operating-system call failed.
    #[error("i/o error at {}: {source}", path.display())]
    Io {
        /// Path being operated on when the call failed.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },

    /// A `kind` column or FFI constant held a value we do not understand.
    #[error("invalid entry kind: {0}")]
    InvalidEntryKind(i64),

    /// A root path was empty, relative, or otherwise unusable.
    #[error("invalid path: {}", .0.display())]
    InvalidPath(PathBuf),

    /// Persisting entries failed.
    #[error("catalog error: {0}")]
    Catalog(String),

    /// Anything else, with context.
    #[error("{0}")]
    Other(String),
}

impl ScanError {
    /// True when this error means "the caller cancelled", not "the disk failed".
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

impl From<std::io::Error> for ScanError {
    fn from(source: std::io::Error) -> Self {
        Self::Io {
            path: PathBuf::new(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_is_recognised() {
        assert!(ScanError::Cancelled.is_cancelled());
        assert!(!ScanError::Other("nope".into()).is_cancelled());
    }

    #[test]
    fn messages_include_the_path() {
        let err = ScanError::PermissionDenied {
            path: PathBuf::from("/private/secret"),
        };
        assert!(err.to_string().contains("/private/secret"));
    }
}
