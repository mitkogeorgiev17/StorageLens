//! The macOS rule catalog (#32): every rule's exact matches and byte total
//! over one fixture volume, with a look-alike beside each real case.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::{scan, D, F};
use storagelens_analysis::rules::{evaluate, Context, Evaluation, RuleSet};
use storagelens_analysis::Risk;
use storagelens_safety::{Guard, Platform};

const NOW: i64 = 1_800_000_000;
const DAY: i64 = 86_400;
const OLD: i64 = NOW - 400 * DAY;
const NEW: i64 = NOW - DAY;
const HOME: &str = "/Users/me";

const XCODE: &str = "Users/me/Library/Developer/Xcode";
const SIM: &str = "Users/me/Library/Developer/CoreSimulator";

fn volume() -> common::Scan {
    let rows: Vec<(String, storagelens_core::EntryKind, u64, i64)> = [
        ("Users", D, 0, 0),
        ("Users/me", D, 0, 0),
        ("Users/me/Library", D, 0, 0),
        ("Users/me/Library/Developer", D, 0, 0),
        (XCODE, D, 0, 0),
        // Derived Data: every project folder, whatever its age.
        (&format!("{XCODE}/DerivedData"), D, 0, 0),
        (&format!("{XCODE}/DerivedData/App-abc"), D, 0, 0),
        (&format!("{XCODE}/DerivedData/App-abc/Build"), F, 1_000, NEW),
        (&format!("{XCODE}/DerivedData/ModuleCache.noindex"), D, 0, 0),
        (
            &format!("{XCODE}/DerivedData/ModuleCache.noindex/m"),
            F,
            10,
            NEW,
        ),
        // Device support: versions unused for 90 days.
        (&format!("{XCODE}/iOS DeviceSupport"), D, 0, 0),
        (&format!("{XCODE}/iOS DeviceSupport/17.0 (21A329)"), D, 0, 0),
        (
            &format!("{XCODE}/iOS DeviceSupport/17.0 (21A329)/Symbols"),
            F,
            2_000,
            OLD,
        ),
        (&format!("{XCODE}/iOS DeviceSupport/18.1 (22B83)"), D, 0, 0),
        (
            &format!("{XCODE}/iOS DeviceSupport/18.1 (22B83)/Symbols"),
            F,
            500,
            NEW,
        ),
        (&format!("{XCODE}/watchOS DeviceSupport"), D, 0, 0),
        (&format!("{XCODE}/watchOS DeviceSupport/10.0"), D, 0, 0),
        (
            &format!("{XCODE}/watchOS DeviceSupport/10.0/Symbols"),
            F,
            300,
            OLD,
        ),
        // Archives: old ones only.
        (&format!("{XCODE}/Archives"), D, 0, 0),
        (&format!("{XCODE}/Archives/2025-01-01"), D, 0, 0),
        (
            &format!("{XCODE}/Archives/2025-01-01/App 1.0.xcarchive"),
            D,
            0,
            0,
        ),
        (
            &format!("{XCODE}/Archives/2025-01-01/App 1.0.xcarchive/Info.plist"),
            F,
            400,
            OLD,
        ),
        (&format!("{XCODE}/Archives/2026-10-30"), D, 0, 0),
        (
            &format!("{XCODE}/Archives/2026-10-30/App 2.0.xcarchive"),
            D,
            0,
            0,
        ),
        (
            &format!("{XCODE}/Archives/2026-10-30/App 2.0.xcarchive/Info.plist"),
            F,
            100,
            NEW,
        ),
        // Previews.
        (&format!("{XCODE}/UserData"), D, 0, 0),
        (&format!("{XCODE}/UserData/Previews"), D, 0, 0),
        (&format!("{XCODE}/UserData/Previews/sim"), F, 50, NEW),
        (&format!("{XCODE}/UserData/KeyBindings"), D, 0, 0),
        // Simulators: devices unused for 90 days; the device set file is not one.
        (SIM, D, 0, 0),
        (&format!("{SIM}/Devices"), D, 0, 0),
        (&format!("{SIM}/Devices/device_set.plist"), F, 2, OLD),
        (&format!("{SIM}/Devices/AAAA"), D, 0, 0),
        (&format!("{SIM}/Devices/AAAA/device.plist"), F, 1, OLD),
        (&format!("{SIM}/Devices/AAAA/data"), F, 600, OLD),
        (&format!("{SIM}/Devices/BBBB"), D, 0, 0),
        (&format!("{SIM}/Devices/BBBB/device.plist"), F, 1, NEW),
        (&format!("{SIM}/Devices/CCCC"), D, 0, 0),
        (&format!("{SIM}/Devices/CCCC/leftover"), F, 9, OLD),
        (&format!("{SIM}/Caches"), D, 0, 0),
        (&format!("{SIM}/Caches/dyld"), F, 70, NEW),
        // Caches: third-party, unused for 90 days. Apple's and Homebrew's are not.
        ("Users/me/Library/Caches", D, 0, 0),
        ("Users/me/Library/Caches/com.spotify.client", D, 0, 0),
        (
            "Users/me/Library/Caches/com.spotify.client/data",
            F,
            800,
            OLD,
        ),
        ("Users/me/Library/Caches/com.apple.Safari", D, 0, 0),
        ("Users/me/Library/Caches/com.apple.Safari/db", F, 90, OLD),
        ("Users/me/Library/Caches/CloudKit", D, 0, 0),
        ("Users/me/Library/Caches/CloudKit/x", F, 40, OLD),
        ("Users/me/Library/Caches/org.fresh", D, 0, 0),
        ("Users/me/Library/Caches/org.fresh/x", F, 10, NEW),
        ("Users/me/Library/Caches/Homebrew", D, 0, 0),
        (
            "Users/me/Library/Caches/Homebrew/node.bottle.tar.gz",
            F,
            120,
            NEW,
        ),
        ("Users/me/Library/Caches/loose-file", F, 5, OLD),
        // Containers.
        ("Users/me/Library/Containers", D, 0, 0),
        ("Users/me/Library/Containers/com.x.editor", D, 0, 0),
        ("Users/me/Library/Containers/com.x.editor/Data", D, 0, 0),
        (
            "Users/me/Library/Containers/com.x.editor/Data/Library",
            D,
            0,
            0,
        ),
        (
            "Users/me/Library/Containers/com.x.editor/Data/Library/Caches",
            D,
            0,
            0,
        ),
        (
            "Users/me/Library/Containers/com.x.editor/Data/Library/Caches/c",
            F,
            200,
            OLD,
        ),
        ("Users/me/Library/Containers/com.apple.mail", D, 0, 0),
        ("Users/me/Library/Containers/com.apple.mail/Data", D, 0, 0),
        (
            "Users/me/Library/Containers/com.apple.mail/Data/Library",
            D,
            0,
            0,
        ),
        (
            "Users/me/Library/Containers/com.apple.mail/Data/Library/Caches",
            D,
            0,
            0,
        ),
        (
            "Users/me/Library/Containers/com.apple.mail/Data/Library/Caches/c",
            F,
            50,
            OLD,
        ),
        (
            "Users/me/Library/Containers/com.apple.mail/Data/Library/Mail Downloads",
            D,
            0,
            0,
        ),
        (
            "Users/me/Library/Containers/com.apple.mail/Data/Library/Mail Downloads/ABC",
            D,
            0,
            0,
        ),
        (
            "Users/me/Library/Containers/com.apple.mail/Data/Library/Mail Downloads/ABC/i.pdf",
            F,
            30,
            OLD,
        ),
        ("Users/me/Library/Containers/com.docker.docker", D, 0, 0),
        (
            "Users/me/Library/Containers/com.docker.docker/Data",
            D,
            0,
            0,
        ),
        (
            "Users/me/Library/Containers/com.docker.docker/Data/vms",
            D,
            0,
            0,
        ),
        (
            "Users/me/Library/Containers/com.docker.docker/Data/vms/0",
            D,
            0,
            0,
        ),
        (
            "Users/me/Library/Containers/com.docker.docker/Data/vms/0/data",
            D,
            0,
            0,
        ),
        (
            "Users/me/Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw",
            F,
            5_000,
            NEW,
        ),
        // Logs, in the home and the system library.
        ("Users/me/Library/Logs", D, 0, 0),
        ("Users/me/Library/Logs/App", D, 0, 0),
        ("Users/me/Library/Logs/App/a.log", F, 40, OLD),
        ("Users/me/Library/Logs/fresh.log", F, 5, NEW),
        // iOS backups.
        ("Users/me/Library/Application Support", D, 0, 0),
        ("Users/me/Library/Application Support/MobileSync", D, 0, 0),
        (
            "Users/me/Library/Application Support/MobileSync/Backup",
            D,
            0,
            0,
        ),
        (
            "Users/me/Library/Application Support/MobileSync/Backup/00008110-AAAA",
            D,
            0,
            0,
        ),
        (
            "Users/me/Library/Application Support/MobileSync/Backup/00008110-AAAA/Manifest.db",
            F,
            3_000,
            NEW,
        ),
        // Mail's own attachments: reported, never offered.
        ("Users/me/Library/Mail", D, 0, 0),
        ("Users/me/Library/Mail/V10", D, 0, 0),
        ("Users/me/Library/Mail/V10/ACCOUNT", D, 0, 0),
        ("Users/me/Library/Mail/V10/ACCOUNT/INBOX.mbox", D, 0, 0),
        (
            "Users/me/Library/Mail/V10/ACCOUNT/INBOX.mbox/Attachments",
            D,
            0,
            0,
        ),
        (
            "Users/me/Library/Mail/V10/ACCOUNT/INBOX.mbox/Attachments/a.jpg",
            F,
            80,
            OLD,
        ),
        // Trash.
        ("Users/me/.Trash", D, 0, 0),
        ("Users/me/.Trash/old.doc", F, 70, OLD),
        // Downloads.
        ("Users/me/Downloads", D, 0, 0),
        ("Users/me/Downloads/App.dmg", F, 900, OLD),
        ("Users/me/Downloads/Photos.ZIP", F, 50, OLD),
        ("Users/me/Downloads/new.pkg", F, 100, NEW),
        ("Users/me/Downloads/report.pdf", F, 20, OLD),
        ("Users/me/Downloads/recent.pdf", F, 20, NEW),
        ("Users/me/Downloads/Old folder", D, 0, 0),
        ("Users/me/Downloads/Old folder/x", F, 10, OLD),
        // npm.
        ("Users/me/.npm", D, 0, 0),
        ("Users/me/.npm/_cacache", D, 0, 0),
        ("Users/me/.npm/_cacache/index", F, 150, NEW),
        ("Users/me/.npm/_logs", D, 0, 0),
        // Outside the home.
        ("Library", D, 0, 0),
        ("Library/Logs", D, 0, 0),
        ("Library/Logs/install.log", F, 60, OLD),
        ("Library/Caches", D, 0, 0),
        ("Library/Caches/com.x", D, 0, 0),
        (".DocumentRevisions-V100", D, 0, 0),
        (".DocumentRevisions-V100/store", F, 700, OLD),
        ("private", D, 0, 0),
        ("private/var", D, 0, 0),
        ("private/var/vm", D, 0, 0),
        ("private/var/vm/swapfile0", F, 1_024, NEW),
    ]
    .into_iter()
    .map(|(path, kind, bytes, modified)| (path.to_owned(), kind, bytes, modified))
    .collect();
    let borrowed: Vec<_> = rows
        .iter()
        .map(|(path, kind, bytes, modified)| (path.as_str(), *kind, *bytes, *modified))
        .collect();
    scan("/", &borrowed)
}

