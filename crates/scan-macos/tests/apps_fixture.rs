//! App inventory against a fake `/Applications` + `~/Library` in a temp dir.
//!
//! Footprints are cross-checked against `du -sk`, the same reference as the
//! scanner accuracy gate.

#![cfg(target_os = "macos")]

use std::fs;
use std::path::Path;
use std::process::Command;

use storagelens_core::{AppInfo, AppStatus};
use storagelens_scan_macos::apps::{inventory, Sources};

fn info_plist(id: &str, version: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>{id}</string>
<key>CFBundleShortVersionString</key><string>{version}</string>
</dict></plist>"#
    )
}

fn write(path: &Path, bytes: usize) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, vec![7u8; bytes]).unwrap();
}

fn bundle(dir: &Path, id: &str, payload: usize) {
    fs::create_dir_all(dir.join("Contents/MacOS")).unwrap();
    fs::write(dir.join("Contents/Info.plist"), info_plist(id, "1.2.3")).unwrap();
    write(&dir.join("Contents/MacOS/main"), payload);
}

fn du_bytes(path: &Path) -> u64 {
    let out = Command::new("du").arg("-sk").arg(path).output().unwrap();
    let text = String::from_utf8(out.stdout).unwrap();
    text.split_whitespace()
        .next()
        .unwrap()
        .parse::<u64>()
        .unwrap()
        * 1024
}

fn by_id<'a>(apps: &'a [AppInfo], id: &str) -> &'a AppInfo {
    apps.iter().find(|app| app.id == id).unwrap()
}

