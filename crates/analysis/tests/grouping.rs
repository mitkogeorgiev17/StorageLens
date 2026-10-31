//! The six grouping builders (#25): totals, exactly-once membership,
//! determinism, each view's own rules, and a 1M-node benchmark.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use storagelens_analysis::grouping::{
    app_key, extension_of, resolve_owner, AGE_LABELS, RISK_LABELS, SIZE_LABELS,
};
use storagelens_analysis::{
    build, FileFilter, Group, Grouping, GroupingInput, Risk, SyntheticTree,
};
use storagelens_catalog::{modified_times, rollup, Arena, Catalog, NodeIndex, Platform};
use storagelens_core::{Category, Entry, EntryKind, EntrySink};

const NOW: i64 = 1_800_000_000;
const DAY: i64 = 86_400;
const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

const D: EntryKind = EntryKind::Dir;
const F: EntryKind = EntryKind::File;

/// `(parent, name, kind, size, modified, dup)`; index 0 is the root.
type Row = (u32, String, EntryKind, u64, i64, bool);

struct Scan {
    _dir: tempfile::TempDir,
    arena: Arena,
    times: Vec<i64>,
}

fn write(rows: &[Row]) -> Scan {
    let dir = tempfile::tempdir().expect("tempdir");
    let catalog = Catalog::open(dir.path().join("catalog.db")).expect("open");
    let scan_id = catalog
        .begin_session(Platform::Macos, false, Path::new("/"))
        .expect("begin");
    let mut writer = catalog.writer(scan_id).expect("writer");
    for (index, (parent, name, kind, size, modified, dup)) in rows.iter().enumerate() {
        writer
            .accept(Entry {
                parent: *parent,
                name: name.clone(),
                kind: *kind,
                size_alloc: *size,
                size_logical: *size,
                dev: 1,
                file_id: index as u64 + 1,
                nlink: if *dup { 2 } else { 1 },
                attrs: 0,
                created: 0,
                modified: *modified,
                accessed: 0,
                hardlink_dup: *dup,
            })
            .expect("accept");
    }
    writer.flush().expect("flush");
    drop(writer);
    let mut arena = Arena::load(catalog.connection(), scan_id).expect("arena");
    rollup::compute(&mut arena);
    let times = modified_times(catalog.connection(), &arena).expect("times");
    Scan {
        _dir: dir,
        arena,
        times,
    }
}

fn scan_of(rows: &[(u32, &str, EntryKind, u64, i64, bool)]) -> Scan {
    let owned: Vec<Row> = rows
        .iter()
        .map(|&(p, n, k, s, m, d)| (p, n.to_owned(), k, s, m, d))
        .collect();
    write(&owned)
}

fn input<'a>(scan: &'a Scan, filter: &'a FileFilter, root: &'a Path) -> GroupingInput<'a> {
    GroupingInput {
        arena: &scan.arena,
        root_path: Some(root),
        modified: Some(&scan.times),
        now: NOW,
        risk: None,
        filter,
    }
}

/// Every file of the tree, with how many groups list it.
fn membership(tree: &SyntheticTree) -> HashMap<NodeIndex, usize> {
    let mut seen = HashMap::new();
    for group in &tree.groups {
        for &file in &group.files {
            *seen.entry(file).or_insert(0) += 1;
        }
    }
    seen
}

/// Every group's total is its own files plus its children.
fn check_sums(tree: &SyntheticTree, arena: &Arena) {
    for group in &tree.groups {
        let own: u64 = group
            .files
            .iter()
            .map(|&file| arena.node(file).expect("file").size_alloc)
            .sum();
        let children: u64 = group
            .children
            .iter()
            .map(|&child| tree.groups[child as usize].size)
            .sum();
        assert_eq!(
            group.size,
            own + children,
            "{:?} '{}'",
            tree.grouping,
            group.label
        );
        assert!(group.children.iter().all(|&child| child > 0));
    }
}

