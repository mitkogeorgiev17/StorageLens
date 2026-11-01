//! Installed-app inventory (#36): bundles, usage evidence, and footprint.
//!
//! Evidence comes from tools that ship with macOS rather than private frameworks:
//! `plutil` for `Info.plist` and launchd plists, `mdls` for Spotlight's
//! `kMDItemLastUsedDate` / `kMDItemUseCount`, `codesign --verify` for the
//! signature, and `proc_pidpath` for running processes. Each runs once per app
//! (or once per launchd plist), never per file.
//!
//! Footprints are measured with [`MacScanner`], one pass per bundle and per
//! satellite, so they agree with the treemap and with `du -skx`.
//!
//! Classification is not done here: every app leaves with
//! [`AppStatus::Unknown`] and `storagelens-analysis` decides.
//!
//! # Not covered
//!
//! - Login items in `backgrounditems.btm` (a keyed archive with no public
//!   reader). Apps registered only there are not marked `autostart`.
//! - Default handlers the user never changed: LaunchServices only records
//!   overrides, and the untouched defaults belong to `com.apple.*` apps anyway.

use std::collections::{HashMap, HashSet};
use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use rayon::prelude::*;
use serde_json::Value;
use storagelens_core::{
    AppInfo, AppStatus, Bytes, CancelToken, Entry, EntrySink, Result, Satellite, ScanOptions,
    Signature, StorageScanner,
};

use crate::walk::MacScanner;

/// Where the inventory looks. [`Sources::for_home`] is the real machine; tests
/// point every field at a temp dir.
#[derive(Debug, Clone)]
pub struct Sources {
    /// Directories holding `.app` bundles, searched one folder level deep.
    pub app_dirs: Vec<PathBuf>,
    /// The user's `~/Library`, for satellites and LaunchServices overrides.
    pub library: PathBuf,
    /// Directories of launchd job plists.
    pub launchd_dirs: Vec<PathBuf>,
    /// The user's per-app cache and temp folders under `/var/folders`
    /// (`DARWIN_USER_CACHE_DIR`, `DARWIN_USER_TEMP_DIR`).
    pub user_dirs: Vec<PathBuf>,
}

impl Sources {
    /// The standard locations for the user whose home is `home`.
    pub fn for_home(home: &Path) -> Self {
        let library = home.join("Library");
        Self {
            app_dirs: vec![
                "/Applications".into(),
                "/System/Applications".into(),
                home.join("Applications"),
            ],
            launchd_dirs: vec![
                library.join("LaunchAgents"),
                "/Library/LaunchAgents".into(),
                "/Library/LaunchDaemons".into(),
            ],
            library,
            user_dirs: [
                libc::_CS_DARWIN_USER_CACHE_DIR,
                libc::_CS_DARWIN_USER_TEMP_DIR,
            ]
            .into_iter()
            .filter_map(confstr_path)
            .collect(),
        }
    }
}

/// A `confstr` path value, such as the user's `/var/folders/…/C/`.
fn confstr_path(name: libc::c_int) -> Option<PathBuf> {
    let mut buf = vec![0u8; 1024];
    // SAFETY: `buf` is writable for its full length, which is passed along.
    let len = unsafe { libc::confstr(name, buf.as_mut_ptr().cast(), buf.len()) };
    (len > 1 && len <= buf.len()).then(|| {
        buf.truncate(len - 1);
        PathBuf::from(String::from_utf8_lossy(&buf).into_owned())
    })
}

/// Every app bundle under `sources`, with evidence and measured footprint.
pub fn inventory(sources: &Sources) -> Vec<AppInfo> {
    let bundles = find_bundles(&sources.app_dirs);
    let running: Vec<PathBuf> = processes().into_iter().map(|(_, exe)| exe).collect();
    let jobs = launchd_jobs(&sources.launchd_dirs);
    let handlers = default_handlers(&sources.library);

    // Four short-lived processes per app; parallel, because each one mostly waits.
    let mut apps: Vec<AppInfo> = bundles.par_iter().map(|path| describe(path)).collect();

    for app in &mut apps {
        let id = app.id.to_lowercase();
        app.running = running.iter().any(|exe| exe.starts_with(&app.path));
        app.autostart = jobs.iter().any(|job| job.references(&app.path, &id));
        app.default_handler = !id.is_empty() && handlers.contains(&id);
    }

    attach_satellites(&mut apps, &sources.library, &sources.user_dirs);

    // Sequential: each scan owns a thread pool already.
    for app in &mut apps {
        app.bytes = measure(&app.path);
        for satellite in &mut app.satellites {
            satellite.bytes = measure(&satellite.path);
        }
    }

    apps
}