fn evaluated(scan: &common::Scan) -> (RuleSet, Evaluation) {
    let rules = RuleSet::builtin(&Context::new(Platform::MacOs, Some(HOME)));
    assert!(rules.problems.is_empty(), "{:?}", rules.problems);
    let guard = Guard::from_resolved(Platform::MacOs, "/", Some(HOME)).expect("guard");
    let evaluation = evaluate(&rules, &scan.arena, "/", Some(&scan.times), NOW, &guard);
    (rules, evaluation)
}

#[test]
fn every_macos_rule_matches_exactly_its_fixture() {
    let scan = volume();
    let (rules, evaluation) = evaluated(&scan);
    assert!(evaluation.refused.is_empty(), "{:?}", evaluation.refused);

    let mut found: BTreeMap<&str, (BTreeSet<&str>, u64)> = BTreeMap::new();
    for hit in &evaluation.hits {
        let entry = found.entry(hit.rule_id.as_str()).or_default();
        entry.0.insert(hit.path.as_str());
        entry.1 += hit.bytes;
    }
    let expected: BTreeMap<&str, (BTreeSet<&str>, u64)> = [
        (
            "macos.xcode.derived_data",
            vec![
                "/Users/me/Library/Developer/Xcode/DerivedData/App-abc",
                "/Users/me/Library/Developer/Xcode/DerivedData/ModuleCache.noindex",
            ],
            1_010,
        ),
        (
            "macos.xcode.device_support",
            vec![
                "/Users/me/Library/Developer/Xcode/iOS DeviceSupport/17.0 (21A329)",
                "/Users/me/Library/Developer/Xcode/watchOS DeviceSupport/10.0",
            ],
            2_300,
        ),
        (
            "macos.xcode.archives",
            vec!["/Users/me/Library/Developer/Xcode/Archives/2025-01-01/App 1.0.xcarchive"],
            400,
        ),
        (
            "macos.xcode.previews",
            vec!["/Users/me/Library/Developer/Xcode/UserData/Previews"],
            50,
        ),
        (
            "macos.simulator.devices",
            vec!["/Users/me/Library/Developer/CoreSimulator/Devices/AAAA"],
            601,
        ),
        (
            "macos.simulator.caches",
            vec!["/Users/me/Library/Developer/CoreSimulator/Caches"],
            70,
        ),
        (
            "macos.user_caches",
            vec!["/Users/me/Library/Caches/com.spotify.client"],
            800,
        ),
        (
            "macos.container_caches",
            vec!["/Users/me/Library/Containers/com.x.editor/Data/Library/Caches"],
            200,
        ),
        (
            "macos.logs",
            vec!["/Users/me/Library/Logs/App", "/Library/Logs/install.log"],
            100,
        ),
        ("macos.trash", vec!["/Users/me/.Trash"], 70),
        (
            "macos.downloads.installers",
            vec![
                "/Users/me/Downloads/App.dmg",
                "/Users/me/Downloads/Photos.ZIP",
            ],
            950,
        ),
        (
            "macos.downloads.old",
            vec![
                "/Users/me/Downloads/report.pdf",
                "/Users/me/Downloads/Old folder",
            ],
            30,
        ),
        (
            "macos.ios_backups",
            vec!["/Users/me/Library/Application Support/MobileSync/Backup/00008110-AAAA"],
            3_000,
        ),
        (
            "macos.mail_attachments",
            vec!["/Users/me/Library/Mail/V10/ACCOUNT/INBOX.mbox/Attachments"],
            80,
        ),
        (
            "macos.mail_downloads",
            vec!["/Users/me/Library/Containers/com.apple.mail/Data/Library/Mail Downloads/ABC"],
            30,
        ),
        (
            "macos.docker_raw",
            vec!["/Users/me/Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw"],
            5_000,
        ),
        (
            "macos.homebrew_cache",
            vec!["/Users/me/Library/Caches/Homebrew"],
            120,
        ),
        ("macos.npm_cache", vec!["/Users/me/.npm/_cacache"], 150),
        (
            "macos.document_revisions",
            vec!["/.DocumentRevisions-V100"],
            700,
        ),
        ("macos.swap", vec!["/private/var/vm"], 1_024),
    ]
    .into_iter()
    .map(|(id, paths, bytes)| (id, (paths.into_iter().collect(), bytes)))
    .collect();
    assert_eq!(found, expected);

    // Every macOS rule has a case in the fixture.
    let macos: BTreeSet<&str> = rules
        .rules
        .iter()
        .map(|compiled| compiled.rule.id.as_str())
        .filter(|id| id.starts_with("macos."))
        .collect();
    assert_eq!(macos, expected.keys().copied().collect());
}

