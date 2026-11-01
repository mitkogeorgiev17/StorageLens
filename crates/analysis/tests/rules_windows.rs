//! The Windows rule catalog (#33): every rule's exact matches and byte total
//! over one fixture drive, report-only cards with a command to run, and no
//! match inside `C:\Windows\System32`.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use common::{scan, D, F};
use storagelens_analysis::rules::{evaluate, Context, Evaluation, RuleSet};
use storagelens_safety::{Guard, Platform, Refusal};

const NOW: i64 = 1_800_000_000;
const DAY: i64 = 86_400;
const OLD: i64 = NOW - 400 * DAY;
const NEW: i64 = NOW - DAY;
const ROOT: &str = r"C:\";
const HOME: &str = r"C:\Users\me";
const LOCAL: &str = "Users/me/AppData/Local";

fn context(temp: &str) -> Context {
    Context::new(Platform::Windows, Some(HOME))
        .with_var("SystemRoot", r"C:\Windows")
        .with_var("SystemDrive", "C:")
        .with_var("LOCALAPPDATA", r"C:\Users\me\AppData\Local")
        .with_var("TEMP", temp)
        .with_var("USERPROFILE", HOME)
}

fn user() -> Context {
    context(r"C:\Users\me\AppData\Local\Temp")
}

fn drive() -> common::Scan {
    let rows: Vec<(String, storagelens_core::EntryKind, u64, i64)> = [
        ("Users", D, 0, 0),
        ("Users/me", D, 0, 0),
        ("Users/me/AppData", D, 0, 0),
        (LOCAL, D, 0, 0),
        // Temp: older than a week.
        (&format!("{LOCAL}/Temp"), D, 0, 0),
        (&format!("{LOCAL}/Temp/old.tmp"), F, 100, OLD),
        (&format!("{LOCAL}/Temp/new.tmp"), F, 5, NEW),
        (&format!("{LOCAL}/Temp/setup-dir"), D, 0, 0),
        (&format!("{LOCAL}/Temp/setup-dir/x"), F, 20, OLD),
        // Browsers: cache folders, not profiles.
        (&format!("{LOCAL}/Google"), D, 0, 0),
        (&format!("{LOCAL}/Google/Chrome"), D, 0, 0),
        (&format!("{LOCAL}/Google/Chrome/User Data"), D, 0, 0),
        (&format!("{LOCAL}/Google/Chrome/User Data/Default"), D, 0, 0),
        (&format!("{LOCAL}/Google/Chrome/User Data/Default/Cache"), D, 0, 0),
        (&format!("{LOCAL}/Google/Chrome/User Data/Default/Cache/Cache_Data"), F, 300, OLD),
        (&format!("{LOCAL}/Google/Chrome/User Data/Default/Code Cache"), D, 0, 0),
        (&format!("{LOCAL}/Google/Chrome/User Data/Default/Code Cache/js"), F, 50, OLD),
        (&format!("{LOCAL}/Google/Chrome/User Data/Default/History"), F, 40, NEW),
        // A profile in use today: its cache is not offered.
        (&format!("{LOCAL}/Google/Chrome/User Data/Profile 1"), D, 0, 0),
        (&format!("{LOCAL}/Google/Chrome/User Data/Profile 1/GPUCache"), D, 0, 0),
        (&format!("{LOCAL}/Google/Chrome/User Data/Profile 1/GPUCache/x"), F, 10, NEW),
        (&format!("{LOCAL}/Microsoft"), D, 0, 0),
        (&format!("{LOCAL}/Microsoft/Edge"), D, 0, 0),
        (&format!("{LOCAL}/Microsoft/Edge/User Data"), D, 0, 0),
        (&format!("{LOCAL}/Microsoft/Edge/User Data/Default"), D, 0, 0),
        (&format!("{LOCAL}/Microsoft/Edge/User Data/Default/Cache"), D, 0, 0),
        (&format!("{LOCAL}/Microsoft/Edge/User Data/Default/Cache/x"), F, 30, OLD),
        (&format!("{LOCAL}/Mozilla"), D, 0, 0),
        (&format!("{LOCAL}/Mozilla/Firefox"), D, 0, 0),
        (&format!("{LOCAL}/Mozilla/Firefox/Profiles"), D, 0, 0),
        (&format!("{LOCAL}/Mozilla/Firefox/Profiles/abc.default"), D, 0, 0),
        (&format!("{LOCAL}/Mozilla/Firefox/Profiles/abc.default/cache2"), D, 0, 0),
        (&format!("{LOCAL}/Mozilla/Firefox/Profiles/abc.default/cache2/x"), F, 60, OLD),
        // Crash dumps, error reports, thumbnails.
        (&format!("{LOCAL}/CrashDumps"), D, 0, 0),
        (&format!("{LOCAL}/CrashDumps/app.exe.123.dmp"), F, 64, NEW),
        (&format!("{LOCAL}/Microsoft/Windows"), D, 0, 0),
        (&format!("{LOCAL}/Microsoft/Windows/WER"), D, 0, 0),
        (&format!("{LOCAL}/Microsoft/Windows/WER/ReportArchive"), D, 0, 0),
        (&format!("{LOCAL}/Microsoft/Windows/WER/ReportArchive/r"), F, 32, NEW),
        (&format!("{LOCAL}/Microsoft/Windows/WER/loose.txt"), F, 3, NEW),
        (&format!("{LOCAL}/Microsoft/Windows/Explorer"), D, 0, 0),
        (&format!("{LOCAL}/Microsoft/Windows/Explorer/thumbcache_256.db"), F, 128, OLD),
        (&format!("{LOCAL}/Microsoft/Windows/Explorer/iconcache_32.db"), F, 16, NEW),
        // Package managers.
        (&format!("{LOCAL}/npm-cache"), D, 0, 0),
        (&format!("{LOCAL}/npm-cache/_cacache"), F, 150, OLD),
        (&format!("{LOCAL}/pip"), D, 0, 0),
        (&format!("{LOCAL}/pip/Cache"), D, 0, 0),
        (&format!("{LOCAL}/pip/Cache/http"), F, 70, OLD),
        ("Users/me/.cargo", D, 0, 0),
        ("Users/me/.cargo/registry", D, 0, 0),
        ("Users/me/.cargo/registry/cache", F, 200, OLD),
        ("Users/me/.cargo/bin", D, 0, 0),
        ("Users/me/.cargo/bin/cargo.exe", F, 9, NEW),
        ("Users/me/.nuget", D, 0, 0),
        ("Users/me/.nuget/packages", D, 0, 0),
        ("Users/me/.nuget/packages/newtonsoft.json", F, 90, OLD),
        // Downloads.
        ("Users/me/Downloads", D, 0, 0),
        ("Users/me/Downloads/setup.exe", F, 900, OLD),
        ("Users/me/Downloads/tool.MSI", F, 100, OLD),
        ("Users/me/Downloads/new.exe", F, 50, NEW),
        ("Users/me/Downloads/doc.pdf", F, 20, OLD),
        // The Windows folder: report-only cards, and look-alikes in System32.
        ("Windows", D, 0, 0),
        ("Windows/Temp", D, 0, 0),
        ("Windows/Temp/x", F, 500, OLD),
        ("Windows/SoftwareDistribution", D, 0, 0),
        ("Windows/SoftwareDistribution/Download", D, 0, 0),
        ("Windows/SoftwareDistribution/Download/kb", F, 800, OLD),
        ("Windows/ServiceProfiles", D, 0, 0),
        ("Windows/ServiceProfiles/NetworkService", D, 0, 0),
        ("Windows/ServiceProfiles/NetworkService/AppData", D, 0, 0),
        ("Windows/ServiceProfiles/NetworkService/AppData/Local", D, 0, 0),
        ("Windows/ServiceProfiles/NetworkService/AppData/Local/Microsoft", D, 0, 0),
        ("Windows/ServiceProfiles/NetworkService/AppData/Local/Microsoft/Windows", D, 0, 0),
        (
            "Windows/ServiceProfiles/NetworkService/AppData/Local/Microsoft/Windows/DeliveryOptimization",
            D,
            0,
            0,
        ),
        (
            "Windows/ServiceProfiles/NetworkService/AppData/Local/Microsoft/Windows/DeliveryOptimization/Cache",
            F,
            400,
            OLD,
        ),
        ("Windows/WinSxS", D, 0, 0),
        ("Windows/WinSxS/amd64_x", F, 2_000, OLD),
        ("Windows/System32", D, 0, 0),
        ("Windows/System32/Temp", D, 0, 0),
        ("Windows/System32/Temp/x", F, 10, OLD),
        ("Windows/System32/thumbcache_1.db", F, 10, OLD),
        ("Windows/System32/config", D, 0, 0),
        ("Windows/System32/config/systemprofile", D, 0, 0),
        ("Windows/System32/config/systemprofile/AppData", D, 0, 0),
        ("Windows/System32/config/systemprofile/AppData/Local", D, 0, 0),
        ("Windows/System32/config/systemprofile/AppData/Local/Temp", D, 0, 0),
        ("Windows/System32/config/systemprofile/AppData/Local/Temp/old.tmp", F, 10, OLD),
        ("Windows/System32/config/systemprofile/AppData/Local/CrashDumps", D, 0, 0),
        ("Windows/System32/config/systemprofile/AppData/Local/CrashDumps/d.dmp", F, 10, OLD),
        // Drive root.
        ("Windows.old", D, 0, 0),
        ("Windows.old/Windows", F, 5_000, OLD),
        ("$Recycle.Bin", D, 0, 0),
        ("$Recycle.Bin/S-1-5-21-1", D, 0, 0),
        ("$Recycle.Bin/S-1-5-21-1/$R1.txt", F, 70, OLD),
        ("hiberfil.sys", F, 8_000, NEW),
        ("pagefile.sys", F, 4_000, NEW),
        ("Program Files", D, 0, 0),
        ("Program Files/App", D, 0, 0),
        ("Program Files/App/Cache", D, 0, 0),
        ("Program Files/App/Cache/x", F, 10, OLD),
    ]
    .into_iter()
    .map(|(path, kind, bytes, modified)| (path.to_owned(), kind, bytes, modified))
    .collect();
    let borrowed: Vec<_> = rows
        .iter()
        .map(|(path, kind, bytes, modified)| (path.as_str(), *kind, *bytes, *modified))
        .collect();
    scan(ROOT, &borrowed)
}