/// `.app` directories directly in each root, or one plain folder below it
/// (`/Applications/Utilities`). Symlinks are skipped so nothing counts twice.
fn find_bundles(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for root in roots {
        for (path, is_dir) in list_dir(root) {
            if !is_dir {
                continue;
            }
            if is_bundle(&path) {
                found.push(path);
            } else {
                found.extend(
                    list_dir(&path)
                        .into_iter()
                        .filter(|(child, is_dir)| *is_dir && is_bundle(child))
                        .map(|(child, _)| child),
                );
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

/// One directory level: `(path, is_real_directory)`. Unreadable → empty.
fn list_dir(dir: &Path) -> Vec<(PathBuf, bool)> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| {
            let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
            (entry.path(), is_dir)
        })
        .collect()
}

fn is_bundle(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
}

/// Everything known about one bundle except running / autostart / handler.
fn describe(path: &Path) -> AppInfo {
    let info = path.join("Contents/Info.plist");
    let id = plist_string(&info, "CFBundleIdentifier").unwrap_or_default();
    let version = plist_string(&info, "CFBundleShortVersionString")
        .or_else(|| plist_string(&info, "CFBundleVersion"));
    let name = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (last_used, use_count) = spotlight_usage(path);

    AppInfo {
        system: is_system(path, &id),
        id,
        name,
        version,
        path: path.to_path_buf(),
        bytes: Bytes::ZERO,
        last_used,
        status: AppStatus::Unknown,
        use_count,
        signature: signature(path),
        running: false,
        autostart: false,
        default_handler: false,
        satellites: Vec::new(),
        uninstaller: find_uninstaller(path).map(|path| path.display().to_string()),
        registry_keys: Vec::new(),
    }
}

/// Ships with the OS: never a removal candidate.
pub fn is_system(path: &Path, bundle_id: &str) -> bool {
    path.starts_with("/System")
        || bundle_id
            .get(..10)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("com.apple."))
}

/// Run a tool; its stdout if it exited 0.
fn tool(program: &str, args: &[&OsStr]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

/// A top-level string from a plist, binary or XML.
fn plist_string(plist: &Path, key: &str) -> Option<String> {
    let value = tool(
        "/usr/bin/plutil",
        &[
            "-extract".as_ref(),
            key.as_ref(),
            "raw".as_ref(),
            "-o".as_ref(),
            "-".as_ref(),
            plist.as_os_str(),
        ],
    )?;
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// A whole plist as JSON. Fails on plists holding dates or data, which the
/// launchd and LaunchServices plists read here do not.
pub(crate) fn plist_json(plist: &Path) -> Option<Value> {
    let json = tool(
        "/usr/bin/plutil",
        &[
            "-convert".as_ref(),
            "json".as_ref(),
            "-o".as_ref(),
            "-".as_ref(),
            plist.as_os_str(),
        ],
    )?;
    serde_json::from_str(&json).ok()
}

/// `(kMDItemLastUsedDate as unix seconds, kMDItemUseCount)`. Either is `None`
/// when Spotlight has no value: indexing off, volume excluded, never launched.
fn spotlight_usage(app: &Path) -> (Option<i64>, Option<u64>) {
    let Some(out) = tool(
        "/usr/bin/mdls",
        &[
            "-name".as_ref(),
            "kMDItemLastUsedDate".as_ref(),
            "-name".as_ref(),
            "kMDItemUseCount".as_ref(),
            app.as_os_str(),
        ],
    ) else {
        return (None, None);
    };
    parse_mdls(&out)
}

/// Parse `mdls` output: `kMDItemX = value` lines, `(null)` when missing.
///
/// For a bundle Spotlight never saw launch, `mdls` still prints a
/// `kMDItemLastUsedDate` — the bundle's file dates, not usage — while the use
/// count stays `(null)`. A date without a count is therefore discarded:
/// treating an install date as a last use would make old installs "unused".
fn parse_mdls(out: &str) -> (Option<i64>, Option<u64>) {
    let mut last_used = None;
    let mut use_count = None;
    for line in out.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "kMDItemLastUsedDate" => last_used = parse_mdls_date(value.trim()),
            "kMDItemUseCount" => use_count = value.trim().parse().ok(),
            _ => {}
        }
    }
    (last_used.filter(|_| use_count.is_some()), use_count)
}

/// `2026-10-31 23:21:40 +0000` → unix seconds. `mdls` always prints UTC.
fn parse_mdls_date(value: &str) -> Option<i64> {
    let mut parts = value.split(' ');
    let (date, time, zone) = (parts.next()?, parts.next()?, parts.next()?);
    if zone != "+0000" {
        return None;
    }
    let num = |s: &str| s.parse::<i64>().ok();
    let mut ymd = date.split('-').map(num);
    let (y, m, d) = (ymd.next()??, ymd.next()??, ymd.next()??);
    let mut hms = time.split(':').map(num);
    let (h, min, s) = (hms.next()??, hms.next()??, hms.next()??);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d) * 86_400 + h * 3_600 + min * 60 + s)
}

