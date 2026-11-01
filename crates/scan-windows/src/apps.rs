//! Installed-app inventory (#37): uninstall keys, usage evidence, footprint.
//!
//! # Evidence
//!
//! - **Installed:** `HKLM` / `HKCU` `…\CurrentVersion\Uninstall` (+ `WOW6432Node`).
//!   `SystemComponent = 1` entries and updates (`ParentKeyName`) are hidden by
//!   Windows itself and skipped here too.
//! - **Last used:** Prefetch (`C:\Windows\Prefetch\EXE-HASH.pf` last-write time,
//!   admin only) and UserAssist (`HKCU\…\Explorer\UserAssist`, per-user run count
//!   and last-run time for programs started from the shell). `LastAccessTime` is
//!   never read: it is disabled by default on modern NTFS.
//!   `Amcache.hve` is not read: the system keeps the hive open exclusively, so it
//!   can only be read from a shadow copy, and it records first execution, not
//!   last use.
//! - **Running:** `EnumProcesses` + `QueryFullProcessImageNameW`.
//! - **Autostart:** `Run` keys, both Startup folders (`.lnk` name), and the
//!   commands of scheduled tasks in `C:\Windows\System32\Tasks`.
//! - **Default handler:** `UserChoice` ProgIds under `FileExts` and the
//!   `http` / `https` / `mailto` URL associations, resolved to their
//!   `shell\open\command` executable.
//!
//! Every registry or file read degrades to "no evidence": a broken key yields
//! fewer signals, which classifies as `unknown` / `in-use`, never `unused`.
//!
//! The matching below is pure and string-based (Windows paths compared
//! case-insensitively), so it is unit-tested on every platform; the reads live
//! in the Windows-only `sys` module.
//!
//! # Known overcount
//!
//! An app installed inside another app's directory is counted in both.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use storagelens_core::{AppInfo, AppStatus, Bytes, Satellite, Signature};

use crate::ffi::filetime_to_unix;

#[cfg(windows)]
pub use sys::{force_quit, inventory, is_registered, run_uninstaller};

/// One `Uninstall\<key>` entry, as read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UninstallEntry {
    /// Subkey name: the app id.
    pub key: String,
    /// `DisplayName`. Entries without one are not shown by Windows either.
    pub name: String,
    /// `DisplayVersion`.
    pub version: Option<String>,
    /// `Publisher`.
    pub publisher: Option<String>,
    /// `InstallLocation`.
    pub location: Option<String>,
    /// `DisplayIcon`: often the main executable, `path,index`.
    pub icon: Option<String>,
    /// `SystemComponent = 1`.
    pub system_component: bool,
    /// `ParentKeyName` is set: an update or sub-component.
    pub has_parent: bool,
    /// `UninstallString`.
    pub uninstall: Option<String>,
    /// `WindowsInstaller = 1`: an MSI package, keyed by its product code.
    pub windows_installer: bool,
}

/// Well-known directories, lowercased, without trailing separators.
#[derive(Debug, Clone, Default)]
pub struct Roots {
    /// `%WINDIR%`.
    pub windows: String,
    /// `%LOCALAPPDATA%`, `%APPDATA%`, `%PROGRAMDATA%`: where satellites live.
    pub data: Vec<String>,
    /// Directories too broad to be an install location (`Program Files`, the
    /// profile, the data roots, …).
    pub too_broad: Vec<String>,
}

/// Everything read from the machine besides the uninstall entries.
#[derive(Debug, Clone, Default)]
pub struct Evidence {
    /// UserAssist: `(exe path, run count, last run unix seconds)`.
    pub user_assist: Vec<(String, u32, Option<i64>)>,
    /// Prefetch: `(lowercase exe file name, last run unix seconds)`.
    pub prefetch: Vec<(String, i64)>,
    /// Executable paths of running processes.
    pub running: Vec<String>,
    /// Executables launched by `Run` keys or scheduled tasks.
    pub autostart: Vec<String>,
    /// Lowercase `.lnk` stems in the Startup folders.
    pub startup_links: Vec<String>,
    /// Executables behind the user's default-handler ProgIds.
    pub handlers: Vec<String>,
}

/// Lowercase with `\` separators and no trailing separator.
pub fn normalize(path: &str) -> String {
    let path = path
        .trim()
        .trim_matches('"')
        .replace('/', "\\")
        .to_lowercase();
    match path.trim_end_matches('\\') {
        // Keep `c:\` a root rather than turning it into the relative `c:`.
        root if root.len() == 2 && root.ends_with(':') => format!("{root}\\"),
        trimmed => trimmed.to_owned(),
    }
}

/// `path` is `dir` or inside it. Both normalized.
pub fn under(path: &str, dir: &str) -> bool {
    path == dir
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('\\') || dir.ends_with('\\'))
}

fn file_name(path: &str) -> &str {
    path.rsplit('\\').next().unwrap_or(path)
}

/// The executable a command line starts: `"C:\x\a.exe" -f`, `C:\x\a.exe -f`,
/// `C:\Program Files\x\a.exe /s` (unquoted with spaces).
pub fn exe_from_command(command: &str) -> Option<String> {
    let command = command.trim();
    if let Some(quoted) = command.strip_prefix('"') {
        let end = quoted.find('"')?;
        return Some(quoted[..end].to_owned()).filter(|exe| !exe.is_empty());
    }
    let lower = command.to_lowercase();
    if let Some(at) = lower.find(".exe") {
        return Some(command[..at + 4].to_owned());
    }
    command.split_whitespace().next().map(str::to_owned)
}

/// The command "Add or remove apps" runs for `entry` (#115): `MsiExec /X` with
/// the product code for an MSI package (its `UninstallString` is often `/I`,
/// which opens repair), else `UninstallString`.
pub fn uninstall_command(entry: &UninstallEntry) -> Option<String> {
    if entry.windows_installer && entry.key.starts_with('{') && entry.key.ends_with('}') {
        return Some(format!("MsiExec.exe /X{}", entry.key));
    }
    entry
        .uninstall
        .as_deref()
        .map(str::trim)
        .filter(|command| !command.is_empty())
        .map(str::to_owned)
}

/// `(program, arguments)` of a command line, for `ShellExecuteExW`.
pub fn split_command(command: &str) -> Option<(String, String)> {
    let command = command.trim();
    let program = exe_from_command(command)?;
    let skip = if command.starts_with('"') {
        program.len() + 2
    } else {
        program.len()
    };
    let arguments = command.get(skip..).unwrap_or_default().trim().to_owned();
    Some((program, arguments))
}