#[test]
fn docker_and_the_other_report_rules_can_never_be_selected() {
    let scan = volume();
    let (_, evaluation) = evaluated(&scan);
    let candidates: BTreeSet<&str> = evaluation
        .candidates()
        .map(|candidate| candidate.hit().rule_id.as_str())
        .collect();
    for id in [
        "macos.docker_raw",
        "macos.trash",
        "macos.mail_attachments",
        "macos.document_revisions",
        "macos.swap",
    ] {
        let hit = evaluation
            .hits
            .iter()
            .find(|hit| hit.rule_id == id)
            .expect(id);
        assert!(!hit.deletable && !hit.selected, "{id}");
        assert!(!candidates.contains(id), "{id}");
    }
    let docker = evaluation
        .hits
        .iter()
        .find(|hit| hit.rule_id == "macos.docker_raw")
        .expect("docker");
    assert_eq!(
        docker.risk,
        Risk::Review,
        "shown as needing a look, with its prune advice"
    );

    // Selected by default: only what is regenerable or plainly stale.
    let selected: BTreeSet<&str> = evaluation
        .hits
        .iter()
        .filter(|hit| hit.selected)
        .map(|hit| hit.rule_id.as_str())
        .collect();
    for id in [
        "macos.xcode.archives",
        "macos.simulator.devices",
        "macos.downloads.old",
        "macos.ios_backups",
    ] {
        assert!(!selected.contains(id), "{id} must be chosen by hand");
    }
    assert!(selected.contains("macos.xcode.derived_data"));
}

