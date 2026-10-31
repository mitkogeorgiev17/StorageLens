//! The cleanup rules engine (#31): golden fixtures for every built-in rule,
//! the guard against system paths, report-only rules, the Reclaim formula,
//! catalog loading and hot reload, and a 1M-node benchmark.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::time::Instant;

use storagelens_analysis::rules::{
    evaluate, fingerprint, risk_weight, Context, Evaluation, Kind, RuleSet, BUILTIN,
};
use storagelens_analysis::{build, FileFilter, Grouping, GroupingInput, Risk};
use storagelens_core::EntryKind;
use storagelens_safety::{Guard, Platform, Refusal};

const NOW: i64 = 1_800_000_000;
const DAY: i64 = 86_400;
const OLD: i64 = NOW - 400 * DAY;
const NEW: i64 = NOW - DAY;
const HOME: &str = "/Users/me";

mod common;

use common::{scan, scan_fast, Scan, D, F};

fn mac() -> Context {
    Context::new(Platform::MacOs, Some(HOME))
}

fn run(rules: &RuleSet, scan: &Scan, root: &str) -> Evaluation {
    let guard = Guard::from_resolved(rules.platform, root, Some(HOME)).expect("guard");
    evaluate(rules, &scan.arena, root, Some(&scan.times), NOW, &guard)
}

/// `(rule id, path)` of every hit.
fn hit_set(evaluation: &Evaluation) -> BTreeSet<(String, String)> {
    evaluation
        .hits
        .iter()
        .map(|hit| (hit.rule_id.clone(), hit.path.clone()))
        .collect()
}

fn set(pairs: &[(&str, &str)]) -> BTreeSet<(String, String)> {
    pairs
        .iter()
        .map(|(id, path)| ((*id).to_owned(), (*path).to_owned()))
        .collect()
}

fn rules(json: &str) -> RuleSet {
    let set = RuleSet::compile(&mac(), [("test.json", json)]);
    assert!(set.problems.is_empty(), "{:?}", set.problems);
    set
}

/// A home with every shared rule's real case next to its look-alikes.
fn home_fixture() -> Scan {
    scan(
        HOME,
        &[
            // node_modules: stale project → hit; nested copy is inside the hit.
            ("web", D, 0, 0),
            ("web/package.json", F, 1, OLD),
            ("web/node_modules", D, 0, 0),
            ("web/node_modules/a.js", F, 100, OLD),
            ("web/node_modules/dep", D, 0, 0),
            ("web/node_modules/dep/node_modules", D, 0, 0),
            ("web/node_modules/dep/node_modules/b.js", F, 50, OLD),
            ("web/dist", D, 0, 0),
            ("web/dist/app.js", F, 30, OLD),
            ("web/.next", D, 0, 0),
            ("web/.next/cache", F, 40, NEW),
            // Fresh project: not stale → no node_modules or dist hit.
            ("fresh", D, 0, 0),
            ("fresh/package.json", F, 1, NEW),
            ("fresh/node_modules", D, 0, 0),
            ("fresh/node_modules/a.js", F, 100, NEW),
            ("fresh/dist", D, 0, 0),
            ("fresh/dist/app.js", F, 10, NEW),
            // No package.json: just a folder that happens to be called that.
            ("notes", D, 0, 0),
            ("notes/node_modules", D, 0, 0),
            ("notes/node_modules/x.md", F, 5, OLD),
            ("notes/dist", D, 0, 0),
            ("notes/.next", D, 0, 0),
            // Python.
            ("py", D, 0, 0),
            ("py/__pycache__", D, 0, 0),
            ("py/__pycache__/m.cpython-312.pyc", F, 8, NEW),
            ("py/.venv", D, 0, 0),
            ("py/.venv/pyvenv.cfg", F, 1, OLD),
            ("py/.venv/lib", F, 200, OLD),
            ("py/venv", D, 0, 0),
            ("py/venv/notes.txt", F, 3, OLD),
            ("py/__pycache__.txt", F, 2, OLD),
            // Rust.
            ("rs", D, 0, 0),
            ("rs/Cargo.toml", F, 1, OLD),
            ("rs/target", D, 0, 0),
            ("rs/target/debug", F, 500, OLD),
            ("aim", D, 0, 0),
            ("aim/target", D, 0, 0),
            ("aim/target/practice.txt", F, 7, OLD),
            // Gradle: only the home's own cache.
            (".gradle", D, 0, 0),
            (".gradle/caches", D, 0, 0),
            (".gradle/caches/jars", F, 70, NEW),
            ("other", D, 0, 0),
            ("other/.gradle", D, 0, 0),
            ("other/.gradle/caches", D, 0, 0),
            // Inside an app bundle: the guard refuses it.
            ("Tool.app", D, 0, 0),
            ("Tool.app/Contents", D, 0, 0),
            ("Tool.app/Contents/package.json", F, 1, OLD),
            ("Tool.app/Contents/node_modules", D, 0, 0),
            ("Tool.app/Contents/node_modules/c.js", F, 9, OLD),
        ],
    )
}

