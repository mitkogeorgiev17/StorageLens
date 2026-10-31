//! Undo (#30, PLAN §15.5): put a deletion's items back where they were.
//!
//! Every item the manifest says was trashed — and, after a crash, every item
//! still `planned` that is no longer at its path — is looked for in the Trash
//! by its volume and file id, then moved back to its original path. Nothing
//! is ever overwritten: an item whose path is taken now stays in the Trash.
//! Each item gets an [`Undone`] result; nothing here panics on a missing or
//! emptied Trash.
//!
//! Restored items are marked in the manifest, so undoing twice reports them
//! as already restored instead of looking for them again.

use std::fs;
use std::path::Path;

use crate::manifest::{Manifest, ManifestError, Status};
use crate::trash::{Bin, Wanted};

/// What undo did with one item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Undone {
    /// Back at its original path.
    Restored,
    /// Restored by an earlier undo.
    AlreadyRestored,
    /// Never moved (refused, skipped, failed, or not reached before a crash).
    NotTrashed,
    /// Not in the Trash any more (emptied, or moved by hand).
    NotInTrash,
    /// Something else is at its original path now; it was left in the Trash.
    Occupied,
    /// The Trash could not be searched, or the move back failed.
    Failed(String),
}

/// One item's undo result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoItem {
    pub path: std::path::PathBuf,
    pub result: Undone,
}

/// What an undo did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UndoReport {
    /// Every item of the manifest, in its order.
    pub items: Vec<UndoItem>,
    /// Set when the manifest could not be updated afterwards. The restores
    /// happened; a second undo would look for them again, not find them in
    /// the Trash, and move nothing.
    pub manifest_warning: Option<String>,
}

impl UndoReport {
    /// How many items ended with `result`.
    pub fn count(&self, result: &Undone) -> usize {
        self.items
            .iter()
            .filter(|item| &item.result == result)
            .count()
    }
}

/// Restore what the manifest at `manifest_path` moved. `now` is the current
/// time in unix seconds, recorded on each restored item. Fails only if the
/// manifest cannot be read.
pub fn undo(manifest_path: &Path, bin: &dyn Bin, now: i64) -> Result<UndoReport, ManifestError> {
    let mut manifest = Manifest::read(manifest_path)?;

    // Which items to look for: trashed ones, and planned ones that are gone.
    let mut results: Vec<Option<Undone>> = manifest
        .items
        .iter()
        .map(|item| match item.status {
            _ if item.restored_at.is_some() => Some(Undone::AlreadyRestored),
            Status::Trashed => None,
            Status::Planned if fs::symlink_metadata(&item.path).is_err() => None,
            _ => Some(Undone::NotTrashed),
        })
        .collect();
    let wanted: Vec<usize> = (0..results.len())
        .filter(|&i| results[i].is_none())
        .collect();
    let query: Vec<Wanted<'_>> = wanted
        .iter()
        .map(|&i| Wanted {
            original: &manifest.items[i].path,
            stamp: &manifest.items[i].stamp,
            trashed_after: manifest.created_at,
        })
        .collect();
    let found = bin.locate(&query);
    drop(query);

    match found {
        Err(error) => {
            for &i in &wanted {
                results[i] = Some(Undone::Failed(format!(
                    "the Trash could not be searched: {error}"
                )));
            }
        }
        Ok(locations) => {
            for (&i, location) in wanted.iter().zip(locations) {
                let item = &mut manifest.items[i];
                let result = match location {
                    None => Undone::NotInTrash,
                    Some(_) if fs::symlink_metadata(&item.path).is_ok() => Undone::Occupied,
                    Some(location) => match bin.restore(&location, &item.path) {
                        Ok(()) => {
                            item.restored_at = Some(now);
                            Undone::Restored
                        }
                        Err(error) => Undone::Failed(error.to_string()),
                    },
                };
                results[i] = Some(result);
            }
        }
    }

    let manifest_warning = manifest
        .write(manifest_path)
        .err()
        .map(|error| error.to_string());
    let items = manifest
        .items
        .iter()
        .zip(results)
        .map(|(item, result)| UndoItem {
            path: item.path.clone(),
            result: result.unwrap_or(Undone::Failed("not attempted".to_owned())),
        })
        .collect();
    Ok(UndoReport {
        items,
        manifest_warning,
    })
}
