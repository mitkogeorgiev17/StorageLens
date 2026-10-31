//! The Trash service (#30, PLAN §15.1): the only way anything is deleted.
//!
//! [`execute`] takes the items of a confirmed receipt and, in order:
//!
//! 1. writes the manifest with every item `planned` — if that fails, it
//!    returns the error and **nothing has moved**;
//! 2. for each item, immediately before moving it: the path guard, then the
//!    preflight re-stat; an item that fails either is recorded with the
//!    reason and left alone;
//! 3. moves the item to the Trash (never `unlink`);
//! 4. writes the manifest again with every outcome.
//!
//! If the second write fails the moves have happened, so it is reported, not
//! returned as an error: undo still works from the first write (see
//! [`crate::manifest`]).
//!
//! Where things go is a [`Bin`]. The app uses [`crate::bin::SystemBin`] (the
//! macOS Trash or the Windows Recycle Bin); tests use a folder in a temp dir,
//! so no test ever touches the real Trash.

use std::io;
use std::path::{Path, PathBuf};

use storagelens_core::FileStamp;

use crate::guard::{Guard, Refusal};
use crate::manifest::{new_path, Manifest, ManifestError, ManifestItem, Status, MANIFEST_VERSION};
use crate::preflight::{preflight, Skip};

/// An item in the Trash that undo is looking for.
#[derive(Debug, Clone, Copy)]
pub struct Wanted<'a> {
    /// Where it was.
    pub original: &'a Path,
    /// What the scan recorded: a move into the Trash keeps its volume and
    /// file id.
    pub stamp: &'a FileStamp,
    /// Unix seconds; the item was trashed at or after this.
    pub trashed_after: i64,
}

/// Somewhere items can be moved to and brought back from.
pub trait Bin {
    /// Move `path` to the Trash.
    fn trash(&self, path: &Path) -> io::Result<()>;
    /// Where each wanted item is in the Trash now (an opaque location for
    /// [`Bin::restore`]), or `None` if it is not there. One pass for the whole
    /// list, however long.
    fn locate(&self, wanted: &[Wanted<'_>]) -> io::Result<Vec<Option<String>>>;
    /// Move the item at `location` back to `original`, which must be free.
    fn restore(&self, location: &str, original: &Path) -> io::Result<()>;
}

/// One item of a confirmed receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub path: PathBuf,
    pub bytes: u64,
    pub rule_id: Option<String>,
    /// What the scan recorded for `path`.
    pub stamp: FileStamp,
}

/// What a deletion did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The manifest; undo takes this path.
    pub manifest: PathBuf,
    /// Every item, with its status and reason.
    pub items: Vec<ManifestItem>,
    /// Allocated bytes moved to the Trash.
    pub trashed_bytes: u64,
    /// Set when the final manifest write failed. The moves happened and undo
    /// still works from the first write.
    pub manifest_warning: Option<String>,
}

impl Outcome {
    /// How many items ended with `status`.
    pub fn count(&self, status: Status) -> usize {
        self.items
            .iter()
            .filter(|item| item.status == status)
            .count()
    }
}

/// Move `items` to `bin`, guarded, re-stat'ed, and recorded in a new manifest
/// in `manifest_dir`. `stamp` is the platform's re-stat (`scan-*::stamp`) and
/// `now` the current time in unix seconds.
pub fn execute(
    guard: &Guard,
    items: &[Item],
    manifest_dir: &Path,
    bin: &dyn Bin,
    stamp: &dyn Fn(&Path) -> io::Result<FileStamp>,
    now: i64,
) -> Result<Outcome, ManifestError> {
    execute_with(guard, items, manifest_dir, bin, stamp, now, &mut |_| true)
}

/// [`execute`], calling `before(index)` ahead of each item. When it returns
/// `false` the deletion stops there: that item and every later one are
/// recorded as [`Status::Cancelled`], never moved, and the manifest is
/// finished as usual — a cancelled deletion is as consistent, and as
/// undoable, as a complete one.
pub fn execute_with(
    guard: &Guard,
    items: &[Item],
    manifest_dir: &Path,
    bin: &dyn Bin,
    stamp: &dyn Fn(&Path) -> io::Result<FileStamp>,
    now: i64,
    before: &mut dyn FnMut(usize) -> bool,
) -> Result<Outcome, ManifestError> {
    let mut manifest = Manifest {
        version: MANIFEST_VERSION,
        created_at: now,
        root: guard.root().to_owned(),
        items: items
            .iter()
            .map(|item| ManifestItem {
                path: item.path.clone(),
                bytes: item.bytes,
                rule_id: item.rule_id.clone(),
                stamp: item.stamp,
                status: Status::Planned,
                reason: None,
                restored_at: None,
            })
            .collect(),
    };
    let path = new_path(manifest_dir, now);
    manifest.write(&path)?;

    let mut trashed_bytes = 0u64;
    let mut cancelled = false;
    for (index, item) in manifest.items.iter_mut().enumerate() {
        cancelled = cancelled || !before(index);
        let (status, reason) = if cancelled {
            (
                Status::Cancelled,
                Some("the deletion was cancelled first".to_owned()),
            )
        } else if let Err(refusal) = guard.check(&item.path) {
            if refusal == Refusal::Unresolvable(io::ErrorKind::NotFound) {
                // Gone since the scan: nothing to refuse, just nothing to move.
                (Status::Skipped, Some(Skip::Missing.to_string()))
            } else {
                (Status::Refused, Some(refusal.to_string()))
            }
        } else if let Err(skip) = preflight(&item.path, &item.stamp, stamp) {
            (Status::Skipped, Some(skip.to_string()))
        } else {
            match bin.trash(&item.path) {
                Ok(()) => {
                    trashed_bytes = trashed_bytes.saturating_add(item.bytes);
                    (Status::Trashed, None)
                }
                Err(error) => (Status::Failed, Some(error.to_string())),
            }
        };
        item.status = status;
        item.reason = reason;
    }

    let manifest_warning = manifest.write(&path).err().map(|error| error.to_string());
    Ok(Outcome {
        manifest: path,
        items: manifest.items,
        trashed_bytes,
        manifest_warning,
    })
}