#[test]
fn every_built_in_rule_matches_exactly_its_golden_set() {
    let scan = home_fixture();
    let rules = RuleSet::builtin(&mac());
    assert!(rules.problems.is_empty(), "{:?}", rules.problems);
    assert_eq!(rules.rules.len(), 7 + 20, "shared + macOS");
    let evaluation = run(&rules, &scan, HOME);
    assert_eq!(
        hit_set(&evaluation),
        set(&[
            ("dev.node_modules", "/Users/me/web/node_modules"),
            ("dev.js_dist", "/Users/me/web/dist"),
            ("dev.next_build", "/Users/me/web/.next"),
            ("dev.python_bytecode", "/Users/me/py/__pycache__"),
            ("dev.python_venv", "/Users/me/py/.venv"),
            ("dev.rust_target", "/Users/me/rs/target"),
            ("dev.gradle_caches", "/Users/me/.gradle/caches"),
        ])
    );
    let refused: Vec<_> = evaluation
        .refused
        .iter()
        .map(|(id, path, why)| (id.as_str(), path.as_str(), *why))
        .collect();
    assert_eq!(
        refused,
        [(
            "dev.node_modules",
            "/Users/me/Tool.app/Contents/node_modules",
            Refusal::BundleInternal
        )]
    );
    let node_modules = &evaluation
        .hits
        .iter()
        .find(|hit| hit.rule_id == "dev.node_modules")
        .expect("hit");
    assert_eq!(
        node_modules.bytes, 150,
        "the nested copy is counted once, inside"
    );
    assert!(node_modules.deletable && node_modules.selected && node_modules.primary);
    let dist = evaluation
        .hits
        .iter()
        .find(|hit| hit.rule_id == "dev.js_dist")
        .expect("hit");
    assert_eq!((dist.risk, dist.selected), (Risk::Review, true));
}

#[test]
fn the_built_in_catalog_is_valid_json_with_calm_explanations() {
    for (name, text) in BUILTIN {
        let rules: Vec<storagelens_analysis::rules::Rule> =
            serde_json::from_str(text).unwrap_or_else(|error| panic!("{name}: {error}"));
        for rule in rules {
            rule.validate().expect("valid");
            assert!(
                rule.explain.len() > 20 && rule.explain.ends_with('.'),
                "{}",
                rule.id
            );
            for word in ["!", "danger", "warning", "immediately"] {
                assert!(
                    !rule.explain.to_lowercase().contains(word),
                    "{}: {word}",
                    rule.id
                );
            }
        }
    }
}

