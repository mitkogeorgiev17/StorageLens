//! What a removed app leaves outside the user's Library (#123): its launchd
//! jobs in `/Library/LaunchDaemons` and `/Library/LaunchAgents`, the helper
//! tools they run from `/Library/PrivilegedHelperTools`, and the installer
//! receipts (`pkgutil`) of the packages that installed it.
//!
//! Strong (ticked) when the evidence names the app: a job that runs a program
//! inside the bundle, lists its bundle id in `AssociatedBundleIdentifiers` or
//! is labelled with it; a receipt whose file list contains the bundle. A job
//! that only shares the vendor's id prefix (`com.docker.vmnetd` for
//! `com.docker.docker`) is offered unticked.
//!
//! These are root-owned, so removing them takes one administrator prompt:
//! jobs are booted out, files are moved into a folder in the user's Trash
//! (not deleted), and receipts are forgotten (`pkgutil --forget` only drops
//! the installer's record).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::apps::plist_json;

pub const JOB_DIRS: [&str; 2] = ["/Library/LaunchDaemons", "/Library/LaunchAgents"];
pub const HELPER_DIR: &str = "/Library/PrivilegedHelperTools";

/// One thing a removed app left in the system domain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemLeftover {
    /// A file path, or the package id for a receipt.
    pub path: String,
    /// `"helper"` (a job plist or helper tool) or `"receipt"`.
    pub kind: &'static str,
    pub strong: bool,
}

/// `com.docker.` for `com.docker.docker`; `None` for ids too short to say
/// anything about a vendor (`foo`, `com.foo`).
pub fn vendor_prefix(bundle_id: &str) -> Option<String> {
    let parts: Vec<&str> = bundle_id.split('.').collect();
    (parts.len() >= 3 && parts.iter().all(|part| !part.is_empty()))
        .then(|| format!("{}.{}.", parts[0], parts[1]).to_lowercase())
}

/// How strongly a launchd job belongs to the app, if at all.
pub fn job_match(job: &Value, app: &Path, bundle_id: &str) -> Option<bool> {
    let id = bundle_id.to_lowercase();
    if id.is_empty() {
        return None;
    }
    let label = job
        .get("Label")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    let program = job
        .get("Program")
        .or_else(|| job.get("ProgramArguments").and_then(|args| args.get(0)))
        .and_then(Value::as_str);
    let associated = match job.get("AssociatedBundleIdentifiers") {
        Some(Value::Array(ids)) => ids
            .iter()
            .filter_map(Value::as_str)
            .any(|other| other.eq_ignore_ascii_case(&id)),
        Some(Value::String(other)) => other.eq_ignore_ascii_case(&id),
        _ => false,
    };
    let strong = associated
        || label == id
        || label
            .strip_prefix(&id)
            .is_some_and(|rest| rest.starts_with('.'))
        || program.is_some_and(|program| Path::new(program).starts_with(app));
    if strong {
        return Some(true);
    }
    vendor_prefix(&id)
        .filter(|prefix| label.starts_with(prefix.as_str()))
        .map(|_| false)
}

/// The job's program, when it is a helper tool.
pub fn helper_tool(job: &Value) -> Option<PathBuf> {
    job.get("Program")
        .or_else(|| job.get("ProgramArguments").and_then(|args| args.get(0)))
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .filter(|program| program.parent() == Some(Path::new(HELPER_DIR)))
}

/// `volume` + `location` from `pkgutil --pkg-info`, as the root the
/// receipt's file list is relative to.
pub fn receipt_root(info: &str) -> PathBuf {
    let field = |name: &str| {
        info.lines()
            .find_map(|line| line.strip_prefix(name))
            .map(|value| value.trim().to_owned())
            .unwrap_or_default()
    };
    let volume = field("volume:");
    let location = field("location:");
    let mut root = PathBuf::from(if volume.is_empty() { "/" } else { &volume });
    if !location.is_empty() {
        root.push(location.trim_start_matches('/'));
    }
    root
}

/// The receipt installed the bundle at `app`.
pub fn receipt_installed(root: &Path, files: &str, app: &Path) -> bool {
    files.lines().any(|line| root.join(line.trim()) == app)
}

/// Safe to put in a single-quoted shell word and to hand to pkgutil.
pub fn safe_package_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// A path this module may remove: a plist in a job folder, or a file in the
/// helper folder, named plainly.
pub fn removable_path(path: &str) -> bool {
    let path = Path::new(path);
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let plain = !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '@'));
    let parent = path.parent().and_then(Path::to_str).unwrap_or_default();
    plain && ((JOB_DIRS.contains(&parent) && name.ends_with(".plist")) || parent == HELPER_DIR)
}

fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', r"'\''"))
}

/// The shell script one admin prompt runs: boot each job out, move the files
/// into `trash_dir`, forget the receipts. Every step is allowed to fail on
/// its own (a job not loaded, a file already gone) without stopping the rest.
pub fn removal_script(items: &[SystemLeftover], trash_dir: &Path, owner: &str) -> String {
    let trash = quote(&trash_dir.display().to_string());
    let mut lines = vec![format!("mkdir -p {trash}")];
    for item in items {
        let path = quote(&item.path);
        if item.kind == "receipt" {
            lines.push(format!("/usr/sbin/pkgutil --forget {path} >/dev/null 2>&1"));
            continue;
        }
        if item.path.ends_with(".plist") {
            lines.push(format!(
                "/bin/launchctl bootout system {path} >/dev/null 2>&1"
            ));
        }
        lines.push(format!("/bin/mv -f {path} {trash}/"));
    }
    lines.push(format!("/usr/sbin/chown -R {} {trash}", quote(owner)));
    lines.push("exit 0".to_owned());
    lines.join(" ; ")
}

/// AppleScript that runs `script` as root behind the standard admin prompt.
pub fn admin_applescript(script: &str, prompt: &str) -> String {
    let escape = |text: &str| text.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "do shell script \"{}\" with prompt \"{}\" with administrator privileges",
        escape(script),
        escape(prompt)
    )
}

