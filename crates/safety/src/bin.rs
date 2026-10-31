//! Bins: where [`crate::trash::execute`] moves things, and how undo finds them.
//!
//! - **macOS.** Items go to the Trash through `NSFileManager.trashItemAtURL`
//!   (the `trash` crate), which needs no Automation permission. The Trash is
//!   `~/.Trash` for the home volume and `<volume>/.Trashes/<uid>` for others.
//!   The crate does not say where an item landed (the Finder renames
//!   collisions: `report 2.pdf`, `report 14-25-01.pdf`), but a move within a
//!   volume keeps the inode, so undo finds each item by its volume and file
//!   id in one pass over the Trash folders. Reading `~/.Trash` needs Full Disk
//!   Access; without it, undo reports that the Trash could not be searched.
//! - **Windows.** Items go to the Recycle Bin (`SHFileOperation` / `IFileOperation`
//!   via the crate), and undo uses the crate's Recycle Bin listing and restore.
//!   An item is matched by its original path and deletion time.
//!
//! The folder helpers here, [`locate_in`] and [`restore_by_rename`], are
//! also what tests use, with a Trash folder inside a temp dir.

use std::fs;
use std::io;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

#[cfg(unix)]
use crate::trash::Wanted;

/// Find each wanted item among the top-level entries of `dirs` by its volume
/// and file id. A folder that does not exist is skipped; one that cannot be
/// read is an error.
#[cfg(unix)]
pub fn locate_in(dirs: &[PathBuf], wanted: &[Wanted<'_>]) -> io::Result<Vec<Option<String>>> {
    use std::collections::HashMap;
    use std::os::unix::fs::MetadataExt;

    let mut index = HashMap::new();
    for dir in dirs {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        // An entry that vanishes while it is listed is simply not there.
        for (path, metadata) in entries
            .filter_map(Result::ok)
            .filter_map(|entry| Some((entry.path(), entry.metadata().ok()?)))
        {
            index.insert((metadata.dev() as u32, metadata.ino()), path);
        }
    }
    Ok(wanted
        .iter()
        .map(|item| {
            index
                .get(&(item.stamp.dev, item.stamp.file_id))
                .and_then(|path| path.to_str().map(str::to_owned))
        })
        .collect())
}

/// Move `location` back to `original`, recreating its folder if needed.
/// Refuses (`AlreadyExists`) if anything is at `original`: nothing is ever
/// overwritten.
pub fn restore_by_rename(location: &Path, original: &Path) -> io::Result<()> {
    if fs::symlink_metadata(original).is_ok() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} is taken", original.display()),
        ));
    }
    fs::create_dir_all(original.parent().unwrap_or(original))?;
    fs::rename(location, original)
}

#[cfg(target_os = "macos")]
pub use macos::SystemBin;
#[cfg(windows)]
pub use windows::SystemBin;

#[cfg(target_os = "macos")]
mod macos {
    use std::collections::BTreeSet;
    use std::fs;
    use std::io;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};

    use super::{locate_in, restore_by_rename};
    use crate::trash::{Bin, Wanted};

    /// The macOS Trash.
    #[derive(Debug, Clone)]
    pub struct SystemBin {
        home: PathBuf,
    }

    impl SystemBin {
        /// The Trash of the user whose home is `home`.
        pub fn new(home: &Path) -> Self {
            Self {
                home: home.to_path_buf(),
            }
        }

        /// Every Trash folder an item from `wanted` can be in.
        pub fn trash_folders(&self, wanted: &[Wanted<'_>]) -> Vec<PathBuf> {
            let uid = fs::metadata(&self.home).map(|m| m.uid()).unwrap_or(0);
            let mut folders = BTreeSet::from([self.home.join(".Trash")]);
            for item in wanted {
                if let Some(volume) = volume_of(item.original) {
                    folders.insert(volume.join(".Trashes").join(uid.to_string()));
                }
            }
            folders.into_iter().collect()
        }
    }

    /// The top folder on the same volume as `path`'s nearest existing ancestor.
    fn volume_of(path: &Path) -> Option<PathBuf> {
        let mut ancestors = path.ancestors().skip(1);
        let (mut top, dev) = ancestors.find_map(|folder| {
            fs::symlink_metadata(folder)
                .ok()
                .map(|m| (folder.to_path_buf(), m.dev()))
        })?;
        for folder in top.clone().ancestors().skip(1) {
            match fs::symlink_metadata(folder) {
                Ok(metadata) if metadata.dev() == dev => top = folder.to_path_buf(),
                _ => break,
            }
        }
        Some(top)
    }

    impl Bin for SystemBin {
        #[cfg_attr(coverage_nightly, coverage(off))]
        fn trash(&self, path: &Path) -> io::Result<()> {
            use trash::macos::{DeleteMethod, TrashContextExtMacos};
            let mut context = trash::TrashContext::new();
            context.set_delete_method(DeleteMethod::NsFileManager);
            context
                .delete(path)
                .map_err(|error| io::Error::other(error.to_string()))
        }

        fn locate(&self, wanted: &[Wanted<'_>]) -> io::Result<Vec<Option<String>>> {
            locate_in(&self.trash_folders(wanted), wanted)
        }

        fn restore(&self, location: &str, original: &Path) -> io::Result<()> {
            restore_by_rename(Path::new(location), original)
        }
    }
}

#[cfg(windows)]
mod windows {
    use std::io;
    use std::path::Path;

    use crate::trash::{Bin, Wanted};

    /// The Windows Recycle Bin.
    #[derive(Debug, Clone, Default)]
    pub struct SystemBin;

    impl SystemBin {
        pub fn new(_home: &Path) -> Self {
            Self
        }
    }

    fn io_error(error: trash::Error) -> io::Error {
        match error {
            trash::Error::RestoreCollision { path, .. } => io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} is taken", path.display()),
            ),
            other => io::Error::other(other.to_string()),
        }
    }

    fn same_path(a: &Path, b: &Path) -> bool {
        a.to_string_lossy()
            .eq_ignore_ascii_case(&b.to_string_lossy())
    }

    impl Bin for SystemBin {
        fn trash(&self, path: &Path) -> io::Result<()> {
            trash::delete(path).map_err(io_error)
        }

        fn locate(&self, wanted: &[Wanted<'_>]) -> io::Result<Vec<Option<String>>> {
            let items = trash::os_limited::list().map_err(io_error)?;
            Ok(wanted
                .iter()
                .map(|want| {
                    items
                        .iter()
                        // The bin's clock and ours may differ by a little.
                        .filter(|item| {
                            same_path(&item.original_path(), want.original)
                                && item.time_deleted >= want.trashed_after - 60
                        })
                        .min_by_key(|item| item.time_deleted)
                        .and_then(|item| item.id.to_str().map(str::to_owned))
                })
                .collect())
        }

        fn restore(&self, location: &str, original: &Path) -> io::Result<()> {
            if std::fs::symlink_metadata(original).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} is taken", original.display()),
                ));
            }
            let item = trash::os_limited::list()
                .map_err(io_error)?
                .into_iter()
                .find(|item| item.id.to_str() == Some(location))
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            trash::os_limited::restore_all([item]).map_err(io_error)
        }
    }
}