#[test]
fn deletable_rules_never_match_a_system_path() {
    let scan = scan(
        "/",
        &[
            ("System", D, 0, 0),
            ("System/Library", D, 0, 0),
            ("System/Library/Caches", D, 0, 0),
            ("System/Library/Caches/x", F, 10, OLD),
            ("usr", D, 0, 0),
            ("usr/local", D, 0, 0),
            ("usr/local/Caches", D, 0, 0),
            ("private", D, 0, 0),
            ("private/var", D, 0, 0),
            ("private/var/vm", D, 0, 0),
            ("private/var/vm/sleepimage", F, 4096, OLD),
            ("Library", D, 0, 0),
            ("Library/Caches", D, 0, 0),
            ("Library/Caches/x", F, 10, OLD),
        ],
    );
    let rules = rules(
        r#"[
          {"id": "greedy.caches", "platform": "any", "title": "Every Caches folder", "glob": "**/Caches",
           "kind": "regenerable", "explain": "Test rule."},
          {"id": "system.vm", "platform": "macos", "title": "Swap and sleep image", "glob": "/private/var/vm",
           "kind": "system", "explain": "Test rule."}
        ]"#,
    );
    let evaluation = run(&rules, &scan, "/");
    assert_eq!(
        hit_set(&evaluation),
        set(&[
            ("greedy.caches", "/Library/Caches"),
            ("system.vm", "/private/var/vm"),
        ])
    );
    let refused: BTreeSet<_> = evaluation
        .refused
        .iter()
        .map(|(_, path, _)| path.as_str())
        .collect();
    assert_eq!(
        refused,
        BTreeSet::from(["/System/Library/Caches", "/usr/local/Caches"])
    );

    // The system rule is reported, never deletable, and frees nothing.
    let vm = evaluation
        .hits
        .iter()
        .find(|hit| hit.rule_id == "system.vm")
        .expect("vm");
    assert!(!vm.deletable && !vm.selected);
    assert_eq!(vm.risk, Risk::Never);
    let candidates: Vec<_> = evaluation
        .candidates()
        .map(|c| c.hit().rule_id.as_str())
        .collect();
    assert_eq!(candidates, ["greedy.caches"]);
    assert_eq!(evaluation.reclaim.score(), 10.0 * 0.95);
}

#[test]
fn report_only_and_never_rules_are_not_deletable() {
    let scan = scan(
        HOME,
        &[
            ("Docker.raw", F, 1_000, OLD),
            ("Backups", D, 0, 0),
            ("Backups/a", F, 500, OLD),
        ],
    );
    let rules = rules(
        r#"[
          {"id": "docker.raw", "platform": "any", "title": "Docker disk image", "glob": "~/Docker.raw",
           "kind": "regenerable", "risk": "review", "action": "report", "explain": "Test rule."},
          {"id": "backups", "platform": "any", "title": "Backups", "glob": "~/Backups",
           "kind": "valuable", "explain": "Test rule."}
        ]"#,
    );
    let evaluation = run(&rules, &scan, HOME);
    assert_eq!(evaluation.hits.len(), 2);
    let docker = evaluation
        .hits
        .iter()
        .find(|hit| hit.rule_id == "docker.raw")
        .expect("docker");
    assert!(!docker.deletable && !docker.selected);
    let backups = evaluation
        .hits
        .iter()
        .find(|hit| hit.rule_id == "backups")
        .expect("backups");
    assert_eq!(
        (backups.risk, backups.deletable, backups.selected),
        (Risk::Keep, true, false)
    );
    assert_eq!(
        evaluation.candidates().count(),
        1,
        "only the backups, with a warning"
    );
    assert_eq!(
        evaluation.reclaim.score(),
        0.0,
        "neither report nor keep counts"
    );

    // A rule cannot claim to be deletable while being `never`.
    for bad in [
        r#"[{"id": "x", "platform": "any", "title": "x", "glob": "/x", "kind": "system", "risk": "safe", "explain": "x"}]"#,
        r#"[{"id": "x", "platform": "any", "title": "x", "glob": "/x", "kind": "regenerable", "risk": "never", "action": "trash", "explain": "x"}]"#,
    ] {
        let set = RuleSet::compile(&mac(), [("bad.json", bad)]);
        assert!(
            set.rules.is_empty() && set.problems.len() == 1,
            "{:?}",
            set.problems
        );
    }
}

