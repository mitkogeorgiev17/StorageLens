//! Scheduled tasks and services an uninstalled app leaves behind (#158).
//!
//! Uninstallers often leave an updater task or a service whose program sat
//! in the install folder. Found by their command pointing into the app's
//! install folder (strong) or by a task or service name containing the
//! app's name (weak). Microsoft's own tasks and anything run from the
//! Windows folder are never offered.
//!
//! Removed with one UAC prompt: each task is exported to XML and each
//! service's registry key to a `.reg` file first, then `schtasks /delete`
//! and `sc delete`.

use crate::apps::{exe_from_command, normalize, under};

/// A scheduled task (`\Vendor\Updater`) or a service (`VendorUpdate`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackgroundItem {
    /// `"task"` or `"service"`.
    pub kind: &'static str,
    /// Task path or service name.
    pub name: String,
    /// The command it runs, environment variables already expanded.
    pub command: String,
}

/// Whether `item` belongs to the app installed in `install_dir` and known by
/// `names` (lowercase): `Some(true)` when its program is in the install
/// folder, `Some(false)` when only its name matches, `None` otherwise.
pub fn belongs(
    item: &BackgroundItem,
    install_dir: &str,
    names: &[String],
    windows: &str,
) -> Option<bool> {
    let task_or_service = item.name.to_lowercase();
    if task_or_service.starts_with(r"\microsoft\") || !safe_name(&item.name) {
        return None;
    }
    let exe = exe_from_command(&item.command).map(|exe| normalize(&exe));
    if exe
        .as_deref()
        .is_some_and(|exe| !windows.is_empty() && under(exe, windows))
    {
        return None;
    }
    let dir = normalize(install_dir);
    if !dir.is_empty() && exe.as_deref().is_some_and(|exe| under(exe, &dir)) {
        return Some(true);
    }
    let leaf = task_or_service.rsplit('\\').next().unwrap_or_default();
    let squashed = leaf.replace([' ', '-', '_'], "");
    names
        .iter()
        .map(|name| name.replace([' ', '-', '_'], ""))
        .filter(|name| name.chars().count() >= 4)
        .any(|name| squashed.contains(&name))
        .then_some(false)
}

/// Safe to put between double quotes in a `cmd` script: no quotes, `%`,
/// `!`, `^`, `&`, `|`, `<` or `>`.
pub fn safe_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.chars().all(|c| {
            c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '\\' | '(' | ')' | '{' | '}')
        })
}

/// The elevated `cmd` script: back up, then delete, each item. A failed
/// backup skips that item's deletion.
pub fn removal_script(items: &[BackgroundItem], backup_dir: &str) -> String {
    let mut lines = vec![
        "@echo off".to_owned(),
        format!("mkdir \"{backup_dir}\" 2>nul"),
    ];
    for (index, item) in items.iter().enumerate() {
        let name = &item.name;
        match item.kind {
            "task" => lines.push(format!(
                "schtasks /query /tn \"{name}\" /xml > \"{backup_dir}\\task-{index}.xml\" && schtasks /delete /tn \"{name}\" /f"
            )),
            _ => lines.push(format!(
                "reg export \"HKLM\\SYSTEM\\CurrentControlSet\\Services\\{name}\" \"{backup_dir}\\service-{index}.reg\" /y && (sc stop \"{name}\" >nul 2>&1 & sc delete \"{name}\")"
            )),
        }
    }
    lines.join("\r\n")
}

#[cfg(windows)]
pub use sys::{find, remove};

#[cfg(windows)]
mod sys {
    use std::fs;
    use std::path::Path;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Registry::HKEY_LOCAL_MACHINE;
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

    use super::{belongs, removal_script, safe_name, BackgroundItem};
    use crate::apps::sys::{env, Key};
    use crate::apps::{decode_task_file, expand_env, normalize, task_commands};

