//! Path guards against a real filesystem (#29): symlink escape, traversal,
//! bundle internals, and resolution failures. Every path is inside a temp dir.

#![cfg(unix)]

use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use storagelens_safety::{Guard, Refusal};

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    outside: PathBuf,
    guard: Guard,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().expect("tempdir");
    let base = fs::canonicalize(dir.path()).expect("canonical temp dir");
    let root = base.join("scanned");
    let outside = base.join("outside");
    let home = base.join("home");
    for folder in [&root, &outside, &home] {
        fs::create_dir(folder).expect("mkdir");
    }
    fs::write(root.join("cache.bin"), b"cache").expect("write");
    fs::write(outside.join("precious.txt"), b"precious").expect("write");
    // Reach the root through a link, so the guard must resolve it.
    symlink(&root, base.join("root-link")).expect("link");
    let guard = Guard::new(&base.join("root-link"), Some(&home)).expect("guard");
    assert_eq!(Path::new(guard.root()), root);
    Fixture {
        _dir: dir,
        root,
        outside,
        guard,
    }
}

#[test]
fn an_ordinary_file_inside_the_root_passes() {
    let f = fixture();
    assert_eq!(f.guard.check(&f.root.join("cache.bin")), Ok(()));
}

#[test]
fn a_symlinked_folder_pointing_outside_cannot_be_used_to_escape() {
    let f = fixture();
    symlink(&f.outside, f.root.join("escape")).expect("link");
    assert_eq!(
        f.guard.check(&f.root.join("escape/precious.txt")),
        Err(Refusal::OutsideRoot)
    );
}

#[test]
fn a_symlink_target_outside_the_root_is_refused() {
    let f = fixture();
    let link = f.root.join("to-precious");
    symlink(f.outside.join("precious.txt"), &link).expect("link");
    assert_eq!(f.guard.check(&link), Err(Refusal::OutsideRoot));
}

#[test]
fn a_symlink_whose_folder_escapes_is_refused() {
    let f = fixture();
    symlink(&f.outside, f.root.join("escape")).expect("link");
    symlink(f.root.join("cache.bin"), f.outside.join("back-in")).expect("link");
    assert_eq!(
        f.guard.check(&f.root.join("escape/back-in")),
        Err(Refusal::OutsideRoot),
        "the link itself lives outside, though it points inside"
    );
}

#[test]
fn links_inside_the_root_and_dangling_links_pass() {
    let f = fixture();
    let inside = f.root.join("alias");
    symlink(f.root.join("cache.bin"), &inside).expect("link");
    assert_eq!(f.guard.check(&inside), Ok(()));
    let dangling = f.root.join("dangling");
    symlink(f.root.join("gone"), &dangling).expect("link");
    assert_eq!(
        f.guard.check(&dangling),
        Ok(()),
        "only the link itself moves"
    );
}

#[test]
fn a_link_loop_is_refused() {
    let f = fixture();
    let a = f.root.join("a");
    let b = f.root.join("b");
    symlink(&b, &a).expect("link");
    symlink(&a, &b).expect("link");
    assert!(matches!(
        f.guard.check(&a),
        Err(Refusal::Unresolvable(kind)) if kind != io::ErrorKind::NotFound
    ));
}

#[test]
fn traversal_is_refused_before_anything_is_resolved() {
    let f = fixture();
    let sneaky = f.root.join("../outside/precious.txt");
    assert!(sneaky.exists(), "the traversal would reach a real file");
    assert_eq!(f.guard.check(&sneaky), Err(Refusal::Traversal));
    assert_eq!(
        f.guard.check(&f.root.join("../../../../etc/passwd")),
        Err(Refusal::Traversal)
    );
    assert_eq!(
        f.guard.check(Path::new("./cache.bin")),
        Err(Refusal::NotAbsolute)
    );
    assert_eq!(
        f.guard.check(Path::new("cache.bin")),
        Err(Refusal::NotAbsolute)
    );
}

#[test]
fn bundle_internals_are_refused() {
    let f = fixture();
    let binary = f.root.join("Old.app/Contents/MacOS");
    fs::create_dir_all(&binary).expect("mkdir");
    fs::write(binary.join("Old"), b"#!").expect("write");
    assert_eq!(
        f.guard.check(&binary.join("Old")),
        Err(Refusal::BundleInternal)
    );
    assert_eq!(f.guard.check(&f.root.join("Old.app")), Ok(()));
}

#[test]
fn the_root_itself_and_missing_paths_are_refused() {
    let f = fixture();
    assert_eq!(f.guard.check(&f.root), Err(Refusal::IsRoot));
    assert_eq!(
        f.guard.check(&f.root.join("missing")),
        Err(Refusal::Unresolvable(io::ErrorKind::NotFound))
    );
}

#[test]
fn a_guard_needs_a_root_that_exists() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert_eq!(
        Guard::new(&dir.path().join("missing"), None).map(|_| ()),
        Err(Refusal::Unresolvable(io::ErrorKind::NotFound))
    );
    assert_eq!(
        Guard::new(dir.path(), Some(&dir.path().join("no-home"))).map(|_| ()),
        Err(Refusal::Unresolvable(io::ErrorKind::NotFound))
    );
}

#[test]
fn an_unreadable_folder_cannot_be_resolved() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let locked = f.root.join("locked");
    fs::create_dir(&locked).expect("mkdir");
    symlink(f.root.join("cache.bin"), locked.join("link")).expect("link");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).expect("chmod");
    let result = f.guard.check(&locked.join("link"));
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).expect("chmod back");
    assert_eq!(
        result,
        Err(Refusal::Unresolvable(io::ErrorKind::PermissionDenied))
    );
}