#[test]
fn the_reclaim_score_follows_plan_section_10() {
    // An outer safe hit with a review hit nested inside, and a keep hit.
    let scan = scan(
        HOME,
        &[
            ("Caches", D, 0, 0),
            ("Caches/a", F, 800, OLD),
            ("Caches/Big", D, 0, 0),
            ("Caches/Big/b", F, 200, OLD),
            ("Mailish", D, 0, 0),
            ("Mailish/m", F, 300, OLD),
        ],
    );
    let rules = rules(
        r#"[
          {"id": "caches", "platform": "any", "title": "Caches", "glob": "~/Caches",
           "kind": "regenerable", "confidence": 0.9, "explain": "Test rule."},
          {"id": "big", "platform": "any", "title": "Big", "glob": "~/Caches/Big",
           "kind": "stale", "confidence": 0.8, "explain": "Test rule."},
          {"id": "mailish", "platform": "any", "title": "Mailish", "glob": "~/Mailish",
           "kind": "valuable", "explain": "Test rule."}
        ]"#,
    );
    let evaluation = run(&rules, &scan, HOME);
    assert_eq!(evaluation.hits.len(), 3);
    let safe = 800.0 * 0.9 * risk_weight(Risk::Safe);
    let review = 200.0 * 0.8 * risk_weight(Risk::Review);
    assert_eq!(evaluation.reclaim.safe, safe);
    assert_eq!(evaluation.reclaim.review, review);
    assert_eq!(evaluation.reclaim.score(), safe + review);
    assert_eq!(risk_weight(Risk::Keep), 0.0);
    assert_eq!(risk_weight(Risk::Never), 0.0);

    // Each file takes the verdict of its nearest hit.
    let file = |name: &str| {
        (0..scan.arena.len() as u32)
            .find(|&i| scan.arena.name(i) == Some(name))
            .expect(name)
    };
    assert_eq!(evaluation.risk_of(file("a")), Some(Risk::Safe));
    assert_eq!(evaluation.risk_of(file("b")), Some(Risk::Review));
    assert_eq!(evaluation.risk_of(file("m")), Some(Risk::Keep));
    assert_eq!(evaluation.risk_of(0), None);
    assert_eq!(evaluation.risk_of(u32::MAX), None);

    // The Cleanliness view buckets by those verdicts and still totals the scan.
    let filter = FileFilter::default();
    let risk = |index| evaluation.risk_of(index);
    let tree = build(
        Grouping::Cleanliness,
        &GroupingInput {
            arena: &scan.arena,
            root_path: Some(Path::new(HOME)),
            modified: Some(&scan.times),
            now: NOW,
            risk: Some(&risk),
            filter: &filter,
        },
    );
    let sizes: Vec<(String, u64)> = tree.groups[0]
        .children
        .iter()
        .map(|&g| {
            (
                tree.groups[g as usize].label.clone(),
                tree.groups[g as usize].size,
            )
        })
        .collect();
    assert_eq!(
        sizes,
        [
            ("Safe to remove".to_owned(), 800),
            ("Review".to_owned(), 200),
            ("Keep".to_owned(), 300)
        ]
    );
}

#[test]
fn several_rules_on_one_node_give_the_most_cautious_verdict() {
    let scan = scan(HOME, &[("x", D, 0, 0), ("x/f", F, 10, OLD)]);
    let rules = rules(
        r#"[
          {"id": "a.safe", "platform": "any", "title": "a", "glob": "~/x", "kind": "regenerable", "explain": "Test rule."},
          {"id": "b.review", "platform": "any", "title": "b", "glob": "~/x", "kind": "leftover", "explain": "Test rule."}
        ]"#,
    );
    let evaluation = run(&rules, &scan, HOME);
    assert_eq!(evaluation.hits.len(), 2);
    let primary: Vec<_> = evaluation
        .hits
        .iter()
        .filter(|hit| hit.primary)
        .map(|hit| hit.rule_id.as_str())
        .collect();
    assert_eq!(primary, ["b.review"]);
    assert_eq!(evaluation.risk_of(1), Some(Risk::Review));
    assert_eq!(evaluation.reclaim.review, 10.0 * 0.6 * 0.5, "counted once");
    assert_eq!(evaluation.reclaim.safe, 0.0);
}