#[test]
fn inventory_matches_du_and_reads_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let apps_dir = root.join("Applications");
    let library = root.join("Library");
    let launchd = root.join("LaunchAgents");

    bundle(&apps_dir.join("Foo.app"), "com.example.foo", 300_000);
    bundle(
        &apps_dir.join("Utilities/Bar.app"),
        "com.example.bar",
        50_000,
    );
    // Same display name as Foo: `Application Support/Foo` is ambiguous.
    bundle(&apps_dir.join("Vendor/Foo.app"), "com.other.foo", 10_000);
    std::os::unix::fs::symlink(apps_dir.join("Foo.app"), apps_dir.join("Alias.app")).unwrap();
    bundle(&apps_dir.join("Mail.app"), "com.apple.mail", 1_000);

    let support = library.join("Application Support");
    write(&support.join("com.example.foo/db.sqlite"), 120_000);
    write(&support.join("Foo/shared.bin"), 40_000);
    write(&library.join("Caches/com.example.foo/blob"), 64_000);
    write(&library.join("Preferences/com.example.foo.plist"), 900);
    write(
        &library.join("Group Containers/TEAM1.com.example.foo/x"),
        5_000,
    );
    write(&library.join("Group Containers/TEAM1.office/x"), 5_000);
    write(&library.join("Containers/com.apple.mail/x"), 5_000);
    write(
        &library.join("Containers/com.example.foo/Data/Library/x"),
        7_000,
    );
    // Its widget, and a different app whose id only starts the same.
    write(
        &library.join("Containers/com.example.foo.widget/Data/x"),
        3_000,
    );
    write(&library.join("Containers/com.example.foobar/Data/x"), 3_000);
    write(&library.join("Application Scripts/com.example.foo/s"), 100);
    write(&library.join("HTTPStorages/com.example.foo/h"), 200);
    write(&library.join("WebKit/com.example.foo/w"), 300);
    write(&library.join("Cookies/com.example.foo.binarycookies"), 50);
    write(&library.join("Logs/com.example.foo/l"), 60);
    write(
        &library.join("Preferences/ByHost/com.example.foo.ABC-123.plist"),
        70,
    );
    write(
        &library.join("LaunchAgents/com.example.foo.updater.plist"),
        80,
    );
    // The per-user cache folder in /var/folders, WebKit's per-app copy too.
    write(&root.join("var/C/com.example.foo/x"), 90);
    write(
        &root.join("var/C/com.apple.WebKit.Networking+com.example.foo/x"),
        90,
    );
    // Named after the app: Bar's home dot folder and crash report are its;
    // `~/.foo` is claimed by both Foos, so by neither.
    write(&root.join(".bar/config"), 10);
    write(&root.join(".foo/config"), 10);
    write(
        &library.join("Logs/DiagnosticReports/Bar-2026-01-01-120000.ips"),
        10,
    );
    write(
        &library.join("Logs/DiagnosticReports/Barista-2026-01-01.ips"),
        10,
    );

    fs::create_dir_all(&launchd).unwrap();
    fs::write(
        launchd.join("com.example.foo.helper.plist"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>Label</key><string>com.example.foo.helper</string></dict></plist>"#,
    )
    .unwrap();
    let ls = library.join("Preferences/com.apple.LaunchServices");
    fs::create_dir_all(&ls).unwrap();
    fs::write(
        ls.join("com.apple.launchservices.secure.plist"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict><key>LSHandlers</key><array><dict>
<key>LSHandlerContentType</key><string>public.html</string>
<key>LSHandlerRoleAll</key><string>COM.EXAMPLE.BAR</string>
</dict></array></dict></plist>"#,
    )
    .unwrap();

    let apps = inventory(&Sources {
        app_dirs: vec![apps_dir.clone()],
        library: library.clone(),
        launchd_dirs: vec![launchd],
        user_dirs: vec![root.join("var/C")],
    });

    let mut ids: Vec<&str> = apps.iter().map(|app| app.id.as_str()).collect();
    ids.sort();
    assert_eq!(
        ids,
        [
            "com.apple.mail",
            "com.example.bar",
            "com.example.foo",
            "com.other.foo"
        ],
        "symlinked bundles are skipped, one folder level is searched"
    );

    let foo = by_id(&apps, "com.example.foo");
    assert_eq!(foo.name, "Foo");
    assert_eq!(foo.version.as_deref(), Some("1.2.3"));
    assert_eq!(foo.status, AppStatus::Unknown, "scanners never classify");
    assert!(foo.autostart && !foo.default_handler && !foo.running && !foo.system);

    let mut satellites: Vec<_> = foo
        .satellites
        .iter()
        .map(|s| s.path.strip_prefix(&root).unwrap().to_path_buf())
        .collect();
    satellites.sort();
    assert_eq!(
        satellites,
        [
            Path::new("Library/Application Scripts/com.example.foo"),
            Path::new("Library/Application Support/com.example.foo"),
            Path::new("Library/Caches/com.example.foo"),
            Path::new("Library/Containers/com.example.foo"),
            Path::new("Library/Containers/com.example.foo.widget"),
            Path::new("Library/Cookies/com.example.foo.binarycookies"),
            Path::new("Library/Group Containers/TEAM1.com.example.foo"),
            Path::new("Library/HTTPStorages/com.example.foo"),
            Path::new("Library/LaunchAgents/com.example.foo.updater.plist"),
            Path::new("Library/Logs/com.example.foo"),
            Path::new("Library/Preferences/ByHost/com.example.foo.ABC-123.plist"),
            Path::new("Library/Preferences/com.example.foo.plist"),
            Path::new("Library/WebKit/com.example.foo"),
            Path::new("var/C/com.apple.WebKit.Networking+com.example.foo"),
            Path::new("var/C/com.example.foo"),
        ],
        "ambiguous `Application Support/Foo` and the shared group are left out"
    );
    assert!(
        foo.satellites.iter().all(|s| s.strong),
        "every one is named after the bundle id"
    );

    let bar = by_id(&apps, "com.example.bar");
    let mut bar_paths: Vec<_> = bar
        .satellites
        .iter()
        .map(|s| s.path.strip_prefix(&root).unwrap().to_path_buf())
        .collect();
    bar_paths.sort();
    assert_eq!(
        bar_paths,
        [
            Path::new(".bar"),
            Path::new("Library/Logs/DiagnosticReports/Bar-2026-01-01-120000.ips"),
        ],
        "named after the app: its dot folder and crash report, not Barista's"
    );
    assert!(
        bar.satellites.iter().all(|s| !s.strong),
        "names are weaker evidence"
    );

    // Footprint == du, per path and in total.
    assert_eq!(foo.bytes.get(), du_bytes(&foo.path));
    let mut total = foo.bytes.get();
    let mut du_total = du_bytes(&foo.path);
    for satellite in &foo.satellites {
        assert_eq!(
            satellite.bytes.get(),
            du_bytes(&satellite.path),
            "{satellite:?}"
        );
        total += satellite.bytes.get();
        du_total += du_bytes(&satellite.path);
    }
    assert_eq!(total, du_total);
    assert!(total > 500_000);

    let bar = by_id(&apps, "com.example.bar");
    assert!(
        bar.default_handler,
        "handler ids compare case-insensitively"
    );
    assert!(!bar.autostart);

    let mail = by_id(&apps, "com.apple.mail");
    assert!(mail.system);
    assert!(mail.satellites.is_empty(), "system apps get no satellites");

    // Never launched: `mdls` reports the file date as "last used", which must
    // not survive as usage evidence.
    assert!(apps.iter().all(|app| app.last_used.is_none()));
}
