//! Startup items (#114): launchd jobs that start at login.
//!
//! - `~/Library/LaunchAgents` — this user's; switched with `launchctl
//!   disable|enable gui/<uid>/<label>`, the persistent override launchd itself
//!   keeps (and System Settings → Login Items shows).
//! - `/Library/LaunchAgents`, `/Library/LaunchDaemons` — every user's or the
//!   system's; listed, but switching them needs admin rights, so the UI sends
//!   the user to System Settings instead.
//!
//! A job counts as a startup item when it has `RunAtLoad` or `KeepAlive`;
//! on-demand jobs (sockets, watch paths, intervals) are not listed. Login
//! items registered through `SMAppService` or the legacy login-items list live
//! in a database only the system reads; System Settings shows those.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use storagelens_core::StartupItem;

use crate::apps::plist_json;

/// System Settings → General → Login Items.
pub const LOGIN_ITEMS_URL: &str =
    "x-apple.systempreferences:com.apple.LoginItems-Settings.extension";

const USER_PREFIX: &str = "agent:";

/// Every startup job for the user whose home is `home`, user agents first.
pub fn startup_items(home: &Path) -> Vec<StartupItem> {
    let uid = current_uid();
    let user = overrides(&format!("gui/{uid}"));
    let system = overrides("system");
    let dirs: [(PathBuf, &str, bool, &HashMap<String, bool>); 3] = [
        (
            home.join("Library/LaunchAgents"),
            "Launch agent · this user",
            true,
            &user,
        ),
        (
            "/Library/LaunchAgents".into(),
            "Launch agent · all users",
            false,
            &user,
        ),
        (
            "/Library/LaunchDaemons".into(),
            "Launch daemon · system",
            false,
            &system,
        ),
    ];
    let mut items = Vec::new();
    for (dir, source, user_owned, disabled) in dirs {
        for (_, value) in jobs(&dir) {
            let Some(mut item) = item_from(&value, source, disabled) else {
                continue;
            };
            if user_owned {
                item.id = format!("{USER_PREFIX}{}", item.id);
                item.can_toggle = true;
            }
            items.push(item);
        }
    }
    items
}

/// Switch the user agent `id` (from [`startup_items`]) on or off for future
/// logins, and stop or start it now. Only this user's agents can be switched.
pub fn set_startup_enabled(home: &Path, id: &str, enabled: bool) -> Result<(), String> {
    let label = id
        .strip_prefix(USER_PREFIX)
        .ok_or_else(|| "only this user's launch agents can be switched here".to_owned())?;
    let plist = jobs(&home.join("Library/LaunchAgents"))
        .into_iter()
        .find(|(_, value)| value.get("Label").and_then(Value::as_str) == Some(label))
        .map(|(path, _)| path)
        .ok_or_else(|| format!("no launch agent labelled {label}"))?;
    let domain = format!("gui/{}", current_uid());
    let service = format!("{domain}/{label}");
    let status = Command::new("/bin/launchctl")
        .args([if enabled { "enable" } else { "disable" }, &service])
        .status()
        .map_err(|error| format!("could not run launchctl: {error}"))?;
    if !status.success() {
        return Err(format!("launchctl could not switch {label} ({status})"));
    }
    // Best effort: the override above is what persists; this only applies it
    // now. Booting out a job that is not loaded (or bootstrapping one that is)
    // fails harmlessly.
    let now = if enabled {
        Command::new("/bin/launchctl")
            .args(["bootstrap", &domain])
            .arg(&plist)
            .output()
    } else {
        Command::new("/bin/launchctl")
            .args(["bootout", &service])
            .output()
    };
    let _ = now;
    Ok(())
}

fn current_uid() -> u32 {
    // SAFETY: getuid cannot fail and has no preconditions.
    unsafe { libc::getuid() }
}

/// `(plist path, plist as JSON)` of every job in `dir`.
fn jobs(dir: &Path) -> Vec<(PathBuf, Value)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "plist"))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| plist_json(&path).map(|value| (path, value)))
        .collect()
}

