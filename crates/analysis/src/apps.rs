//! App classification (PLAN §11): evidence in, [`AppStatus`] out.
//!
//! ```text
//! unknown       = no last-used date               → never flagged
//! in-use        = system, running, autostart, default handler, or used < 90 d
//! likely-unused = last used 90–180 days ago
//! unused        = last used > 180 days ago
//! ```
//!
//! One table for both platforms; only the evidence differs. The scanners
//! supply it: `storagelens-scan-macos::apps` (Spotlight, discarding dates that
//! are file dates rather than launches) and `storagelens-scan-windows::apps`
//! (Prefetch and UserAssist, never `LastAccessTime`).

use storagelens_core::{AppInfo, AppStatus, Bytes};

const DAY: i64 = 86_400;
/// Older than this, with no other evidence: likely unused.
pub const LIKELY_UNUSED_AFTER: i64 = 90 * DAY;
/// Older than this, with no other evidence: unused.
pub const UNUSED_AFTER: i64 = 180 * DAY;

/// Classify one app at `now` (unix seconds).
pub fn classify(app: &AppInfo, now: i64) -> AppStatus {
    if app.system || app.running || app.autostart || app.default_handler {
        return AppStatus::InUse;
    }
    let Some(last_used) = app.last_used else {
        return AppStatus::Unknown;
    };
    match now.saturating_sub(last_used) {
        age if age > UNUSED_AFTER => AppStatus::Unused,
        age if age > LIKELY_UNUSED_AFTER => AppStatus::LikelyUnused,
        _ => AppStatus::InUse,
    }
}

/// Set `status` on every app.
pub fn assess(apps: &mut [AppInfo], now: i64) {
    for app in apps {
        app.status = classify(app, now);
    }
}

/// Bundle plus associated data: the total cost of removing the app.
pub fn footprint(app: &AppInfo) -> Bytes {
    app.satellites.iter().fold(app.bytes, |sum, satellite| {
        sum.saturating_add(satellite.bytes)
    })
}

/// Whether to suggest removing the app. Only an `unused` / `likely-unused`
/// status qualifies, and that already excludes system apps, default handlers,
/// running apps and autostart entries.
pub fn suggest_removal(app: &AppInfo, now: i64) -> bool {
    matches!(
        classify(app, now),
        AppStatus::Unused | AppStatus::LikelyUnused
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use storagelens_core::{Satellite, Signature};

    const NOW: i64 = 1_800_000_000;

    fn app(days_ago: Option<i64>) -> AppInfo {
        AppInfo {
            id: "com.example.foo".into(),
            name: "Foo".into(),
            version: None,
            path: "/Applications/Foo.app".into(),
            bytes: Bytes::new(1_000),
            last_used: days_ago.map(|days| NOW - days * DAY),
            status: AppStatus::Unknown,
            use_count: Some(3),
            signature: Signature::Valid,
            running: false,
            autostart: false,
            default_handler: false,
            system: false,
            satellites: Vec::new(),
            uninstaller: None,
        }
    }

    #[test]
    fn age_thresholds() {
        assert_eq!(classify(&app(Some(1)), NOW), AppStatus::InUse);
        assert_eq!(classify(&app(Some(90)), NOW), AppStatus::InUse);
        assert_eq!(classify(&app(Some(91)), NOW), AppStatus::LikelyUnused);
        assert_eq!(classify(&app(Some(180)), NOW), AppStatus::LikelyUnused);
        assert_eq!(classify(&app(Some(181)), NOW), AppStatus::Unused);
        // Clock skew: a future date is recent use, not an error.
        assert_eq!(classify(&app(Some(-5)), NOW), AppStatus::InUse);
    }

    #[test]
    fn missing_metadata_is_unknown_never_unused() {
        let unknown = app(None);
        assert_eq!(classify(&unknown, NOW), AppStatus::Unknown);
        assert!(!suggest_removal(&unknown, NOW));
    }

    #[test]
    fn evidence_overrides_age() {
        type Flag = fn(&mut AppInfo);
        let flags: [(&str, Flag); 4] = [
            ("system", |a| a.system = true),
            ("running", |a| a.running = true),
            ("autostart", |a| a.autostart = true),
            ("default handler", |a| a.default_handler = true),
        ];
        for (what, set) in flags {
            let mut old = app(Some(1_000));
            set(&mut old);
            assert_eq!(classify(&old, NOW), AppStatus::InUse, "{what}");
            assert!(!suggest_removal(&old, NOW), "{what}");
            // Even with no metadata at all.
            let mut bare = app(None);
            set(&mut bare);
            assert!(!suggest_removal(&bare, NOW), "{what}");
        }
    }

    #[test]
    fn assess_and_footprint() {
        let mut apps = vec![app(Some(400)), app(None)];
        apps[0].satellites = vec![
            Satellite {
                path: PathBuf::from("/a"),
                bytes: Bytes::new(20),
            },
            Satellite {
                path: PathBuf::from("/b"),
                bytes: Bytes::new(300),
            },
        ];
        assess(&mut apps, NOW);
        assert_eq!(apps[0].status, AppStatus::Unused);
        assert_eq!(apps[1].status, AppStatus::Unknown);
        assert_eq!(footprint(&apps[0]), Bytes::new(1_320));
        assert_eq!(footprint(&apps[1]), Bytes::new(1_000));
    }
}