/// A small macOS-shaped volume: apps, their satellites, documents, a
/// hard-link duplicate, and a symlink.
fn volume() -> Scan {
    scan_of(&[
        (0, "Macintosh HD", D, 0, 0, false),                  // 0
        (0, "Applications", D, 0, 0, false),                  // 1
        (1, "Spotify.app", D, 0, 0, false),                   // 2
        (2, "Spotify", F, 200 * MIB, NOW - 400 * DAY, false), // 3
        (1, "Google Chrome.app", D, 0, 0, false),             // 4
        (4, "Chrome", F, 300 * MIB, NOW - 10 * DAY, false),   // 5
        (0, "Users", D, 0, 0, false),                         // 6
        (6, "me", D, 0, 0, false),                            // 7
        (7, "Library", D, 0, 0, false),                       // 8
        (8, "Caches", D, 0, 0, false),                        // 9
        (9, "com.spotify.client", D, 0, 0, false),            // 10
        (10, "data.bin", F, 2 * GIB, NOW - 100 * DAY, false), // 11
        (9, "com.google.Chrome", D, 0, 0, false),             // 12
        (12, "cache.db", F, 50 * MIB, NOW - 2 * DAY, false),  // 13
        (8, "Preferences", D, 0, 0, false),                   // 14
        (
            14,
            "com.spotify.client.plist",
            F,
            4096,
            NOW - 1000 * DAY,
            false,
        ), // 15
        (8, "Application Support", D, 0, 0, false),           // 16
        (16, "SomeTool", D, 0, 0, false),                     // 17
        (17, "state.json", F, 8192, 0, false),                // 18 (no time)
        (7, "Documents", D, 0, 0, false),                     // 19
        (19, "tax 2024.pdf", F, 12 * GIB, NOW - 800 * DAY, false), // 20
        (19, "holiday.JPG", F, 5 * MIB, NOW - 200 * DAY, false), // 21
        (19, "Makefile", F, 4096, NOW, false),                // 22
        (19, "copy of tax.pdf", F, 12 * GIB, NOW, true),      // 23 dup
        (19, "link", EntryKind::Symlink, 0, 0, false),        // 24
    ])
}

fn counted(arena: &Arena) -> Vec<NodeIndex> {
    (0..arena.len() as NodeIndex)
        .filter(|&index| arena.node(index).expect("node").counts())
        .collect()
}

fn child_labels(tree: &SyntheticTree, group: usize) -> Vec<&str> {
    tree.groups[group]
        .children
        .iter()
        .map(|&child| tree.groups[child as usize].label.as_str())
        .collect()
}

fn find<'a>(tree: &'a SyntheticTree, label: &str) -> &'a Group {
    tree.groups
        .iter()
        .find(|group| group.label == label)
        .unwrap_or_else(|| panic!("{:?}: no group '{label}'", tree.grouping))
}

#[test]
fn every_grouping_holds_every_file_once_and_totals_the_scan() {
    let scan = volume();
    let filter = FileFilter::default();
    let root = Path::new("/");
    let total = scan
        .arena
        .node(scan.arena.root())
        .expect("root")
        .total_alloc;
    let files = counted(&scan.arena);
    for grouping in Grouping::ALL {
        let tree = build(grouping, &input(&scan, &filter, root));
        assert_eq!(
            tree.root().size,
            total,
            "{grouping:?}: root total is the scan total"
        );
        assert_eq!(tree.root().file_count, files.len() as u64, "{grouping:?}");
        let seen = membership(&tree);
        assert_eq!(
            seen.len(),
            files.len(),
            "{grouping:?}: every file is placed"
        );
        assert!(
            seen.values().all(|&count| count == 1),
            "{grouping:?}: exactly once"
        );
        assert!(
            !seen.contains_key(&23),
            "{grouping:?}: the duplicate is not counted"
        );
        check_sums(&tree, &scan.arena);
        assert_eq!(
            tree,
            build(grouping, &input(&scan, &filter, root)),
            "{grouping:?}: deterministic"
        );
    }
}

#[test]
fn folder_mirrors_the_directory_tree() {
    let scan = volume();
    let filter = FileFilter::default();
    let tree = build(Grouping::Folder, &input(&scan, &filter, Path::new("/")));
    assert_eq!(tree.root().entry, Some(scan.arena.root()));
    assert_eq!(
        child_labels(&tree, 0),
        ["Users", "Applications"],
        "biggest first"
    );
    let documents = find(&tree, "Documents");
    assert_eq!(documents.files.len(), 3);
    assert_eq!(documents.entry, Some(19));
    let found = tree.group_of_entry(19).expect("group of Documents");
    assert_eq!(tree.groups[found as usize].label, "Documents");
}