/// launchd's persistent enable/disable overrides in `domain`: label → disabled.
fn overrides(domain: &str) -> HashMap<String, bool> {
    Command::new("/bin/launchctl")
        .args(["print-disabled", domain])
        .output()
        .ok()
        .map(|output| parse_overrides(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or_default()
}

/// Lines like `"com.foo.agent" => disabled` (or `=> true` on older systems).
fn parse_overrides(text: &str) -> HashMap<String, bool> {
    text.lines()
        .filter_map(|line| {
            let (label, state) = line.trim().split_once("=>")?;
            let label = label.trim().trim_matches('"');
            let disabled = match state.trim() {
                "disabled" | "true" => true,
                "enabled" | "false" => false,
                _ => return None,
            };
            (!label.is_empty()).then(|| (label.to_owned(), disabled))
        })
        .collect()
}

/// A startup item from a job plist, or `None` for an on-demand job.
fn item_from(
    value: &Value,
    source: &str,
    overrides: &HashMap<String, bool>,
) -> Option<StartupItem> {
    let label = value.get("Label")?.as_str()?.to_owned();
    let truthy = |key: &str| match value.get(key) {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Object(_)) => true,
        _ => false,
    };
    if !truthy("RunAtLoad") && !truthy("KeepAlive") {
        return None;
    }
    let arguments: Vec<&str> = value
        .get("ProgramArguments")
        .and_then(Value::as_array)
        .map(|args| args.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let program = value
        .get("Program")
        .and_then(Value::as_str)
        .or_else(|| arguments.first().copied());
    let command = if arguments.is_empty() {
        program.unwrap_or_default().to_owned()
    } else {
        arguments.join(" ")
    };
    let plist_disabled = value.get("Disabled").and_then(Value::as_bool) == Some(true);
    let disabled = overrides.get(&label).copied().unwrap_or(plist_disabled);
    Some(StartupItem {
        name: program.and_then(app_name).unwrap_or_else(|| label.clone()),
        id: label,
        command,
        source: source.to_owned(),
        enabled: !disabled,
        can_toggle: false,
    })
}

/// `Foo` for a program inside `…/Foo.app/…`.
fn app_name(program: &str) -> Option<String> {
    Path::new(program)
        .ancestors()
        .filter_map(|dir| dir.file_name()?.to_str())
        .filter_map(|name| name.strip_suffix(".app"))
        .last()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn overrides_parse_both_spellings() {
        let parsed = parse_overrides(
            "disabled services = {\n\t\"com.a\" => disabled\n\t\"com.b\" => enabled\n\t\"com.c\" => true\n}\n",
        );
        assert_eq!(parsed.get("com.a"), Some(&true));
        assert_eq!(parsed.get("com.b"), Some(&false));
        assert_eq!(parsed.get("com.c"), Some(&true));
        assert_eq!(parsed.len(), 3);
    }

    #[test]
    fn only_jobs_that_start_at_login() {
        let none = HashMap::new();
        let on_demand = json!({"Label": "com.x", "Program": "/bin/x", "StartInterval": 60});
        assert!(item_from(&on_demand, "s", &none).is_none());

        let agent = json!({
            "Label": "com.foo.helper",
            "ProgramArguments": ["/Applications/Foo.app/Contents/MacOS/Helper", "--bg"],
            "RunAtLoad": true,
        });
        let item = item_from(&agent, "s", &none).expect("item");
        assert_eq!(item.name, "Foo");
        assert_eq!(
            item.command,
            "/Applications/Foo.app/Contents/MacOS/Helper --bg"
        );
        assert!(item.enabled);
    }

    #[test]
    fn override_beats_the_plist_disabled_key() {
        let job = json!({"Label": "com.x", "Program": "/usr/local/bin/x", "KeepAlive": {"SuccessfulExit": false}, "Disabled": true});
        let none = HashMap::new();
        assert!(!item_from(&job, "s", &none).expect("item").enabled);
        let enabled = HashMap::from([("com.x".to_owned(), false)]);
        let item = item_from(&job, "s", &enabled).expect("item");
        assert!(item.enabled);
        assert_eq!(item.name, "com.x");
    }

    #[test]
    fn outermost_app_names_nested_helpers() {
        assert_eq!(
            app_name("/Applications/Foo.app/Contents/Library/Bar.app/Contents/MacOS/bar")
                .as_deref(),
            Some("Foo")
        );
        assert_eq!(app_name("/usr/bin/true"), None);
    }
}