#[test]
fn the_macos_rules_leave_the_reclaim_score_to_what_can_go() {
    let scan = volume();
    let (_, evaluation) = evaluated(&scan);
    // Safe: derived 1010 × .98, device support 2300 × .95, previews 50 × .95,
    // simulator caches 70 × .95, user caches 800 × .9, containers 200 × .9,
    // logs 100 × .9, Homebrew 120 × .95, npm 150 × .95.
    let safe = 1_010.0 * 0.98
        + 2_300.0 * 0.95
        + 50.0 * 0.95
        + 70.0 * 0.95
        + 800.0 * 0.9
        + 200.0 * 0.9
        + 100.0 * 0.9
        + 120.0 * 0.95
        + 150.0 * 0.95;
    // Review (× 0.5): archives 400 × .7, simulators 601 × .75, installers
    // 950 × .85, old downloads 30 × .6, Mail downloads 30 × .85.
    let review = (400.0 * 0.7 + 601.0 * 0.75 + 950.0 * 0.85 + 30.0 * 0.6 + 30.0 * 0.85) * 0.5;
    assert!(
        (evaluation.reclaim.safe - safe).abs() < 1e-6,
        "{} vs {safe}",
        evaluation.reclaim.safe
    );
    assert!(
        (evaluation.reclaim.review - review).abs() < 1e-6,
        "{} vs {review}",
        evaluation.reclaim.review
    );
}

