//! Path guards (#29, PLAN §15.4): the check every path passes before anything
//! moves it.
//!
//! A [`Guard`] is built for one cleanup, from the scanned root it may act in
//! and the user's home folder. [`Guard::check`] then refuses a path that
//!
//! - is relative, or has a `.` / `..` component (traversal);
//! - cannot be resolved, or is not valid UTF-8;
//! - after resolving every symlink and junction, is not strictly inside the
//!   root — so a link pointing out of the root is refused, and the root itself
//!   is never a target;
//! - is, is inside, or contains a system location: `/System`, `/usr`, `/bin`,
//!   `/sbin`, `/private/var/db`, `/private/var/vm`, … on macOS; `X:\Windows`,
//!   `X:\Program Files`, … on Windows;
//! - is, or contains, a protected folder of the home (keychains, mail, `.ssh`,
//!   the Photos and Music folders) or the home itself;
//! - names something PLAN §15.6 says never to touch, anywhere: `.ssh`, keychain
//!   files, a Photos or Music library, Time Machine backups, `pagefile.sys`, …;
//! - is inside an application or other bundle (`X.app/Contents/…`). The bundle
//!   itself may be removed (M5); its insides never are.
//!
//! A symlink that is itself the target is checked twice: as the link (its
//! folder resolved, its own name kept), which is what the Trash moves, and as
//! what it points to, unless it dangles.
//!
//! Resolving touches the filesystem and allocates. The rules themselves —
//! [`Guard::check_resolved`] on an already-resolved path — compare string
//! slices only: no allocation and no syscalls, so they can run on every item
//! of a large plan.
//!
//! Every comparison against a deny rule ignores ASCII case, on both
//! platforms: refusing too much is safe, refusing too little is not. Root
//! containment compares exactly on macOS (resolved paths carry the on-disk
//! case) and ignoring case on Windows.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use crate::Platform;

/// Why a path may not be touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Not an absolute path.
    NotAbsolute,
    /// Has a `.` or `..` component.
    Traversal,
    /// Not valid UTF-8, so the rules cannot be checked.
    NotUtf8,
    /// Resolving it failed (it is missing, unreadable, or a link loop).
    Unresolvable(io::ErrorKind),
    /// Resolves to somewhere outside the root this cleanup may act in.
    OutsideRoot,
    /// Is the root itself.
    IsRoot,
    /// Is, is inside, or contains this system location.
    System(&'static str),
    /// Is, or contains, a protected folder of the home, or the home itself.
    Protected,
    /// Is or is inside something never touched (PLAN §15.6), named here.
    NeverTouch(&'static str),
    /// Is inside an application or other bundle.
    BundleInternal,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotAbsolute => f.write_str("not an absolute path"),
            Self::Traversal => f.write_str("the path contains “.” or “..”"),
            Self::NotUtf8 => f.write_str("the path is not valid UTF-8"),
            Self::Unresolvable(kind) => write!(f, "the path could not be resolved ({kind})"),
            Self::OutsideRoot => f.write_str("it is outside the scanned folder"),
            Self::IsRoot => f.write_str("it is the scanned folder itself"),
            Self::System(location) => write!(f, "{location} is a system location"),
            Self::Protected => f.write_str("it is, or contains, a protected folder"),
            Self::NeverTouch(name) => write!(f, "{name} is never touched"),
            Self::BundleInternal => f.write_str("it is inside an application bundle"),
        }
    }
}

impl std::error::Error for Refusal {}

/// A system location, as components. `"*:"` matches any drive.
struct System {
    shown: &'static str,
    components: &'static [&'static str],
}