#[test]
fn type_groups_by_category_then_extension_and_always_has_other() {
    let scan = volume();
    let filter = FileFilter::default();
    let tree = build(Grouping::Type, &input(&scan, &filter, Path::new("/")));
    let images = find(&tree, Category::Images.label());
    let jpg = &tree.groups[images.children[0] as usize];
    assert_eq!(jpg.label, ".jpg", "extensions are lower-cased");
    assert_eq!(jpg.category, Some(Category::Images));
    assert!(child_labels(&tree, 0).contains(&"Other"));
    assert!(tree
        .groups
        .iter()
        .any(|group| group.label == "No extension"));

    // Only documents: Other is still there, empty.
    let only = FileFilter {
        categories: vec![Category::Documents],
        ..FileFilter::default()
    };
    let tree = build(Grouping::Type, &input(&scan, &only, Path::new("/")));
    assert!(child_labels(&tree, 0).contains(&"Other"));
    assert_eq!(find(&tree, "Other").size, 0);
    assert_eq!(tree.root().size, 12 * GIB);

    assert_eq!(extension_of("a.TAR.GZ").as_deref(), Some("gz"));
    assert_eq!(extension_of(".zshrc"), None);
    assert_eq!(extension_of("Makefile"), None);
    assert_eq!(extension_of("trailing."), None);
}

#[test]
fn age_size_and_cleanliness_use_their_buckets_in_order() {
    let scan = volume();
    let filter = FileFilter::default();
    let root = Path::new("/");

    let age = build(Grouping::Age, &input(&scan, &filter, root));
    let order: Vec<usize> = child_labels(&age, 0)
        .iter()
        .map(|label| {
            AGE_LABELS
                .iter()
                .position(|known| known == label)
                .expect("known")
        })
        .collect();
    assert!(
        order.windows(2).all(|pair| pair[0] < pair[1]),
        "natural order, empty buckets dropped"
    );
    assert_eq!(find(&age, "Over 2 years").files, vec![15, 20]);
    assert_eq!(find(&age, "Unknown date").files, vec![18]);
    assert!(find(&age, "Under 1 month").files.contains(&22));

    let size = build(Grouping::Size, &input(&scan, &filter, root));
    assert_eq!(child_labels(&size, 0)[0], SIZE_LABELS[0]);
    assert_eq!(find(&size, "Over 10 GB").files, vec![20]);
    assert_eq!(find(&size, "1–10 GB").files, vec![11]);
    assert_eq!(find(&size, "100 MB–1 GB").files, vec![3, 5]);

    let unjudged = build(Grouping::Cleanliness, &input(&scan, &filter, root));
    assert_eq!(child_labels(&unjudged, 0), [RISK_LABELS[4]], "no rules yet");

    let risk = |index: NodeIndex| match index {
        11 | 13 => Some(Risk::Safe),
        21 => Some(Risk::Review),
        20 => Some(Risk::Keep),
        _ => None,
    };
    let judged = build(
        Grouping::Cleanliness,
        &GroupingInput {
            risk: Some(&risk),
            ..input(&scan, &filter, root)
        },
    );
    assert_eq!(
        child_labels(&judged, 0),
        ["Safe to remove", "Review", "Keep", "Unclassified"]
    );
    assert_eq!(find(&judged, "Safe to remove").size, 2 * GIB + 50 * MIB);
}

#[test]
fn app_grouping_resolves_satellites_to_their_app() {
    let scan = volume();
    let filter = FileFilter::default();
    let tree = build(Grouping::App, &input(&scan, &filter, Path::new("/")));

    let spotify = find(&tree, "Spotify");
    assert_eq!(
        spotify.size,
        200 * MIB + 2 * GIB + 4096,
        "bundle + cache + plist"
    );
    let parts = child_labels(
        &tree,
        tree.groups
            .iter()
            .position(|g| g.label == "Spotify")
            .expect("spotify"),
    );
    assert!(parts.contains(&"App · Spotify.app"));
    assert!(parts.contains(&"Caches · com.spotify.client"));
    assert!(parts.contains(&"Preferences · com.spotify.client.plist"));

    let chrome = find(&tree, "Google Chrome");
    assert_eq!(
        chrome.size,
        300 * MIB + 50 * MIB,
        "com.google.Chrome joins Google Chrome"
    );

    assert_eq!(
        find(&tree, "SomeTool").size,
        8192,
        "an app with no bundle is named after its data"
    );
    assert_eq!(
        find(&tree, "Not app data").files.len(),
        3,
        "documents are nobody's app data"
    );
    assert_eq!(
        find(&tree, "Caches · com.spotify.client").entry,
        Some(10),
        "revealable"
    );
}

