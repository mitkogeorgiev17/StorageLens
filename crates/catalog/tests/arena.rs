//! Arena and rollup tests (#17).
//!
//! Trees are generated from a seeded LCG and written through the real
//! [`EntryWriter`], so the arena is loaded exactly as it would be after a scan.
//! Many seeds stand in for a property-testing crate (AGENTS.md §2.3: no new
//! dependency for something a loop can do).
//!
//! - **Structure** — every parent/child/sibling link is checked against the rows
//!   it was built from.
//! - **Rollup vs SQL** — every directory's `dir_rollup` row is compared with a
//!   recursive-CTE `SUM` over its descendants.
//! - **Incremental vs full** — random size changes, then `recompute_dirty`, must
//!   equal a full recompute, in memory and in `dir_rollup`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Instant;

use rusqlite::params;
use storagelens_catalog::rollup;
use storagelens_catalog::{Arena, Catalog, CatalogError, Platform, NIL};
use storagelens_core::{Entry, EntryKind, EntrySink};

/// Minimal deterministic PRNG (Knuth's MMIX LCG).
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound.max(1)
    }
}

/// One generated row, kept so tests can compare the arena with its source.
#[derive(Debug, Clone)]
struct Row {
    parent: u32,
    name: String,
    kind: EntryKind,
    alloc: u64,
    logical: u64,
    dup: bool,
}

/// A random tree of `count` entries. Every parent is an earlier directory, which
/// is all the writer and the arena require of acceptance order.
fn random_tree(seed: u64, count: usize) -> Vec<Row> {
    let mut rng = Lcg(seed);
    let mut rows = vec![Row {
        parent: 0,
        name: "root".to_owned(),
        kind: EntryKind::Dir,
        alloc: 0,
        logical: 0,
        dup: false,
    }];
    let mut dirs = vec![0u32];

    for index in 1..count {
        // Bias toward recent directories so trees get deep, not just wide.
        let pick = if rng.below(4) == 0 {
            rng.below(dirs.len() as u64) as usize
        } else {
            dirs.len() - 1 - rng.below(dirs.len().min(4) as u64) as usize
        };
        let parent = dirs[pick];
        let kind = match rng.below(20) {
            0..=4 => EntryKind::Dir,
            5 => EntryKind::Symlink,
            _ => EntryKind::File,
        };
        let (alloc, logical) = if kind == EntryKind::File {
            let logical = rng.below(1 << 20);
            (logical.div_ceil(4096) * 4096, logical)
        } else {
            (0, 0)
        };
        let dup = kind == EntryKind::File && rng.below(25) == 0;
        if kind == EntryKind::Dir {
            dirs.push(index as u32);
        }
        rows.push(Row {
            parent,
            name: format!("n{index}-{}", ["a", "Фото", "📷", "مرحبا"][index % 4]),
            kind,
            alloc,
            logical,
            dup,
        });
    }
    rows
}

/// Write rows as one scan; returns `(catalog, scan_id)`.
fn write_scan(dir: &tempfile::TempDir, rows: &[Row]) -> (Catalog, i64) {
    let catalog = Catalog::open(dir.path().join("catalog.db")).expect("open");
    let scan_id = catalog
        .begin_session(Platform::Macos, false, Path::new("/scanned"))
        .expect("begin");
    write_into(&catalog, scan_id, rows);
    (catalog, scan_id)
}

fn write_into(catalog: &Catalog, scan_id: i64, rows: &[Row]) {
    let mut writer = catalog.writer(scan_id).expect("writer");
    for (index, row) in rows.iter().enumerate() {
        writer
            .accept(Entry {
                parent: row.parent,
                name: row.name.clone(),
                kind: row.kind,
                size_alloc: row.alloc,
                size_logical: row.logical,
                dev: 1,
                file_id: index as u64 + 1,
                nlink: if row.dup { 2 } else { 1 },
                attrs: 0,
                created: 0,
                modified: 0,
                accessed: 0,
                hardlink_dup: row.dup,
            })
            .expect("accept");
    }
    writer.flush().expect("flush");
}