#[test]
fn excludes_files_braces_and_staleness() {
    let scan = scan(
        HOME,
        &[
            ("Downloads", D, 0, 0),
            ("Downloads/old.dmg", F, 100, OLD),
            ("Downloads/old.PKG", F, 100, OLD),
            ("Downloads/new.dmg", F, 100, NEW),
            ("Downloads/keep.dmg", F, 100, OLD),
            ("Downloads/old.zip", F, 100, OLD),
            ("Downloads/unknown.dmg", F, 100, 0),
            ("Downloads/folder.dmg", D, 0, 0),
        ],
    );
    let rules = rules(
        r#"[{"id": "installers", "platform": "macos", "title": "Old installers",
             "glob": "~/Downloads/*.{dmg,pkg}", "exclude": ["~/Downloads/keep.*"], "match": "file",
             "kind": "stale", "stale_days": 90, "explain": "Test rule."}]"#,
    );
    let evaluation = run(&rules, &scan, HOME);
    assert_eq!(
        hit_set(&evaluation),
        set(&[
            ("installers", "/Users/me/Downloads/old.PKG"),
            ("installers", "/Users/me/Downloads/old.dmg"),
        ])
    );

    // Without mtimes, a stale rule cannot prove anything is stale.
    let guard = Guard::from_resolved(Platform::MacOs, HOME, Some(HOME)).expect("guard");
    let blind = evaluate(&rules, &scan.arena, HOME, None, NOW, &guard);
    assert!(blind.hits.is_empty());
}

#[test]
fn a_scan_root_inside_a_rule_is_matched_from_its_real_path() {
    let root = "/Users/me/Library/Caches";
    let scan = scan(root, &[("com.app", D, 0, 0), ("com.app/x", F, 10, OLD)]);
    let rules = rules(
        r#"[
          {"id": "caches", "platform": "macos", "title": "Caches", "glob": "~/Library/Caches/*", "kind": "regenerable", "explain": "Test rule."},
          {"id": "whole", "platform": "macos", "title": "All caches", "glob": "~/Library/Caches", "kind": "regenerable", "explain": "Test rule."}
        ]"#,
    );
    let evaluation = run(&rules, &scan, root);
    assert_eq!(
        hit_set(&evaluation),
        set(&[("caches", "/Users/me/Library/Caches/com.app")])
    );
    assert_eq!(
        evaluation.refused.len(),
        1,
        "the scanned folder itself is never a target"
    );
    assert_eq!(evaluation.refused[0].2, Refusal::IsRoot);
}

#[test]
fn hits_become_rule_hit_rows() {
    let scan = home_fixture();
    let evaluation = run(&RuleSet::builtin(&mac()), &scan, HOME);
    let rows = evaluation.rows(&scan.arena);
    scan.catalog
        .replace_rule_hits(scan.scan_id, &rows)
        .expect("store");
    scan.catalog
        .replace_rule_hits(scan.scan_id, &rows)
        .expect("store again replaces");
    let stored = scan.catalog.rule_hits(scan.scan_id).expect("load");
    assert_eq!(stored.len(), 7);
    assert_eq!(stored[0].rule_id, "dev.rust_target", "biggest first");
    assert_eq!(stored[0].bytes, 500);
    assert_eq!(stored[0].risk, "safe");
    assert!(stored[0].selected);
    let id = stored[0].entry_id.expect("entry id");
    assert_eq!(
        scan.arena.index_of(id).and_then(|i| scan.arena.name(i)),
        Some("target")
    );
    assert!(stored.iter().any(|row| row.risk == "review"));
}

