//! Microsoft Store / MSIX apps (#122).
//!
//! They are not in the `Uninstall` registry keys, so the classic inventory
//! misses them. Listed through WinRT's `PackageManager.FindPackagesForUser`
//! (what Settings → Apps uses), from Windows PowerShell 5.1, which ships with
//! every supported Windows and resolves display names from package resources.
//! Removed with `Remove-AppxPackage`, as Settings does: per user, no
//! elevation, and Windows deletes the package's own data with it.
//!
//! Skipped: frameworks, resource packages and packages signed `System` (the
//! in-box apps Windows itself keeps).

use std::path::PathBuf;

use serde::Deserialize;
use storagelens_core::{AppInfo, AppStatus, Bytes, Satellite, Signature};

#[cfg(windows)]
pub use sys::{is_installed, remove, store_apps};

/// App ids and uninstall commands of Store apps start with this.
pub const PREFIX: &str = "msix:";

/// One package as the PowerShell probe prints it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Package {
    pub full_name: String,
    pub family: String,
    pub name: Option<String>,
    pub version: Option<String>,
    pub location: Option<String>,
    pub framework: bool,
    pub resource: bool,
    pub kind: Option<String>,
}

/// The probe's JSON: an array, or a bare object when there is one package.
pub fn parse_packages(json: &str) -> Vec<Package> {
    let json = json.trim();
    if json.is_empty() {
        return Vec::new();
    }
    serde_json::from_str::<Vec<Package>>(json)
        .or_else(|_| serde_json::from_str::<Package>(json).map(|one| vec![one]))
        .unwrap_or_default()
}

/// A package full name is `Name_Version_Arch_ResourceId_PublisherId`: only
/// these characters, so it is safe inside a single-quoted PowerShell string.
pub fn safe_full_name(name: &str) -> bool {
    !name.is_empty()
        && name.contains('_')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '~'))
}

/// The packages worth listing as apps, as [`AppInfo`] (sizes still zero).
/// `local_app_data` is `%LOCALAPPDATA%`, for each package's data folder;
/// `running(dir)` says whether a process runs from inside `dir`.
pub fn to_apps(
    packages: &[Package],
    local_app_data: Option<&str>,
    running: &dyn Fn(&str) -> bool,
    exists: &dyn Fn(&str) -> bool,
) -> Vec<AppInfo> {
    packages
        .iter()
        .filter(|package| {
            !package.framework
                && !package.resource
                && package.kind.as_deref() != Some("System")
                && safe_full_name(&package.full_name)
        })
        .filter_map(|package| {
            let name = package
                .name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty() && !name.starts_with("ms-resource:"))?;
            let location = package.location.clone().unwrap_or_default();
            let data = local_app_data
                .filter(|_| !package.family.is_empty())
                .map(|root| format!(r"{root}\Packages\{}", package.family))
                .filter(|path| exists(path));
            Some(AppInfo {
                id: format!("{PREFIX}{}", package.full_name),
                name: name.to_owned(),
                version: package.version.clone(),
                running: !location.is_empty() && running(&location),
                path: PathBuf::from(&location),
                bytes: Bytes::ZERO,
                last_used: None,
                status: AppStatus::Unknown,
                use_count: None,
                signature: Signature::Valid,
                autostart: false,
                default_handler: false,
                system: false,
                satellites: data
                    .into_iter()
                    .map(|path| Satellite {
                        path: PathBuf::from(path),
                        bytes: Bytes::ZERO,
                        strong: true,
                    })
                    .collect(),
                uninstaller: Some(format!("{PREFIX}{}", package.full_name)),
                registry_keys: Vec::new(),
            })
        })
        .collect()
}

/// Lists every package of the current user as JSON.
pub const LIST_SCRIPT: &str = r#"$ErrorActionPreference = 'SilentlyContinue'
[void][Windows.Management.Deployment.PackageManager, Windows.Management.Deployment, ContentType = WindowsRuntime]
$pm = New-Object Windows.Management.Deployment.PackageManager
$rows = @($pm.FindPackagesForUser('') | ForEach-Object {
  $v = $_.Id.Version
  [pscustomobject]@{
    fullName = $_.Id.FullName
    family = $_.Id.FamilyName
    name = $_.DisplayName
    version = "$($v.Major).$($v.Minor).$($v.Build).$($v.Revision)"
    location = $_.InstalledLocation.Path
    framework = [bool]$_.IsFramework
    resource = [bool]$_.IsResourcePackage
    kind = [string]$_.SignatureKind
  }
})
ConvertTo-Json -InputObject $rows -Compress"#;

/// `Remove-AppxPackage` for one package; exit 0 when removed.
pub fn remove_script(full_name: &str) -> String {
    format!(
        "try {{ Remove-AppxPackage -Package '{full_name}' -ErrorAction Stop; exit 0 }} \
         catch {{ [Console]::Error.WriteLine($_.Exception.Message); exit 1 }}"
    )
}