/// Days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `codesign --verify` without `--deep`: checks the main executable and the
/// resource seal, in well under a second even for large bundles.
fn signature(app: &Path) -> Signature {
    let Ok(output) = Command::new("/usr/bin/codesign")
        .args(["--verify".as_ref(), "--no-strict".as_ref(), app.as_os_str()])
        .output()
    else {
        return Signature::Unknown;
    };
    if output.status.success() {
        Signature::Valid
    } else if String::from_utf8_lossy(&output.stderr).contains("not signed at all") {
        Signature::Unsigned
    } else {
        Signature::Invalid
    }
}

/// A vendor uninstaller shipped with `app` (#115): an `Uninstall….app` in the
/// app's own folder (`/Applications/Foo/Uninstall Foo.app`, never directly in
/// an Applications folder, where it would belong to no app in particular) or
/// in its `Contents/Resources`.
fn find_uninstaller(app: &Path) -> Option<PathBuf> {
    let folder = app
        .parent()
        .filter(|dir| dir.file_name().is_some_and(|name| name != "Applications"));
    folder
        .map(Path::to_path_buf)
        .into_iter()
        .chain([app.join("Contents/Resources")])
        .find_map(|dir| uninstaller_in(&dir, app))
}

fn uninstaller_in(dir: &Path, app: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = list_dir(dir)
        .into_iter()
        .filter(|(path, is_dir)| {
            *is_dir
                && path.as_path() != app
                && is_bundle(path)
                && path
                    .file_name()
                    .and_then(OsStr::to_str)
                    .is_some_and(|name| name.to_lowercase().contains("uninstall"))
        })
        .map(|(path, _)| path)
        .collect();
    found.sort();
    found.into_iter().next()
}

/// Open the vendor uninstaller at `path` (#115).
pub fn run_uninstaller(path: &str) -> std::result::Result<(), String> {
    let status = Command::new("/usr/bin/open")
        .arg(path)
        .status()
        .map_err(|error| format!("could not open the uninstaller: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("could not open the uninstaller ({status})"))
    }
}

/// Force quit every process whose executable is inside `app` (#113), like
/// Activity Monitor's Force Quit: `SIGKILL`, so unsaved work is lost. Returns
/// how many were stopped and how many refused (another user's, or protected).
pub fn force_quit(app: &Path) -> (usize, usize) {
    let own = i32::try_from(std::process::id()).unwrap_or(-1);
    let (mut stopped, mut failed) = (0, 0);
    for (pid, _) in processes()
        .into_iter()
        .filter(|(pid, exe)| *pid != own && exe.starts_with(app))
    {
        // SAFETY: plain syscall on a pid; failure is reported via the return value.
        if unsafe { libc::kill(pid, libc::SIGKILL) } == 0 {
            stopped += 1;
        } else {
            failed += 1;
        }
    }
    (stopped, failed)
}

/// Pid and executable path of every process we can see.
fn processes() -> Vec<(libc::pid_t, PathBuf)> {
    // SAFETY: a null buffer of size 0 asks only for the process count.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let Ok(count) = usize::try_from(count) else {
        return Vec::new();
    };
    // Headroom for processes started between the two calls.
    let mut pids: Vec<libc::pid_t> = vec![0; count + 64];
    let Ok(capacity) = libc::c_int::try_from(std::mem::size_of_val(pids.as_slice())) else {
        return Vec::new();
    };
    // SAFETY: `pids` is valid for writes of `capacity` bytes.
    let filled = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), capacity) };
    pids.truncate(usize::try_from(filled).unwrap_or(0));

    let mut buffer = [0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    pids.into_iter()
        .filter_map(|pid| {
            // SAFETY: `buffer` is valid for writes of its full length, which is
            // the size passed.
            let len =
                unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
            let len = usize::try_from(len).ok().filter(|&len| len > 0)?;
            Some((pid, PathBuf::from(OsStr::from_bytes(buffer.get(..len)?))))
        })
        .collect()
}

