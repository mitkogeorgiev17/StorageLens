//! Trash, manifest, and undo (#30). Every test moves temp files into a
//! "Trash" folder inside its own temp dir — never the real Trash.

#![cfg(unix)]

use std::cell::Cell;
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::os::unix::fs::{symlink, MetadataExt, PermissionsExt};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

use storagelens_core::{EntryKind, FileStamp};
use storagelens_safety::bin::{locate_in, restore_by_rename};
use storagelens_safety::{
    execute, execute_with, undo, Bin, Guard, Item, Manifest, ManifestError, Status, Undone, Wanted,
};

const NOW: i64 = 1_793_456_701;

/// Stat like the platform crate: never follow a final link.
fn stamp(path: &Path) -> io::Result<FileStamp> {
    let metadata = fs::symlink_metadata(path)?;
    let file_type = metadata.file_type();
    let kind = if file_type.is_symlink() {
        EntryKind::Symlink
    } else if file_type.is_dir() {
        EntryKind::Dir
    } else {
        EntryKind::File
    };
    Ok(FileStamp {
        kind,
        size_logical: metadata.len(),
        modified: metadata.mtime(),
        dev: metadata.dev() as u32,
        file_id: metadata.ino(),
        attrs: 0,
    })
}

/// A Trash that is a folder: collisions get " 2", " 3", … like the Finder's.
struct FolderBin {
    dir: PathBuf,
    /// Panic on this trash call (1-based), to simulate a crash.
    crash_on: Option<usize>,
    calls: Cell<usize>,
}

impl FolderBin {
    fn new(dir: PathBuf) -> Self {
        fs::create_dir_all(&dir).expect("trash dir");
        Self {
            dir,
            crash_on: None,
            calls: Cell::new(0),
        }
    }
}

impl Bin for FolderBin {
    fn trash(&self, path: &Path) -> io::Result<()> {
        self.calls.set(self.calls.get() + 1);
        assert_ne!(Some(self.calls.get()), self.crash_on, "simulated crash");
        let name = path
            .file_name()
            .expect("name")
            .to_string_lossy()
            .into_owned();
        let mut target = self.dir.join(&name);
        let mut n = 1;
        while fs::symlink_metadata(&target).is_ok() {
            n += 1;
            target = self.dir.join(format!("{name} {n}"));
        }
        fs::rename(path, target)
    }

    fn locate(&self, wanted: &[Wanted<'_>]) -> io::Result<Vec<Option<String>>> {
        locate_in(std::slice::from_ref(&self.dir), wanted)
    }

    fn restore(&self, location: &str, original: &Path) -> io::Result<()> {
        restore_by_rename(Path::new(location), original)
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    manifests: PathBuf,
    bin: FolderBin,
    guard: Guard,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = fs::canonicalize(dir.path()).expect("canonical");
    let root = base.join("scanned");
    let manifests = base.join("manifests");
    fs::create_dir_all(root.join("Cache/deep")).expect("mkdir");
    fs::create_dir_all(root.join("Other")).expect("mkdir");
    fs::create_dir(&manifests).expect("mkdir");
    fs::write(root.join("old.iso"), vec![7u8; 10_000]).expect("write");
    fs::write(root.join("Cache/blob"), b"cache").expect("write");
    fs::write(root.join("Cache/deep/x"), b"deep").expect("write");
    fs::write(root.join("Other/old.iso"), b"same name, other folder").expect("write");
    symlink(root.join("old.iso"), root.join("alias")).expect("link");
    let guard = Guard::new(&root, None).expect("guard");
    Fixture {
        bin: FolderBin::new(base.join("Trash")),
        _dir: dir,
        root,
        manifests,
        guard,
    }
}

impl Fixture {
    fn item(&self, relative: &str) -> Item {
        let path = self.root.join(relative);
        Item {
            stamp: stamp(&path).expect("stamp"),
            bytes: 4096,
            rule_id: Some("test.rule".to_owned()),
            path,
        }
    }

    fn items(&self) -> Vec<Item> {
        ["old.iso", "Cache", "Other/old.iso", "alias"]
            .iter()
            .map(|relative| self.item(relative))
            .collect()
    }
}

/// Every path below `root` with its kind, bytes, mtime, and inode. A
/// folder's mtime is left out: moving a child out and back changes it.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, (bool, Vec<u8>, i64, u64)> {
    let mut out = BTreeMap::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).expect("read_dir") {
            let path = entry.expect("entry").path();
            let metadata = fs::symlink_metadata(&path).expect("lstat");
            let bytes = if metadata.is_file() {
                fs::read(&path).expect("read")
            } else if metadata.file_type().is_symlink() {
                fs::read_link(&path)
                    .expect("readlink")
                    .into_os_string()
                    .into_encoded_bytes()
            } else {
                stack.push(path.clone());
                Vec::new()
            };
            let relative = path.strip_prefix(root).expect("below root").to_path_buf();
            out.insert(
                relative,
                (
                    metadata.is_dir(),
                    bytes,
                    if metadata.is_dir() {
                        0
                    } else {
                        metadata.mtime()
                    },
                    metadata.ino(),
                ),
            );
        }
    }
    out
}