#[test]
fn windows_rules_expand_environment_variables() {
    let context = Context::new(Platform::Windows, Some(r"C:\Users\me\"))
        .with_var("LocalAppData", r"C:\Users\me\AppData\Local\");
    let rules = RuleSet::compile(
        &context,
        [(
            "windows.json",
            r#"[
              {"id": "crash", "platform": "windows", "title": "Crash dumps", "glob": "%LOCALAPPDATA%\\CrashDumps", "kind": "regenerable", "explain": "Test rule."},
              {"id": "temp", "platform": "windows", "title": "Temp", "glob": "%TEMP%\\*", "kind": "regenerable", "explain": "Test rule."},
              {"id": "mac.only", "platform": "macos", "title": "Mac", "glob": "~/x", "kind": "regenerable", "explain": "Test rule."},
              {"id": "home", "platform": "any", "title": "Home cache", "glob": "~\\.cache", "kind": "regenerable", "explain": "Test rule."}
            ]"#,
        )],
    );
    assert_eq!(
        rules.problems.len(),
        1,
        "%TEMP% is not set: {:?}",
        rules.problems
    );
    assert!(rules.problems[0].contains("%TEMP% is not set"));
    assert!(
        rules.get("mac.only").is_none(),
        "other platforms' rules are left out"
    );
    assert!(rules.get("home").is_some());

    let root = r"C:\Users\me";
    let scan = scan(
        root,
        &[
            ("AppData", D, 0, 0),
            ("AppData/Local", D, 0, 0),
            ("AppData/Local/CrashDumps", D, 0, 0),
            ("AppData/Local/CrashDumps/app.dmp", F, 64, OLD),
            (".cache", D, 0, 0),
        ],
    );
    let guard = Guard::from_resolved(Platform::Windows, root, Some(root)).expect("guard");
    let evaluation = evaluate(&rules, &scan.arena, root, None, NOW, &guard);
    assert_eq!(
        hit_set(&evaluation),
        set(&[
            ("crash", r"C:\Users\me\AppData\Local\CrashDumps"),
            ("home", r"C:\Users\me\.cache"),
        ])
    );
}

#[test]
fn invalid_rules_are_skipped_with_a_reason() {
    let context = Context::new(Platform::MacOs, None);
    let rules = RuleSet::compile(
        &context,
        [
            ("broken.json", "{not json"),
            (
                "mixed.json",
                r#"[
                  {"id": "", "platform": "any", "title": "x", "glob": "/x", "kind": "regenerable", "explain": "x"},
                  {"id": "conf", "platform": "any", "title": "x", "glob": "/x", "kind": "regenerable", "confidence": 2.0, "explain": "x"},
                  {"id": "noglob", "platform": "any", "title": "x", "glob": [], "kind": "regenerable", "explain": "x"},
                  {"id": "relative", "platform": "any", "title": "x", "glob": "node_modules", "kind": "regenerable", "explain": "x"},
                  {"id": "badexclude", "platform": "any", "title": "x", "glob": "/x", "exclude": ["/x/{a"], "kind": "regenerable", "explain": "x"},
                  {"id": "nohome", "platform": "any", "title": "x", "glob": "~/x", "kind": "regenerable", "explain": "x"},
                  {"id": "unclosed", "platform": "any", "title": "x", "glob": "%TEMP", "kind": "regenerable", "explain": "x"},
                  {"id": "ok", "platform": "any", "title": "x", "glob": ["/a", "/b"], "kind": "regenerable", "explain": "x"}
                ]"#,
            ),
        ],
    );
    assert_eq!(rules.rules.len(), 1, "{:?}", rules.problems);
    assert_eq!(rules.rules[0].globs.len(), 2);
    assert_eq!(rules.problems.len(), 8, "{:#?}", rules.problems);
    for (problem, words) in rules.problems.iter().zip([
        "broken.json",
        "needs an id",
        "confidence",
        "at least one glob",
        "must be absolute",
        "braces",
        "needs a home",
        "unclosed",
    ]) {
        assert!(problem.contains(words), "{problem} should mention {words}");
    }
}

#[test]
fn a_rules_folder_overrides_and_reloads_without_recompiling() {
    let dir = tempfile::tempdir().expect("tempdir");
    let empty = fingerprint(dir.path());
    assert_eq!(RuleSet::load(&mac(), dir.path()).rules.len(), 27);
    assert_eq!(
        fingerprint(&dir.path().join("missing")),
        fingerprint(&dir.path().join("also-missing"))
    );

    // Replace a built-in rule and add one.
    fs::write(
        dir.path().join("mine.json"),
        r#"[
          {"id": "dev.rust_target", "platform": "any", "title": "Rust output, any age", "glob": "**/target",
           "match": "dir", "when_sibling": ["Cargo.toml"], "kind": "regenerable", "explain": "Test rule."},
          {"id": "mine.logs", "platform": "any", "title": "Logs", "glob": "~/logs", "kind": "regenerable", "explain": "Test rule."}
        ]"#,
    )
    .expect("write");
    fs::write(dir.path().join("notes.txt"), "not a catalog").expect("write");
    let changed = fingerprint(dir.path());
    assert_ne!(changed, empty, "a new catalog file changes the fingerprint");
    let loaded = RuleSet::load(&mac(), dir.path());
    assert!(loaded.problems.is_empty(), "{:?}", loaded.problems);
    assert_eq!(loaded.rules.len(), 28);
    assert_eq!(
        loaded.get("dev.rust_target").expect("rule").rule.stale_days,
        None
    );
    assert_eq!(
        loaded.get("dev.rust_target").expect("rule").rule.kind,
        Kind::Regenerable
    );

    // An unreadable catalog file is a problem, not a failure.
    fs::create_dir(dir.path().join("folder.json")).expect("mkdir");
    let with_problem = RuleSet::load(&mac(), dir.path());
    assert_eq!(with_problem.rules.len(), 28);
    assert_eq!(with_problem.problems.len(), 1);
    assert!(with_problem.problems[0].contains("folder.json"));
}