const MACOS_SYSTEM: &[System] = &[
    System {
        shown: "/System",
        components: &["System"],
    },
    System {
        shown: "/usr",
        components: &["usr"],
    },
    System {
        shown: "/bin",
        components: &["bin"],
    },
    System {
        shown: "/sbin",
        components: &["sbin"],
    },
    System {
        shown: "/private/etc",
        components: &["private", "etc"],
    },
    System {
        shown: "/private/var/db",
        components: &["private", "var", "db"],
    },
    System {
        shown: "/private/var/vm",
        components: &["private", "var", "vm"],
    },
    System {
        shown: "/cores",
        components: &["cores"],
    },
    System {
        shown: "/.DocumentRevisions-V100",
        components: &[".DocumentRevisions-V100"],
    },
    System {
        shown: "/.Spotlight-V100",
        components: &[".Spotlight-V100"],
    },
    System {
        shown: "/.fseventsd",
        components: &[".fseventsd"],
    },
];

const WINDOWS_SYSTEM: &[System] = &[
    System {
        shown: "C:\\Windows",
        components: &["*:", "Windows"],
    },
    System {
        shown: "C:\\Program Files",
        components: &["*:", "Program Files"],
    },
    System {
        shown: "C:\\Program Files (x86)",
        components: &["*:", "Program Files (x86)"],
    },
    System {
        shown: "C:\\Recovery",
        components: &["*:", "Recovery"],
    },
    System {
        shown: "C:\\System Volume Information",
        components: &["*:", "System Volume Information"],
    },
    System {
        shown: "C:\\$Recycle.Bin",
        components: &["*:", "$Recycle.Bin"],
    },
];

/// Folders of the home that are refused, and whose ancestors are refused.
/// `true`: everything inside is refused too. `false`: only the folder and its
/// ancestors (so `~/Pictures` cannot be emptied wholesale, but a file in it
/// can go).
const HOME_PROTECTED: &[(&str, bool)] = &[
    (".ssh", true),
    (".gnupg", true),
    ("Library/Keychains", true),
    ("Library/Mail", true),
    ("Pictures", false),
    ("Music", false),
];

/// Components never touched, wherever they appear (PLAN §15.6).
const NEVER_NAMES: &[&str] = &[
    ".ssh",
    ".gnupg",
    "Backups.backupdb",
    ".DocumentRevisions-V100",
    ".Spotlight-V100",
    ".fseventsd",
    "pagefile.sys",
    "hiberfil.sys",
    "swapfile.sys",
    "System Volume Information",
    "$Recycle.Bin",
];

/// Component endings never touched, wherever they appear.
const NEVER_SUFFIXES: &[&str] = &[
    ".photoslibrary",
    ".musiclibrary",
    ".keychain",
    ".keychain-db",
    ".backupbundle",
];

/// `Library/<name>` folders never touched, in any user's library.
const NEVER_IN_LIBRARY: &[&str] = &["Keychains", "Mail"];

/// Bundles: nothing below one of these may be touched.
const BUNDLE_SUFFIXES: &[&str] = &[
    ".app",
    ".appex",
    ".bundle",
    ".framework",
    ".kext",
    ".mdimporter",
    ".plugin",
    ".prefpane",
    ".qlgenerator",
    ".saver",
    ".systemextension",
    ".xpc",
];

struct Protected {
    path: String,
    inside: bool,
}

/// The checks for one cleanup: what roots it may act in, and what is protected.
pub struct Guard {
    platform: Platform,
    /// The roots a scan covered, in scan order. One for an ordinary scan, and
    /// several for a multi-root scan (#195). The first is the primary.
    roots: Vec<String>,
    protected: Vec<Protected>,
}

impl fmt::Debug for Guard {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Guard").field("roots", &self.roots).finish()
    }
}

fn unresolvable(error: io::Error) -> Refusal {
    Refusal::Unresolvable(error.kind())
}

/// `path` resolved, as a string.
fn resolve(path: &Path) -> Result<String, Refusal> {
    let resolved = fs::canonicalize(path).map_err(unresolvable)?;
    resolved
        .into_os_string()
        .into_string()
        .map_err(|_| Refusal::NotUtf8)
}