/// Exit 0 while the package is still installed.
pub fn installed_script(full_name: &str) -> String {
    let name = full_name.split('_').next().unwrap_or(full_name);
    format!(
        "if (Get-AppxPackage -Name '{name}' | Where-Object {{ $_.PackageFullName -eq '{full_name}' }}) \
         {{ exit 0 }} else {{ exit 1 }}"
    )
}

#[cfg(windows)]
mod sys {
    use std::os::windows::process::CommandExt;
    use std::path::Path;
    use std::process::{Command, Output};

    use storagelens_core::AppInfo;

    use super::{
        installed_script, parse_packages, remove_script, safe_full_name, to_apps, LIST_SCRIPT,
    };
    use crate::apps::{normalize, under};

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn powershell(script: &str) -> std::io::Result<Output> {
        Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
    }

    /// The current user's Store apps. Empty when PowerShell is unavailable.
    pub fn store_apps(running: &[String]) -> Vec<AppInfo> {
        let Ok(output) = powershell(LIST_SCRIPT) else {
            return Vec::new();
        };
        let packages = parse_packages(&String::from_utf8_lossy(&output.stdout));
        let running: Vec<String> = running.iter().map(|exe| normalize(exe)).collect();
        let local = std::env::var("LOCALAPPDATA").ok();
        to_apps(
            &packages,
            local.as_deref(),
            &|dir| {
                let dir = normalize(dir);
                running.iter().any(|exe| under(exe, &dir))
            },
            &|path| Path::new(path).exists(),
        )
    }

    /// Uninstall the package `full_name` for this user. Returns when done.
    pub fn remove(full_name: &str) -> Result<(), String> {
        if !safe_full_name(full_name) {
            return Err(format!("{full_name} is not a package name"));
        }
        let output = powershell(&remove_script(full_name))
            .map_err(|error| format!("could not run PowerShell: {error}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "Windows could not remove the app: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }

    /// The package is still installed for this user. `true` when unsure, so
    /// a failed check never reports an uninstall as finished.
    pub fn is_installed(full_name: &str) -> bool {
        if !safe_full_name(full_name) {
            return true;
        }
        powershell(&installed_script(full_name))
            .map(|output| output.status.success())
            .unwrap_or(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = "Contoso.Notes_1.2.3.0_x64__8wekyb3d8bbwe";

    fn package(name: &str, kind: &str) -> Package {
        Package {
            full_name: FULL.into(),
            family: "Contoso.Notes_8wekyb3d8bbwe".into(),
            name: Some(name.into()),
            version: Some("1.2.3.0".into()),
            location: Some(
                r"C:\Program Files\WindowsApps\Contoso.Notes_1.2.3.0_x64__8wekyb3d8bbwe".into(),
            ),
            kind: Some(kind.into()),
            ..Package::default()
        }
    }

    #[test]
    fn parses_an_array_or_a_single_object() {
        let one = r#"{"fullName":"A_1_x64__p","family":"A_p","name":"A","framework":false}"#;
        assert_eq!(parse_packages(one).len(), 1);
        let many = format!("[{one},{one}]");
        assert_eq!(parse_packages(&many).len(), 2);
        assert!(parse_packages("").is_empty());
        assert!(parse_packages("not json").is_empty());
    }

    #[test]
    fn only_removable_user_apps_become_rows() {
        let framework = Package {
            framework: true,
            ..package("VCLibs", "Store")
        };
        let packages = [
            package("Notes", "Store"),
            package("Calculator", "System"),
            package("ms-resource:AppName", "Store"),
            framework,
            Package {
                full_name: "bad'; rm".into(),
                ..package("Evil", "Store")
            },
        ];
        let apps = to_apps(
            &packages,
            Some(r"C:\Users\u\AppData\Local"),
            &|dir| dir.contains("Contoso"),
            &|_| true,
        );
        assert_eq!(apps.len(), 1);
        let notes = &apps[0];
        assert_eq!(notes.id, format!("msix:{FULL}"));
        assert_eq!(notes.uninstaller.as_deref(), Some(&*format!("msix:{FULL}")));
        assert!(notes.running);
        assert_eq!(
            notes.satellites[0].path,
            PathBuf::from(r"C:\Users\u\AppData\Local\Packages\Contoso.Notes_8wekyb3d8bbwe")
        );
    }

    #[test]
    fn scripts_embed_only_safe_names() {
        assert!(safe_full_name(FULL));
        assert!(!safe_full_name("a'b_c"));
        assert!(!safe_full_name("nounderscore"));
        assert!(remove_script(FULL).contains(&format!("-Package '{FULL}'")));
        assert!(installed_script(FULL).contains("-Name 'Contoso.Notes'"));
    }
}
