//! Deletion manifests (#30, PLAN §15.5): `trash-<timestamp>.json`, written
//! **before** anything moves.
//!
//! A manifest lists every item of a deletion: its original path, size, rule,
//! and the stamp the scan recorded (kind, volume, file id, size, mtime). It is
//! written once with every item `planned`, before the first move — if that
//! write fails, nothing moves — and again with each item's outcome after the
//! last. Both writes are atomic (a temp file, flushed to disk, renamed over the
//! target), so a crash leaves either the old manifest or the new one, never a
//! torn file.
//!
//! The stamp is what makes a crash survivable: undo finds a trashed item by
//! its volume and file id (a move into the Trash keeps both), so items moved
//! after the first write and before a crash are restored even though the
//! manifest still calls them `planned`.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use storagelens_core::FileStamp;

/// The manifest format this build writes and reads.
pub const MANIFEST_VERSION: u32 = 1;

/// What happened to one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Status {
    /// Not yet attempted. After a crash, possibly moved.
    Planned,
    /// Moved to the Trash.
    Trashed,
    /// Refused by the path guard; never moved.
    Refused,
    /// Skipped by the preflight re-stat; never moved.
    Skipped,
    /// The move was attempted and failed.
    Failed,
    /// The deletion was cancelled before this item; never moved.
    Cancelled,
}

/// One item of a deletion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestItem {
    pub path: PathBuf,
    /// Allocated bytes, as the receipt showed them.
    pub bytes: u64,
    pub rule_id: Option<String>,
    /// What the scan recorded; the preflight compares against it and undo
    /// finds the item in the Trash by it.
    pub stamp: FileStamp,
    pub status: Status,
    /// Why it was refused, skipped, or failed.
    pub reason: Option<String>,
    /// When undo put it back, unix seconds.
    pub restored_at: Option<i64>,
}

/// A whole deletion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    /// Unix seconds when the deletion started.
    pub created_at: i64,
    /// The scanned root the guard allowed.
    pub root: String,
    pub items: Vec<ManifestItem>,
}

/// A manifest could not be written or read.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("could not write the deletion manifest {path}: {source}")]
    Write { path: PathBuf, source: io::Error },
    #[error("could not read the deletion manifest {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("the deletion manifest {path} is not valid: {message}")]
    Invalid { path: PathBuf, message: String },
}

impl Manifest {
    /// Write to `path` atomically: a sibling temp file, flushed, renamed over.
    pub fn write(&self, path: &Path) -> Result<(), ManifestError> {
        let temp = path.with_extension("json.tmp");
        let written = (|| {
            // Serialising these plain structs cannot fail; if it ever did, the
            // error is reported like any other write failure.
            let text = serde_json::to_vec_pretty(self).map_err(io::Error::from)?;
            let mut file = fs::File::create(&temp)?;
            file.write_all(&text)?;
            file.sync_all()?;
            fs::rename(&temp, path)
        })();
        written.map_err(|source| ManifestError::Write {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Read and validate the manifest at `path`.
    pub fn read(path: &Path) -> Result<Self, ManifestError> {
        let text = fs::read(path).map_err(|source| ManifestError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        let invalid = |message: String| ManifestError::Invalid {
            path: path.to_path_buf(),
            message,
        };
        let manifest: Self =
            serde_json::from_slice(&text).map_err(|error| invalid(error.to_string()))?;
        if manifest.version != MANIFEST_VERSION {
            return Err(invalid(format!(
                "version {} is not supported (this build reads version {MANIFEST_VERSION})",
                manifest.version
            )));
        }
        Ok(manifest)
    }
}

/// A new manifest path in `dir`: `trash-20261031T142501Z.json`, with `-2`,
/// `-3`, … if a deletion in the same second already used the name.
pub fn new_path(dir: &Path, unix_seconds: i64) -> PathBuf {
    let stamp = utc(unix_seconds);
    let mut path = dir.join(format!("trash-{stamp}.json"));
    let mut n = 1;
    while path.exists() {
        n += 1;
        path = dir.join(format!("trash-{stamp}-{n}.json"));
    }
    path
}

/// `YYYYMMDDTHHMMSSZ` for unix seconds (proleptic Gregorian, UTC).
pub fn utc(unix_seconds: i64) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let seconds = unix_seconds.rem_euclid(86_400);
    // Howard Hinnant's days-to-civil.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        seconds / 3_600,
        seconds / 60 % 60,
        seconds % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_utc() {
        assert_eq!(utc(0), "19700101T000000Z");
        assert_eq!(utc(951_782_400), "20000229T000000Z");
        assert_eq!(utc(1_793_456_701), "20261031T142501Z");
        assert_eq!(utc(-1), "19691231T235959Z");
    }
}