impl Guard {
    /// A guard for this platform, acting only strictly inside `allowed_root`.
    /// Both paths are resolved here; `home` adds its protected folders.
    pub fn new(allowed_root: &Path, home: Option<&Path>) -> Result<Self, Refusal> {
        let root = resolve(allowed_root)?;
        let home = home.map(resolve).transpose()?;
        Self::from_resolved(Platform::CURRENT, &root, home.as_deref())
    }

    /// [`new`](Self::new) for a scan of several roots (#195), each resolved
    /// here.
    pub fn new_roots(allowed_roots: &[PathBuf], home: Option<&Path>) -> Result<Self, Refusal> {
        let roots: Result<Vec<String>, Refusal> =
            allowed_roots.iter().map(|root| resolve(root)).collect();
        let roots = roots?;
        let home = home.map(resolve).transpose()?;
        let roots: Vec<&str> = roots.iter().map(String::as_str).collect();
        Self::from_roots(Platform::CURRENT, &roots, home.as_deref())
    }

    /// A guard from paths that are already resolved, for `platform`. Touches
    /// no filesystem, so either platform's rules can be checked anywhere.
    pub fn from_resolved(
        platform: Platform,
        allowed_root: &str,
        home: Option<&str>,
    ) -> Result<Self, Refusal> {
        Self::from_roots(platform, std::slice::from_ref(&allowed_root), home)
    }

    /// [`from_resolved`](Self::from_resolved) for a scan of several roots
    /// (#195). A path is allowed when it lies strictly inside any one of them.
    pub fn from_roots(
        platform: Platform,
        allowed_roots: &[&str],
        home: Option<&str>,
    ) -> Result<Self, Refusal> {
        let mut roots = Vec::with_capacity(allowed_roots.len());
        for root in allowed_roots {
            well_formed(platform, root)?;
            roots.push((*root).to_owned());
        }
        let mut protected = Vec::new();
        if let Some(home) = home {
            well_formed(platform, home)?;
            protected.push(Protected {
                path: home.to_owned(),
                inside: false,
            });
            for (relative, inside) in HOME_PROTECTED {
                protected.push(Protected {
                    path: format!("{home}/{relative}"),
                    inside: *inside,
                });
            }
        }
        Ok(Self {
            platform,
            roots,
            protected,
        })
    }

    /// The primary (first) root this guard acts in.
    pub fn root(&self) -> &str {
        self.roots.first().map_or("", String::as_str)
    }

    /// Resolve `path` and check it. Run immediately before any mutation.
    pub fn check(&self, path: &Path) -> Result<(), Refusal> {
        if !path.is_absolute() {
            return Err(Refusal::NotAbsolute);
        }
        if path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        {
            return Err(Refusal::Traversal);
        }
        let metadata = fs::symlink_metadata(path).map_err(unresolvable)?;
        if !metadata.file_type().is_symlink() {
            return self.check_resolved(&resolve(path)?);
        }
        // The link itself is what moves: its folder resolved, its name kept.
        let folder = path.parent().unwrap_or(path);
        let name = path.file_name().unwrap_or_default();
        let link = fs::canonicalize(folder).map_err(unresolvable)?.join(name);
        self.check_path(&link)?;
        // And what it points to must be safe too, unless it points nowhere.
        match fs::canonicalize(path) {
            Ok(target) => self.check_path(&target),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(unresolvable(error)),
        }
    }

    fn check_path(&self, path: &Path) -> Result<(), Refusal> {
        self.check_resolved(path.to_str().ok_or(Refusal::NotUtf8)?)
    }

