//! Preflight re-stat (#29): a file changed or replaced after the scan is
//! skipped, with the reason. Real temp files, re-stat'ed with the platform's
//! own `stamp`, so the test also proves an untouched file passes.

#![cfg(any(target_os = "macos", windows))]

use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, SystemTime};

#[cfg(target_os = "macos")]
use storagelens_scan_macos::stamp;
#[cfg(windows)]
use storagelens_scan_windows::stamp;

use storagelens_core::{EntryKind, FileStamp};
use storagelens_safety::{compare, preflight, Platform, Skip};

/// Set `path`'s modification time to `seconds` after the epoch.
fn set_modified(path: &Path, seconds: u64) {
    let file = fs::File::options().write(true).open(path).expect("open");
    file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(seconds))
        .expect("set mtime");
}

#[test]
fn an_untouched_file_and_folder_pass() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("old.log");
    fs::write(&file, b"log").expect("write");
    let scanned = stamp(&file).expect("stamp");
    assert_eq!(preflight(&file, &scanned, stamp), Ok(()));
    let folder = stamp(dir.path()).expect("stamp");
    assert_eq!(folder.kind, EntryKind::Dir);
    assert_eq!(preflight(dir.path(), &folder, stamp), Ok(()));
}

#[test]
fn a_file_replaced_after_the_scan_is_skipped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("report.pdf");
    fs::write(&file, b"scanned!").expect("write");
    set_modified(&file, 1_600_000_000);
    let scanned = stamp(&file).expect("stamp");

    // Same name, same size, same mtime: only the file id gives it away.
    let replacement = dir.path().join("replacement");
    fs::write(&replacement, b"precious").expect("write");
    set_modified(&replacement, 1_600_000_000);
    fs::rename(&replacement, &file).expect("replace");
    let now = stamp(&file).expect("stamp");
    assert_eq!(
        (now.size_logical, now.modified),
        (scanned.size_logical, scanned.modified)
    );
    assert_eq!(preflight(&file, &scanned, stamp), Err(Skip::Replaced));
}

#[test]
fn a_file_changed_after_the_scan_is_skipped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("notes.txt");
    fs::write(&file, b"v1").expect("write");
    set_modified(&file, 1_600_000_000);
    let scanned = stamp(&file).expect("stamp");

    fs::write(&file, b"version 2").expect("rewrite");
    set_modified(&file, 1_600_000_000);
    assert_eq!(
        preflight(&file, &scanned, stamp),
        Err(Skip::Resized {
            expected: 2,
            found: 9
        })
    );

    fs::write(&file, b"v3").expect("rewrite");
    set_modified(&file, 1_600_000_100);
    assert_eq!(
        preflight(&file, &scanned, stamp),
        Err(Skip::Modified {
            expected: 1_600_000_000,
            found: 1_600_000_100
        })
    );
}

#[test]
fn a_folder_that_gained_a_file_is_skipped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let folder = dir.path().join("Cache");
    fs::create_dir(&folder).expect("mkdir");
    let scanned = FileStamp {
        modified: 1_600_000_000,
        ..stamp(&folder).expect("stamp")
    };
    fs::write(folder.join("new"), b"x").expect("write");
    assert!(matches!(
        preflight(&folder, &scanned, stamp),
        Err(Skip::Modified { .. })
    ));
}

#[test]
fn a_file_that_became_a_folder_or_vanished_is_skipped() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("thing");
    fs::write(&path, b"x").expect("write");
    let scanned = stamp(&path).expect("stamp");
    fs::remove_file(&path).expect("remove test file");
    assert_eq!(preflight(&path, &scanned, stamp), Err(Skip::Missing));
    fs::create_dir(&path).expect("mkdir");
    assert_eq!(
        preflight(&path, &scanned, stamp),
        Err(Skip::KindChanged {
            expected: EntryKind::File,
            found: EntryKind::Dir
        })
    );
}

fn file() -> FileStamp {
    FileStamp {
        kind: EntryKind::File,
        size_logical: 10,
        modified: 1_600_000_000,
        dev: 1,
        file_id: 42,
        attrs: 0,
    }
}

#[test]
fn every_difference_is_caught() {
    let scanned = file();
    let mac = |found: FileStamp| compare(Platform::MacOs, &scanned, Ok(found));
    assert_eq!(mac(file()), Ok(()));
    assert_eq!(mac(FileStamp { dev: 2, ..file() }), Err(Skip::Replaced));
    assert_eq!(
        mac(FileStamp {
            file_id: 43,
            ..file()
        }),
        Err(Skip::Replaced)
    );
    assert_eq!(
        compare(
            Platform::MacOs,
            &scanned,
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        ),
        Err(Skip::Unreadable(io::ErrorKind::PermissionDenied))
    );

    // A folder's size is not compared; its time is.
    let folder = FileStamp {
        kind: EntryKind::Dir,
        ..file()
    };
    let grown = FileStamp {
        size_logical: 99,
        ..folder
    };
    assert_eq!(compare(Platform::MacOs, &folder, Ok(grown)), Ok(()));
}

#[test]
fn locked_and_system_items_are_skipped() {
    let scanned = file();
    for flag in [0x2, 0x4, 0x2_0000, 0x4_0000, 0x8_0000, 0x10_0000] {
        let found = FileStamp {
            attrs: flag,
            ..file()
        };
        assert_eq!(
            compare(Platform::MacOs, &scanned, Ok(found)),
            Err(Skip::Locked),
            "{flag:#x}"
        );
    }
    let hidden = FileStamp {
        attrs: 0x8000,
        ..file()
    };
    assert_eq!(compare(Platform::MacOs, &scanned, Ok(hidden)), Ok(()));
    let system = FileStamp {
        attrs: 0x4,
        ..file()
    };
    assert_eq!(
        compare(Platform::Windows, &scanned, Ok(system)),
        Err(Skip::Locked)
    );
    let archive = FileStamp {
        attrs: 0x20,
        ..file()
    };
    assert_eq!(compare(Platform::Windows, &scanned, Ok(archive)), Ok(()));
}

#[test]
fn skips_explain_themselves() {
    for (skip, words) in [
        (Skip::Missing, "no longer"),
        (Skip::Unreadable(io::ErrorKind::PermissionDenied), "checked"),
        (
            Skip::KindChanged {
                expected: EntryKind::File,
                found: EntryKind::Dir,
            },
            "File when scanned and is a Dir",
        ),
        (Skip::Replaced, "different file"),
        (
            Skip::Resized {
                expected: 1,
                found: 2,
            },
            "from 1 to 2",
        ),
        (
            Skip::Modified {
                expected: 1,
                found: 2,
            },
            "modified",
        ),
        (Skip::Locked, "locked"),
    ] {
        assert!(skip.to_string().contains(words), "{skip:?}: {skip}");
    }
}