/// `DisplayIcon` → file path: strip quotes and a trailing `,index`.
pub fn icon_path(icon: &str) -> String {
    let icon = icon.trim().trim_matches('"');
    match icon.rsplit_once(',') {
        Some((path, index)) if index.trim().parse::<i32>().is_ok() => {
            path.trim().trim_matches('"').to_owned()
        }
        _ => icon.to_owned(),
    }
}

/// Replace `%NAME%` with `lookup(NAME)`; unknown names are left as they are.
pub fn expand_env(text: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => match lookup(&after[..end]) {
                Some(value) => {
                    out.push_str(&value);
                    rest = &after[end + 1..];
                }
                None => {
                    out.push('%');
                    rest = after;
                }
            },
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// ROT13, the "encryption" of UserAssist value names.
pub fn rot13(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            'a'..='z' => (((c as u8 - b'a') + 13) % 26 + b'a') as char,
            'A'..='Z' => (((c as u8 - b'A') + 13) % 26 + b'A') as char,
            _ => c,
        })
        .collect()
}

/// A UserAssist `Count` value (Windows 7+, 72 bytes): run count at offset 4,
/// last-run `FILETIME` at offset 60. A zero time means "never recorded".
pub fn user_assist_record(data: &[u8]) -> Option<(u32, Option<i64>)> {
    let count = u32::from_le_bytes(data.get(4..8)?.try_into().ok()?);
    let ticks = u64::from_le_bytes(data.get(60..68)?.try_into().ok()?);
    Some((count, (ticks != 0).then(|| filetime_to_unix(ticks))))
}

/// Known-folder GUIDs that prefix UserAssist paths.
pub const KNOWN_FOLDERS: [(&str, &str); 8] = [
    ("{6D809377-6AF0-444B-8957-A3773F02200E}", "ProgramW6432"),
    (
        "{7C5A40EF-A0FB-4BFC-874A-C0F2E0B9FA8E}",
        "ProgramFiles(x86)",
    ),
    ("{F38BF404-1D43-42F2-9305-67DE0B28FC23}", "windir"),
    ("{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}", "windir\\System32"),
    ("{D65231B0-B2F1-4857-A4CE-A8E7C6EA7D27}", "windir\\SysWOW64"),
    ("{F1B32785-6FBA-4FCF-9D55-7B8E7F157091}", "LOCALAPPDATA"),
    ("{3EB685DB-65F9-4CF6-A03A-E3EF65729F3D}", "APPDATA"),
    ("{62AB5D82-FDC1-4DC3-A9DD-070D1D495D97}", "ProgramData"),
];

/// Replace a leading known-folder GUID with its path. The table's values are
/// an environment variable, optionally followed by a subpath.
pub fn expand_known_folder(path: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    if !path.starts_with('{') {
        return Some(path.to_owned());
    }
    let (guid, rest) = path.split_once('}')?;
    let guid = format!("{guid}}}").to_uppercase();
    let (_, target) = KNOWN_FOLDERS.iter().find(|(known, _)| *known == guid)?;
    let (var, sub) = target.split_once('\\').unwrap_or((target, ""));
    let base = lookup(var)?;
    let base = if sub.is_empty() {
        base
    } else {
        format!("{base}\\{sub}")
    };
    Some(format!("{base}{rest}"))
}

/// `CHROME.EXE-1A2B3C4D.pf` → `chrome.exe`.
pub fn prefetch_exe(file: &str) -> Option<String> {
    let lower = file.to_lowercase();
    let stem = lower.strip_suffix(".pf")?;
    let (exe, hash) = stem.rsplit_once('-')?;
    (hash.len() == 8 && hash.chars().all(|c| c.is_ascii_hexdigit()) && !exe.is_empty())
        .then(|| exe.to_owned())
}

/// `<Command>` values from a scheduled-task XML file, entities decoded.
pub fn task_commands(xml: &str) -> Vec<String> {
    xml.split("<Command>")
        .skip(1)
        .filter_map(|tail| tail.split_once("</Command>").map(|(command, _)| command))
        .map(|command| {
            command
                .replace("&quot;", "\"")
                .replace("&apos;", "'")
                .replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&amp;", "&")
        })
        .collect()
}