    fn tasks(windows: &str) -> Vec<BackgroundItem> {
        fn visit(root: &Path, dir: &Path, depth: u32, out: &mut Vec<BackgroundItem>) {
            let Ok(entries) = fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                let path = entry.path();
                if kind.is_dir() && depth < 8 {
                    visit(root, &path, depth + 1, out);
                } else if kind.is_file() {
                    let Ok(bytes) = fs::read(&path) else {
                        continue;
                    };
                    let name = path
                        .strip_prefix(root)
                        .map(|rest| format!("\\{}", rest.display()))
                        .unwrap_or_default();
                    for command in task_commands(&decode_task_file(&bytes)) {
                        out.push(BackgroundItem {
                            kind: "task",
                            name: name.clone(),
                            command: expand_env(&command, &env),
                        });
                    }
                }
            }
        }
        let root = Path::new(windows).join(r"System32\Tasks");
        let mut out = Vec::new();
        visit(&root, &root, 0, &mut out);
        out
    }

    fn services() -> Vec<BackgroundItem> {
        const SERVICES: &str = r"SYSTEM\CurrentControlSet\Services";
        let Some(key) = Key::open(HKEY_LOCAL_MACHINE, SERVICES) else {
            return Vec::new();
        };
        key.subkeys()
            .into_iter()
            .filter_map(|name| {
                let service = Key::open(HKEY_LOCAL_MACHINE, &format!(r"{SERVICES}\{name}"))?;
                // Win32 services only (own or shared process), not drivers.
                let kind = service.dword("Type")?;
                (kind & 0x30 != 0).then_some(())?;
                let image = service.string("ImagePath")?;
                Some(BackgroundItem {
                    kind: "service",
                    name,
                    command: expand_env(&image, &env),
                })
            })
            .collect()
    }

    /// Tasks and services of the app installed in `install_dir`, known by
    /// `names`: `(name, kind, strong)`.
    pub fn find(install_dir: &str, names: &[String]) -> Vec<(String, &'static str, bool)> {
        let windows = env("windir").map(|dir| normalize(&dir)).unwrap_or_default();
        let mut out: Vec<(String, &'static str, bool)> = Vec::new();
        for item in tasks(&windows).into_iter().chain(services()) {
            if let Some(strong) = belongs(&item, install_dir, names, &windows) {
                if !out
                    .iter()
                    .any(|(name, kind, _)| *name == item.name && *kind == item.kind)
                {
                    out.push((item.name, item.kind, strong));
                }
            }
        }
        out
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(Some(0)).collect()
    }

    /// Back up and delete `items` with one UAC prompt; waits until done.
    pub fn remove(items: &[(String, &'static str)], backup_dir: &Path) -> Result<(), String> {
        if let Some((bad, _)) = items.iter().find(|(name, _)| !safe_name(name)) {
            return Err(format!("{bad} cannot be removed safely"));
        }
        fs::create_dir_all(backup_dir).map_err(|error| error.to_string())?;
        let items: Vec<BackgroundItem> = items
            .iter()
            .map(|(name, kind)| BackgroundItem {
                kind,
                name: name.clone(),
                command: String::new(),
            })
            .collect();
        let script_path = backup_dir.join("remove.cmd");
        fs::write(
            &script_path,
            removal_script(&items, &backup_dir.display().to_string()),
        )
        .map_err(|error| error.to_string())?;
        let verb = wide("runas");
        let program = wide("cmd.exe");
        let arguments = wide(&format!("/c \"{}\"", script_path.display()));
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(program.as_ptr()),
            lpParameters: PCWSTR(arguments.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        // SAFETY: `info` is fully initialised and its strings outlive the call.
        unsafe { ShellExecuteExW(&mut info) }
            .map_err(|error| format!("Windows did not allow it: {error}"))?;
        let mut code = 1u32;
        // SAFETY: SEE_MASK_NOCLOSEPROCESS gave us the process handle, which we
        // close after waiting on it.
        unsafe {
            WaitForSingleObject(info.hProcess, INFINITE);
            let _ = GetExitCodeProcess(info.hProcess, &mut code);
            let _ = CloseHandle(info.hProcess);
        }
        if code == 0 {
            Ok(())
        } else {
            Err(format!(
                "some items could not be removed (exit code {code})"
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(kind: &'static str, name: &str, command: &str) -> BackgroundItem {
        BackgroundItem {
            kind,
            name: name.into(),
            command: command.into(),
        }
    }

    #[test]
    fn program_in_the_install_folder_is_strong_a_name_is_weak() {
        let names = vec!["acme sync".to_owned()];
        let dir = r"C:\Program Files\Acme Sync";
        let win = r"c:\windows";
        assert_eq!(
            belongs(
                &item(
                    "task",
                    r"\Acme\Updater",
                    r#""C:\Program Files\Acme Sync\up.exe" /c"#
                ),
                dir,
                &names,
                win
            ),
            Some(true)
        );
        assert_eq!(
            belongs(
                &item("service", "AcmeSyncHelper", r"C:\ProgramData\x\helper.exe"),
                dir,
                &names,
                win
            ),
            Some(false)
        );
        assert_eq!(
            belongs(
                &item("service", "Spooler", r"C:\Windows\System32\spoolsv.exe"),
                dir,
                &names,
                win
            ),
            None
        );
        assert_eq!(
            belongs(
                &item(
                    "task",
                    r"\Microsoft\Windows\AcmeSync",
                    r"C:\Program Files\Acme Sync\x.exe"
                ),
                dir,
                &names,
                win
            ),
            None,
            "never Microsoft's tasks"
        );
        assert_eq!(
            belongs(
                &item("task", r"\Other", r"C:\Other\o.exe"),
                dir,
                &names,
                win
            ),
            None
        );
    }

    #[test]
    fn the_script_backs_up_before_deleting_and_quotes_safely() {
        assert!(safe_name(r"\Acme\Updater Task"));
        assert!(!safe_name("bad\" & del"));
        let script = removal_script(
            &[
                item("task", r"\Acme\Updater", ""),
                item("service", "AcmeSvc", ""),
            ],
            r"C:\b",
        );
        assert!(script.contains(
            r#"schtasks /query /tn "\Acme\Updater" /xml > "C:\b\task-0.xml" && schtasks /delete"#
        ));
        assert!(script.contains(r#"reg export "HKLM\SYSTEM\CurrentControlSet\Services\AcmeSvc" "C:\b\service-1.reg" /y && (sc stop"#));
    }
}
