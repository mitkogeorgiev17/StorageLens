//! Scan fixtures for the rules tests: a catalog in a temp dir, written from
//! `(path below the root, kind, bytes, mtime)` rows.

#![allow(dead_code)]

use std::path::Path;

use storagelens_catalog::{modified_times, rollup, Arena, Catalog, Platform as CatalogPlatform};
use storagelens_core::{Entry, EntryKind, EntrySink};

pub const D: EntryKind = EntryKind::Dir;
pub const F: EntryKind = EntryKind::File;

pub struct Scan {
    pub _dir: tempfile::TempDir,
    pub catalog: Catalog,
    pub scan_id: i64,
    pub arena: Arena,
    pub times: Vec<i64>,
}

/// Rows are `(path below the root, kind, bytes, mtime)`; folders are created
/// as they are named, parents first.
pub fn scan(root: &str, rows: &[(&str, EntryKind, u64, i64)]) -> Scan {
    let dir = tempfile::tempdir().expect("tempdir");
    let catalog = Catalog::open(dir.path().join("catalog.db")).expect("open");
    let scan_id = catalog
        .begin_session(CatalogPlatform::Macos, false, Path::new(root))
        .expect("begin");
    let mut writer = catalog.writer(scan_id).expect("writer");
    let mut paths: Vec<String> = vec![String::new()];
    let entry = |parent: u32, name: &str, kind: EntryKind, size: u64, modified: i64| Entry {
        parent,
        name: name.to_owned(),
        kind,
        size_alloc: size,
        size_logical: size,
        dev: 1,
        file_id: 0,
        nlink: 1,
        attrs: 0,
        created: 0,
        modified,
        accessed: 0,
        hardlink_dup: false,
    };
    writer.accept(entry(0, "root", D, 0, 0)).expect("root");
    for &(path, kind, size, modified) in rows {
        let (parent_path, name) = path.rsplit_once('/').unwrap_or(("", path));
        let parent = paths
            .iter()
            .position(|known| known == parent_path)
            .unwrap_or_else(|| panic!("{path}: parent {parent_path} not listed yet"));
        writer
            .accept(entry(parent as u32, name, kind, size, modified))
            .expect("accept");
        paths.push(path.to_owned());
    }
    writer.flush().expect("flush");
    drop(writer);
    let mut arena = Arena::load(catalog.connection(), scan_id).expect("arena");
    rollup::compute(&mut arena);
    let times = modified_times(catalog.connection(), &arena).expect("times");
    Scan {
        _dir: dir,
        catalog,
        scan_id,
        arena,
        times,
    }
}

/// [`scan`] with a path → index map, for large fixtures.
pub fn scan_fast(root: &str, rows: &[(&str, EntryKind, u64, i64)]) -> Scan {
    use std::collections::HashMap;
    let dir = tempfile::tempdir().expect("tempdir");
    let catalog = Catalog::open(dir.path().join("catalog.db")).expect("open");
    let scan_id = catalog
        .begin_session(CatalogPlatform::Macos, false, Path::new(root))
        .expect("begin");
    let mut writer = catalog.writer(scan_id).expect("writer");
    let entry = |parent: u32, name: &str, kind: EntryKind, size: u64, modified: i64| Entry {
        parent,
        name: name.to_owned(),
        kind,
        size_alloc: size,
        size_logical: size,
        dev: 1,
        file_id: 0,
        nlink: 1,
        attrs: 0,
        created: 0,
        modified,
        accessed: 0,
        hardlink_dup: false,
    };
    writer.accept(entry(0, "root", D, 0, 0)).expect("root");
    let mut index: HashMap<&str, u32> = HashMap::from([("", 0)]);
    for (position, &(path, kind, size, modified)) in rows.iter().enumerate() {
        let (parent_path, name) = path.rsplit_once('/').unwrap_or(("", path));
        writer
            .accept(entry(index[parent_path], name, kind, size, modified))
            .expect("accept");
        index.insert(path, position as u32 + 1);
    }
    writer.flush().expect("flush");
    drop(writer);
    let mut arena = Arena::load(catalog.connection(), scan_id).expect("arena");
    rollup::compute(&mut arena);
    let times = modified_times(catalog.connection(), &arena).expect("times");
    Scan {
        _dir: dir,
        catalog,
        scan_id,
        arena,
        times,
    }
}