fn evaluated(scan: &common::Scan, context: &Context) -> (RuleSet, Evaluation) {
    let rules = RuleSet::builtin(context);
    assert!(rules.problems.is_empty(), "{:?}", rules.problems);
    let guard = Guard::from_resolved(Platform::Windows, ROOT, Some(HOME)).expect("guard");
    let evaluation = evaluate(&rules, &scan.arena, ROOT, Some(&scan.times), NOW, &guard);
    (rules, evaluation)
}

#[test]
fn every_windows_rule_matches_exactly_its_fixture() {
    let scan = drive();
    let (rules, evaluation) = evaluated(&scan, &user());
    assert!(evaluation.refused.is_empty(), "{:?}", evaluation.refused);

    let mut found: BTreeMap<&str, (BTreeSet<&str>, u64)> = BTreeMap::new();
    for hit in &evaluation.hits {
        let entry = found.entry(hit.rule_id.as_str()).or_default();
        entry.0.insert(hit.path.as_str());
        entry.1 += hit.bytes;
    }
    let local = r"C:\Users\me\AppData\Local";
    let chrome = format!(r"{local}\Google\Chrome\User Data");
    let expected: Vec<(&str, Vec<String>, u64)> = vec![
        (
            "windows.user_temp",
            vec![format!(r"{local}\Temp\old.tmp"), format!(r"{local}\Temp\setup-dir")],
            120,
        ),
        (
            "windows.browser_caches",
            vec![
                format!(r"{chrome}\Default\Cache"),
                format!(r"{chrome}\Default\Code Cache"),
                format!(r"{local}\Microsoft\Edge\User Data\Default\Cache"),
                format!(r"{local}\Mozilla\Firefox\Profiles\abc.default\cache2"),
            ],
            440,
        ),
        (
            "windows.downloads.installers",
            vec![r"C:\Users\me\Downloads\setup.exe".into(), r"C:\Users\me\Downloads\tool.MSI".into()],
            1_000,
        ),
        ("windows.crash_dumps", vec![format!(r"{local}\CrashDumps\app.exe.123.dmp")], 64),
        ("windows.error_reports", vec![format!(r"{local}\Microsoft\Windows\WER\ReportArchive")], 32),
        (
            "windows.thumbnail_cache",
            vec![format!(r"{local}\Microsoft\Windows\Explorer\thumbcache_256.db")],
            128,
        ),
        ("windows.npm_cache", vec![format!(r"{local}\npm-cache")], 150),
        ("windows.pip_cache", vec![format!(r"{local}\pip\Cache")], 70),
        ("windows.cargo_registry", vec![r"C:\Users\me\.cargo\registry".into()], 200),
        ("windows.nuget_packages", vec![r"C:\Users\me\.nuget\packages".into()], 90),
        ("windows.system_temp", vec![r"C:\Windows\Temp".into()], 500),
        ("windows.update_cache", vec![r"C:\Windows\SoftwareDistribution\Download".into()], 800),
        (
            "windows.delivery_optimization",
            vec![r"C:\Windows\ServiceProfiles\NetworkService\AppData\Local\Microsoft\Windows\DeliveryOptimization".into()],
            400,
        ),
        ("windows.old", vec![r"C:\Windows.old".into()], 5_000),
        ("windows.recycle_bin", vec![r"C:\$Recycle.Bin".into()], 70),
        ("windows.hibernation_file", vec![r"C:\hiberfil.sys".into()], 8_000),
        ("windows.pagefile", vec![r"C:\pagefile.sys".into()], 4_000),
        ("windows.component_store", vec![r"C:\Windows\WinSxS".into()], 2_000),
    ];
    let expected_map: BTreeMap<&str, (BTreeSet<&str>, u64)> = expected
        .iter()
        .map(|(id, paths, bytes)| (*id, (paths.iter().map(String::as_str).collect(), *bytes)))
        .collect();
    assert_eq!(found, expected_map);

    let windows: BTreeSet<&str> = rules
        .rules
        .iter()
        .map(|compiled| compiled.rule.id.as_str())
        .filter(|id| id.starts_with("windows."))
        .collect();
    assert_eq!(
        windows,
        expected_map.keys().copied().collect(),
        "every rule has a case"
    );
    assert!(
        rules.get("macos.trash").is_none(),
        "macOS rules stay out of a Windows set"
    );
}