#[test]
fn links_match_the_rows_they_were_built_from() {
    for seed in 1..=40 {
        let rows = random_tree(seed, 300 + seed as usize * 7);
        let dir = tempfile::tempdir().expect("tempdir");
        let (catalog, scan_id) = write_scan(&dir, &rows);
        let arena = Arena::load(catalog.connection(), scan_id).expect("load");

        assert_eq!(arena.len(), rows.len(), "seed {seed}");
        assert_eq!(arena.node(arena.root()).expect("root").parent, NIL);

        let mut expected_children: BTreeMap<u32, BTreeSet<u32>> = BTreeMap::new();
        for (index, row) in rows.iter().enumerate().skip(1) {
            expected_children
                .entry(row.parent)
                .or_default()
                .insert(index as u32);
        }

        let mut seen_as_child = 0usize;
        for (index, row) in rows.iter().enumerate() {
            let index = index as u32;
            let node = arena.node(index).expect("node");
            assert_eq!(arena.name(index), Some(row.name.as_str()), "seed {seed}");
            assert_eq!(node.kind(), row.kind);
            assert_eq!(node.size_alloc, row.alloc);
            assert_eq!(arena.size_logical(index), Some(row.logical));
            assert_eq!(node.is_hardlink_dup(), row.dup);
            if index != 0 {
                assert_eq!(node.parent, row.parent, "seed {seed}, node {index}");
            }

            // Children: exactly the expected set, each once, no cycle.
            let children: Vec<u32> = arena.children(index).take(rows.len() + 1).collect();
            assert!(children.len() <= rows.len(), "sibling cycle under {index}");
            let unique: BTreeSet<u32> = children.iter().copied().collect();
            assert_eq!(unique.len(), children.len(), "a child appears twice");
            assert_eq!(
                unique,
                expected_children.remove(&index).unwrap_or_default(),
                "seed {seed}, children of {index}"
            );
            for child in &children {
                assert_eq!(arena.node(*child).expect("child").parent, index);
            }
            seen_as_child += children.len();

            // Ancestors end at the root without looping.
            let ancestors: Vec<u32> = arena.ancestors(index).take(rows.len() + 1).collect();
            assert!(ancestors.len() < rows.len());
            if index != 0 {
                assert_eq!(ancestors.last(), Some(&0), "every chain ends at the root");
            }

            assert_eq!(
                arena.index_of(arena.entry_id(index).expect("id")),
                Some(index)
            );
        }
        assert_eq!(
            seen_as_child,
            rows.len() - 1,
            "every non-root node has one parent"
        );
    }
}

/// `(alloc, logical, files)` per directory id, straight from SQL.
fn sql_rollups(catalog: &Catalog, scan_id: i64) -> BTreeMap<i64, (u64, u64, u64)> {
    let dir_kind = i64::from(EntryKind::Dir.as_u8());
    let file_kind = i64::from(EntryKind::File.as_u8());
    let mut statement = catalog
        .connection()
        .prepare(
            "WITH RECURSIVE sub(dir, id) AS (
               SELECT id, id FROM entries WHERE scan_id = ?1 AND kind = ?2
               UNION ALL
               SELECT sub.dir, e.id FROM entries e JOIN sub ON e.parent_id = sub.id
               WHERE e.scan_id = ?1
             )
             SELECT sub.dir,
                    COALESCE(SUM(CASE WHEN e.kind = ?3 AND e.hardlink_dup = 0
                                      THEN e.size_alloc END), 0),
                    COALESCE(SUM(CASE WHEN e.kind = ?3 AND e.hardlink_dup = 0
                                      THEN e.size_logical END), 0),
                    COUNT(CASE WHEN e.kind = ?3 AND e.hardlink_dup = 0 THEN 1 END)
             FROM sub JOIN entries e ON e.id = sub.id
             GROUP BY sub.dir",
        )
        .expect("prepare");
    statement
        .query_map(params![scan_id, dir_kind, file_kind], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                (
                    row.get::<_, i64>(1)? as u64,
                    row.get::<_, i64>(2)? as u64,
                    row.get::<_, i64>(3)? as u64,
                ),
            ))
        })
        .expect("query")
        .map(|row| row.expect("row"))
        .collect()
}

fn stored_rollups(catalog: &Catalog, scan_id: i64) -> BTreeMap<i64, (u64, u64, u64)> {
    let mut statement = catalog
        .connection()
        .prepare(
            "SELECT dir_id, total_alloc, total_logical, file_count \
             FROM dir_rollup WHERE scan_id = ?1",
        )
        .expect("prepare");
    statement
        .query_map(params![scan_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                (
                    row.get::<_, i64>(1)? as u64,
                    row.get::<_, i64>(2)? as u64,
                    row.get::<_, i64>(3)? as u64,
                ),
            ))
        })
        .expect("query")
        .map(|row| row.expect("row"))
        .collect()
}

#[test]
fn rollups_equal_sql_sums_per_directory() {
    for seed in 100..106 {
        let rows = random_tree(seed, 600);
        let dir = tempfile::tempdir().expect("tempdir");
        let (catalog, scan_id) = write_scan(&dir, &rows);
        let arena = Arena::load(catalog.connection(), scan_id).expect("load");
        rollup::write(catalog.connection(), &arena).expect("write rollup");

        let expected = sql_rollups(&catalog, scan_id);
        assert_eq!(
            stored_rollups(&catalog, scan_id),
            expected,
            "seed {seed}: dir_rollup disagrees with SUM over descendants"
        );

        // The root's rollup is the scan total (invariant 11's denominator).
        let root = arena.root();
        let total: u64 = rows
            .iter()
            .filter(|row| row.kind == EntryKind::File && !row.dup)
            .map(|row| row.alloc)
            .sum();
        assert_eq!(arena.node(root).expect("root").total_alloc, total);
    }
}

