//! The macOS Trash adapter (#30). Finding and restoring are tested against a
//! fake home with its own `.Trash` in a temp dir. The real move to the real
//! Trash is one ignored test, run by hand:
//! `cargo test -p storagelens-safety --test system_bin -- --ignored`.

#![cfg(target_os = "macos")]

use std::fs;
use std::path::Path;

use storagelens_safety::bin::SystemBin;
use storagelens_safety::{Bin, Wanted};
use storagelens_scan_macos::stamp;

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

    let bin = SystemBin::new(&home);
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
    let bin = SystemBin::new(dir.path());
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
    // `/dev` is its own filesystem: the climb stops below `/`. Read-only.
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = SystemBin::new(dir.path());
    let scanned = stamp(dir.path()).expect("stamp");
    let wanted = [Wanted {
        original: Path::new("/dev/fd/storagelens-test"),
        stamp: &scanned,
        trashed_after: 0,
    }];
    let folders = bin.trash_folders(&wanted);
    assert!(
        folders.iter().any(|folder| folder.starts_with("/dev")),
        "{folders:?}"
    );
}