/// A task file is UTF-16 LE with a BOM; tolerate UTF-8 too.
pub fn decode_task_file(bytes: &[u8]) -> String {
    match bytes {
        [0xFF, 0xFE, rest @ ..] => {
            let units: Vec<u16> = rest
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            String::from_utf16_lossy(&units)
        }
        _ => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// The install directory of an entry, normalized, if it is plausible: a real
/// directory (`is_dir`) that is not a drive root or a well-known parent like
/// `Program Files`. Falls back to `DisplayIcon`'s directory.
///
/// Returns `(normalized, as written)`: the first for matching, the second for
/// display.
pub fn install_dir(
    entry: &UninstallEntry,
    roots: &Roots,
    is_dir: &dyn Fn(&str) -> bool,
) -> Option<(String, String)> {
    let plausible = |dir: &str| {
        dir.len() > 3
            && dir.contains('\\')
            && !roots.too_broad.iter().any(|broad| broad == dir)
            && is_dir(dir)
    };
    let as_written = |path: &str| {
        let path = path.trim().trim_matches('"');
        let trimmed = path.trim_end_matches(['\\', '/']);
        if trimmed.len() < path.len() && trimmed.ends_with(':') {
            path.to_owned()
        } else {
            trimmed.to_owned()
        }
    };
    let from_location = entry
        .location
        .as_deref()
        .map(|location| (normalize(location), as_written(location)))
        .filter(|(dir, _)| plausible(dir));
    from_location.or_else(|| {
        let icon = icon_path(entry.icon.as_deref()?);
        let dir = icon.rsplit_once(['\\', '/']).map(|(dir, _)| dir)?;
        normalize(&icon)
            .ends_with(".exe")
            .then(|| (normalize(dir), as_written(dir)))
            .filter(|(dir, _)| plausible(dir))
    })
}

/// Combine uninstall entries and evidence into apps (sizes still zero).
///
/// - `exe_names(dir)`: lowercase `*.exe` file names directly in `dir`.
/// - `exists(path)`: a satellite candidate exists.
pub fn assemble(
    entries: &[UninstallEntry],
    evidence: &Evidence,
    roots: &Roots,
    is_dir: &dyn Fn(&str) -> bool,
    exe_names: &dyn Fn(&str) -> Vec<String>,
    exists: &dyn Fn(&str) -> bool,
) -> Vec<AppInfo> {
    // One app per install directory (or per name, when there is none): the
    // 32- and 64-bit keys and HKLM/HKCU often describe the same install.
    let mut seen = HashSet::new();
    let installed: Vec<(&UninstallEntry, Option<(String, String)>)> = entries
        .iter()
        .filter(|entry| {
            !entry.name.trim().is_empty() && !entry.system_component && !entry.has_parent
        })
        .map(|entry| (entry, install_dir(entry, roots, is_dir)))
        .filter(|(entry, dir)| {
            seen.insert(match dir {
                Some((dir, _)) => dir.clone(),
                None => format!("name:{}", entry.name.to_lowercase()),
            })
        })
        .collect();
    let display: Vec<String> = installed
        .iter()
        .map(|(_, dir)| {
            dir.as_ref()
                .map(|(_, shown)| shown.clone())
                .unwrap_or_default()
        })
        .collect();
    let apps: Vec<(&UninstallEntry, Option<String>)> = installed
        .into_iter()
        .map(|(entry, dir)| (entry, dir.map(|(dir, _)| dir)))
        .collect();

    // Executable names per app. A name two apps share (`update.exe`) says
    // nothing about either, so it is dropped before Prefetch matching.
    let names: Vec<HashSet<String>> = apps
        .iter()
        .map(|(entry, dir)| {
            let Some(dir) = dir else {
                return HashSet::new();
            };
            let mut names: HashSet<String> = exe_names(dir).into_iter().collect();
            if let Some(icon) = entry
                .icon
                .as_deref()
                .map(|icon| normalize(&icon_path(icon)))
            {
                if icon.ends_with(".exe") && under(&icon, dir) {
                    names.insert(file_name(&icon).to_owned());
                }
            }
            names
        })
        .collect();
    let mut name_claims: HashMap<&str, usize> = HashMap::new();
    for name in names.iter().flatten() {
        *name_claims.entry(name).or_default() += 1;
    }

    let normalized = |paths: &[String]| paths.iter().map(|p| normalize(p)).collect::<Vec<_>>();
    let running = normalized(&evidence.running);
    let autostart = normalized(&evidence.autostart);
    let handlers = normalized(&evidence.handlers);
    let user_assist: Vec<(String, u32, Option<i64>)> = evidence
        .user_assist
        .iter()
        .map(|(path, count, last)| (normalize(path), *count, *last))
        .collect();

    let candidates: Vec<Vec<String>> = apps
        .iter()
        .map(|(entry, dir)| satellite_candidates(entry, dir.as_deref(), roots, exists))
        .collect();
    let mut satellite_claims: HashMap<&str, usize> = HashMap::new();
    for path in candidates.iter().flatten() {
        *satellite_claims.entry(path).or_default() += 1;
    }

    apps.iter()
        .zip(&names)
        .zip(&candidates)
        .zip(display)
        .map(|((((entry, dir), names), satellites), shown)| {
            let in_dir = |path: &String| dir.as_deref().is_some_and(|dir| under(path, dir));
            let unique: Vec<&String> = names
                .iter()
                .filter(|name| name_claims.get(name.as_str()) == Some(&1))
                .collect();

            let assisted: Vec<&(String, u32, Option<i64>)> = user_assist
                .iter()
                .filter(|(path, ..)| in_dir(path))
                .collect();
            let use_count = (!assisted.is_empty())
                .then(|| assisted.iter().map(|(_, count, _)| u64::from(*count)).sum());
            let last_used = assisted
                .iter()
                .filter_map(|(_, _, last)| *last)
                .chain(
                    evidence
                        .prefetch
                        .iter()
                        .filter(|(exe, _)| unique.contains(&exe))
                        .map(|(_, time)| *time),
                )
                .max();

            let link_named = |link: &String| {
                *link == entry.name.to_lowercase()
                    || unique
                        .iter()
                        .any(|name| name.strip_suffix(".exe") == Some(link.as_str()))
            };

            let registry_keys = crate::leftover_keys::candidates(
                &[entry.name.as_str(), file_name(&shown)],
                entry.publisher.as_deref(),
            );
            AppInfo {
                id: entry.key.clone(),
                name: entry.name.trim().to_owned(),
                version: entry.version.clone(),
                path: PathBuf::from(shown),
                bytes: Bytes::ZERO,
                last_used,
                status: AppStatus::Unknown,
                use_count,
                signature: Signature::Unknown,
                running: running.iter().any(in_dir),
                autostart: autostart.iter().any(in_dir)
                    || evidence.startup_links.iter().any(link_named),
                default_handler: handlers.iter().any(in_dir),
                uninstaller: uninstall_command(entry),
                registry_keys,
                system: dir
                    .as_deref()
                    .is_some_and(|dir| !roots.windows.is_empty() && under(dir, &roots.windows)),
                satellites: satellites
                    .iter()
                    .filter(|path| satellite_claims.get(path.as_str()) == Some(&1))
                    .map(|path| Satellite {
                        path: PathBuf::from(path),
                        bytes: Bytes::ZERO,
                        // `<root>\<publisher>\<name>`, not a bare `<root>\<name>`.
                        strong: roots.data.iter().any(|root| {
                            path.strip_prefix(root.as_str())
                                .is_some_and(|rest| rest.trim_start_matches('\\').contains('\\'))
                        }),
                    })
                    .collect(),
            }
        })
        .collect()
}

/// `<data root>\<name>` and `<data root>\<publisher>\<name>` for the install
/// directory's name and the display name — never `<data root>\<publisher>`
/// alone, which a vendor's apps share. Paths overlapping the install
/// directory are left out so nothing counts twice.
fn satellite_candidates(
    entry: &UninstallEntry,
    dir: Option<&str>,
    roots: &Roots,
    exists: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    if dir.is_some_and(|dir| !roots.windows.is_empty() && under(dir, &roots.windows)) {
        return Vec::new();
    }
    let clean = |name: &str| {
        let name = name.trim().to_lowercase();
        (!name.is_empty() && !name.contains(['\\', '/', ':'])).then_some(name)
    };
    let mut names: Vec<String> = Vec::new();
    names.extend(dir.map(file_name).and_then(clean));
    names.extend(clean(&entry.name));
    let publisher = entry.publisher.as_deref().and_then(clean);

    let mut out: Vec<String> = Vec::new();
    for root in &roots.data {
        for name in &names {
            out.push(format!("{root}\\{name}"));
            if let Some(publisher) = &publisher {
                if publisher != name {
                    out.push(format!("{root}\\{publisher}\\{name}"));
                }
            }
        }
    }
    let mut seen = HashSet::new();
    out.retain(|path| {
        let overlaps = dir.is_some_and(|dir| under(path, dir) || under(dir, path));
        !overlaps && seen.insert(path.clone()) && exists(path)
    });
    out
}

#[cfg(windows)]
pub(crate) mod sys {
    //! The Windows reads behind [`inventory`]. Each returns empty on failure.

    use std::fs;
    use std::path::Path;

    use storagelens_core::{
        AppInfo, Bytes, CancelToken, Entry, EntrySink, Result, ScanOptions, StorageScanner,
    };
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS, HANDLE,
    };
    use windows::Win32::System::ProcessStatus::EnumProcesses;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegEnumKeyExW, RegEnumValueW, RegGetValueW, RegOpenKeyExW,
        RegSetValueExW, HKEY, HKEY_CLASSES_ROOT, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ,
        KEY_SET_VALUE, REG_BINARY, REG_OPTION_NON_VOLATILE, REG_ROUTINE_FLAGS, RRF_RT_REG_DWORD,
        RRF_RT_REG_SZ,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, TerminateProcess, PROCESS_ACCESS_RIGHTS,
        PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    };
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    use super::{
        assemble, decode_task_file, exe_from_command, expand_env, expand_known_folder, normalize,
        prefetch_exe, rot13, split_command, task_commands, under, user_assist_record, Evidence,
        Roots, UninstallEntry,
    };
    use crate::walk::WinScanner;

    const UNINSTALL: [(HKEY, &str); 3] = [
        (
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
        (
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
        (
            HKEY_CURRENT_USER,
            r"Software\Microsoft\Windows\CurrentVersion\Uninstall",
        ),
    ];
    const RUN: [(HKEY, &str); 3] = [
        (
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Run",
        ),
        (
            HKEY_LOCAL_MACHINE,
            r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Run",
        ),
        (
            HKEY_CURRENT_USER,
            r"Software\Microsoft\Windows\CurrentVersion\Run",
        ),
    ];

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub(crate) fn env(name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|value| !value.is_empty())
    }

    /// An opened registry key, closed on drop.
    pub(crate) struct Key(HKEY);

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: `self.0` was opened by `RegOpenKeyExW` and is closed once.
            let _ = unsafe { RegCloseKey(self.0) };
        }
    }

    impl Key {
        pub(crate) fn open(parent: HKEY, path: &str) -> Option<Self> {
            let path = wide(path);
            let mut key = HKEY::default();
            // SAFETY: `path` is NUL-terminated and `key` is writable.
            let status =
                unsafe { RegOpenKeyExW(parent, PCWSTR(path.as_ptr()), None, KEY_READ, &mut key) };
            (status == ERROR_SUCCESS).then_some(Self(key))
        }

        /// Open `path` for writing values, creating it if missing.
        pub(crate) fn create(parent: HKEY, path: &str) -> Option<Self> {
            let path = wide(path);
            let mut key = HKEY::default();
            // SAFETY: `path` is NUL-terminated and `key` is writable.
            let status = unsafe {
                RegCreateKeyExW(
                    parent,
                    PCWSTR(path.as_ptr()),
                    None,
                    PCWSTR::null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_SET_VALUE,
                    None,
                    &mut key,
                    None,
                )
            };
            (status == ERROR_SUCCESS).then_some(Self(key))
        }

        /// Write a `REG_BINARY` value.
        pub(crate) fn set_binary(&self, name: &str, data: &[u8]) -> bool {
            let name = wide(name);
            // SAFETY: the key is open for writing; `name` is NUL-terminated.
            let status = unsafe {
                RegSetValueExW(self.0, PCWSTR(name.as_ptr()), None, REG_BINARY, Some(data))
            };
            status == ERROR_SUCCESS
        }

        fn subkeys(&self) -> Vec<String> {
            let mut out = Vec::new();
            let mut name = [0u16; 256];
            for index in 0u32.. {
                let mut length = name.len() as u32;
                // SAFETY: the key is open; `name` is writable for `length` units.
                let status = unsafe {
                    RegEnumKeyExW(
                        self.0,
                        index,
                        Some(PWSTR(name.as_mut_ptr())),
                        &mut length,
                        None,
                        None,
                        None,
                        None,
                    )
                };
                if status == ERROR_MORE_DATA {
                    continue;
                }
                if status != ERROR_SUCCESS {
                    break;
                }
                out.push(String::from_utf16_lossy(&name[..length as usize]));
            }
            out
        }

        /// `(name, data)` of every value. Values over 8 KiB are skipped.
        pub(crate) fn values(&self) -> Vec<(String, Vec<u8>)> {
            let mut out = Vec::new();
            let mut name = vec![0u16; 2048];
            let mut data = vec![0u8; 8192];
            for index in 0u32.. {
                let mut name_len = name.len() as u32;
                let mut data_len = data.len() as u32;
                // SAFETY: the key is open; `name` and `data` are writable for the
                // lengths passed.
                let status = unsafe {
                    RegEnumValueW(
                        self.0,
                        index,
                        Some(PWSTR(name.as_mut_ptr())),
                        &mut name_len,
                        None,
                        None,
                        Some(data.as_mut_ptr()),
                        Some(&mut data_len),
                    )
                };
                if status == ERROR_NO_MORE_ITEMS {
                    break;
                }
                if status != ERROR_SUCCESS {
                    continue;
                }
                let data_len = (data_len as usize).min(data.len());
                out.push((
                    String::from_utf16_lossy(&name[..name_len as usize]),
                    data[..data_len].to_vec(),
                ));
            }
            out
        }

        /// A string value (`REG_EXPAND_SZ` expanded). `""` is the default value.
        pub(crate) fn string(&self, value: &str) -> Option<String> {
            let value = wide(value);
            let mut bytes = 0u32;
            // SAFETY: size query: no buffer, `bytes` receives the size.
            let status = unsafe {
                RegGetValueW(
                    self.0,
                    PCWSTR::null(),
                    PCWSTR(value.as_ptr()),
                    RRF_RT_REG_SZ,
                    None,
                    None,
                    Some(&mut bytes),
                )
            };
            if status != ERROR_SUCCESS || bytes == 0 {
                return None;
            }
            // Expansion can grow the value between the two calls; leave room.
            let mut buffer = vec![0u16; bytes as usize / 2 + 260];
            let mut bytes = (buffer.len() * 2) as u32;
            // SAFETY: `buffer` is writable for `bytes` bytes; RRF_RT_REG_SZ
            // NUL-terminates.
            let status = unsafe {
                RegGetValueW(
                    self.0,
                    PCWSTR::null(),
                    PCWSTR(value.as_ptr()),
                    RRF_RT_REG_SZ,
                    None,
                    Some(buffer.as_mut_ptr().cast()),
                    Some(&mut bytes),
                )
            };
            if status != ERROR_SUCCESS {
                return None;
            }
            let units = (bytes as usize / 2).min(buffer.len());
            let end = buffer[..units]
                .iter()
                .position(|&u| u == 0)
                .unwrap_or(units);
            let text = String::from_utf16_lossy(&buffer[..end]);
            (!text.trim().is_empty()).then_some(text)
        }

        fn dword(&self, value: &str) -> Option<u32> {
            let value = wide(value);
            let mut data = 0u32;
            let mut bytes = 4u32;
            // SAFETY: `data` is writable for 4 bytes, the size passed.
            let status = unsafe {
                RegGetValueW(
                    self.0,
                    PCWSTR::null(),
                    PCWSTR(value.as_ptr()),
                    REG_ROUTINE_FLAGS(RRF_RT_REG_DWORD.0),
                    None,
                    Some((&mut data as *mut u32).cast()),
                    Some(&mut bytes),
                )
            };
            (status == ERROR_SUCCESS).then_some(data)
        }
    }

    fn uninstall_entries() -> Vec<UninstallEntry> {
        let mut out = Vec::new();
        for (hive, path) in UNINSTALL {
            let Some(root) = Key::open(hive, path) else {
                continue;
            };
            for name in root.subkeys() {
                let Some(key) = Key::open(root.0, &name) else {
                    continue;
                };
                out.push(UninstallEntry {
                    name: key.string("DisplayName").unwrap_or_default(),
                    version: key.string("DisplayVersion"),
                    publisher: key.string("Publisher"),
                    location: key.string("InstallLocation"),
                    icon: key.string("DisplayIcon"),
                    system_component: key.dword("SystemComponent") == Some(1),
                    has_parent: key.string("ParentKeyName").is_some(),
                    uninstall: key.string("UninstallString"),
                    windows_installer: key.dword("WindowsInstaller") == Some(1),
                    key: name,
                });
            }
        }
        out
    }

    fn user_assist() -> Vec<(String, u32, Option<i64>)> {
        let Some(root) = Key::open(
            HKEY_CURRENT_USER,
            r"Software\Microsoft\Windows\CurrentVersion\Explorer\UserAssist",
        ) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for guid in root.subkeys() {
            let Some(count) = Key::open(root.0, &format!(r"{guid}\Count")) else {
                continue;
            };
            for (name, data) in count.values() {
                let path = rot13(&name);
                if !path.to_lowercase().ends_with(".exe") {
                    continue;
                }
                let (Some(path), Some((runs, last))) =
                    (expand_known_folder(&path, &env), user_assist_record(&data))
                else {
                    continue;
                };
                out.push((path, runs, last));
            }
        }
        out
    }

    /// Unreadable without admin rights; then simply empty.
    fn prefetch(windows: &str) -> Vec<(String, i64)> {
        let Ok(entries) = fs::read_dir(Path::new(windows).join("Prefetch")) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let exe = prefetch_exe(&entry.file_name().to_string_lossy())?;
                let modified = entry.metadata().ok()?.modified().ok()?;
                let secs = modified
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()?
                    .as_secs();
                Some((exe, i64::try_from(secs).ok()?))
            })
            .collect()
    }

    fn running() -> Vec<String> {
        processes(PROCESS_QUERY_LIMITED_INFORMATION, |_, _| ())
            .into_iter()
            .map(|(_, exe)| exe)
            .collect()
    }

    /// The uninstall key `key` still exists in any hive (#120): `false` once
    /// the app's uninstaller has finished and removed its entry.
    pub fn is_registered(key: &str) -> bool {
        if let Some(package) = key.strip_prefix(crate::store_apps::PREFIX) {
            return crate::store_apps::is_installed(package);
        }
        UNINSTALL
            .iter()
            .any(|(hive, path)| Key::open(*hive, &format!(r"{path}\{key}")).is_some())
    }

    /// Start an app's uninstaller (#115) the way "Add or remove apps" does.
    /// Through the shell, so an uninstaller that needs administrator rights
    /// gets its UAC prompt. Returns once it has started: many uninstallers
    /// hand off to a copy of themselves, so waiting would not mean "done".
    pub fn run_uninstaller(command: &str) -> std::result::Result<(), String> {
        // A Store app (#122): removed by Windows itself, the way Settings does.
        if let Some(package) = command.strip_prefix(crate::store_apps::PREFIX) {
            return crate::store_apps::remove(package);
        }
        let (program, arguments) =
            split_command(command).ok_or_else(|| "the uninstall command is empty".to_owned())?;
        let program = wide(&program);
        let arguments = wide(&arguments);
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOASYNC,
            lpFile: PCWSTR(program.as_ptr()),
            lpParameters: PCWSTR(arguments.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        // SAFETY: `info` is fully initialised and its strings outlive the call.
        unsafe { ShellExecuteExW(&mut info) }
            .map_err(|error| format!("could not start the uninstaller: {error}"))
    }

    /// Force quit every process whose executable is inside `dir` (#113), like
    /// Task Manager's End task: `TerminateProcess`, so unsaved work is lost.
    /// Returns how many were stopped and how many refused (elevated or
    /// protected processes).
    pub fn force_quit(dir: &Path) -> (usize, usize) {
        let dir = normalize(&dir.display().to_string());
        let own = std::process::id();
        let (mut stopped, mut failed) = (0, 0);
        processes(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
            |pid, exe_and_handle| {
                let (exe, handle) = exe_and_handle;
                if pid == own || !under(&normalize(exe), &dir) {
                    return;
                }
                // SAFETY: `handle` is open with PROCESS_TERMINATE.
                if unsafe { TerminateProcess(handle, 1) }.is_ok() {
                    stopped += 1;
                } else {
                    failed += 1;
                }
            },
        );
        (stopped, failed)
    }

    /// Pid and image path of every process that opens with `access`; `visit`
    /// sees each one while its handle is still open.
    fn processes(
        access: PROCESS_ACCESS_RIGHTS,
        mut visit: impl FnMut(u32, (&str, HANDLE)),
    ) -> Vec<(u32, String)> {
        let mut pids = vec![0u32; 8192];
        let mut needed = 0u32;
        // SAFETY: `pids` is writable for the byte count passed.
        let listed = unsafe {
            EnumProcesses(
                pids.as_mut_ptr(),
                (pids.len() * std::mem::size_of::<u32>()) as u32,
                &mut needed,
            )
        };
        if listed.is_err() {
            return Vec::new();
        }
        pids.truncate(needed as usize / std::mem::size_of::<u32>());

        let mut buffer = vec![0u16; 32_768];
        pids.into_iter()
            .filter_map(|pid| {
                // SAFETY: plain call; failure (protected or exited process) is
                // an `Err` and skipped.
                let handle = unsafe { OpenProcess(access, false, pid) }.ok()?;
                let mut size = buffer.len() as u32;
                // SAFETY: `handle` is open; `buffer` is writable for `size` units.
                let named = unsafe {
                    QueryFullProcessImageNameW(
                        handle,
                        PROCESS_NAME_WIN32,
                        PWSTR(buffer.as_mut_ptr()),
                        &mut size,
                    )
                };
                let exe = named
                    .ok()
                    .map(|()| String::from_utf16_lossy(&buffer[..size as usize]));
                if let Some(exe) = &exe {
                    visit(pid, (exe, handle));
                }
                // SAFETY: `handle` came from `OpenProcess` and is closed once.
                let _ = unsafe { CloseHandle(handle) };
                Some((pid, exe?))
            })
            .collect()
    }

    fn run_keys() -> Vec<String> {
        RUN.iter()
            .filter_map(|(hive, path)| Key::open(*hive, path))
            .flat_map(|key| {
                key.values()
                    .into_iter()
                    .filter_map(|(name, _)| key.string(&name))
                    .collect::<Vec<_>>()
            })
            .filter_map(|command| exe_from_command(&expand_env(&command, &env)))
            .collect()
    }

    /// Scheduled-task commands, from the task XML files (depth-limited).
    fn task_exes(windows: &str) -> Vec<String> {
        fn visit(dir: &Path, depth: u32, out: &mut Vec<String>) {
            let Ok(entries) = fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                if kind.is_dir() && depth < 8 {
                    visit(&entry.path(), depth + 1, out);
                } else if kind.is_file() {
                    if let Ok(bytes) = fs::read(entry.path()) {
                        out.extend(task_commands(&decode_task_file(&bytes)));
                    }
                }
            }
        }
        let mut commands = Vec::new();
        visit(
            &Path::new(windows).join(r"System32\Tasks"),
            0,
            &mut commands,
        );
        commands
            .iter()
            .filter_map(|command| exe_from_command(&expand_env(command, &env)))
            .collect()
    }

    fn startup_links() -> Vec<String> {
        [
            env("APPDATA").map(|d| format!(r"{d}\Microsoft\Windows\Start Menu\Programs\Startup")),
            env("ProgramData")
                .map(|d| format!(r"{d}\Microsoft\Windows\Start Menu\Programs\StartUp")),
        ]
        .into_iter()
        .flatten()
        .filter_map(|dir| fs::read_dir(dir).ok())
        .flat_map(|entries| entries.flatten())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_lowercase();
            name.strip_suffix(".lnk").map(str::to_owned)
        })
        .collect()
    }

    fn handlers() -> Vec<String> {
        let mut prog_ids = Vec::new();
        if let Some(exts) = Key::open(
            HKEY_CURRENT_USER,
            r"Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts",
        ) {
            for ext in exts.subkeys() {
                if let Some(choice) = Key::open(exts.0, &format!(r"{ext}\UserChoice")) {
                    prog_ids.extend(choice.string("ProgId"));
                }
            }
        }
        for scheme in ["http", "https", "mailto"] {
            let path = format!(
                r"Software\Microsoft\Windows\Shell\Associations\UrlAssociations\{scheme}\UserChoice"
            );
            if let Some(choice) = Key::open(HKEY_CURRENT_USER, &path) {
                prog_ids.extend(choice.string("ProgId"));
            }
        }
        prog_ids.sort();
        prog_ids.dedup();
        prog_ids
            .iter()
            .filter_map(|id| Key::open(HKEY_CLASSES_ROOT, &format!(r"{id}\shell\open\command")))
            .filter_map(|key| key.string(""))
            .filter_map(|command| exe_from_command(&expand_env(&command, &env)))
            .collect()
    }

    fn roots() -> Roots {
        let windows = env("windir")
            .or_else(|| env("SystemRoot"))
            .unwrap_or_default();
        let data: Vec<String> = ["LOCALAPPDATA", "APPDATA", "ProgramData"]
            .into_iter()
            .filter_map(env)
            .map(|dir| normalize(&dir))
            .collect();
        let mut too_broad: Vec<String> = [
            "ProgramFiles",
            "ProgramFiles(x86)",
            "ProgramW6432",
            "CommonProgramFiles",
            "CommonProgramFiles(x86)",
            "USERPROFILE",
            "PUBLIC",
            "windir",
        ]
        .into_iter()
        .filter_map(env)
        .map(|dir| normalize(&dir))
        .collect();
        too_broad.extend(data.iter().cloned());
        if let Some(local) = env("LOCALAPPDATA") {
            too_broad.push(normalize(&format!(r"{local}\Programs")));
        }
        Roots {
            windows: normalize(&windows),
            data,
            too_broad,
        }
    }

    /// Allocated bytes below `path`, from one scanner pass. Unreadable → zero.
    fn measure(path: &Path) -> Bytes {
        struct Discard;
        impl EntrySink for Discard {
            fn accept(&mut self, _: Entry) -> Result<()> {
                Ok(())
            }
            fn flush(&mut self) -> Result<()> {
                Ok(())
            }
        }
        WinScanner::new(ScanOptions::default())
            .scan(
                &[path.to_path_buf()],
                &mut Discard,
                CancelToken::new(),
                &|_| {},
            )
            .map(|result| result.bytes_alloc)
            .unwrap_or(Bytes::ZERO)
    }

    /// Every installed app, with evidence and measured footprint.
    pub fn inventory() -> Vec<AppInfo> {
        let roots = roots();
        let windows = env("windir").unwrap_or_else(|| r"C:\Windows".into());
        let mut autostart = run_keys();
        autostart.extend(task_exes(&windows));
        let evidence = Evidence {
            user_assist: user_assist(),
            prefetch: prefetch(&windows),
            running: running(),
            autostart,
            startup_links: startup_links(),
            handlers: handlers(),
        };

        let is_dir = |path: &str| Path::new(path).is_dir();
        let exe_names = |dir: &str| {
            fs::read_dir(dir)
                .map(|entries| {
                    entries
                        .flatten()
                        .map(|entry| entry.file_name().to_string_lossy().to_lowercase())
                        .filter(|name| name.ends_with(".exe"))
                        .collect()
                })
                .unwrap_or_default()
        };
        let exists = |path: &str| fs::symlink_metadata(path).is_ok();

        let mut apps = assemble(
            &uninstall_entries(),
            &evidence,
            &roots,
            &is_dir,
            &exe_names,
            &exists,
        );
        // Store / MSIX apps are not in the Uninstall keys (#122).
        apps.extend(crate::store_apps::store_apps(&evidence.running));
        for app in &mut apps {
            if !app.path.as_os_str().is_empty() {
                app.bytes = measure(&app.path);
            }
            for satellite in &mut app.satellites {
                satellite.bytes = measure(&satellite.path);
            }
        }
        apps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_compare_case_insensitively_by_component() {
        assert_eq!(
            normalize(r#""C:\Program Files\Foo\""#),
            r"c:\program files\foo"
        );
        assert_eq!(normalize(r"C:\"), r"c:\");
        assert!(under(
            r"c:\program files\foo\bin\a.exe",
            r"c:\program files\foo"
        ));
        assert!(under(r"c:\program files\foo", r"c:\program files\foo"));
        assert!(!under(
            r"c:\program files\foobar\a.exe",
            r"c:\program files\foo"
        ));
        assert!(under(r"c:\x", r"c:\"));
    }

    #[test]
    fn command_lines_yield_their_executable() {
        assert_eq!(
            exe_from_command(r#""C:\Program Files\Foo\foo.exe" --tray"#).as_deref(),
            Some(r"C:\Program Files\Foo\foo.exe")
        );
        assert_eq!(
            exe_from_command(r"C:\Program Files\Foo\foo.EXE /background").as_deref(),
            Some(r"C:\Program Files\Foo\foo.EXE")
        );
        assert_eq!(
            exe_from_command("rundll32 x.dll").as_deref(),
            Some("rundll32")
        );
        assert_eq!(exe_from_command(r#""""#), None);
        assert_eq!(exe_from_command("  "), None);
    }

    #[test]
    fn icons_lose_quotes_and_index() {
        assert_eq!(icon_path(r#""C:\a\b.exe",0"#), r"C:\a\b.exe");
        assert_eq!(icon_path(r"C:\a\b.exe,-101"), r"C:\a\b.exe");
        assert_eq!(icon_path(r"C:\a,b\c.ico"), r"C:\a,b\c.ico");
    }

    #[test]
    fn env_expansion_leaves_unknown_names() {
        let lookup =
            |name: &str| (name == "LOCALAPPDATA").then(|| r"C:\Users\u\AppData\Local".into());
        assert_eq!(
            expand_env(r"%LOCALAPPDATA%\Foo\foo.exe", &lookup),
            r"C:\Users\u\AppData\Local\Foo\foo.exe"
        );
        assert_eq!(expand_env("%NOPE%\\x 100%", &lookup), "%NOPE%\\x 100%");
    }

    #[test]
    fn user_assist_decoding() {
        assert_eq!(
            rot13("P:\\Cebtenz Svyrf\\Sbb\\sbb.rkr"),
            "C:\\Program Files\\Foo\\foo.exe"
        );
        let mut data = vec![0u8; 72];
        data[4..8].copy_from_slice(&7u32.to_le_bytes());
        // 2020-01-01T00:00:00Z as FILETIME.
        data[60..68].copy_from_slice(&132_223_104_000_000_000u64.to_le_bytes());
        assert_eq!(user_assist_record(&data), Some((7, Some(1_577_836_800))));
        assert_eq!(user_assist_record(&[0u8; 72]), Some((0, None)));
        assert_eq!(
            user_assist_record(&[0u8; 16]),
            None,
            "short data is not an error"
        );

        let lookup = |name: &str| match name {
            "ProgramW6432" => Some(r"C:\Program Files".into()),
            "windir" => Some(r"C:\Windows".into()),
            _ => None,
        };
        assert_eq!(
            expand_known_folder(
                r"{6d809377-6af0-444b-8957-a3773f02200e}\Foo\foo.exe",
                &lookup
            )
            .as_deref(),
            Some(r"C:\Program Files\Foo\foo.exe")
        );
        assert_eq!(
            expand_known_folder(r"{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\cmd.exe", &lookup)
                .as_deref(),
            Some(r"C:\Windows\System32\cmd.exe")
        );
        assert_eq!(expand_known_folder(r"{00000000-0000}\x.exe", &lookup), None);
    }

    #[test]
    fn prefetch_names() {
        assert_eq!(
            prefetch_exe("CHROME.EXE-1A2B3C4D.pf").as_deref(),
            Some("chrome.exe")
        );
        assert_eq!(
            prefetch_exe("MY-APP.EXE-0000ABCD.pf").as_deref(),
            Some("my-app.exe")
        );
        assert_eq!(prefetch_exe("Layout.ini"), None);
        assert_eq!(prefetch_exe("X.EXE-NOTHEX!!.pf"), None);
    }

    #[test]
    fn task_xml_commands() {
        let xml = "<Exec><Command>&quot;%LOCALAPPDATA%\\Foo\\up.exe&quot;</Command>\
                   <Arguments>/c</Arguments></Exec><Exec><Command>C:\\b.exe</Command></Exec>";
        assert_eq!(
            task_commands(xml),
            ["\"%LOCALAPPDATA%\\Foo\\up.exe\"", "C:\\b.exe"]
        );
        let mut utf16 = vec![0xFF, 0xFE];
        utf16.extend(
            "<Command>a</Command>"
                .encode_utf16()
                .flat_map(u16::to_le_bytes),
        );
        assert_eq!(task_commands(&decode_task_file(&utf16)), ["a"]);
        assert!(task_commands("<Command>unterminated").is_empty());
    }

    fn roots() -> Roots {
        Roots {
            windows: r"c:\windows".into(),
            data: vec![
                r"c:\users\u\appdata\local".into(),
                r"c:\users\u\appdata\roaming".into(),
                r"c:\programdata".into(),
            ],
            too_broad: vec![
                r"c:\program files".into(),
                r"c:\users\u\appdata\local".into(),
            ],
        }
    }

    fn entry(key: &str, name: &str, location: &str) -> UninstallEntry {
        UninstallEntry {
            key: key.into(),
            name: name.into(),
            location: (!location.is_empty()).then(|| location.into()),
            ..UninstallEntry::default()
        }
    }

    #[test]
    fn install_dir_rejects_broad_locations_and_falls_back_to_icon() {
        let is_dir = |_: &str| true;
        let roots = roots();
        let mut broad = entry("k", "Foo", r"C:\Program Files\");
        assert_eq!(install_dir(&broad, &roots, &is_dir), None);
        broad.icon = Some(r#""C:\Program Files\Foo\foo.exe",0"#.into());
        assert_eq!(
            install_dir(&broad, &roots, &is_dir),
            Some((
                r"c:\program files\foo".into(),
                r"C:\Program Files\Foo".into()
            ))
        );
        assert_eq!(install_dir(&entry("k", "R", r"C:\"), &roots, &is_dir), None);
        let missing = |_: &str| false;
        assert_eq!(
            install_dir(&entry("k", "F", r"C:\Foo"), &roots, &missing),
            None
        );
    }

    #[test]
    fn assemble_matches_evidence_conservatively() {
        let mut foo = entry("FooKey", "Foo", r"C:\Program Files\Foo");
        foo.publisher = Some("Acme".into());
        foo.version = Some("2.0".into());
        let bar = entry("BarKey", "Bar", r"C:\Program Files\Bar");
        let hidden = UninstallEntry {
            system_component: true,
            ..entry("Hidden", "Runtime", r"C:\Program Files\Runtime")
        };
        let update = UninstallEntry {
            has_parent: true,
            ..entry("KB1", "Update for Foo", r"C:\Program Files\Foo")
        };
        // Same install as Foo from the 32-bit key: one app.
        let dup = entry("FooKey32", "Foo", r"c:\program files\foo\");
        let sys = entry("Sys", "Inbox", r"C:\Windows\Inbox");
        let bare = entry("Bare", "No Location", "");

        let evidence = Evidence {
            user_assist: vec![(r"C:\Program Files\Foo\foo.exe".into(), 12, Some(1_000))],
            prefetch: vec![
                ("foo.exe".into(), 2_000),
                ("update.exe".into(), 9_999),
                ("bar.exe".into(), 500),
            ],
            running: vec![r"C:\PROGRAM FILES\BAR\bar.exe".into()],
            autostart: vec![r"C:\Program Files\Foo\tray.exe".into()],
            startup_links: vec!["bar".into()],
            handlers: vec![r"c:\program files\foo\foo.exe".into()],
        };
        let exe_names = |dir: &str| match dir {
            r"c:\program files\foo" => vec!["foo.exe".into(), "update.exe".into()],
            r"c:\program files\bar" => vec!["bar.exe".into(), "update.exe".into()],
            _ => Vec::new(),
        };
        let existing = [
            r"c:\users\u\appdata\local\foo",
            r"c:\users\u\appdata\roaming\acme\foo",
            r"c:\programdata\bar",
            r"c:\users\u\appdata\local\inbox",
        ];
        let exists = |path: &str| existing.contains(&path);

        let apps = assemble(
            &[foo, bar, hidden, update, dup, sys, bare],
            &evidence,
            &roots(),
            &|_| true,
            &exe_names,
            &exists,
        );
        let ids: Vec<&str> = apps.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["FooKey", "BarKey", "Sys", "Bare"]);

        let foo = &apps[0];
        assert_eq!(
            foo.path,
            PathBuf::from(r"C:\Program Files\Foo"),
            "display keeps case"
        );
        assert_eq!(foo.use_count, Some(12));
        assert_eq!(
            foo.last_used,
            Some(2_000),
            "newest of UserAssist and Prefetch"
        );
        assert!(foo.autostart && foo.default_handler && !foo.running && !foo.system);
        let satellites: Vec<_> = foo.satellites.iter().map(|s| s.path.clone()).collect();
        assert_eq!(
            satellites,
            [
                PathBuf::from(r"c:\users\u\appdata\local\foo"),
                PathBuf::from(r"c:\users\u\appdata\roaming\acme\foo"),
            ]
        );
        let strong: Vec<bool> = foo.satellites.iter().map(|s| s.strong).collect();
        assert_eq!(strong, [false, true], "only publisher\\app is strong");

        let bar = &apps[1];
        assert_eq!(bar.last_used, Some(500), "shared update.exe is ignored");
        assert_eq!(bar.use_count, None);
        assert!(bar.running && bar.autostart && !bar.default_handler);

        let sys = &apps[2];
        assert!(sys.system && sys.satellites.is_empty());

        let bare = &apps[3];
        assert_eq!(bare.path, PathBuf::new());
        assert_eq!(bare.last_used, None, "no evidence → unknown, never unused");
        assert!(apps.iter().all(|a| a.status == AppStatus::Unknown));
    }

    #[test]
    fn shared_satellites_go_to_neither_app() {
        let a = entry("A", "Studio", r"C:\Program Files\A\Studio");
        let b = entry("B", "Studio", r"C:\Program Files\B\Studio");
        let exists = |path: &str| path == r"c:\users\u\appdata\roaming\studio";
        let apps = assemble(
            &[a, b],
            &Evidence::default(),
            &roots(),
            &|_| true,
            &|_| Vec::new(),
            &exists,
        );
        assert!(apps.iter().all(|app| app.satellites.is_empty()));
    }

    #[test]
    fn satellites_never_overlap_the_install_dir() {
        // A per-user install lives in %LOCALAPPDATA%\Discord itself.
        let discord = entry("Discord", "Discord", r"C:\Users\u\AppData\Local\Discord");
        let exists = |_: &str| true;
        let apps = assemble(
            &[discord],
            &Evidence::default(),
            &roots(),
            &|_| true,
            &|_| Vec::new(),
            &exists,
        );
        let paths: Vec<_> = apps[0].satellites.iter().map(|s| s.path.clone()).collect();
        assert_eq!(
            paths,
            [
                PathBuf::from(r"c:\users\u\appdata\roaming\discord"),
                PathBuf::from(r"c:\programdata\discord"),
            ]
        );
    }

    #[test]
    fn uninstall_commands_like_add_or_remove_apps() {
        let msi = UninstallEntry {
            key: "{1234-ABCD}".into(),
            uninstall: Some("MsiExec.exe /I{1234-ABCD}".into()),
            windows_installer: true,
            ..UninstallEntry::default()
        };
        assert_eq!(
            uninstall_command(&msi).as_deref(),
            Some("MsiExec.exe /X{1234-ABCD}")
        );
        let exe = UninstallEntry {
            key: "Foo".into(),
            uninstall: Some(r#" "C:\Program Files\Foo\uninst.exe" /S "#.into()),
            ..UninstallEntry::default()
        };
        assert_eq!(
            uninstall_command(&exe).as_deref(),
            Some(r#""C:\Program Files\Foo\uninst.exe" /S"#)
        );
        assert_eq!(uninstall_command(&UninstallEntry::default()), None);
    }

    #[test]
    fn commands_split_into_program_and_arguments() {
        assert_eq!(
            split_command(r#""C:\Program Files\Foo\uninst.exe" /S --x"#),
            Some((r"C:\Program Files\Foo\uninst.exe".into(), "/S --x".into()))
        );
        assert_eq!(
            split_command(r"C:\Foo\unins000.exe /SILENT"),
            Some((r"C:\Foo\unins000.exe".into(), "/SILENT".into()))
        );
        assert_eq!(
            split_command("MsiExec.exe /X{1}"),
            Some(("MsiExec.exe".into(), "/X{1}".into()))
        );
        assert_eq!(split_command("   "), None);
    }
}