    /// Check a path that is already resolved (no links, absolute). This is the
    /// fast path: it neither allocates nor touches the filesystem.
    pub fn check_resolved(&self, resolved: &str) -> Result<(), Refusal> {
        let platform = self.platform;
        well_formed(platform, resolved)?;
        let target = || components(platform, resolved);

        let exact = |a: &str, b: &str| {
            (platform == Platform::Windows && a.eq_ignore_ascii_case(b)) || a == b
        };
        // Inside any root passes; a target equal to one is the root itself.
        let mut inside = false;
        let mut is_root = false;
        for root in &self.roots {
            match relation(target(), components(platform, root), exact) {
                Relation::Inside => {
                    inside = true;
                    break;
                }
                Relation::Equal => is_root = true,
                Relation::Ancestor | Relation::Unrelated => {}
            }
        }
        if is_root {
            return Err(Refusal::IsRoot);
        }
        if !inside {
            return Err(Refusal::OutsideRoot);
        }

        let system = match platform {
            Platform::MacOs => MACOS_SYSTEM,
            Platform::Windows => WINDOWS_SYSTEM,
        };
        for location in system {
            if relation(target(), location.components.iter().copied(), matches_rule)
                != Relation::Unrelated
            {
                return Err(Refusal::System(location.shown));
            }
        }

        for folder in &self.protected {
            match relation(target(), components(platform, &folder.path), ignore_case) {
                Relation::Equal | Relation::Ancestor => return Err(Refusal::Protected),
                Relation::Inside if folder.inside => return Err(Refusal::Protected),
                Relation::Inside | Relation::Unrelated => {}
            }
        }

        let mut previous = "";
        let mut parts = target().peekable();
        while let Some(part) = parts.next() {
            if let Some(name) = never_touched(previous, part) {
                return Err(Refusal::NeverTouch(name));
            }
            if parts.peek().is_some()
                && BUNDLE_SUFFIXES.iter().any(|suffix| ends_with(part, suffix))
            {
                return Err(Refusal::BundleInternal);
            }
            previous = part;
        }
        Ok(())
    }
}

/// The rule a component breaks, if any.
fn never_touched(previous: &str, part: &str) -> Option<&'static str> {
    if let Some(name) = NEVER_NAMES
        .iter()
        .find(|name| part.eq_ignore_ascii_case(name))
    {
        return Some(name);
    }
    if let Some(suffix) = NEVER_SUFFIXES.iter().find(|suffix| ends_with(part, suffix)) {
        return Some(suffix);
    }
    if previous.eq_ignore_ascii_case("Library") {
        return NEVER_IN_LIBRARY
            .iter()
            .find(|name| part.eq_ignore_ascii_case(name))
            .copied();
    }
    None
}