/// A container whose bundle id ends in `.app` looks like an app bundle from
/// its path, so the guard refuses its caches. Too cautious, never too bold:
/// the match is listed as refused, not dropped silently.
#[test]
fn a_container_named_like_a_bundle_is_refused_not_cleaned() {
    let scan = scan(
        "/",
        &[
            ("Users", D, 0, 0),
            ("Users/me", D, 0, 0),
            ("Users/me/Library", D, 0, 0),
            ("Users/me/Library/Containers", D, 0, 0),
            ("Users/me/Library/Containers/com.vendor.app", D, 0, 0),
            ("Users/me/Library/Containers/com.vendor.app/Data", D, 0, 0),
            (
                "Users/me/Library/Containers/com.vendor.app/Data/Library",
                D,
                0,
                0,
            ),
            (
                "Users/me/Library/Containers/com.vendor.app/Data/Library/Caches",
                D,
                0,
                0,
            ),
            (
                "Users/me/Library/Containers/com.vendor.app/Data/Library/Caches/c",
                F,
                10,
                OLD,
            ),
        ],
    );
    let (_, evaluation) = evaluated(&scan);
    assert!(evaluation.hits.is_empty());
    assert_eq!(evaluation.refused.len(), 1);
    assert_eq!(evaluation.refused[0].0, "macos.container_caches");
}
