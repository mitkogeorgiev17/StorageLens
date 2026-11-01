//! Startup items (#114): what Task Manager's Startup apps tab lists.
//!
//! - `Run` values under `HKCU` and `HKLM` (+ `WOW6432Node`).
//! - Shortcuts in this user's and the common Startup folder.
//!
//! Switching one is what Task Manager and Settings → Apps → Startup do: the
//! entry stays, and a `StartupApproved` value next to it says whether
//! Explorer runs it at login (first byte odd = disabled). `HKLM` entries need
//! an elevated process; this user's never do. Scheduled tasks triggered at
//! logon are not listed (Task Scheduler owns those).

#[cfg(windows)]
pub use sys::{set_startup_enabled, startup_items};

/// Settings → Apps → Startup.
pub const STARTUP_SETTINGS_URL: &str = "ms-settings:startupapps";

/// Whose list an entry is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hive {
    User,
    Machine,
}

/// Which list: the `Run` key, the 32-bit `Run` key, or the Startup folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum List {
    Run,
    Run32,
    Folder,
}

impl List {
    /// The `StartupApproved` subkey that switches entries of this list.
    pub fn approved_key(self) -> String {
        let name = match self {
            Self::Run => "Run",
            Self::Run32 => "Run32",
            Self::Folder => "StartupFolder",
        };
        format!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\{name}")
    }
}

/// `user:run:OneDrive` — hive, list and value (or shortcut file) name.
pub fn item_id(hive: Hive, list: List, name: &str) -> String {
    let hive = match hive {
        Hive::User => "user",
        Hive::Machine => "machine",
    };
    let list = match list {
        List::Run => "run",
        List::Run32 => "run32",
        List::Folder => "folder",
    };
    format!("{hive}:{list}:{name}")
}

/// The inverse of [`item_id`].
pub fn parse_id(id: &str) -> Option<(Hive, List, &str)> {
    let mut parts = id.splitn(3, ':');
    let hive = match parts.next()? {
        "user" => Hive::User,
        "machine" => Hive::Machine,
        _ => return None,
    };
    let list = match parts.next()? {
        "run" => List::Run,
        "run32" => List::Run32,
        "folder" => List::Folder,
        _ => return None,
    };
    let name = parts.next().filter(|name| !name.is_empty())?;
    Some((hive, list, name))
}

/// A `StartupApproved` value's verdict; no value means enabled.
pub fn approved_enabled(data: Option<&[u8]>) -> bool {
    data.and_then(|data| data.first())
        .is_none_or(|first| first & 1 == 0)
}

/// The 12-byte value Task Manager writes: `02` = enabled, `03` + the FILETIME
/// it was disabled at.
pub fn approved_value(enabled: bool, filetime: u64) -> [u8; 12] {
    let mut value = [0u8; 12];
    if enabled {
        value[0] = 0x02;
    } else {
        value[0] = 0x03;
        value[4..].copy_from_slice(&filetime.to_le_bytes());
    }
    value
}

/// Now as a Windows FILETIME (100 ns since 1601).
pub fn filetime_now() -> u64 {
    const UNIX_EPOCH_AS_FILETIME: u64 = 116_444_736_000_000_000;
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    UNIX_EPOCH_AS_FILETIME + u64::try_from(since.as_nanos() / 100).unwrap_or(0)
}

#[cfg(windows)]
mod sys {
    use std::collections::HashMap;
    use std::fs;

    use storagelens_core::StartupItem;
    use windows::Win32::System::Registry::{HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};

    use super::{approved_enabled, approved_value, filetime_now, item_id, parse_id, Hive, List};
    use crate::apps::sys::{env, Key};
    use crate::elevation::is_elevated;

    fn root(hive: Hive) -> HKEY {
        match hive {
            Hive::User => HKEY_CURRENT_USER,
            Hive::Machine => HKEY_LOCAL_MACHINE,
        }
    }

