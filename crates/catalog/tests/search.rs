//! Filename search (#24): unicode and emoji names, prefix matching, ranking,
//! paging, scan isolation, and automatic indexing of new scans.

use std::path::Path;
use std::time::Instant;

use storagelens_catalog::{rollup, Arena, Catalog, Platform, SearchCursor, SearchHit};
use storagelens_core::{Entry, EntryKind, EntrySink};

fn entry(parent: u32, name: &str, kind: EntryKind, size: u64, modified: i64, dup: bool) -> Entry {
    Entry {
        parent,
        name: name.to_owned(),
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
    }
}

/// Write one scan of `entries` (index 0 must be the root) and its rollup.
fn scan(catalog: &Catalog, entries: Vec<Entry>) -> i64 {
    let scan_id = catalog
        .begin_session(Platform::Macos, false, Path::new("/scanned"))
        .expect("begin");
    let mut writer = catalog.writer(scan_id).expect("writer");
    for entry in entries {
        writer.accept(entry).expect("accept");
    }
    writer.flush().expect("flush");
    drop(writer);
    let mut arena = Arena::load(catalog.connection(), scan_id).expect("arena");
    rollup::compute(&mut arena);
    rollup::write(catalog.connection(), &arena).expect("rollup");
    scan_id
}

fn names(hits: &[SearchHit]) -> Vec<&str> {
    hits.iter().map(|hit| hit.name.as_str()).collect()
}

fn sample() -> Vec<Entry> {
    use EntryKind::{Dir, File};
    vec![
        entry(0, "root", Dir, 0, 0, false),
        entry(0, "Фотографии", Dir, 0, 0, false),
        entry(1, "отпуск.jpg", File, 8192, 100, false),
        entry(0, "📷 Camera Roll", Dir, 0, 0, false),
        entry(3, "IMG_2024_beach.HEIC", File, 40_960, 200, false),
        entry(3, "IMG_2023_snow.heic", File, 40_960, 300, false),
        entry(0, "café menu.pdf", File, 4096, 50, false),
        entry(0, "مرحبا.txt", File, 4096, 60, false),
        entry(0, "हिन्दी नोट्स.md", File, 4096, 70, false),
        entry(0, "copy of IMG_2024_beach.HEIC", File, 40_960, 400, true),
        entry(0, "report-final.docx", File, 12_288, 80, false),
    ]
}

#[test]
fn unicode_emoji_and_accented_names_are_searchable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let catalog = Catalog::open(dir.path().join("catalog.db")).expect("open");
    let scan_id = scan(&catalog, sample());
    let find = |query: &str| catalog.search(scan_id, query, None, 50).expect("search");

    assert_eq!(
        names(&find("фото")),
        ["Фотографии"],
        "Cyrillic, case-folded prefix"
    );
    assert_eq!(names(&find("отпуск")), ["отпуск.jpg"]);
    assert_eq!(names(&find("📷")), ["📷 Camera Roll"], "emoji are tokens");
    assert_eq!(names(&find("cafe")), ["café menu.pdf"], "accents fold");
    assert_eq!(names(&find("CAFÉ")), ["café menu.pdf"]);
    assert_eq!(names(&find("مرحبا")), ["مرحبا.txt"], "Arabic");
    assert_eq!(
        names(&find("हिन्दी")),
        ["हिन्दी नोट्स.md"],
        "combining marks stay in words"
    );
    assert_eq!(
        names(&find("report final")),
        ["report-final.docx"],
        "terms are ANDed"
    );
    assert_eq!(
        names(&find("final report")),
        ["report-final.docx"],
        "in any order"
    );
    assert!(find("report missing").is_empty());
    assert!(find("").is_empty());
    assert!(find("\" OR *").is_empty(), "syntax is quoted, not executed");
}

#[test]
fn results_rank_by_size_then_recency_and_skip_hard_link_duplicates() {
    let dir = tempfile::tempdir().expect("tempdir");
    let catalog = Catalog::open(dir.path().join("catalog.db")).expect("open");
    let scan_id = scan(&catalog, sample());

    let hits = catalog.search(scan_id, "img_20", None, 50).expect("search");
    // Same size: the more recently modified first. The duplicate is left out.
    assert_eq!(names(&hits), ["IMG_2023_snow.heic", "IMG_2024_beach.HEIC"]);

    let folders = catalog.search(scan_id, "camera", None, 50).expect("search");
    assert_eq!(folders[0].kind, EntryKind::Dir);
    assert_eq!(
        folders[0].size, 81_920,
        "a folder is ranked by its subtree total"
    );
}

