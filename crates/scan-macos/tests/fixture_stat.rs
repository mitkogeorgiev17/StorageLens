//! Compares every field the bulk parser returns with `stat(2)`.
//!
//! `getattrlistbulk`'s buffer layout is under-documented and partially
//! filesystem-specific. The only trustworthy oracle is the kernel's own `stat(2)`,
//! so this test walks the deterministic fixture tree from
//! [`storagelens_core::fixtures`] — hard links, symlinks, a sparse file, unicode
//! and RTL names, a 255-byte name, 200-level nesting, and an unreadable directory —
//! and checks every entry field by field.
//!
//! The fixture's unreadable directory is expected: opening it must fail with
//! `PermissionDenied`, and the test skips its subtree rather than aborting.

#![cfg(target_os = "macos")]

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::os::macos::fs::MetadataExt as _;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use storagelens_core::fixtures::FixtureTree;
use storagelens_scan_macos::attrlist::{
    ATTR_FILE_ALLOCSIZE, ATTR_FILE_LINKCOUNT, ATTR_FILE_TOTALSIZE,
};
use storagelens_scan_macos::{DirReader, ObjType};

#[test]
fn parsed_entries_match_stat_for_the_fixture_tree() {
    let tree = FixtureTree::generate().expect("generate fixture tree");
    let denied = tree.manifest().paths.denied_dir.clone();

    let mut pending = vec![tree.root().to_path_buf()];
    let mut skipped: Vec<PathBuf> = Vec::new();
    let mut entries_checked = 0u64;

    while let Some(dir) = pending.pop() {
        let reader = match DirReader::open(&dir) {
            Ok(reader) => reader,
            Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                skipped.push(dir);
                continue;
            }
            Err(error) => panic!("open {}: {error}", dir.display()),
        };

        let mut names = BTreeSet::new();
        for item in reader {
            let entry = item.unwrap_or_else(|error| panic!("read {}: {error}", dir.display()));
            let path = dir.join(&entry.name);

            let metadata = fs::symlink_metadata(&path)
                .unwrap_or_else(|error| panic!("stat {}: {error}", path.display()));

            let file_name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            assert_eq!(entry.name, file_name, "name for {}", path.display());

            assert_eq!(
                entry.dev,
                metadata.dev() as u32,
                "dev for {}",
                path.display()
            );
            assert_eq!(
                entry.file_id,
                metadata.ino(),
                "inode for {}",
                path.display()
            );
            assert_eq!(
                entry.flags,
                metadata.st_flags(),
                "flags for {}",
                path.display()
            );
            assert_eq!(
                entry.created,
                metadata.st_birthtime(),
                "birth time for {}",
                path.display()
            );
            assert_eq!(
                entry.modified,
                metadata.mtime(),
                "modification time for {}",
                path.display()
            );
            assert_eq!(
                entry.accessed,
                metadata.atime(),
                "access time for {}",
                path.display()
            );

            let file_type = metadata.file_type();
            let expected_kind = if file_type.is_dir() {
                ObjType::Directory
            } else if file_type.is_symlink() {
                ObjType::Symlink
            } else if file_type.is_file() {
                ObjType::Regular
            } else {
                panic!(
                    "fixture created an unexpected file type at {}",
                    path.display()
                );
            };
            assert_eq!(
                entry.obj_type,
                expected_kind,
                "object type for {}",
                path.display()
            );

            if entry.returned.file_has(ATTR_FILE_ALLOCSIZE) {
                assert!(
                    entry.returned.file_has(ATTR_FILE_LINKCOUNT),
                    "link count missing while allocated size is present for {}",
                    path.display()
                );
                assert!(
                    entry.returned.file_has(ATTR_FILE_TOTALSIZE),
                    "total size missing while allocated size is present for {}",
                    path.display()
                );
                assert_eq!(
                    entry.size_alloc,
                    metadata.blocks().saturating_mul(512),
                    "allocated bytes for {}",
                    path.display()
                );
                assert_eq!(
                    entry.size_logical,
                    metadata.len(),
                    "logical bytes for {}",
                    path.display()
                );
                assert_eq!(
                    entry.nlink,
                    metadata.nlink() as u32,
                    "link count for {}",
                    path.display()
                );
            } else {
                // APFS omits the file group for directories: getattrlistbulk has
                // no directory-size attribute. Directory sizes are filled by the
                // walker with one lstat per directory, not by this parser.
                assert!(
                    expected_kind.is_dir(),
                    "file attributes missing for non-directory {}",
                    path.display()
                );
                assert_eq!(entry.size_alloc, 0, "size for {}", path.display());
                assert_eq!(entry.size_logical, 0, "size for {}", path.display());
            }

            if expected_kind.is_dir() {
                pending.push(path);
            }

            names.insert(entry.name);
            entries_checked = entries_checked.saturating_add(1);
        }

        let on_disk: BTreeSet<String> = fs::read_dir(&dir)
            .unwrap_or_else(|error| panic!("read_dir {}: {error}", dir.display()))
            .map(|item| {
                item.unwrap_or_else(|error| panic!("entry in {}: {error}", dir.display()))
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();

        assert_eq!(names, on_disk, "entry names in {}", dir.display());
    }

    assert!(
        entries_checked > 20,
        "expected the fixture tree to contain many entries, saw {entries_checked}"
    );

    match denied {
        Some(denied) => assert_eq!(
            skipped,
            vec![denied],
            "only the fixture's deliberately unreadable directory may be skipped"
        ),
        None => assert!(skipped.is_empty(), "unexpected unreadable directories"),
    }
}