#[test]
fn report_only_rules_carry_a_command_to_run() {
    let rules = RuleSet::builtin(&user());
    let mut report_only = 0;
    for compiled in rules
        .rules
        .iter()
        .filter(|c| c.rule.id.starts_with("windows."))
    {
        let rule = &compiled.rule;
        if rule.deletable() {
            assert!(
                rule.command.is_none(),
                "{}: deletable rules need no command",
                rule.id
            );
            continue;
        }
        report_only += 1;
        let command = rule.command.as_deref().unwrap_or_default();
        assert!(!command.trim().is_empty(), "{} has no command", rule.id);
    }
    assert_eq!(report_only, 8);
    let command = |id: &str| rules.get(id).and_then(|c| c.rule.command.clone());
    assert_eq!(
        command("windows.hibernation_file").as_deref(),
        Some("powercfg /h off")
    );
    assert_eq!(
        command("windows.component_store").as_deref(),
        Some("DISM /Online /Cleanup-Image /StartComponentCleanup")
    );
}

#[test]
fn nothing_inside_system32_is_ever_matched() {
    let scan = drive();
    for context in [
        user(),
        // Running as SYSTEM, %TEMP% points into System32.
        context(r"C:\Windows\System32\config\systemprofile\AppData\Local\Temp"),
    ] {
        let (_, evaluation) = evaluated(&scan, &context);
        for hit in &evaluation.hits {
            assert!(
                !hit.path
                    .to_ascii_lowercase()
                    .starts_with(r"c:\windows\system32"),
                "{} matched {}",
                hit.rule_id,
                hit.path
            );
        }
        for (_, path, refusal) in &evaluation.refused {
            assert_eq!(*refusal, Refusal::System(r"C:\Windows"), "{path}");
        }
    }
    let (_, as_system) = evaluated(
        &scan,
        &context(r"C:\Windows\System32\config\systemprofile\AppData\Local\Temp"),
    );
    let refused: BTreeSet<&str> = as_system
        .refused
        .iter()
        .map(|(_, path, _)| path.as_str())
        .collect();
    assert_eq!(
        refused,
        BTreeSet::from([r"C:\Windows\System32\config\systemprofile\AppData\Local\Temp\old.tmp"]),
        "the guard, not the glob, keeps it out"
    );
}

#[test]
fn only_the_users_own_files_are_offered() {
    let scan = drive();
    let (_, evaluation) = evaluated(&scan, &user());
    for candidate in evaluation.candidates() {
        assert!(
            candidate.hit().path.starts_with(r"C:\Users\me\"),
            "{}",
            candidate.hit().path
        );
    }
    assert_eq!(evaluation.candidates().count(), 15);
}