#[test]
fn delete_then_undo_round_trips_exactly() {
    let f = fixture();
    let before = snapshot(&f.root);
    let items = f.items();

    let outcome = execute(&f.guard, &items, &f.manifests, &f.bin, &stamp, NOW).expect("execute");
    assert_eq!(outcome.count(Status::Trashed), 4, "{:#?}", outcome.items);
    assert_eq!(outcome.trashed_bytes, 4 * 4096);
    assert_eq!(outcome.manifest_warning, None);
    assert_eq!(
        outcome.manifest.file_name().and_then(|n| n.to_str()),
        Some("trash-20261031T142501Z.json")
    );
    for item in &items {
        assert!(
            fs::symlink_metadata(&item.path).is_err(),
            "{:?} still there",
            item.path
        );
    }
    assert!(f.root.join("old.iso").metadata().is_err());
    assert!(
        f.bin.dir.join("old.iso 2").exists(),
        "the second old.iso was renamed in the Trash"
    );

    let stored = Manifest::read(&outcome.manifest).expect("manifest");
    assert_eq!(stored.items, outcome.items);
    assert_eq!(stored.root, f.root.to_str().expect("utf-8"));
    assert_eq!(stored.created_at, NOW);

    let report = undo(&outcome.manifest, &f.bin, NOW + 60).expect("undo");
    assert_eq!(report.count(&Undone::Restored), 4, "{:#?}", report.items);
    assert_eq!(report.manifest_warning, None);
    assert_eq!(
        snapshot(&f.root),
        before,
        "every path, byte, mtime, and inode is back"
    );
    assert_eq!(fs::read_dir(&f.bin.dir).expect("trash").count(), 0);

    let again = undo(&outcome.manifest, &f.bin, NOW + 120).expect("undo again");
    assert_eq!(again.count(&Undone::AlreadyRestored), 4);
    let stored = Manifest::read(&outcome.manifest).expect("manifest");
    assert!(stored
        .items
        .iter()
        .all(|item| item.restored_at == Some(NOW + 60)));
}

#[test]
fn an_item_missing_from_the_trash_is_reported_not_fatal() {
    let f = fixture();
    let outcome =
        execute(&f.guard, &f.items(), &f.manifests, &f.bin, &stamp, NOW).expect("execute");
    // "Empty the Trash" for one item.
    fs::remove_file(f.bin.dir.join("old.iso")).expect("remove from test trash");

    let report = undo(&outcome.manifest, &f.bin, NOW).expect("undo");
    assert_eq!(report.items[0].result, Undone::NotInTrash);
    assert_eq!(report.count(&Undone::Restored), 3);
    assert!(f.root.join("Cache/deep/x").exists());
}