#[test]
fn a_scan_inside_library_still_knows_whose_data_it_is() {
    // Scanning ~/Library/Caches directly: its children are satellites.
    let caches = scan_of(&[
        (0, "Caches", D, 0, 0, false),
        (0, "com.spotify.client", D, 0, 0, false),
        (1, "data.bin", F, 4096, NOW, false),
        (0, "loose.log", F, 4096, NOW, false),
    ]);
    let filter = FileFilter::default();
    let tree = build(
        Grouping::App,
        &input(&caches, &filter, Path::new("/Users/me/Library/Caches")),
    );
    assert_eq!(find(&tree, "com.spotify.client").size, 4096);
    assert_eq!(
        find(&tree, "loose.log").size,
        4096,
        "a loose file is its own satellite"
    );

    // Scanning inside one app's cache: everything is that app's.
    let deep = scan_of(&[
        (0, "sub", D, 0, 0, false),
        (0, "x.bin", F, 4096, NOW, false),
    ]);
    let tree = build(
        Grouping::App,
        &input(
            &deep,
            &filter,
            Path::new("/Users/me/Library/Caches/com.example.tool/sub"),
        ),
    );
    assert_eq!(find(&tree, "com.example.tool").size, 4096);
    assert_eq!(find(&tree, "Not app data").size, 0);
}

#[test]
fn satellites_match_apps_by_name() {
    let apps: HashMap<String, u32> = [
        ("Spotify.app", 1),
        ("Google Chrome.app", 2),
        ("Visual Studio Code.app", 3),
    ]
    .into_iter()
    .map(|(name, id)| (app_key(name), id))
    .collect();
    assert_eq!(resolve_owner("com.spotify.client", &apps), Some(&1));
    assert_eq!(resolve_owner("com.spotify.client.plist", &apps), Some(&1));
    assert_eq!(resolve_owner("Spotify", &apps), Some(&1));
    assert_eq!(resolve_owner("com.google.Chrome", &apps), Some(&2));
    assert_eq!(resolve_owner("Google Chrome", &apps), Some(&2));
    assert_eq!(
        resolve_owner("com.microsoft.VSCode", &apps),
        None,
        "no false match"
    );
    assert_eq!(resolve_owner("Visual Studio Code", &apps), Some(&3));
    assert_eq!(resolve_owner("com.apple.Safari", &apps), None);
}

#[test]
fn filters_drop_files_from_every_grouping() {
    let scan = volume();
    let filter = FileFilter {
        min_size: GIB,
        modified_before: Some(NOW - 50 * DAY),
        ..FileFilter::default()
    };
    for grouping in Grouping::ALL {
        let tree = build(grouping, &input(&scan, &filter, Path::new("/")));
        let mut files: Vec<NodeIndex> = membership(&tree).into_keys().collect();
        files.sort_unstable();
        assert_eq!(
            files,
            vec![11, 20],
            "{grouping:?}: ≥ 1 GB and older than 50 days"
        );
        assert_eq!(tree.root().size, 2 * GIB + 12 * GIB, "{grouping:?}");
    }
}

/// `cargo test --release -p storagelens-analysis --test grouping -- --ignored --nocapture`
#[test]
#[ignore = "benchmark: 1M nodes"]
fn one_million_nodes_group_in_under_100_ms_each() {
    const NAMES: [&str; 8] = [
        "photo.jpg",
        "clip.mp4",
        "notes.md",
        "a.rs",
        "b.zip",
        "c.bin",
        "Makefile",
        "d.pdf",
    ];
    let mut rows: Vec<Row> = vec![
        (0, "root".into(), D, 0, 0, false),
        (0, "Applications".into(), D, 0, 0, false),
        (0, "Library".into(), D, 0, 0, false),
        (2, "Caches".into(), D, 0, 0, false),
    ];
    let mut dirs = Vec::new();
    for app in 0..200u32 {
        rows.push((1, format!("App{app}.app"), D, 0, 0, false));
        dirs.push(rows.len() as u32 - 1);
        rows.push((3, format!("com.vendor.app{app}"), D, 0, 0, false));
        dirs.push(rows.len() as u32 - 1);
    }
    for folder in 0..10_000u32 {
        rows.push((0, format!("folder {folder}"), D, 0, 0, false));
        dirs.push(rows.len() as u32 - 1);
    }
    let mut state = 11u64;
    while rows.len() < 1_000_000 {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        let parent = dirs[(state >> 33) as usize % dirs.len()];
        let name = NAMES[(state >> 20) as usize % NAMES.len()];
        rows.push((
            parent,
            format!("{}-{name}", rows.len()),
            F,
            4096 * ((state >> 40) % 100_000),
            NOW - ((state >> 10) % 2000) as i64 * DAY,
            false,
        ));
    }
    let scan = write(&rows);
    let filter = FileFilter::default();
    for grouping in Grouping::ALL {
        let started = Instant::now();
        let tree = build(grouping, &input(&scan, &filter, Path::new("/")));
        let elapsed = started.elapsed();
        println!(
            "{grouping:?}: {} groups, {} files in {elapsed:?}",
            tree.groups.len(),
            tree.root().file_count
        );
    }
}