    fn approved(hive: Hive, list: List) -> HashMap<String, Vec<u8>> {
        Key::open(root(hive), &list.approved_key())
            .map(|key| {
                key.values()
                    .into_iter()
                    .map(|(name, data)| (name.to_lowercase(), data))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Every `Run` value and Startup-folder shortcut, this user's first.
    pub fn startup_items() -> Vec<StartupItem> {
        let elevated = is_elevated();
        let mut items = Vec::new();
        let runs = [
            (
                Hive::User,
                List::Run,
                r"Software\Microsoft\Windows\CurrentVersion\Run",
                "Run key · this user",
            ),
            (
                Hive::Machine,
                List::Run,
                r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run",
                "Run key · all users",
            ),
            (
                Hive::Machine,
                List::Run32,
                r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Run",
                "Run key · all users (32-bit)",
            ),
        ];
        for (hive, list, path, source) in runs {
            let Some(key) = Key::open(root(hive), path) else {
                continue;
            };
            let switches = approved(hive, list);
            for (name, _) in key.values() {
                let Some(command) = key.string(&name) else {
                    continue;
                };
                items.push(StartupItem {
                    id: item_id(hive, list, &name),
                    enabled: approved_enabled(
                        switches.get(&name.to_lowercase()).map(Vec::as_slice),
                    ),
                    can_toggle: hive == Hive::User || elevated,
                    name,
                    command,
                    source: source.to_owned(),
                });
            }
        }
        let folders = [
            (
                Hive::User,
                env("APPDATA")
                    .map(|d| format!(r"{d}\Microsoft\Windows\Start Menu\Programs\Startup")),
                "Startup folder · this user",
            ),
            (
                Hive::Machine,
                env("ProgramData")
                    .map(|d| format!(r"{d}\Microsoft\Windows\Start Menu\Programs\StartUp")),
                "Startup folder · all users",
            ),
        ];
        for (hive, dir, source) in folders {
            let Some(entries) = dir.and_then(|dir| fs::read_dir(dir).ok()) else {
                continue;
            };
            let switches = approved(hive, List::Folder);
            for entry in entries.flatten() {
                let file = entry.file_name().to_string_lossy().into_owned();
                if file.eq_ignore_ascii_case("desktop.ini") {
                    continue;
                }
                let name = file.strip_suffix(".lnk").unwrap_or(&file).to_owned();
                items.push(StartupItem {
                    id: item_id(hive, List::Folder, &file),
                    enabled: approved_enabled(
                        switches.get(&file.to_lowercase()).map(Vec::as_slice),
                    ),
                    can_toggle: hive == Hive::User || elevated,
                    name,
                    command: entry.path().display().to_string(),
                    source: source.to_owned(),
                });
            }
        }
        items
    }

    /// Switch the entry `id` (from [`startup_items`]) for future logins.
    pub fn set_startup_enabled(id: &str, enabled: bool) -> Result<(), String> {
        let (hive, list, name) =
            parse_id(id).ok_or_else(|| format!("unknown startup item {id}"))?;
        if hive == Hive::Machine && !is_elevated() {
            return Err(
                "Switching an entry for all users needs StorageLens to run as administrator."
                    .into(),
            );
        }
        let key = Key::create(root(hive), &list.approved_key())
            .ok_or_else(|| "could not open the StartupApproved key".to_owned())?;
        if key.set_binary(name, &approved_value(enabled, filetime_now())) {
            Ok(())
        } else {
            Err(format!("could not switch {name}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip() {
        for (hive, list) in [
            (Hive::User, List::Run),
            (Hive::Machine, List::Run32),
            (Hive::Machine, List::Folder),
        ] {
            let id = item_id(hive, list, "Foo: bar.lnk");
            assert_eq!(parse_id(&id), Some((hive, list, "Foo: bar.lnk")));
        }
        assert_eq!(parse_id("user:run:"), None);
        assert_eq!(parse_id("other:run:x"), None);
    }

    #[test]
    fn approved_values_like_task_manager() {
        assert!(approved_enabled(None));
        assert!(approved_enabled(Some(&[0x02, 0, 0, 0])));
        assert!(approved_enabled(Some(&[0x06, 0, 0, 0])));
        assert!(!approved_enabled(Some(&[0x03, 0, 0, 0])));
        assert!(!approved_enabled(Some(&[0x07])));

        assert_eq!(
            approved_value(true, 99),
            [2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        let off = approved_value(false, 0x0102_0304_0506_0708);
        assert_eq!(off[..4], [3, 0, 0, 0]);
        assert_eq!(off[4..], 0x0102_0304_0506_0708u64.to_le_bytes());
        assert!(!approved_enabled(Some(&off)));
    }

    #[test]
    fn filetime_is_after_2020() {
        // 2020-01-01 as FILETIME.
        assert!(filetime_now() > 132_223_104_000_000_000);
    }
}