/// What a launchd job points at.
#[derive(Debug, Default)]
struct Job {
    label: String,
    program: Option<PathBuf>,
    bundle_ids: Vec<String>,
}

impl Job {
    fn from_plist(value: &Value) -> Self {
        let text = |v: &Value| v.as_str().map(str::to_lowercase);
        let program = value
            .get("Program")
            .or_else(|| value.get("ProgramArguments").and_then(|args| args.get(0)))
            .and_then(Value::as_str)
            .map(PathBuf::from);
        let bundle_ids = match value.get("AssociatedBundleIdentifiers") {
            Some(Value::Array(ids)) => ids.iter().filter_map(text).collect(),
            Some(id) => text(id).into_iter().collect(),
            None => Vec::new(),
        };
        Self {
            label: value.get("Label").and_then(text).unwrap_or_default(),
            program,
            bundle_ids,
        }
    }

    /// The job launches something inside `app`, or names its (lowercased) id.
    fn references(&self, app: &Path, id: &str) -> bool {
        self.program.as_deref().is_some_and(|p| p.starts_with(app))
            || (!id.is_empty()
                && (self.bundle_ids.iter().any(|b| b == id)
                    || self.label == id
                    || self
                        .label
                        .strip_prefix(id)
                        .is_some_and(|rest| rest.starts_with('.'))))
    }
}

fn launchd_jobs(dirs: &[PathBuf]) -> Vec<Job> {
    dirs.iter()
        .flat_map(|dir| list_dir(dir))
        .filter(|(path, is_dir)| !is_dir && path.extension().is_some_and(|ext| ext == "plist"))
        .collect::<Vec<_>>()
        .par_iter()
        .filter_map(|(path, _)| plist_json(path))
        .map(|value| Job::from_plist(&value))
        .collect()
}

/// Lowercased bundle ids that the user made the default for a file type, an
/// extension, or a web / mail scheme.
fn default_handlers(library: &Path) -> HashSet<String> {
    let plist =
        library.join("Preferences/com.apple.LaunchServices/com.apple.launchservices.secure.plist");
    plist_json(&plist)
        .map(|value| handlers_from(&value))
        .unwrap_or_default()
}

fn handlers_from(value: &Value) -> HashSet<String> {
    const ROLES: [&str; 4] = [
        "LSHandlerRoleAll",
        "LSHandlerRoleViewer",
        "LSHandlerRoleEditor",
        "LSHandlerRoleShell",
    ];
    // An app's own private scheme (`docker-desktop:`) is not a reason to keep it.
    const SCHEMES: [&str; 3] = ["http", "https", "mailto"];

    let Some(handlers) = value.get("LSHandlers").and_then(Value::as_array) else {
        return HashSet::new();
    };
    handlers
        .iter()
        .filter(|handler| {
            handler.get("LSHandlerContentType").is_some()
                || handler.get("LSHandlerContentTag").is_some()
                || handler
                    .get("LSHandlerURLScheme")
                    .and_then(Value::as_str)
                    .is_some_and(|scheme| SCHEMES.contains(&scheme.to_lowercase().as_str()))
        })
        .flat_map(|handler| ROLES.iter().filter_map(|role| handler.get(*role)))
        .filter_map(Value::as_str)
        .filter(|id| *id != "-")
        .map(str::to_lowercase)
        .collect()
}

/// What an app's leftovers can be named after.
struct Owner {
    id: String,
    /// File name, `CFBundleName`, `CFBundleExecutable`: weaker evidence.
    names: Vec<String>,
    /// Code-signing team, for its group containers.
    team: Option<String>,
}