#[test]
fn pages_cover_every_match_exactly_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let catalog = Catalog::open(dir.path().join("catalog.db")).expect("open");
    let mut entries = vec![entry(0, "root", EntryKind::Dir, 0, 0, false)];
    for index in 0..257u64 {
        // Many ties on size and time, so the id breaks them.
        entries.push(entry(
            0,
            &format!("invoice-{index}.pdf"),
            EntryKind::File,
            4096 * (index % 5),
            (index % 3) as i64,
            false,
        ));
    }
    let scan_id = scan(&catalog, entries);

    let mut seen = Vec::new();
    let mut cursor = None;
    loop {
        let page = catalog
            .search(scan_id, "invoice", cursor, 50)
            .expect("page");
        if page.is_empty() {
            break;
        }
        let last = page.last().expect("last");
        cursor = Some(SearchCursor {
            size: last.size,
            modified: last.modified,
            id: last.id,
        });
        seen.extend(page);
    }
    assert_eq!(seen.len(), 257);
    let mut ids: Vec<i64> = seen.iter().map(|hit| hit.id).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 257, "no row twice");
    assert!(seen
        .windows(2)
        .all(|pair| (pair[0].size, pair[0].modified, pair[0].id)
            > (pair[1].size, pair[1].modified, pair[1].id)));
}

#[test]
fn scans_are_indexed_once_kept_separate_and_cleaned_up_on_delete() {
    let dir = tempfile::tempdir().expect("tempdir");
    let catalog = Catalog::open(dir.path().join("catalog.db")).expect("open");
    let first = scan(&catalog, sample());
    catalog.index_names(first).expect("index");
    catalog
        .index_names(first)
        .expect("indexing twice is a no-op");
    let second = scan(
        &catalog,
        vec![
            entry(0, "root", EntryKind::Dir, 0, 0, false),
            entry(0, "report-draft.docx", EntryKind::File, 4096, 1, false),
        ],
    );

    let in_first = catalog.search(first, "report", None, 50).expect("first");
    let in_second = catalog.search(second, "report", None, 50).expect("second");
    assert_eq!(names(&in_first), ["report-final.docx"]);
    assert_eq!(
        names(&in_second),
        ["report-draft.docx"],
        "no rebuild step needed"
    );
}

/// `cargo test --release -p storagelens-catalog --test search -- --ignored --nocapture`
#[test]
#[ignore = "benchmark: 1M names"]
fn one_million_names_search_in_milliseconds() {
    const WORDS: [&str; 12] = [
        "photo", "invoice", "report", "backup", "node", "cache", "Фото", "📷", "café", "build",
        "notes", "IMG",
    ];
    let dir = tempfile::tempdir().expect("tempdir");
    let catalog = Catalog::open(dir.path().join("catalog.db")).expect("open");
    let mut entries = vec![entry(0, "root", EntryKind::Dir, 0, 0, false)];
    for d in 0..1000u32 {
        entries.push(entry(
            0,
            &format!("folder {d}"),
            EntryKind::Dir,
            0,
            0,
            false,
        ));
    }
    let mut state = 7u64;
    for index in 0..1_000_000u64 {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        let word = WORDS[(state >> 40) as usize % WORDS.len()];
        let other = WORDS[(state >> 20) as usize % WORDS.len()];
        entries.push(entry(
            1 + (index % 1000) as u32,
            &format!("{word}_{index} {other}.dat"),
            EntryKind::File,
            4096 * ((state >> 33) % 1000),
            (state >> 50) as i64,
            false,
        ));
    }
    let started = Instant::now();
    let scan_id = scan(&catalog, entries);
    println!("wrote 1M entries and rollup in {:?}", started.elapsed());
    let started = Instant::now();
    catalog.index_names(scan_id).expect("index");
    println!("indexed their names in {:?}", started.elapsed());

    for query in [
        "invoice_42",
        "report",
        "Фото",
        "📷",
        "cafe",
        "img 99",
        "zzz",
        "p",
    ] {
        let started = Instant::now();
        let hits = catalog.search(scan_id, query, None, 100).expect("search");
        println!(
            "{query:>12}: {:>3} hits in {:?}",
            hits.len(),
            started.elapsed()
        );
    }
}

#[test]
fn prune_older_drops_older_scans_and_their_names() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("catalog.db");
    let catalog = Catalog::open(&path).expect("open");
    let old = scan(&catalog, sample());
    catalog.index_names(old).expect("index old");
    let kept = scan(&catalog, sample());
    catalog.index_names(kept).expect("index kept");
    let newer = scan(&catalog, sample());
    let indexed = || -> i64 {
        catalog
            .connection()
            .query_row(
                "SELECT COUNT(*) FROM entries_fts WHERE entries_fts MATCH 'img*'",
                [],
                |row| row.get(0),
            )
            .expect("fts rows")
    };
    let both = indexed();

    Catalog::prune_older(&path, kept).expect("prune");

    assert_eq!(catalog.entry_count(old).expect("count"), 0);
    assert!(catalog.session(old).is_err(), "old session gone");
    assert_eq!(
        catalog.entry_count(kept).expect("count"),
        sample().len() as u64
    );
    assert_eq!(
        catalog.entry_count(newer).expect("count"),
        sample().len() as u64,
        "newer kept"
    );
    let hits = catalog.search(kept, "img_20", None, 50).expect("search");
    assert_eq!(names(&hits), ["IMG_2023_snow.heic", "IMG_2024_beach.HEIC"]);
    // Two scans were indexed; only the kept one's names are left.
    assert_eq!(indexed() * 2, both, "the old scan's names are gone");
}
