//! Location categories (#95, PLAN §9.5): where a file lives decides its
//! category before its extension does.
//!
//! On a real full-disk scan, half the bytes had no telling extension: app
//! caches, app data, files inside `.app` bundles, the OS itself, and build
//! output. Their extensions (none, `.o`, `.rlib`, `.dat`, …) say nothing, but
//! their location says everything. So a directory can carry a category, and
//! every file below it takes that category, whatever its extension.
//!
//! The rules are pure functions of path components, identical on both
//! platforms where the layout is shared (AGENTS.md invariant 6), and tested on
//! both everywhere: Windows paths are passed as components, never parsed.
//!
//! # How a directory is classified
//!
//! 1. Below an **Apps**, **System**, **Developer** or **Caches** directory,
//!    everything inherits it.
//! 2. A directory **name** can say it on its own, at any depth: a cache folder
//!    (`.cache`, `Caches`, `GPUCache`, …), a build or toolchain folder
//!    (`node_modules`, `.rustup`, `DerivedData`, `target/debug`, …), or an app
//!    bundle (`*.app`). Inside App data, only cache names refine it.
//! 3. Below **App data**, everything else stays App data.
//! 4. Otherwise the **absolute location**: `~/Library/…`, `/System`, `/usr`,
//!    `C:\Windows`, `Program Files`, `AppData`, …
//! 5. Nothing matched: no location category, the extension decides.

use crate::Category;

/// Cache folder names, matched case-insensitively at any depth.
const CACHE_NAMES: &[&str] = &[
    ".cache",
    "cache",
    "caches",
    "code cache",
    "gpucache",
    "shadercache",
    "grshadercache",
    "cachestorage",
    "cache_data",
];

/// Build output, dependency and toolchain folders, at any depth.
const DEVELOPER_NAMES: &[&str] = &[
    "node_modules",
    ".rustup",
    ".cargo",
    ".npm",
    ".pnpm-store",
    ".yarn",
    ".gradle",
    ".m2",
    ".nuget",
    "deriveddata",
    "__pycache__",
    ".venv",
    ".tox",
    ".terraform",
    ".next",
];

/// Children of a `target` folder that make it Cargo's build output. A bare
/// `target` is too common a name to claim on its own.
const CARGO_TARGET_CHILDREN: &[&str] = &["debug", "release", "doc", "tmp"];

/// `~/Library` children that hold the user's own files, not app data.
const LIBRARY_USER_CONTENT: &[&str] = &["mobile documents", "cloudstorage"];

/// Directories at the top of a macOS (or other Unix) volume that belong to
/// the OS.
const UNIX_SYSTEM_ROOTS: &[&str] = &[
    "system", "usr", "bin", "sbin", "private", "cores", "library", "dev", "etc", "var",
];

/// Where location rules apply. Built once per scan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Locations {
    /// The home folder as normal components (`["Users", "me"]`).
    home: Option<Vec<String>>,
    windows: bool,
}

impl Locations {
    /// For this platform, with the user's home folder (absolute), if known.
    pub fn new(home: Option<&std::path::Path>) -> Self {
        Self::with(
            home.map(|home| {
                components(home)
                    .iter()
                    .map(|part| (*part).to_owned())
                    .collect()
            }),
            cfg!(windows),
        )
    }

    /// For any platform: `home` as normal components, `windows` for its
    /// layout. Tests use this to check Windows rules on every OS.
    pub fn with(home: Option<Vec<String>>, windows: bool) -> Self {
        Self { home, windows }
    }

    /// The category everything under a directory takes, or `None` to let
    /// extensions decide. `parent` is the parent directory's own result;
    /// `parts` are the directory's normal path components from the volume
    /// root (no drive, no separators).
    pub fn for_dir(&self, parent: Option<Category>, parts: &[&str]) -> Option<Category> {
        if let Some(category) = self.exact(parts) {
            return Some(category);
        }
        if let Some(category) = parent {
            if is_terminal(category) {
                return Some(category);
            }
        }
        let name = parts.last().copied().unwrap_or_default();
        if is_any(name, CACHE_NAMES) {
            return Some(Category::Caches);
        }
        if parent == Some(Category::AppData) {
            return parent;
        }
        if is_any(name, DEVELOPER_NAMES) || is_cargo_output(parts) {
            return Some(Category::Developer);
        }
        if name.len() > 4
            && name
                .get(name.len() - 4..)
                .is_some_and(|tail| tail.eq_ignore_ascii_case(".app"))
        {
            return Some(Category::Apps);
        }
        if parent.is_some() {
            return parent;
        }
        self.absolute(parts)
    }

    /// Exact folders that refine the System (`/private`, `/Library`) or App
    /// data (`AppData\Local`) around them. Checked before inheritance; below
    /// them, everything inherits the refined category.
    fn exact(&self, parts: &[&str]) -> Option<Category> {
        if self.windows {
            let rest = self.home.as_deref().and_then(|home| strip(parts, home))?;
            return matches!(rest, [a, l, t] if is(a, "appdata") && is(l, "local") && is(t, "temp"))
                .then_some(Category::Caches);
        }
        match parts {
            [p, v, f] if is(p, "private") && is(v, "var") && is(f, "folders") => {
                Some(Category::Caches)
            }
            [l, d] if is(l, "library") && is(d, "developer") => Some(Category::Developer),
            [l, c] if is(l, "library") && is(c, "caches") => Some(Category::Caches),
            _ => None,
        }
    }