#[test]
fn undo_never_overwrites_what_is_there_now() {
    let f = fixture();
    let outcome = execute(
        &f.guard,
        &[f.item("old.iso")],
        &f.manifests,
        &f.bin,
        &stamp,
        NOW,
    )
    .expect("execute");
    fs::write(f.root.join("old.iso"), b"a new file").expect("write");

    let report = undo(&outcome.manifest, &f.bin, NOW).expect("undo");
    assert_eq!(report.items[0].result, Undone::Occupied);
    assert_eq!(
        fs::read(f.root.join("old.iso")).expect("read"),
        b"a new file"
    );
    assert!(
        f.bin.dir.join("old.iso").exists(),
        "the trashed file stays in the Trash"
    );
}

#[test]
fn a_manifest_write_failure_moves_nothing() {
    let f = fixture();
    let not_a_folder = f.root.join("old.iso");
    let error = execute(&f.guard, &f.items(), &not_a_folder, &f.bin, &stamp, NOW)
        .expect_err("cannot write the manifest");
    assert!(matches!(error, ManifestError::Write { .. }));
    assert!(error.to_string().contains("could not write"));
    assert_eq!(f.bin.calls.get(), 0, "the Trash was never called");
    for item in f.items() {
        assert!(fs::symlink_metadata(&item.path).is_ok());
    }
}

#[test]
fn a_crash_mid_deletion_can_still_be_undone() {
    let mut f = fixture();
    f.bin.crash_on = Some(3);
    let before = snapshot(&f.root);
    let items = f.items();
    let crashed = catch_unwind(AssertUnwindSafe(|| {
        execute(&f.guard, &items, &f.manifests, &f.bin, &stamp, NOW)
    }));
    assert!(crashed.is_err(), "the bin crashed on the third item");
    assert!(
        fs::symlink_metadata(&items[0].path).is_err(),
        "two items moved before the crash"
    );

    // The manifest on disk is the first write: every item still planned.
    let manifest = f.manifests.join("trash-20261031T142501Z.json");
    let stored = Manifest::read(&manifest).expect("manifest survives");
    assert!(stored
        .items
        .iter()
        .all(|item| item.status == Status::Planned));

    let report = undo(&manifest, &f.bin, NOW).expect("undo");
    assert_eq!(report.count(&Undone::Restored), 2, "{:#?}", report.items);
    assert_eq!(report.count(&Undone::NotTrashed), 2, "the rest never moved");
    assert_eq!(snapshot(&f.root), before);
}

#[test]
fn refused_skipped_and_failed_items_are_recorded_and_left_alone() {
    let f = fixture();
    let mut changed = f.item("Cache/blob");
    fs::write(&changed.path, b"changed after the scan").expect("write");
    let refused = Item {
        path: f.root.join("../manifests"),
        ..f.item("old.iso")
    };
    let gone = Item {
        path: f.root.join("Other/missing"),
        ..f.item("Other/old.iso")
    };
    changed.rule_id = None;
    // A link inside the root to the manifests folder outside it.
    symlink(&f.manifests, f.root.join("escape")).expect("link");
    let escape = Item {
        path: f.root.join("escape"),
        ..f.item("old.iso")
    };
    let items = [refused, changed, gone, escape];

    let outcome = execute(&f.guard, &items, &f.manifests, &f.bin, &stamp, NOW).expect("execute");
    let statuses: Vec<Status> = outcome.items.iter().map(|item| item.status).collect();
    assert_eq!(
        statuses,
        [
            Status::Refused,
            Status::Skipped,
            Status::Skipped,
            Status::Refused
        ]
    );
    assert!(outcome.items[0]
        .reason
        .as_deref()
        .is_some_and(|r| r.contains("..")));
    assert!(outcome.items[1]
        .reason
        .as_deref()
        .is_some_and(|r| r.contains("size")));
    assert!(outcome.items[2]
        .reason
        .as_deref()
        .is_some_and(|r| r.contains("no longer there")));
    assert_eq!(outcome.trashed_bytes, 0);
    assert_eq!(f.bin.calls.get(), 0);

    let report = undo(&outcome.manifest, &f.bin, NOW).expect("undo");
    assert!(outcome.items[3]
        .reason
        .as_deref()
        .is_some_and(|r| r.contains("outside")));
    assert_eq!(report.count(&Undone::NotTrashed), 4);
}

/// A bin whose every operation fails.
struct BrokenBin;