fn owner(app: &AppInfo) -> Owner {
    let info = app.path.join("Contents/Info.plist");
    let mut names = vec![app.name.clone()];
    names.extend(plist_string(&info, "CFBundleName"));
    names.extend(plist_string(&info, "CFBundleExecutable"));
    // Too short or too generic to be one app's.
    names.retain(|name| {
        name.chars().count() >= 3 && !GENERIC_NAMES.contains(&name.to_lowercase().as_str())
    });
    names.sort_by_key(|name| name.to_lowercase());
    names.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    Owner {
        id: app.id.clone(),
        names,
        team: team_id(&app.path),
    }
}

/// Executable and bundle names many apps share; never evidence.
const GENERIC_NAMES: [&str; 6] = ["electron", "app", "launcher", "helper", "main", "applet"];

/// The code-signing team of `app`, from `codesign -dv`.
fn team_id(app: &Path) -> Option<String> {
    let output = Command::new("/usr/bin/codesign")
        .args(["-dv".as_ref(), app.as_os_str()])
        .output()
        .ok()?;
    String::from_utf8_lossy(&output.stderr)
        .lines()
        .find_map(|line| line.strip_prefix("TeamIdentifier="))
        .map(str::trim)
        .filter(|team| !team.is_empty() && *team != "not set")
        .map(str::to_owned)
}

/// Candidate satellites by bundle id, bundle names and signing team. A path
/// claimed by two installed apps is ambiguous and given to neither:
/// deleting it with one app would break the other.
fn attach_satellites(apps: &mut [AppInfo], library: &Path, user_dirs: &[PathBuf]) {
    let groups: Vec<PathBuf> = list_dir(&library.join("Group Containers"))
        .into_iter()
        .filter(|(_, is_dir)| *is_dir)
        .map(|(path, _)| path)
        .collect();

    let candidates: Vec<Vec<PathBuf>> = apps
        .par_iter()
        .map(|app| {
            if app.system || app.id.is_empty() {
                return Vec::new();
            }
            satellite_paths(library, user_dirs, &owner(app), &groups)
        })
        .collect();

    let key = |path: &Path| path.to_string_lossy().to_lowercase();
    let mut claims: HashMap<String, usize> = HashMap::new();
    for path in candidates.iter().flatten() {
        *claims.entry(key(path)).or_default() += 1;
    }

    for (app, paths) in apps.iter_mut().zip(candidates) {
        let id = app.id.to_lowercase();
        app.satellites = paths
            .into_iter()
            .filter(|path| claims.get(&key(path)) == Some(&1))
            .map(|path| Satellite {
                // Named after the bundle id, not just the app's name.
                strong: path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().to_lowercase().contains(&id)),
                path,
                bytes: Bytes::ZERO,
            })
            .collect();
    }
}

/// `name` is the bundle id `id` itself or one of its extensions or files:
/// `<id>`, `<id>.widgetextension`, `<id>.plist`, `<id>.<host-uuid>.plist`,
/// and WebKit's per-app folders `com.apple.WebKit.Networking+<id>`. Never a
/// longer id that merely starts the same (`<id>2`).
fn own_name(name: &str, id: &str) -> bool {
    let name = name.to_lowercase();
    let id = id.to_lowercase();
    let name = name
        .rsplit_once('+')
        .map_or(name.as_str(), |(_, tail)| tail);
    name == id
        || name
            .strip_prefix(&id)
            .is_some_and(|tail| tail.starts_with('.'))
}

/// Library folders where apps keep files named after their bundle id.
const ID_FOLDERS: [&str; 12] = [
    "Application Support",
    "Caches",
    "Preferences",
    "Preferences/ByHost",
    "Saved Application State",
    "HTTPStorages",
    "WebKit",
    "Cookies",
    "Logs",
    "Containers",
    "Application Scripts",
    "LaunchAgents",
];

/// Library folders where apps keep folders named after the app itself.
const NAME_FOLDERS: [&str; 3] = ["Application Support", "Caches", "Logs"];

