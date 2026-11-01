//! Registry keys an uninstalled app may leave behind (#121).
//!
//! Strict on purpose: only `Software\<Publisher>\<App>` is a strong match
//! (ticked in the UI). `Software\<App>` is offered as a name-only match,
//! unticked. Never `Software\<Publisher>` itself (other apps of the same
//! vendor live there), never a shared root (`Microsoft`, `Classes`,
//! `Policies`, …), and never a Microsoft app's keys.
//!
//! Every key is exported with `reg export` before it is deleted, so it can be
//! restored with `reg import` (or a double-click on the `.reg` file).

use storagelens_core::RegistryKey;

#[cfg(windows)]
pub use sys::{delete_with_backup, key_exists, restore_backup};

/// Names that are shared roots, never one app's.
const SHARED: [&str; 12] = [
    "microsoft",
    "microsoft corporation",
    "classes",
    "clients",
    "policies",
    "wow6432node",
    "windows",
    "registeredapplications",
    "odbc",
    "software",
    "system",
    "default",
];

/// Company suffixes vendors drop from their registry folder name
/// (`Acme, Inc.` publishes under `Software\Acme`).
const SUFFIXES: [&str; 11] = [
    "inc",
    "inc.",
    "llc",
    "ltd",
    "ltd.",
    "limited",
    "corporation",
    "corp",
    "corp.",
    "gmbh",
    "co.",
];

const BASES: [&str; 3] = [
    r"HKEY_CURRENT_USER\Software",
    r"HKEY_LOCAL_MACHINE\SOFTWARE",
    r"HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node",
];

fn usable(name: &str) -> Option<String> {
    let name = name.trim().trim_end_matches(['.', ',']).trim();
    (!name.is_empty()
        && !name.contains(['\\', '/', '*', '?'])
        && !SHARED.contains(&name.to_lowercase().as_str()))
    .then(|| name.to_owned())
}

/// `Acme, Inc.` → `Acme`; unchanged when no suffix matches.
pub fn strip_company_suffix(publisher: &str) -> String {
    let mut words: Vec<&str> = publisher.split_whitespace().collect();
    while words.len() > 1
        && words
            .last()
            .is_some_and(|last| SUFFIXES.contains(&last.to_lowercase().as_str()))
    {
        words.pop();
    }
    words.join(" ").trim_end_matches(',').to_owned()
}