impl Bin for BrokenBin {
    fn trash(&self, _: &Path) -> io::Result<()> {
        Err(io::Error::other("the bin is full"))
    }
    fn locate(&self, _: &[Wanted<'_>]) -> io::Result<Vec<Option<String>>> {
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    }
    fn restore(&self, _: &str, _: &Path) -> io::Result<()> {
        Err(io::Error::other("cannot move it back"))
    }
}

#[test]
fn trash_and_search_failures_are_reported_per_item() {
    let f = fixture();
    let outcome = execute(
        &f.guard,
        &[f.item("old.iso")],
        &f.manifests,
        &BrokenBin,
        &stamp,
        NOW,
    )
    .expect("execute");
    assert_eq!(outcome.items[0].status, Status::Failed);
    assert_eq!(outcome.items[0].reason.as_deref(), Some("the bin is full"));
    assert!(f.root.join("old.iso").exists());

    // A deletion that worked, then a Trash that cannot be searched.
    let worked = execute(
        &f.guard,
        &[f.item("old.iso")],
        &f.manifests,
        &f.bin,
        &stamp,
        NOW,
    )
    .expect("execute");
    assert_ne!(
        worked.manifest, outcome.manifest,
        "same second, new manifest name"
    );
    assert!(worked
        .manifest
        .to_string_lossy()
        .ends_with("142501Z-2.json"));
    let report = undo(&worked.manifest, &BrokenBin, NOW).expect("undo");
    assert!(matches!(&report.items[0].result, Undone::Failed(why) if why.contains("searched")));
}

/// Finds everything, restores nothing.
struct StuckBin(FolderBin);

impl Bin for StuckBin {
    fn trash(&self, path: &Path) -> io::Result<()> {
        self.0.trash(path)
    }
    fn locate(&self, wanted: &[Wanted<'_>]) -> io::Result<Vec<Option<String>>> {
        self.0.locate(wanted)
    }
    fn restore(&self, location: &str, original: &Path) -> io::Result<()> {
        BrokenBin.restore(location, original)
    }
}

#[test]
fn a_failed_move_back_is_reported() {
    let f = fixture();
    let bin = StuckBin(FolderBin::new(f.bin.dir.clone()));
    let outcome = execute(
        &f.guard,
        &[f.item("old.iso")],
        &f.manifests,
        &bin,
        &stamp,
        NOW,
    )
    .expect("execute");
    let report = undo(&outcome.manifest, &bin, NOW).expect("undo");
    assert_eq!(
        report.items[0].result,
        Undone::Failed("cannot move it back".to_owned())
    );
}

/// Locks the manifest folder on its first move, so the final write fails.
struct LockingBin<'a>(&'a FolderBin, &'a Path);

impl Bin for LockingBin<'_> {
    fn trash(&self, path: &Path) -> io::Result<()> {
        fs::set_permissions(self.1, fs::Permissions::from_mode(0o555))?;
        self.0.trash(path)
    }
    fn locate(&self, wanted: &[Wanted<'_>]) -> io::Result<Vec<Option<String>>> {
        self.0.locate(wanted)
    }
    fn restore(&self, location: &str, original: &Path) -> io::Result<()> {
        self.0.restore(location, original)
    }
}

#[test]
fn a_failed_final_manifest_write_is_a_warning_and_undo_still_works() {
    let f = fixture();
    let bin = LockingBin(&f.bin, &f.manifests);
    let outcome = execute(
        &f.guard,
        &[f.item("old.iso")],
        &f.manifests,
        &bin,
        &stamp,
        NOW,
    )
    .expect("execute");
    assert_eq!(outcome.items[0].status, Status::Trashed);
    assert!(outcome.manifest_warning.is_some());

    let report = undo(&outcome.manifest, &bin, NOW).expect("undo from the first write");
    assert_eq!(report.items[0].result, Undone::Restored);
    assert!(
        report.manifest_warning.is_some(),
        "the manifest folder is still locked"
    );
    fs::set_permissions(&f.manifests, fs::Permissions::from_mode(0o755)).expect("unlock");
    assert!(f.root.join("old.iso").exists());
}

#[test]
fn unreadable_and_foreign_manifests_are_errors() {
    let f = fixture();
    let missing = f.manifests.join("trash-missing.json");
    assert!(matches!(
        undo(&missing, &f.bin, NOW),
        Err(ManifestError::Read { .. })
    ));

    let garbage = f.manifests.join("garbage.json");
    fs::write(&garbage, b"{not json").expect("write");
    let error = undo(&garbage, &f.bin, NOW).expect_err("garbage");
    assert!(matches!(error, ManifestError::Invalid { .. }));
    assert!(error.to_string().contains("not valid"));

    let future = f.manifests.join("future.json");
    fs::write(
        &future,
        br#"{"version":2,"createdAt":0,"root":"/","items":[]}"#,
    )
    .expect("write");
    let error = undo(&future, &f.bin, NOW).expect_err("future");
    assert!(error.to_string().contains("version 2"));
    assert!(matches!(
        undo(&missing, &f.bin, NOW).expect_err("missing").to_string(),
        text if text.contains("could not read")
    ));
}

#[test]
fn locating_skips_missing_folders_and_fails_on_unreadable_ones() {
    let f = fixture();
    let item = f.item("old.iso");
    let wanted = [Wanted {
        original: &item.path,
        stamp: &item.stamp,
        trashed_after: NOW,
    }];
    let missing = f.bin.dir.join("nope");
    assert_eq!(locate_in(&[missing], &wanted).expect("skips"), [None]);

    let locked = f.bin.dir.join("locked");
    fs::create_dir(&locked).expect("mkdir");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");
    let result = locate_in(std::slice::from_ref(&locked), &wanted);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("chmod back");
    assert_eq!(
        result.expect_err("unreadable").kind(),
        io::ErrorKind::PermissionDenied
    );

    assert_eq!(
        restore_by_rename(&f.bin.dir, &f.root.join("old.iso"))
            .expect_err("taken")
            .kind(),
        io::ErrorKind::AlreadyExists
    );
}

#[test]
fn a_restore_recreates_a_missing_folder() {
    let f = fixture();
    let outcome = execute(
        &f.guard,
        &[f.item("Cache/deep/x")],
        &f.manifests,
        &f.bin,
        &stamp,
        NOW,
    )
    .expect("execute");
    fs::remove_dir_all(f.root.join("Cache")).expect("remove test folder");
    let report = undo(&outcome.manifest, &f.bin, NOW).expect("undo");
    assert_eq!(report.items[0].result, Undone::Restored);
    assert_eq!(
        fs::read(f.root.join("Cache/deep/x")).expect("read"),
        b"deep"
    );
}

#[test]
fn a_cancelled_deletion_stops_cleanly_and_can_be_undone() {
    let f = fixture();
    let before = snapshot(&f.root);
    let items = f.items();
    let mut seen = Vec::new();
    let outcome = execute_with(
        &f.guard,
        &items,
        &f.manifests,
        &f.bin,
        &stamp,
        NOW,
        &mut |index| {
            seen.push(index);
            index < 2
        },
    )
    .expect("execute");
    assert_eq!(seen, [0, 1, 2], "asked before each item until it said stop");
    let statuses: Vec<Status> = outcome.items.iter().map(|item| item.status).collect();
    assert_eq!(
        statuses,
        [
            Status::Trashed,
            Status::Trashed,
            Status::Cancelled,
            Status::Cancelled
        ]
    );
    assert!(outcome.items[2]
        .reason
        .as_deref()
        .is_some_and(|r| r.contains("cancelled")));
    assert!(
        fs::symlink_metadata(&items[2].path).is_ok(),
        "cancelled items never move"
    );
    assert_eq!(
        Manifest::read(&outcome.manifest).expect("manifest").items,
        outcome.items
    );

    let report = undo(&outcome.manifest, &f.bin, NOW).expect("undo");
    assert_eq!(report.count(&Undone::Restored), 2);
    assert_eq!(report.count(&Undone::NotTrashed), 2);
    assert_eq!(snapshot(&f.root), before);
}