    /// Rule 4: a location fixed by the OS layout.
    fn absolute(&self, parts: &[&str]) -> Option<Category> {
        if let Some(rest) = self.home.as_deref().and_then(|home| strip(parts, home)) {
            return if self.windows {
                home_windows(rest)
            } else {
                home_unix(rest)
            };
        }
        if self.windows {
            return match parts {
                [top, ..] if is(top, "windows") || is(top, "programdata") => Some(Category::System),
                [top, ..] if is(top, "program files") || is(top, "program files (x86)") => {
                    Some(Category::Apps)
                }
                _ => None,
            };
        }
        match parts {
            [top, next, ..] if is(top, "opt") && is(next, "homebrew") => Some(Category::Developer),
            [top, ..] if is_any(top, UNIX_SYSTEM_ROOTS) => Some(Category::System),
            _ => None,
        }
    }
}

/// `~/…` on macOS (and other Unix homes).
fn home_unix(rest: &[&str]) -> Option<Category> {
    match rest {
        [library, child, ..] if is(library, "library") => {
            Some(if is(child, "caches") || is(child, "logs") {
                Category::Caches
            } else if is(child, "developer") {
                Category::Developer
            } else if is_any(child, LIBRARY_USER_CONTENT) {
                return None;
            } else {
                Category::AppData
            })
        }
        _ => None,
    }
}

/// `%USERPROFILE%\…` on Windows.
fn home_windows(rest: &[&str]) -> Option<Category> {
    match rest {
        [appdata, local, temp, ..]
            if is(appdata, "appdata") && is(local, "local") && is(temp, "temp") =>
        {
            Some(Category::Caches)
        }
        [appdata, _, ..] if is(appdata, "appdata") => Some(Category::AppData),
        _ => None,
    }
}

/// Categories nothing below can change.
fn is_terminal(category: Category) -> bool {
    matches!(
        category,
        Category::Apps | Category::System | Category::Developer | Category::Caches
    )
}

/// `…/target/debug` and friends.
fn is_cargo_output(parts: &[&str]) -> bool {
    matches!(parts, [.., target, child] if is(target, "target") && is_any(child, CARGO_TARGET_CHILDREN))
}

/// `parts` below `prefix`, compared case-insensitively.
fn strip<'a, 'b>(parts: &'a [&'b str], prefix: &[String]) -> Option<&'a [&'b str]> {
    if parts.len() < prefix.len() {
        return None;
    }
    let (head, rest) = parts.split_at(prefix.len());
    head.iter()
        .zip(prefix)
        .all(|(part, want)| is(part, want))
        .then_some(rest)
}

fn is(part: &str, name: &str) -> bool {
    part.eq_ignore_ascii_case(name)
}

fn is_any(part: &str, names: &[&str]) -> bool {
    names.iter().any(|name| is(part, name))
}