/// Existing satellite paths for one app, deduplicated case-insensitively
/// (APFS is case-insensitive by default, so `Foo` and `foo` are one place).
fn satellite_paths(
    library: &Path,
    user_dirs: &[PathBuf],
    owner: &Owner,
    groups: &[PathBuf],
) -> Vec<PathBuf> {
    let id = owner.id.as_str();
    let named = |path: &Path, test: &dyn Fn(&str) -> bool| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(test)
    };
    let mut paths: Vec<PathBuf> = Vec::new();

    // `<id>`, `<id>.*` and `…+<id>` in every Library folder and the per-user
    // cache and temp folders. A container moves to the Trash only with Full
    // Disk Access; macOS refuses even an administrator without it.
    let id_dirs = ID_FOLDERS
        .iter()
        .map(|folder| library.join(folder))
        .chain(user_dirs.iter().cloned());
    for dir in id_dirs {
        paths.extend(
            list_dir(&dir)
                .into_iter()
                .map(|(path, _)| path)
                .filter(|path| named(path, &|name| own_name(name, id))),
        );
    }

    // Named after the app: `Application Support/Code`, `~/.vscode`, crash
    // reports `Code-2026-01-01-….ips`.
    let home = library.parent();
    let short = id.rsplit('.').next().unwrap_or_default();
    for name in &owner.names {
        for folder in NAME_FOLDERS {
            paths.push(library.join(folder).join(name));
        }
        if let Some(home) = home {
            let dot = name.to_lowercase().replace(' ', "");
            paths.push(home.join(format!(".{dot}")));
        }
        let prefix = format!("{}-", name.to_lowercase());
        let helper = format!("{} helper", name.to_lowercase());
        paths.extend(
            list_dir(&library.join("Logs/DiagnosticReports"))
                .into_iter()
                .map(|(path, _)| path)
                .filter(|path| {
                    named(path, &|file| {
                        let file = file.to_lowercase();
                        (file.starts_with(&prefix) || file.starts_with(&helper))
                            && [".ips", ".crash", ".diag", ".hang"]
                                .iter()
                                .any(|ext| file.ends_with(ext))
                    })
                }),
        );
    }
    if short.chars().count() >= 3 {
        if let Some(home) = home {
            paths.push(home.join(format!(".{}", short.to_lowercase())));
        }
    }

    // Group containers: `<TEAM>.<id>` and below, and every group of the
    // app's signing team (Office's `UBF8T346G9.Office`). Another installed
    // app of the same team claims them too, which keeps them.
    let id_lower = id.to_lowercase();
    let team = owner.team.as_deref().map(str::to_lowercase);
    paths.extend(
        groups
            .iter()
            .filter(|group| {
                let Some((prefix, rest)) = group
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(str::to_lowercase)
                    .and_then(|name| {
                        name.split_once('.')
                            .map(|(a, b)| (a.to_owned(), b.to_owned()))
                    })
                else {
                    return false;
                };
                team.as_deref() == Some(prefix.as_str())
                    || rest == id_lower
                    || rest
                        .strip_prefix(&id_lower)
                        .is_some_and(|tail| tail.starts_with('.'))
            })
            .cloned(),
    );

    let mut seen = HashSet::new();
    paths.retain(|path| {
        fs::symlink_metadata(path).is_ok() && seen.insert(path.to_string_lossy().to_lowercase())
    });
    paths
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

    MacScanner::new(ScanOptions::default())
        .scan(
            &[path.to_path_buf()],
            &mut Discard,
            CancelToken::new(),
            &|_| {},
        )
        .map(|result| result.bytes_alloc)
        .unwrap_or(Bytes::ZERO)
}