fn tool(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Everything the app at `app` (bundle id `bundle_id`) left in the system
/// domain that still exists. Strong matches first.
pub fn find(app: &Path, bundle_id: &str) -> Vec<SystemLeftover> {
    // Apple's own jobs and receipts belong to the system, whatever matches.
    if bundle_id.to_lowercase().starts_with("com.apple.") {
        return Vec::new();
    }
    let mut out: Vec<SystemLeftover> = Vec::new();
    let mut push = |path: String, kind, strong| {
        if !out.iter().any(|item: &SystemLeftover| item.path == path) {
            out.push(SystemLeftover { path, kind, strong });
        }
    };

    for dir in JOB_DIRS {
        let Ok(entries) = fs::read_dir(dir) else {
            continue;
        };
        let mut plists: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
        plists.sort();
        for plist in plists {
            let Some(job) = plist_json(&plist) else {
                continue;
            };
            let Some(strong) = job_match(&job, app, bundle_id) else {
                continue;
            };
            push(plist.display().to_string(), "helper", strong);
            if let Some(tool) = helper_tool(&job).filter(|tool| tool.exists()) {
                push(tool.display().to_string(), "helper", strong);
            }
        }
    }

    // Receipts: only packages of the same vendor are opened (`--files` is
    // slow), and one counts only when it installed this very bundle.
    if let Some(prefix) = vendor_prefix(bundle_id) {
        let ids = tool("/usr/sbin/pkgutil", &["--pkgs"]).unwrap_or_default();
        for id in ids.lines().map(str::trim).filter(|id| safe_package_id(id)) {
            let lower = id.to_lowercase();
            if !lower.starts_with(&prefix) || lower.starts_with("com.apple.") {
                continue;
            }
            let (Some(info), Some(files)) = (
                tool("/usr/sbin/pkgutil", &["--pkg-info", id]),
                tool("/usr/sbin/pkgutil", &["--files", id]),
            ) else {
                continue;
            };
            let root = receipt_root(&info);
            if !receipt_installed(&root, &files, app) {
                continue;
            }
            push(id.to_owned(), "receipt", true);
            for line in files.lines() {
                let path = root.join(line.trim());
                if path.parent() == Some(Path::new(HELPER_DIR)) && path.exists() {
                    push(path.display().to_string(), "helper", true);
                }
            }
        }
    }

    out.retain(|item| item.kind == "receipt" || removable_path(&item.path));
    out.sort_by_key(|item| !item.strong);
    out
}

/// Remove `items` (from [`find`]) with one administrator prompt; files land in
/// `trash_dir`, owned by `owner` afterwards.
pub fn remove(
    items: &[SystemLeftover],
    trash_dir: &Path,
    owner: &str,
    app_name: &str,
) -> Result<(), String> {
    if let Some(bad) = items.iter().find(|item| {
        if item.kind == "receipt" {
            !safe_package_id(&item.path)
        } else {
            !removable_path(&item.path)
        }
    }) {
        return Err(format!("{} cannot be removed here", bad.path));
    }
    let script = removal_script(items, trash_dir, owner);
    let prompt = format!("StorageLens wants to remove what {app_name} left in the system.");
    let output = Command::new("/usr/bin/osascript")
        .args(["-e", &admin_applescript(&script, &prompt)])
        .output()
        .map_err(|error| format!("could not ask for administrator rights: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("-128") {
            Err("Cancelled: nothing was removed.".to_owned())
        } else {
            Err(format!("Removing failed: {}", stderr.trim()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const DOCKER: &str = "/Applications/Docker.app";

    #[test]
    fn jobs_match_by_bundle_id_program_or_vendor_prefix() {
        let app = Path::new(DOCKER);
        let socket = json!({"Label": "com.docker.socket", "Program": "/Library/PrivilegedHelperTools/com.docker.socket", "AssociatedBundleIdentifiers": ["com.docker.docker"]});
        assert_eq!(job_match(&socket, app, "com.docker.docker"), Some(true));
        let vmnetd = json!({"Label": "com.docker.vmnetd", "Program": "/Library/PrivilegedHelperTools/com.docker.vmnetd"});
        assert_eq!(job_match(&vmnetd, app, "com.docker.docker"), Some(false));
        let inside = json!({"Label": "x.y.z", "ProgramArguments": ["/Applications/Docker.app/Contents/MacOS/helper"]});
        assert_eq!(job_match(&inside, app, "com.docker.docker"), Some(true));
        let other = json!({"Label": "com.other.thing"});
        assert_eq!(job_match(&other, app, "com.docker.docker"), None);
        assert_eq!(job_match(&socket, app, ""), None);
        assert_eq!(
            helper_tool(&socket),
            Some(PathBuf::from(
                "/Library/PrivilegedHelperTools/com.docker.socket"
            ))
        );
        assert_eq!(helper_tool(&inside), None);
    }

    #[test]
    fn vendor_prefixes_need_three_parts() {
        assert_eq!(
            vendor_prefix("com.docker.docker").as_deref(),
            Some("com.docker.")
        );
        assert_eq!(vendor_prefix("com.foo"), None);
        assert_eq!(vendor_prefix("a..b"), None);
    }

    #[test]
    fn receipts_count_only_when_they_installed_the_bundle() {
        let root = receipt_root("package-id: x\nvolume: /\nlocation: Applications\n");
        assert_eq!(root, PathBuf::from("/Applications"));
        assert!(receipt_installed(
            &root,
            "Docker.app\nDocker.app/Contents\n",
            Path::new(DOCKER)
        ));
        let rootless = receipt_root("volume: /\nlocation: \n");
        assert!(receipt_installed(
            &rootless,
            "Applications/Docker.app\n",
            Path::new(DOCKER)
        ));
        assert!(!receipt_installed(
            &rootless,
            "Applications\nLibrary\n",
            Path::new(DOCKER)
        ));
    }

    #[test]
    fn only_plain_files_in_the_system_folders() {
        assert!(removable_path(
            "/Library/LaunchDaemons/com.docker.vmnetd.plist"
        ));
        assert!(removable_path(
            "/Library/PrivilegedHelperTools/com.docker.vmnetd"
        ));
        assert!(!removable_path("/Library/LaunchDaemons/notaplist"));
        assert!(!removable_path("/Library/LaunchDaemons/../../etc/passwd"));
        assert!(!removable_path(
            "/System/Library/LaunchDaemons/com.apple.x.plist"
        ));
        assert!(!removable_path("/Library/PrivilegedHelperTools/a b"));
        assert!(!removable_path("/Library/PrivilegedHelperTools"));
        assert!(safe_package_id("com.docker.pkg"));
        assert!(!safe_package_id("x'; rm -rf /"));
    }

    #[test]
    fn script_boots_out_moves_and_forgets() {
        let items = [
            SystemLeftover {
                path: "/Library/LaunchDaemons/com.docker.vmnetd.plist".into(),
                kind: "helper",
                strong: true,
            },
            SystemLeftover {
                path: "com.docker.pkg".into(),
                kind: "receipt",
                strong: true,
            },
        ];
        let script = removal_script(&items, Path::new("/Users/u/.Trash/Docker leftovers"), "u");
        assert!(script
            .contains("launchctl bootout system '/Library/LaunchDaemons/com.docker.vmnetd.plist'"));
        assert!(script.contains("/bin/mv -f '/Library/LaunchDaemons/com.docker.vmnetd.plist' '/Users/u/.Trash/Docker leftovers'/"));
        assert!(script.contains("pkgutil --forget 'com.docker.pkg'"));
        assert!(script.contains("chown -R 'u'"));
        let apple = admin_applescript("echo \"hi\" \\ x", "Say \"ok\"");
        assert!(apple.contains("echo \\\"hi\\\" \\\\ x"));
        assert!(apple.ends_with("with administrator privileges"));
    }
}