/// A path's normal components as text (lossy names are skipped: no rule
/// matches them).
pub fn components(path: &std::path::Path) -> Vec<&str> {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mac() -> Locations {
        Locations::with(Some(vec!["Users".into(), "me".into()]), false)
    }

    fn win() -> Locations {
        Locations::with(Some(vec!["Users".into(), "me".into()]), true)
    }

    /// Classify each directory on the way down, as the writer does.
    fn walk(locations: &Locations, path: &str, separator: char) -> Option<Category> {
        let parts: Vec<&str> = path
            .split(separator)
            .filter(|part| !part.is_empty() && !part.ends_with(':'))
            .collect();
        let mut category = None;
        for depth in 1..=parts.len() {
            category = locations.for_dir(category, &parts[..depth]);
        }
        category
    }

    #[test]
    fn macos_home_library() {
        let m = mac();
        assert_eq!(
            walk(&m, "/Users/me/Library/Caches/com.spotify.client/Data", '/'),
            Some(Category::Caches)
        );
        assert_eq!(
            walk(&m, "/Users/me/Library/Logs/x", '/'),
            Some(Category::Caches)
        );
        assert_eq!(
            walk(
                &m,
                "/Users/me/Library/Application Support/stremio-server",
                '/'
            ),
            Some(Category::AppData)
        );
        assert_eq!(
            walk(&m, "/Users/me/Library/Containers/com.apple.x/Data", '/'),
            Some(Category::AppData)
        );
        assert_eq!(
            walk(&m, "/Users/me/Library/Developer/Xcode", '/'),
            Some(Category::Developer)
        );
        assert_eq!(
            walk(&m, "/users/ME/library/caches", '/'),
            Some(Category::Caches),
            "case-insensitive"
        );
        assert_eq!(
            walk(
                &m,
                "/Users/me/Library/Mobile Documents/com~apple~CloudDocs",
                '/'
            ),
            None,
            "iCloud Drive is the user's"
        );
        assert_eq!(walk(&m, "/Users/me/Documents/Photos", '/'), None);
    }

    #[test]
    fn caches_inside_app_data_are_caches() {
        let m = mac();
        assert_eq!(
            walk(
                &m,
                "/Users/me/Library/Application Support/discord/Cache/Cache_Data",
                '/'
            ),
            Some(Category::Caches)
        );
        assert_eq!(
            walk(
                &m,
                "/Users/me/Library/Application Support/Code/GPUCache",
                '/'
            ),
            Some(Category::Caches)
        );
        assert_eq!(
            walk(&m, "/Users/me/Library/Application Support/Code/User", '/'),
            Some(Category::AppData)
        );
    }

    #[test]
    fn macos_system_and_apps() {
        let m = mac();
        assert_eq!(
            walk(&m, "/System/Library/PrivateFrameworks", '/'),
            Some(Category::System)
        );
        assert_eq!(walk(&m, "/private/var/vm", '/'), Some(Category::System));
        assert_eq!(
            walk(&m, "/private/var/folders/xy/T", '/'),
            Some(Category::Caches)
        );
        assert_eq!(walk(&m, "/usr/libexec", '/'), Some(Category::System));
        assert_eq!(
            walk(&m, "/Library/Developer/CommandLineTools", '/'),
            Some(Category::Developer)
        );
        assert_eq!(walk(&m, "/Library/Java", '/'), Some(Category::System));
        assert_eq!(
            walk(&m, "/opt/homebrew/Cellar", '/'),
            Some(Category::Developer)
        );
        assert_eq!(
            walk(
                &m,
                "/Applications/Microsoft Word.app/Contents/Resources",
                '/'
            ),
            Some(Category::Apps)
        );
        assert_eq!(walk(&m, "/Applications/Utilities", '/'), None);
        assert_eq!(
            walk(&m, "/Users/me/Desktop/Tool.APP/Contents", '/'),
            Some(Category::Apps)
        );
        assert_eq!(
            walk(&m, "/Volumes/Backup/System", '/'),
            None,
            "only the top of the volume"
        );
    }

    #[test]
    fn developer_folders_anywhere() {
        let m = mac();
        assert_eq!(
            walk(&m, "/Users/me/Desktop/StorageLens/target/release/deps", '/'),
            Some(Category::Developer)
        );
        assert_eq!(
            walk(
                &m,
                "/Users/me/Desktop/StorageLens/src-tauri/target/debug",
                '/'
            ),
            Some(Category::Developer)
        );
        assert_eq!(
            walk(&m, "/Users/me/Desktop/web/node_modules/react", '/'),
            Some(Category::Developer)
        );
        assert_eq!(
            walk(&m, "/Users/me/.rustup/toolchains", '/'),
            Some(Category::Developer)
        );
        assert_eq!(
            walk(&m, "/Users/me/.cache/huggingface", '/'),
            Some(Category::Caches)
        );
        assert_eq!(
            walk(&m, "/Users/me/Documents/target", '/'),
            None,
            "a bare target is not claimed"
        );
        assert_eq!(walk(&m, "/Users/me/Documents/target/plans", '/'), None);
    }

    #[test]
    fn windows_layout() {
        let w = win();
        assert_eq!(
            walk(&w, r"C:\Windows\System32", '\\'),
            Some(Category::System)
        );
        assert_eq!(
            walk(&w, r"C:\ProgramData\Microsoft", '\\'),
            Some(Category::System)
        );
        assert_eq!(
            walk(&w, r"C:\Program Files\Mozilla Firefox", '\\'),
            Some(Category::Apps)
        );
        assert_eq!(
            walk(&w, r"C:\Program Files (x86)\Steam", '\\'),
            Some(Category::Apps)
        );
        assert_eq!(
            walk(&w, r"C:\Users\me\AppData\Local\Temp\x", '\\'),
            Some(Category::Caches)
        );
        assert_eq!(
            walk(&w, r"C:\Users\me\AppData\Roaming\Code\User", '\\'),
            Some(Category::AppData)
        );
        assert_eq!(
            walk(
                &w,
                r"C:\Users\me\AppData\Local\Google\Chrome\User Data\Default\Cache",
                '\\'
            ),
            Some(Category::Caches)
        );
        assert_eq!(
            walk(&w, r"C:\Users\me\Documents\proj\node_modules", '\\'),
            Some(Category::Developer)
        );
        assert_eq!(walk(&w, r"C:\Users\me\Documents", '\\'), None);
        assert_eq!(walk(&w, r"D:\Games", '\\'), None);
        // macOS-only rules never fire on Windows.
        assert_eq!(walk(&w, r"C:\Library\x", '\\'), None);
    }

    #[test]
    fn without_a_home_only_system_rules_apply() {
        let none = Locations::with(None, false);
        assert_eq!(
            walk(&none, "/Users/me/Library/Caches", '/'),
            Some(Category::Caches),
            "by name"
        );
        assert_eq!(
            walk(&none, "/Users/me/Library/Application Support", '/'),
            None
        );
        assert_eq!(walk(&none, "/System", '/'), Some(Category::System));
    }
}
