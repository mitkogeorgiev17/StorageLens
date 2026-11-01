//! The macOS Trash adapter (#30). Finding and restoring are tested against a
//! fake home with its own `.Trash` in a temp dir. The real move to the real
//! Trash is one ignored test, run by hand:
//! `cargo test -p storagelens-safety --test system_bin -- --ignored`.

#![cfg(target_os = "macos")]

use std::fs;
use std::path::Path;
use std::sync::Arc;

use storagelens_safety::bin::SystemBin;
use storagelens_safety::{Bin, Wanted};
use storagelens_scan_macos::stamp;

/// One made-up volume: everything under `base` is device 1; nothing else exists.
fn temp_volume(base: &Path) -> storagelens_safety::bin::DeviceOf {
    let base = base.to_path_buf();
    Arc::new(move |path: &Path| path.starts_with(&base).then_some(1))
}

#[test]
fn a_renamed_item_is_found_in_the_home_trash_and_put_back() {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = fs::canonicalize(dir.path()).expect("canonical");
    let home = base.join("home");
    let trash = home.join(".Trash");
    fs::create_dir_all(&trash).expect("mkdir");
    let original = base.join("report.pdf");
    fs::write(&original, b"pdf").expect("write");
    let scanned = stamp(&original).expect("stamp");
    // What the Trash does on a name collision.
    fs::rename(&original, trash.join("report 14-25-01.pdf")).expect("move");

    let bin = SystemBin::with_devices(&home, temp_volume(&base));
    let wanted = [Wanted {
        original: &original,
        stamp: &scanned,
        trashed_after: 0,
    }];
    let folders = bin.trash_folders(&wanted);
    assert!(folders.contains(&trash));
    assert!(
        folders.iter().any(|folder| folder.ends_with(
            Path::new(".Trashes").join(
                fs::metadata(&home)
                    .map(|m| std::os::unix::fs::MetadataExt::uid(&m).to_string())
                    .expect("uid")
            )
        )),
        "{folders:?}"
    );
    let found = bin.locate(&wanted).expect("locate");
    let location = found[0].clone().expect("found by volume and file id");
    assert!(location.ends_with("report 14-25-01.pdf"));
    bin.restore(&location, &original).expect("restore");
    assert_eq!(fs::read(&original).expect("read"), b"pdf");
}

#[test]
fn an_item_with_no_existing_folder_only_searches_the_home_trash() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = SystemBin::with_devices(dir.path(), temp_volume(dir.path()));
    let scanned = stamp(dir.path()).expect("stamp");
    let original = Path::new("nowhere/at/all");
    let wanted = [Wanted {
        original,
        stamp: &scanned,
        trashed_after: 0,
    }];
    assert_eq!(bin.trash_folders(&wanted), [dir.path().join(".Trash")]);
    assert_eq!(bin.locate(&wanted).expect("locate"), [None]);
}

/// Moves one temp file to **your real Trash** and puts it back. Ignored: it
/// touches `~/.Trash` and needs Full Disk Access for the terminal.
#[test]
#[ignore = "touches the real Trash; run by hand"]
fn the_real_trash_round_trips_a_temp_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let original = fs::canonicalize(dir.path())
        .expect("canonical")
        .join("storagelens-undo-test.txt");
    fs::write(&original, b"StorageLens undo test").expect("write");
    let scanned = stamp(&original).expect("stamp");
    let home = std::env::var_os("HOME").expect("HOME");
    let bin = SystemBin::new(Path::new(&home));

    bin.trash(&original).expect("trash");
    assert!(!original.exists());
    let wanted = [Wanted {
        original: &original,
        stamp: &scanned,
        trashed_after: 0,
    }];
    let location = bin.locate(&wanted).expect("locate")[0]
        .clone()
        .expect("in the Trash");
    bin.restore(&location, &original).expect("restore");
    assert_eq!(fs::read(&original).expect("read"), b"StorageLens undo test");
}

#[test]
fn the_volume_trash_is_at_the_top_of_the_item_volume() {
    // Two made-up volumes inside a temp dir: `outer` holds `outer/Volume`,
    // a separate device. Nothing outside the temp dir is looked at.
    let dir = tempfile::tempdir().expect("tempdir");
    let outer = fs::canonicalize(dir.path()).expect("canonical");
    let volume = outer.join("Volume");
    let (outer_c, volume_c) = (outer.clone(), volume.clone());
    let devices: storagelens_safety::bin::DeviceOf = Arc::new(move |path: &Path| {
        if path.starts_with(&volume_c) {
            Some(2)
        } else if path.starts_with(&outer_c) {
            Some(1)
        } else {
            None
        }
    });
    let bin = SystemBin::with_devices(&outer, devices);
    let scanned = stamp(dir.path()).expect("stamp");
    let original = volume.join("Projects/old/cache");
    let wanted = [Wanted {
        original: &original,
        stamp: &scanned,
        trashed_after: 0,
    }];
    let folders = bin.trash_folders(&wanted);
    let uid = std::os::unix::fs::MetadataExt::uid(&fs::metadata(&outer).expect("meta"));
    assert_eq!(
        folders,
        [
            outer.join(".Trash"),
            volume.join(".Trashes").join(uid.to_string())
        ],
        "the climb stops at the volume's top, not at the folder holding it"
    );
    assert!(format!("{bin:?}").contains("SystemBin"));
    // The real constructor builds the same thing, looking volumes up itself.
    assert!(format!("{:?}", SystemBin::new(&outer)).contains(&outer.display().to_string()));
}

#[test]
fn only_permission_refusals_are_retried_through_finder() {
    use storagelens_safety::bin::needs_finder;
    assert!(needs_finder(
        "`trashItemAtURL` failed: “Microsoft Word” couldn’t be moved to the trash because you don’t have permission to access it."
    ));
    assert!(needs_finder("Operation not permitted (os error 1)"));
    assert!(!needs_finder("No such file or directory (os error 2)"));
}