/// Candidate keys for an app known by `names` (display name, install folder
/// name) and `publisher`. Whether they exist is checked when they are offered.
pub fn candidates(names: &[&str], publisher: Option<&str>) -> Vec<RegistryKey> {
    let publisher_is_microsoft =
        publisher.is_some_and(|publisher| publisher.to_lowercase().starts_with("microsoft"));
    if publisher_is_microsoft {
        return Vec::new();
    }
    let mut apps: Vec<String> = names.iter().filter_map(|name| usable(name)).collect();
    apps.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    let mut publishers: Vec<String> = publisher
        .into_iter()
        .flat_map(|publisher| [publisher.to_owned(), strip_company_suffix(publisher)])
        .filter_map(|publisher| usable(&publisher))
        .collect();
    publishers.dedup_by(|a, b| a.eq_ignore_ascii_case(b));

    let mut out: Vec<RegistryKey> = Vec::new();
    for base in BASES {
        for app in &apps {
            for publisher in &publishers {
                if !publisher.eq_ignore_ascii_case(app) {
                    out.push(RegistryKey {
                        path: format!(r"{base}\{publisher}\{app}"),
                        strong: true,
                    });
                }
            }
            out.push(RegistryKey {
                path: format!(r"{base}\{app}"),
                strong: false,
            });
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|key| seen.insert(key.path.to_lowercase()));
    out
}

/// Deleting it needs an elevated process.
pub fn needs_admin(path: &str) -> bool {
    path.starts_with("HKEY_LOCAL_MACHINE")
}

#[cfg(windows)]
mod sys {
    use std::os::windows::process::CommandExt;
    use std::path::Path;
    use std::process::Command;

    use windows::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    use crate::apps::sys::Key;

    /// No console window flashing up for `reg.exe`.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn split(path: &str) -> Option<(HKEY, &str)> {
        let (hive, rest) = path.split_once('\\')?;
        let root = match hive {
            "HKEY_CURRENT_USER" => HKEY_CURRENT_USER,
            "HKEY_LOCAL_MACHINE" => HKEY_LOCAL_MACHINE,
            _ => return None,
        };
        Some((root, rest))
    }

    pub fn key_exists(path: &str) -> bool {
        split(path).is_some_and(|(root, rest)| Key::open(root, rest).is_some())
    }

    fn reg(args: &[&std::ffi::OsStr]) -> Result<(), String> {
        let output = Command::new("reg.exe")
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|error| format!("could not run reg.exe: {error}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).trim().to_owned())
        }
    }

    /// Export `path` to `backup`, then delete it. Nothing is deleted unless
    /// the backup was written.
    pub fn delete_with_backup(path: &str, backup: &Path) -> Result<(), String> {
        reg(&[
            "export".as_ref(),
            path.as_ref(),
            backup.as_os_str(),
            "/y".as_ref(),
        ])
        .map_err(|error| format!("backup failed, nothing deleted: {error}"))?;
        if !backup.is_file() {
            return Err("backup failed, nothing deleted".to_owned());
        }
        reg(&["delete".as_ref(), path.as_ref(), "/f".as_ref()])
    }

    /// Put a backed-up key back.
    pub fn restore_backup(backup: &Path) -> Result<(), String> {
        reg(&["import".as_ref(), backup.as_os_str()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(keys: &[RegistryKey], strong: bool) -> Vec<&str> {
        keys.iter()
            .filter(|key| key.strong == strong)
            .map(|key| key.path.as_str())
            .collect()
    }

    #[test]
    fn publisher_and_app_is_strong_app_alone_is_weak() {
        let keys = candidates(&["Foo", "foo"], Some("Acme, Inc."));
        assert_eq!(
            paths(&keys, true),
            [
                r"HKEY_CURRENT_USER\Software\Acme, Inc\Foo",
                r"HKEY_CURRENT_USER\Software\Acme\Foo",
                r"HKEY_LOCAL_MACHINE\SOFTWARE\Acme, Inc\Foo",
                r"HKEY_LOCAL_MACHINE\SOFTWARE\Acme\Foo",
                r"HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Acme, Inc\Foo",
                r"HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Acme\Foo",
            ]
        );
        assert_eq!(
            paths(&keys, false),
            [
                r"HKEY_CURRENT_USER\Software\Foo",
                r"HKEY_LOCAL_MACHINE\SOFTWARE\Foo",
                r"HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Foo",
            ]
        );
    }

    #[test]
    fn never_a_vendor_root_shared_key_or_microsoft_app() {
        let keys = candidates(&["Acme"], Some("Acme"));
        assert!(
            keys.iter().all(|key| !key.strong),
            "Software\\Acme\\Acme is not offered"
        );
        assert!(candidates(&["Classes", "Policies"], Some("Microsoft")).is_empty());
        assert!(candidates(&["Teams"], Some("Microsoft Corporation")).is_empty());
        assert!(candidates(&["", "a\\b"], None).is_empty());
    }

    #[test]
    fn suffixes_and_backup_names() {
        assert_eq!(strip_company_suffix("Foo Software GmbH"), "Foo Software");
        assert_eq!(strip_company_suffix("Inc."), "Inc.");
        assert_eq!(strip_company_suffix("Bar Corp."), "Bar");
        assert!(needs_admin(r"HKEY_LOCAL_MACHINE\SOFTWARE\Foo"));
        assert!(!needs_admin(r"HKEY_CURRENT_USER\Software\Foo"));
    }
}