/// Whether this process may modify apps (macOS App Management, #155):
/// opens a user-owned app's `Info.plist` for writing and closes it without
/// writing, which also makes macOS list StorageLens under App Management.
/// `None` when no user-owned app exists to tell.
pub fn app_management_access() -> Option<bool> {
    use std::os::unix::fs::MetadataExt;
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let plist = find_bundles(&[PathBuf::from("/Applications")])
        .into_iter()
        .filter(|app| fs::metadata(app).is_ok_and(|meta| meta.uid() == uid))
        .map(|app| app.join("Contents/Info.plist"))
        .find(|plist| plist.is_file())?;
    match fs::OpenOptions::new().write(true).open(&plist) {
        Ok(_) => Some(true),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Some(false),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mdls_dates_are_utc_seconds() {
        assert_eq!(parse_mdls_date("1970-01-01 00:00:00 +0000"), Some(0));
        assert_eq!(
            parse_mdls_date("2026-10-31 23:21:40 +0000"),
            Some(1_793_488_900)
        );
        assert_eq!(
            parse_mdls_date("2024-02-29 00:00:00 +0000"),
            Some(1_709_164_800)
        );
        assert_eq!(parse_mdls_date("(null)"), None);
        assert_eq!(parse_mdls_date("2026-13-01 00:00:00 +0000"), None);
    }

    #[test]
    fn mdls_null_is_missing() {
        let out = "kMDItemLastUsedDate = (null)\nkMDItemUseCount     = (null)\n";
        assert_eq!(parse_mdls(out), (None, None));
        // Unindexed: the "last used" date is really the file date.
        let out = "kMDItemLastUsedDate = 2019-12-31 22:00:00 +0000\nkMDItemUseCount     = (null)\n";
        assert_eq!(parse_mdls(out), (None, None));
        let out = "kMDItemLastUsedDate = 2026-10-01 15:00:41 +0000\nkMDItemUseCount     = 50\n";
        assert_eq!(parse_mdls(out), (Some(1_790_866_841), Some(50)));
    }

    #[test]
    fn system_apps_by_path_or_id() {
        assert!(is_system(Path::new("/System/Applications/Mail.app"), ""));
        assert!(is_system(
            Path::new("/Applications/Safari.app"),
            "com.apple.Safari"
        ));
        assert!(is_system(Path::new("/Applications/X.app"), "COM.APPLE.x"));
        assert!(!is_system(
            Path::new("/Applications/Firefox.app"),
            "org.mozilla.firefox"
        ));
        assert!(!is_system(
            Path::new("/Applications/A.app"),
            "com.applesauce.a"
        ));
    }

    #[test]
    fn launchd_jobs_match_path_label_or_association() {
        let app = Path::new("/Applications/Docker.app");
        let by_path = Job::from_plist(&json!({
            "Label": "x",
            "ProgramArguments": ["/Applications/Docker.app/Contents/MacOS/helper"],
        }));
        assert!(by_path.references(app, "com.docker.docker"));

        let by_label = Job::from_plist(&json!({"Label": "com.docker.docker.helper"}));
        assert!(by_label.references(app, "com.docker.docker"));
        let unrelated = Job::from_plist(&json!({"Label": "com.docker.dockerish"}));
        assert!(!unrelated.references(app, "com.docker.docker"));

        let by_assoc = Job::from_plist(&json!({
            "Label": "com.docker.vmnetd",
            "AssociatedBundleIdentifiers": ["com.docker.docker"],
        }));
        assert!(by_assoc.references(app, "com.docker.docker"));
        assert!(!by_assoc.references(app, ""));
    }

    #[test]
    fn handlers_skip_private_schemes() {
        let handlers = handlers_from(&json!({"LSHandlers": [
            {"LSHandlerRoleAll": "org.mozilla.firefox", "LSHandlerURLScheme": "http"},
            {"LSHandlerRoleViewer": "com.foo.Viewer", "LSHandlerContentType": "public.html"},
            {"LSHandlerRoleAll": "com.docker.docker", "LSHandlerURLScheme": "docker-desktop"},
            {"LSHandlerRoleAll": "-", "LSHandlerContentTag": "env"},
        ]}));
        let mut found: Vec<_> = handlers.into_iter().collect();
        found.sort();
        assert_eq!(found, ["com.foo.viewer", "org.mozilla.firefox"]);
    }

    #[test]
    fn vendor_uninstallers_beside_or_inside_the_app() {
        let root = tempfile::tempdir().expect("tempdir");
        let apps = root.path().join("Applications");
        let suite = apps.join("Foo");
        let foo = suite.join("Foo.app");
        fs::create_dir_all(foo.join("Contents/Resources")).expect("foo");
        fs::create_dir_all(suite.join("Uninstall Foo.app")).expect("uninstaller");
        assert_eq!(
            find_uninstaller(&foo),
            Some(suite.join("Uninstall Foo.app"))
        );

        // Directly in Applications: someone else's uninstaller, not this app's.
        let bar = apps.join("Bar.app");
        fs::create_dir_all(bar.join("Contents/Resources")).expect("bar");
        fs::create_dir_all(apps.join("Uninstall Baz.app")).expect("baz");
        assert_eq!(find_uninstaller(&bar), None);
        fs::create_dir_all(bar.join("Contents/Resources/BarUninstaller.app")).expect("inner");
        assert_eq!(
            find_uninstaller(&bar),
            Some(bar.join("Contents/Resources/BarUninstaller.app"))
        );
    }
}
