//! Duplicate finder against real files planted in a temp dir.

mod common;

use std::fs;
use std::path::Path;

use common::{scan, D, F};
use storagelens_analysis::dupes::{candidates, find, Options, Progress, MIN_SIZE};
use storagelens_core::CancelToken;
use storagelens_safety::Guard;

const MB: usize = 1 << 20;

fn bytes(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn fast() -> Options {
    Options {
        duty: 1.0,
        ..Options::default()
    }
}

/// `(relative path, content)`; directories are created as needed and listed
/// for the catalog parents first.
fn plant(
    root: &Path,
    files: &[(&str, Vec<u8>)],
) -> Vec<(String, storagelens_core::EntryKind, u64, i64)> {
    let mut rows = Vec::new();
    let mut dirs = std::collections::BTreeSet::new();
    for (path, content) in files {
        let full = root.join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(&full, content).unwrap();
        let mut prefix = String::new();
        for part in path.split('/').collect::<Vec<_>>().split_last().unwrap().1 {
            prefix = if prefix.is_empty() {
                (*part).to_owned()
            } else {
                format!("{prefix}/{part}")
            };
            if dirs.insert(prefix.clone()) {
                rows.push((prefix.clone(), D, 0, 0));
            }
        }
        rows.push(((*path).to_owned(), F, content.len() as u64, 0));
    }
    rows
}

#[test]
fn finds_every_planted_duplicate_and_nothing_else() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let a = bytes(MB + MB / 2, 1);
    let b = bytes(2 * MB, 2);
    let mut b_last = b.clone();
    *b_last.last_mut().unwrap() ^= 1;
    let mut b_first = b.clone();
    b_first[0] ^= 1;
    // Same size and same first 64 KiB as `a`: only the full hash separates it.
    let mut a_middle = a.clone();
    a_middle[MB] ^= 1;
    let small = bytes(100_000, 3);
    let lib = bytes(MB, 4);
    let bundle = bytes(MB, 5);

    let rows = plant(
        &root,
        &[
            ("a1.bin", a.clone()),
            ("docs/a2.bin", a.clone()),
            ("docs/deep/a3.bin", a.clone()),
            ("a_middle.bin", a_middle),
            ("b1.bin", b.clone()),
            ("b2.bin", b),
            ("b_last.bin", b_last),
            ("b_first.bin", b_first),
            ("s1.bin", small.clone()),
            ("s2.bin", small),
            ("home/Library/x1", lib.clone()),
            ("home/Library/x2", lib),
            ("Foo.app/Contents/r1", bundle.clone()),
            ("Foo.app/Contents/r2", bundle),
        ],
    );
    let rows: Vec<(&str, _, u64, i64)> = rows
        .iter()
        .map(|(p, k, s, m)| (p.as_str(), *k, *s, *m))
        .collect();
    let scanned = scan(root.to_str().unwrap(), &rows);
    let home = root.join("home");
    let guard = Guard::new(&root, Some(&home)).unwrap();

    let found = candidates(&scanned.arena, &root, &guard, Some(&home), MIN_SIZE);
    assert!(
        found
            .iter()
            .all(|c| !c.path.starts_with(home.join("Library"))
                && !c.path.to_string_lossy().contains("Foo.app")
                && c.size_logical >= MIN_SIZE),
        "~/Library, bundle internals and small files are skipped: {found:?}"
    );

    let mut updates: Vec<Progress> = Vec::new();
    let report = find(found, fast(), &CancelToken::new(), &mut |p| updates.push(p));
    assert!(!report.cancelled);
    assert_eq!(report.unreadable, 0);

    let groups: Vec<Vec<String>> = report
        .groups
        .iter()
        .map(|group| {
            group
                .files
                .iter()
                .map(|f| {
                    f.path
                        .strip_prefix(&root)
                        .unwrap()
                        .display()
                        .to_string()
                        // Paths carry the platform separator; the expectations
                        // below are written with `/`.
                        .replace(std::path::MAIN_SEPARATOR, "/")
                })
                .collect()
        })
        .collect();
    assert_eq!(
        groups,
        [
            vec!["a1.bin", "docs/a2.bin", "docs/deep/a3.bin"],
            vec!["b1.bin", "b2.bin"],
        ],
        "largest reclaimable first (3 MiB, then 2 MiB); near-misses never reported"
    );
    assert_eq!(report.groups[0].reclaimable(), 2 * (MB + MB / 2) as u64);
    assert_eq!(report.groups[0].hash.len(), 64, "hex BLAKE3");

    let last = updates.last().copied().unwrap();
    assert_eq!(last.groups_done, last.groups_total);
    assert_eq!(last.groups_total, 2, "two sizes shared by ≥ 2 candidates");
    // Stage 2 reads 64 KiB of each of the 8 candidates; stage 3 reads in full
    // only the files whose partial hash collided (a ×3 + a_middle, b ×2 +
    // b_last). b_first never reaches stage 3.
    let full = 4 * a.len() as u64 + 3 * 2 * MB as u64;
    assert_eq!(last.bytes_read, 8 * 64 * 1024 + full);
}

#[test]
fn a_file_changed_since_the_scan_is_unreadable_not_a_duplicate() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let content = bytes(MB, 7);
    let rows = plant(&root, &[("d1", content.clone()), ("d2", content)]);
    let rows: Vec<(&str, _, u64, i64)> = rows
        .iter()
        .map(|(p, k, s, m)| (p.as_str(), *k, *s, *m))
        .collect();
    let scanned = scan(root.to_str().unwrap(), &rows);
    fs::write(root.join("d2"), bytes(MB - 1, 7)).unwrap();

    let guard = Guard::new(&root, None).unwrap();
    let found = candidates(&scanned.arena, &root, &guard, None, MIN_SIZE);
    assert_eq!(found.len(), 2);
    let report = find(found, fast(), &CancelToken::new(), &mut |_| {});
    assert!(report.groups.is_empty());
    assert_eq!(report.unreadable, 1);
}

#[test]
fn cancel_stops_mid_group() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let content = bytes(4 * MB, 9);
    let rows = plant(
        &root,
        &[
            ("c1", content.clone()),
            ("c2", content.clone()),
            ("c3", content),
        ],
    );
    let rows: Vec<(&str, _, u64, i64)> = rows
        .iter()
        .map(|(p, k, s, m)| (p.as_str(), *k, *s, *m))
        .collect();
    let scanned = scan(root.to_str().unwrap(), &rows);
    let guard = Guard::new(&root, None).unwrap();
    let found = candidates(&scanned.arena, &root, &guard, None, MIN_SIZE);

    let cancel = CancelToken::new();
    let mut last = Progress::default();
    let report = find(found, fast(), &cancel, &mut |p| {
        last = p;
        // After the partial hashes, during the first full hash.
        if p.files_hashed == 3 {
            cancel.cancel();
        }
    });
    assert!(report.cancelled);
    assert!(report.groups.is_empty(), "the group never finished");
    assert_eq!(last.groups_done, 0);
    assert!(
        last.bytes_read < 3 * 4 * MB as u64,
        "stopped before reading every file in full: {last:?}"
    );
}