#[test]
fn incremental_rollup_matches_a_full_rebuild() {
    for seed in 200..225 {
        let rows = random_tree(seed, 800);
        let dir = tempfile::tempdir().expect("tempdir");
        let (catalog, scan_id) = write_scan(&dir, &rows);
        let mut arena = Arena::load(catalog.connection(), scan_id).expect("load");
        rollup::write(catalog.connection(), &arena).expect("write rollup");

        // Change some files and mark their parents (and, sometimes, an ancestor
        // further up) dirty.
        let mut rng = Lcg(seed * 31);
        let files: Vec<u32> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.kind == EntryKind::File)
            .map(|(index, _)| index as u32)
            .collect();
        let mut dirty = Vec::new();
        for _ in 0..(1 + rng.below(12)) {
            let file = files[rng.below(files.len() as u64) as usize];
            let logical = rng.below(1 << 22);
            assert!(arena.set_sizes(file, logical.div_ceil(4096) * 4096, logical));
            let parent = arena.node(file).expect("file").parent;
            dirty.push(parent);
            if rng.below(3) == 0 {
                if let Some(higher) = arena.ancestors(parent).next() {
                    dirty.push(higher);
                }
            }
        }

        let touched = rollup::recompute_dirty(&mut arena, &dirty);
        rollup::write_dirs(catalog.connection(), &arena, &touched).expect("write dirs");

        let mut full = arena.clone();
        rollup::compute(&mut full);
        for index in 0..arena.len() as u32 {
            assert_eq!(
                arena.node(index).expect("node").total_alloc,
                full.node(index).expect("node").total_alloc,
                "seed {seed}, node {index}: alloc"
            );
            assert_eq!(arena.total_logical(index), full.total_logical(index));
            assert_eq!(arena.file_count(index), full.file_count(index));
        }

        // The upserted rows must match a full rewrite.
        let incremental = stored_rollups(&catalog, scan_id);
        rollup::write(catalog.connection(), &full).expect("full write");
        assert_eq!(
            incremental,
            stored_rollups(&catalog, scan_id),
            "seed {seed}"
        );
    }
}

#[test]
fn scans_load_independently() {
    let dir = tempfile::tempdir().expect("tempdir");
    let first = random_tree(7, 200);
    let second = random_tree(8, 300);
    let (catalog, first_id) = write_scan(&dir, &first);
    let second_id = catalog
        .begin_session(Platform::Macos, false, Path::new("/scanned"))
        .expect("begin");
    write_into(&catalog, second_id, &second);

    let a = Arena::load(catalog.connection(), first_id).expect("first");
    let b = Arena::load(catalog.connection(), second_id).expect("second");
    assert_eq!(a.len(), 200);
    assert_eq!(b.len(), 300);
    assert_eq!(b.name(0), Some("root"));
    assert!(b.entry_id(0) > a.entry_id(199), "ids continue across scans");
    assert_eq!(a.index_of(b.entry_id(0).expect("id")), None);
}

#[test]
fn damaged_catalogs_are_errors() {
    let dir = tempfile::tempdir().expect("tempdir");
    let catalog = Catalog::open(dir.path().join("catalog.db")).expect("open");
    let empty = catalog
        .begin_session(Platform::Macos, false, Path::new("/scanned"))
        .expect("begin");
    assert!(matches!(
        Arena::load(catalog.connection(), empty),
        Err(CatalogError::Corrupt(_))
    ));

    let rows = random_tree(9, 50);
    write_into(&catalog, empty, &rows);
    // Punch a hole in the id sequence.
    catalog
        .connection()
        .execute(
            "DELETE FROM entries WHERE scan_id = ?1 AND id = (SELECT MIN(id) + 10 FROM entries \
             WHERE scan_id = ?1)",
            params![empty],
        )
        .expect("delete");
    assert!(matches!(
        Arena::load(catalog.connection(), empty),
        Err(CatalogError::Corrupt(_))
    ));
}

/// PLAN budget: 1M nodes load in < 2 s and the arena stays far under 500 MB.
/// Ignored by default (it writes a million rows first); run with
/// `cargo test -p storagelens-catalog --release --test arena -- --ignored --nocapture`.
#[test]
#[ignore = "benchmark: writes 1M rows"]
fn a_million_nodes_load_within_budget() {
    let rows = random_tree(42, 1_000_000);
    let dir = tempfile::tempdir().expect("tempdir");
    let (catalog, scan_id) = write_scan(&dir, &rows);

    let started = Instant::now();
    let arena = Arena::load(catalog.connection(), scan_id).expect("load");
    let loaded = started.elapsed();

    let started = Instant::now();
    rollup::write(catalog.connection(), &arena).expect("write rollup");
    let written = started.elapsed();

    let heap_mb = arena.heap_bytes() as f64 / (1024.0 * 1024.0);
    eprintln!(
        "1M nodes: load + rollup {loaded:?}, dir_rollup write {written:?}, arena heap {heap_mb:.1} MB"
    );
    assert!(loaded.as_secs_f64() < 2.0, "load took {loaded:?}");
    assert!(heap_mb < 500.0, "arena heap {heap_mb:.1} MB");
}