#[test]
fn an_empty_arena_has_no_hits() {
    let scan = scan(HOME, &[]);
    let evaluation = run(&RuleSet::builtin(&mac()), &scan, HOME);
    assert!(evaluation.hits.is_empty());
    assert_eq!(evaluation.reclaim.score(), 0.0);
}

/// `cargo test --release -p storagelens-analysis --test rules -- --ignored --nocapture`
#[test]
#[ignore = "benchmark: 1M nodes"]
fn one_million_nodes_evaluate_in_one_pass() {
    let mut rows: Vec<(String, EntryKind, u64, i64)> = Vec::new();
    for project in 0..20_000u32 {
        let base = format!("p{project}");
        rows.push((base.clone(), D, 0, 0));
        rows.push((format!("{base}/package.json"), F, 1, OLD));
        rows.push((format!("{base}/node_modules"), D, 0, 0));
        rows.push((format!("{base}/src"), D, 0, 0));
        for file in 0..24 {
            rows.push((format!("{base}/node_modules/m{file}.js"), F, 4096, OLD));
            rows.push((format!("{base}/src/f{file}.ts"), F, 4096, OLD));
        }
    }
    let borrowed: Vec<(&str, EntryKind, u64, i64)> = rows
        .iter()
        .map(|(p, k, s, m)| (p.as_str(), *k, *s, *m))
        .collect();
    // Parents are found by position; index them for the fixture builder.
    let scan = scan_fast(HOME, &borrowed);
    assert!(scan.arena.len() > 1_000_000);
    let rules = RuleSet::builtin(&mac());
    let started = Instant::now();
    let evaluation = run(&rules, &scan, HOME);
    let elapsed = started.elapsed();
    println!(
        "rules over {} nodes: {:?}, {} hits",
        scan.arena.len(),
        elapsed,
        evaluation.hits.len()
    );
    assert_eq!(evaluation.hits.len(), 20_000);
    assert!(elapsed.as_millis() < 1_000, "{elapsed:?}");
}
