//! Positional file listings (#23): the in-memory order matches a reference
//! sort of the rows it was built from, filters and reverses correctly, and
//! slices by position without touching SQL.

use std::path::Path;
use std::time::Instant;

use storagelens_catalog::{file_order, modified_times, slice, Arena, Catalog, FileSort, Platform};
use storagelens_core::{Category, Entry, EntryKind, EntrySink};

struct Lcg(u64);

impl Lcg {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) % bound.max(1)
    }
}

const EXTENSIONS: [&str; 6] = ["jpg", "mp4", "pdf", "rs", "zip", "bin"];

/// A flat-ish tree: a few directories, then `files` files with varied sizes
/// (many ties), times (some unknown), categories, and hard-link duplicates.
fn write_scan(dir: &tempfile::TempDir, files: usize, seed: u64) -> (Catalog, i64) {
    let catalog = Catalog::open(dir.path().join("catalog.db")).expect("open");
    let scan_id = catalog
        .begin_session(Platform::Macos, false, Path::new("/scanned"))
        .expect("begin");
    let mut writer = catalog.writer(scan_id).expect("writer");
    let mut rng = Lcg(seed);
    let entry = |parent, name: String, kind, size, modified, dup| Entry {
        parent,
        name,
        kind,
        size_alloc: size,
        size_logical: size,
        dev: 1,
        file_id: 0,
        nlink: if dup { 2 } else { 1 },
        attrs: 0,
        created: 0,
        modified,
        accessed: 0,
        hardlink_dup: dup,
    };
    writer
        .accept(entry(0, "root".into(), EntryKind::Dir, 0, 0, false))
        .expect("root");
    for d in 1..=4u32 {
        writer
            .accept(entry(0, format!("d{d}"), EntryKind::Dir, 0, 0, false))
            .expect("dir");
    }
    for index in 0..files {
        let parent = 1 + rng.below(4) as u32;
        let size = 4096 * rng.below(50);
        let modified = if rng.below(10) == 0 {
            0
        } else {
            1_600_000_000 + rng.below(1000) as i64
        };
        let dup = rng.below(30) == 0;
        let ext = EXTENSIONS[rng.below(EXTENSIONS.len() as u64) as usize];
        writer
            .accept(entry(
                parent,
                format!("f{index}.{ext}"),
                EntryKind::File,
                size,
                modified,
                dup,
            ))
            .expect("file");
        if rng.below(15) == 0 {
            writer
                .accept(entry(
                    parent,
                    format!("link{index}"),
                    EntryKind::Symlink,
                    0,
                    0,
                    false,
                ))
                .expect("link");
        }
    }
    writer.flush().expect("flush");
    (catalog, scan_id)
}

#[test]
fn orders_match_a_reference_sort() {
    for seed in 1..=8 {
        let dir = tempfile::tempdir().expect("tempdir");
        let (catalog, scan_id) = write_scan(&dir, 400 + seed as usize * 37, seed);
        let arena = Arena::load(catalog.connection(), scan_id).expect("load");
        let times = modified_times(catalog.connection(), &arena).expect("times");

        let counted: Vec<u32> = (0..arena.len() as u32)
            .filter(|&index| arena.node(index).expect("node").counts())
            .collect();
        let size = |index: u32| arena.node(index).expect("node").size_alloc;
        let category = |index: u32| {
            let category = arena.node(index).expect("node").category();
            Category::ALL
                .iter()
                .position(|c| *c == category)
                .expect("known")
        };
        let time = |index: u32| match times[index as usize] {
            i64::MIN => (1, 0),
            known => (0, known),
        };

        let mut by_size = counted.clone();
        by_size.sort_by_key(|&i| (std::cmp::Reverse(size(i)), i));
        assert_eq!(
            file_order(&arena, FileSort::Size, 0, None),
            by_size,
            "seed {seed}"
        );

        let mut by_type = counted.clone();
        by_type.sort_by_key(|&i| (category(i), std::cmp::Reverse(size(i)), i));
        assert_eq!(
            file_order(&arena, FileSort::Type, 0, None),
            by_type,
            "seed {seed}"
        );

        let mut by_age = counted.clone();
        by_age.sort_by_key(|&i| (time(i), std::cmp::Reverse(size(i)), i));
        let age = file_order(&arena, FileSort::Modified, 0, Some(&times));
        assert_eq!(age, by_age, "seed {seed}");
        let first_unknown = age.iter().position(|&i| time(i).0 == 1);
        if let Some(first) = first_unknown {
            assert!(
                age[first..].iter().all(|&i| time(i).0 == 1),
                "unknown times sort last"
            );
        }

        let min = 4096 * 25;
        let filtered = file_order(&arena, FileSort::Size, min, None);
        assert_eq!(
            filtered,
            by_size
                .iter()
                .copied()
                .filter(|&i| size(i) >= min)
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn slices_are_positional_in_both_directions() {
    let order: Vec<u32> = (10..20).collect();
    assert_eq!(slice(&order, 0, 3, false), vec![10, 11, 12]);
    assert_eq!(
        slice(&order, 8, 5, false),
        vec![18, 19],
        "clamped at the end"
    );
    assert_eq!(slice(&order, 0, 3, true), vec![19, 18, 17]);
    assert_eq!(slice(&order, 9, 3, true), vec![10]);
    assert!(slice(&order, 10, 3, false).is_empty());
    assert!(slice(&order, usize::MAX, 3, true).is_empty());
    assert!(slice(&order, 2, 0, false).is_empty());

    // Reading every page backwards reverses the whole order.
    let mut backwards = Vec::new();
    for start in (0..order.len()).step_by(3) {
        backwards.extend(slice(&order, start, 3, true));
    }
    assert_eq!(backwards, order.iter().rev().copied().collect::<Vec<_>>());
}

/// The order a million-file scan needs, built once: run with
/// `cargo test --release -p storagelens-catalog --test listing -- --ignored --nocapture`.
#[test]
#[ignore = "benchmark: 1M files"]
fn one_million_files_sort_quickly_and_slice_instantly() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (catalog, scan_id) = write_scan(&dir, 1_000_000, 42);
    let arena = Arena::load(catalog.connection(), scan_id).expect("load");

    let started = Instant::now();
    let times = modified_times(catalog.connection(), &arena).expect("times");
    let load = started.elapsed();
    for sort in [FileSort::Size, FileSort::Type, FileSort::Modified] {
        let started = Instant::now();
        let order = file_order(&arena, sort, 0, Some(&times));
        let built = started.elapsed();
        let started = Instant::now();
        let page = slice(&order, order.len() / 2, 100, true);
        let sliced = started.elapsed();
        assert_eq!(page.len(), 100);
        println!(
            "{sort:?}: {} files ordered in {built:?}, a mid-list page in {sliced:?}; \
             {} MB for the order",
            order.len(),
            order.len() * 4 / 1_000_000,
        );
    }
    println!("modified times loaded in {load:?}");
}