fn ignore_case(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// A system-location component: `"*:"` matches any drive letter.
fn matches_rule(part: &str, rule: &str) -> bool {
    if rule == "*:" {
        let bytes = part.as_bytes();
        return bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    }
    part.eq_ignore_ascii_case(rule)
}

/// `part` ends with the ASCII `suffix`, ignoring ASCII case. Compares bytes,
/// so a multi-byte character before the suffix is never split.
fn ends_with(part: &str, suffix: &str) -> bool {
    let (part, suffix) = (part.as_bytes(), suffix.as_bytes());
    part.len() >= suffix.len() && part[part.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
}

/// The path without Windows' verbatim prefix (`\\?\`, `\??\`).
fn plain(platform: Platform, path: &str) -> &str {
    if platform == Platform::Windows {
        for prefix in [r"\\?\", r"\??\"] {
            if let Some(rest) = path.strip_prefix(prefix) {
                return rest;
            }
        }
    }
    path
}

/// Absolute, with no `.` or `..` component.
fn well_formed(platform: Platform, path: &str) -> Result<(), Refusal> {
    let path = plain(platform, path);
    let absolute = match platform {
        Platform::MacOs => path.starts_with('/'),
        Platform::Windows => {
            let bytes = path.as_bytes();
            let drive = bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'\\' | b'/');
            drive || path.starts_with(r"\\") || path.starts_with(r"UNC\")
        }
    };
    if !absolute {
        return Err(Refusal::NotAbsolute);
    }
    if components(platform, path).any(|part| part == "." || part == "..") {
        return Err(Refusal::Traversal);
    }
    Ok(())
}

/// The non-empty components of `path`. Windows also splits on `\`.
fn components(platform: Platform, path: &str) -> impl Iterator<Item = &str> + Clone {
    let windows = platform == Platform::Windows;
    plain(platform, path)
        .split(move |c: char| c == '/' || windows && c == '\\')
        .filter(|part| !part.is_empty())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Relation {
    Equal,
    /// The target is inside the other path.
    Inside,
    /// The target contains the other path.
    Ancestor,
    Unrelated,
}

/// How `target` relates to `other`, comparing components with `same`.
fn relation<'a, 'b>(
    mut target: impl Iterator<Item = &'a str>,
    mut other: impl Iterator<Item = &'b str>,
    same: impl Fn(&str, &str) -> bool,
) -> Relation {
    loop {
        match (target.next(), other.next()) {
            (Some(a), Some(b)) if same(a, b) => {}
            (Some(_), Some(_)) => return Relation::Unrelated,
            (None, None) => return Relation::Equal,
            (Some(_), None) => return Relation::Inside,
            (None, Some(_)) => return Relation::Ancestor,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mac(root: &str) -> Guard {
        Guard::from_resolved(Platform::MacOs, root, Some("/Users/me")).expect("guard")
    }

    fn win(root: &str) -> Guard {
        Guard::from_resolved(Platform::Windows, root, Some(r"C:\Users\me")).expect("guard")
    }

    #[test]
    fn only_paths_strictly_inside_the_root_pass() {
        let guard = mac("/Users/me/Downloads");
        assert_eq!(guard.root(), "/Users/me/Downloads");
        assert_eq!(guard.check_resolved("/Users/me/Downloads/a.zip"), Ok(()));
        assert_eq!(guard.check_resolved("/Users/me/Downloads//x/"), Ok(()));
        assert_eq!(
            guard.check_resolved("/Users/me/Downloads"),
            Err(Refusal::IsRoot)
        );
        assert_eq!(
            guard.check_resolved("/Users/me/Downloads2/a"),
            Err(Refusal::OutsideRoot)
        );
        assert_eq!(guard.check_resolved("/Users/me"), Err(Refusal::OutsideRoot));
        assert_eq!(
            guard.check_resolved("/Users/me/downloads/a.zip"),
            Err(Refusal::OutsideRoot),
            "macOS containment is exact: resolved paths carry the disk's case"
        );
        assert_eq!(
            win(r"C:\Users\me\Downloads").check_resolved(r"\\?\c:\users\ME\downloads\a.zip"),
            Ok(()),
            "Windows containment ignores case and the verbatim prefix"
        );
        assert_eq!(
            win(r"C:\Data").check_resolved(r"C:\Other\x"),
            Err(Refusal::OutsideRoot)
        );
    }

    #[test]
    fn a_multi_root_guard_allows_each_root() {
        let guard = Guard::from_roots(Platform::Windows, &[r"C:\", r"D:\"], Some(r"C:\Users\me"))
            .expect("guard");
        assert_eq!(guard.root(), r"C:\");
        assert_eq!(guard.check_resolved(r"c:\Users\me\Downloads\a.zip"), Ok(()));
        assert_eq!(guard.check_resolved(r"D:\Games\save.dat"), Ok(()));
        assert_eq!(guard.check_resolved(r"C:\"), Err(Refusal::IsRoot));
        assert_eq!(guard.check_resolved(r"D:\"), Err(Refusal::IsRoot));
        assert_eq!(guard.check_resolved(r"E:\other"), Err(Refusal::OutsideRoot));
        assert!(matches!(
            guard.check_resolved(r"C:\Windows\System32\drivers\etc\hosts"),
            Err(Refusal::System(_))
        ));
        assert!(matches!(
            guard.check_resolved(r"D:\Users\me\.ssh\id_rsa"),
            Err(Refusal::NeverTouch(_))
        ));
    }

    #[test]
    fn malformed_paths_are_refused() {
        let guard = mac("/");
        assert_eq!(
            guard.check_resolved("relative/a"),
            Err(Refusal::NotAbsolute)
        );
        assert_eq!(guard.check_resolved(""), Err(Refusal::NotAbsolute));
        assert_eq!(
            guard.check_resolved("/Users/me/../../etc/passwd"),
            Err(Refusal::Traversal)
        );
        assert_eq!(guard.check_resolved("/Users/./me"), Err(Refusal::Traversal));
        let guard = win(r"C:\");
        assert_eq!(
            guard.check_resolved(r"C:relative"),
            Err(Refusal::NotAbsolute)
        );
        assert_eq!(guard.check_resolved(r"1:\x"), Err(Refusal::NotAbsolute));
        assert_eq!(guard.check_resolved(r"C:x\y"), Err(Refusal::NotAbsolute));
        assert_eq!(guard.check_resolved("C:"), Err(Refusal::NotAbsolute));
        assert_eq!(
            guard.check_resolved(r"C:\Users\..\Windows"),
            Err(Refusal::Traversal)
        );
        assert_eq!(guard.check_resolved(r"\??\C:\Data\x"), Ok(()));
        assert_eq!(guard.check_resolved("C:/Data/x"), Ok(()));

        assert_eq!(
            Guard::from_resolved(Platform::MacOs, "Users", None).map(|_| ()),
            Err(Refusal::NotAbsolute)
        );
        assert_eq!(
            Guard::from_resolved(Platform::MacOs, "/", Some("/Users/../x")).map(|_| ()),
            Err(Refusal::Traversal)
        );
        let unc =
            Guard::from_resolved(Platform::Windows, r"\\?\UNC\nas\share", None).expect("UNC root");
        assert_eq!(unc.check_resolved(r"\\?\UNC\nas\share\old.iso"), Ok(()));
        assert_eq!(
            unc.check_resolved(r"\\nas\share\old.iso"),
            Err(Refusal::OutsideRoot)
        );
        let plain_unc =
            Guard::from_resolved(Platform::Windows, r"\\nas\share", None).expect("UNC root");
        assert_eq!(plain_unc.check_resolved(r"\\nas\share\old.iso"), Ok(()));
        assert!(format!("{plain_unc:?}").contains("nas"));
    }

    #[test]
    fn macos_system_locations_and_their_ancestors_are_refused() {
        let guard = mac("/");
        for (path, shown) in [
            ("/System/Library/Fonts/a.ttf", "/System"),
            ("/usr/local/lib/x", "/usr"),
            ("/USR/bin", "/usr"),
            ("/bin", "/bin"),
            ("/sbin/x", "/sbin"),
            ("/private/var/db/x", "/private/var/db"),
            ("/private/var/vm/sleepimage", "/private/var/vm"),
            ("/private/var", "/private/var/db"),
            ("/private", "/private/etc"),
            ("/private/etc/hosts", "/private/etc"),
            ("/cores/core.1", "/cores"),
            ("/.DocumentRevisions-V100/x", "/.DocumentRevisions-V100"),
            ("/.Spotlight-V100", "/.Spotlight-V100"),
            ("/.fseventsd/x", "/.fseventsd"),
        ] {
            assert_eq!(
                guard.check_resolved(path),
                Err(Refusal::System(shown)),
                "{path}"
            );
        }
        assert_eq!(guard.check_resolved("/private/var/folders/x"), Ok(()));
        assert_eq!(guard.check_resolved("/Library/Caches/x"), Ok(()));
        assert_eq!(guard.check_resolved("/"), Err(Refusal::IsRoot));
        assert_eq!(guard.check_resolved("/Users"), Err(Refusal::Protected));
        assert_eq!(guard.check_resolved("/Users/System"), Ok(()));
    }

    #[test]
    fn windows_system_locations_on_any_drive_are_refused() {
        let guard = win(r"\\?\C:\");
        for (path, shown) in [
            (r"C:\Windows\System32\x.dll", r"C:\Windows"),
            (r"\\?\c:\WINDOWS", r"C:\Windows"),
            (r"D:\Program Files\App\x", r"C:\Program Files"),
            (r"C:\Program Files (x86)\x", r"C:\Program Files (x86)"),
            (r"C:\Recovery", r"C:\Recovery"),
            (
                r"C:\System Volume Information\x",
                r"C:\System Volume Information",
            ),
            (r"C:\$Recycle.Bin\S-1\x", r"C:\$Recycle.Bin"),
        ] {
            let guard = if path.contains("D:") {
                win(r"D:\")
            } else {
                win(r"C:\")
            };
            assert_eq!(
                guard.check_resolved(path),
                Err(Refusal::System(shown)),
                "{path}"
            );
        }
        assert_eq!(guard.check_resolved(r"C:\Data\Windows\x"), Ok(()));
        assert_eq!(
            guard.check_resolved(r"C:\Users\me\AppData\Local\Temp\x"),
            Ok(())
        );
        let share = Guard::from_resolved(Platform::Windows, r"\\nas\C:", None).expect("share");
        assert_eq!(
            share.check_resolved(r"\\nas\C:\Windows"),
            Ok(()),
            "a share is not a drive"
        );
        let odd = Guard::from_resolved(Platform::Windows, r"\\1:\Windows", None).expect("share");
        assert_eq!(
            odd.check_resolved(r"\\1:\Windows\x"),
            Ok(()),
            "a server named like a drive is not one"
        );
    }

    #[test]
    fn the_home_and_its_protected_folders_are_refused() {
        let guard = mac("/Users");
        for path in [
            "/Users/me",
            "/Users/me/Pictures",
            "/Users/me/Music",
            "/Users/me/Library",
            "/Users/me/.ssh/id_ed25519",
            "/Users/me/.gnupg",
            "/Users/me/Library/Keychains/login.keychain-db",
        ] {
            assert!(
                matches!(
                    guard.check_resolved(path),
                    Err(Refusal::Protected | Refusal::NeverTouch(_))
                ),
                "{path}"
            );
        }
        assert_eq!(
            guard.check_resolved("/Users/me/Library"),
            Err(Refusal::Protected)
        );
        assert_eq!(
            guard.check_resolved("/Users/me/.ssh"),
            Err(Refusal::Protected)
        );
        assert_eq!(guard.check_resolved("/Users/me/Pictures/old.png"), Ok(()));
        assert_eq!(guard.check_resolved("/Users/me/Library/Caches/x"), Ok(()));
        assert_eq!(guard.check_resolved("/Users/other/Library/Caches"), Ok(()));
        let no_home = Guard::from_resolved(Platform::MacOs, "/Users", None).expect("guard");
        assert_eq!(no_home.check_resolved("/Users/me"), Ok(()));
        assert_eq!(
            win(r"C:\Users").check_resolved(r"C:\USERS\ME\.SSH\config"),
            Err(Refusal::Protected)
        );
    }

    #[test]
    fn never_touched_names_are_refused_anywhere() {
        let guard = mac("/Volumes/Backup");
        for (path, name) in [
            ("/Volumes/Backup/Backups.backupdb/x", "Backups.backupdb"),
            ("/Volumes/Backup/old/.ssh", ".ssh"),
            (
                "/Volumes/Backup/Photos Library.photoslibrary",
                ".photoslibrary",
            ),
            (
                "/Volumes/Backup/m/Music Library.MUSICLIBRARY/x",
                ".musiclibrary",
            ),
            ("/Volumes/Backup/k/login.keychain", ".keychain"),
            ("/Volumes/Backup/k/login.keychain-db", ".keychain-db"),
            ("/Volumes/Backup/x.backupbundle", ".backupbundle"),
            ("/Volumes/Backup/u/Library/Keychains", "Keychains"),
            ("/Volumes/Backup/u/library/mail/V10/x", "Mail"),
            ("/Volumes/Backup/pagefile.sys", "pagefile.sys"),
            ("/Volumes/Backup/hiberfil.sys", "hiberfil.sys"),
            ("/Volumes/Backup/swapfile.sys", "swapfile.sys"),
            ("/Volumes/Backup/.fseventsd", ".fseventsd"),
            ("/Volumes/Backup/.Spotlight-V100", ".Spotlight-V100"),
            (
                "/Volumes/Backup/.DocumentRevisions-V100",
                ".DocumentRevisions-V100",
            ),
            ("/Volumes/Backup/gnu/.gnupg", ".gnupg"),
            (
                "/Volumes/Backup/System Volume Information",
                "System Volume Information",
            ),
            ("/Volumes/Backup/$Recycle.Bin", "$Recycle.Bin"),
        ] {
            assert_eq!(
                guard.check_resolved(path),
                Err(Refusal::NeverTouch(name)),
                "{path}"
            );
        }
        assert_eq!(guard.check_resolved("/Volumes/Backup/Mail/x"), Ok(()));
        assert_eq!(
            guard.check_resolved("/Volumes/Backup/Library/Caches"),
            Ok(())
        );
        assert_eq!(guard.check_resolved("/Volumes/Backup/my.ssh"), Ok(()));
        assert_eq!(
            guard.check_resolved("/Volumes/Backup/é.keychain"),
            Err(Refusal::NeverTouch(".keychain"))
        );
    }

    #[test]
    fn bundle_insides_are_refused_but_a_bundle_itself_is_not() {
        let guard = mac("/Applications");
        assert_eq!(guard.check_resolved("/Applications/Old.app"), Ok(()));
        assert_eq!(
            guard.check_resolved("/Applications/Old.app/Contents/MacOS/Old"),
            Err(Refusal::BundleInternal)
        );
        assert_eq!(
            guard.check_resolved("/Applications/Old.APP/Contents"),
            Err(Refusal::BundleInternal)
        );
        for suffix in BUNDLE_SUFFIXES {
            let path = format!("/Applications/X{suffix}/y");
            assert_eq!(
                guard.check_resolved(&path),
                Err(Refusal::BundleInternal),
                "{path}"
            );
        }
        assert_eq!(guard.check_resolved("/Applications/app/x"), Ok(()));
        assert_eq!(guard.check_resolved("/Applications/Ä/x"), Ok(()));
    }

    #[test]
    fn refusals_explain_themselves() {
        for (refusal, words) in [
            (Refusal::NotAbsolute, "absolute"),
            (Refusal::Traversal, ".."),
            (Refusal::NotUtf8, "UTF-8"),
            (Refusal::Unresolvable(io::ErrorKind::NotFound), "resolved"),
            (Refusal::OutsideRoot, "outside"),
            (Refusal::IsRoot, "itself"),
            (Refusal::System("/usr"), "/usr is a system"),
            (Refusal::Protected, "protected"),
            (Refusal::NeverTouch(".ssh"), ".ssh is never"),
            (Refusal::BundleInternal, "bundle"),
        ] {
            assert!(refusal.to_string().contains(words), "{refusal:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_resolved_path_that_is_not_utf8_is_refused() {
        use std::os::unix::ffi::OsStrExt;
        let guard = mac("/");
        let path = Path::new(std::ffi::OsStr::from_bytes(b"/tmp/\xff"));
        assert_eq!(guard.check_path(path), Err(Refusal::NotUtf8));
    }
}
