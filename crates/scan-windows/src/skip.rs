//! Which directories the Windows walker refuses to enter, and why.
//!
//! Pure string rules over the path and [`ScanOptions`] — no syscalls — so they
//! are unit-tested on every CI runner, not just Windows.
//!
//! # Comparisons are case-insensitive
//!
//! NTFS names are case-preserving but matched case-insensitively, so
//! `c:\users\ME\skip` and `C:\Users\me\Skip` are the same directory (PLAN §7.3:
//! never compare paths by exact string). Both sides are normalised once — `\\?\`
//! stripped, `/` turned into `\`, trailing separators dropped, lower-cased — and
//! matched component-wise, so `C:\data` excludes `C:\data\x` but not
//! `C:\database`.
//!
//! # System paths
//!
//! PLAN §7.3 skips exactly two directories, at the root of any volume:
//! `$Recycle.Bin` (deleted files; the cleanup rules report it separately) and
//! `System Volume Information` (restore points and the indexer; unreadable when
//! unelevated). `C:\Windows` and `C:\Program Files` are **not** skipped: they are
//! real space, `WinSxS` is a reported cleanup target (PLAN §10), and the app
//! inventory needs `Program Files`. The safety layer, not the scanner, keeps them
//! from being deleted.
//!
//! # Mounts
//!
//! A volume mounted into a folder is an `IO_REPARSE_TAG_MOUNT_POINT` reparse
//! point, which the walker records as a link and never enters (see `ffi`). So the
//! scan never leaves its root volume, and `cross_filesystems` has no effect in the
//! `FindFirstFileExW` walker.

use std::path::Path;

use storagelens_core::{ScanOptions, SkipReason};

/// Volume-root directories skipped when [`ScanOptions::skip_system`] is set.
pub(crate) const SYSTEM_DIRS: &[&str] = &["$Recycle.Bin", "System Volume Information"];

/// Normalise a path for comparison. See the module docs.
pub(crate) fn normalize(path: &str) -> String {
    let path = if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        path.to_owned()
    };

    let mut path = path.replace('/', "\\").to_lowercase();
    while path.ends_with('\\') {
        path.pop();
    }
    path
}

/// The part of a normalised path below its volume root, if any.
///
/// `c:\a\b` → `a\b`; `\\server\share\a` → `a`; `c:` → `None`.
fn volume_relative(path: &str) -> Option<&str> {
    if let Some(unc) = path.strip_prefix(r"\\") {
        // Skip the server and share components.
        let mut parts = unc.splitn(3, '\\');
        let _server = parts.next()?;
        let _share = parts.next()?;
        return parts.next().filter(|rest| !rest.is_empty());
    }

    let bytes = path.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' {
        return path[2..].strip_prefix('\\').filter(|rest| !rest.is_empty());
    }
    None
}

/// Whether normalised `path` is `prefix` or lies below it.
fn has_prefix(path: &str, prefix: &str) -> bool {
    match path.strip_prefix(prefix) {
        Some(rest) => rest.is_empty() || rest.starts_with('\\'),
        None => false,
    }
}

/// Whether normalised `path` is on, or below, the system skip list.
pub(crate) fn is_system_path(path: &str) -> bool {
    let Some(relative) = volume_relative(path) else {
        return false;
    };
    let first = relative.split('\\').next().unwrap_or(relative);
    SYSTEM_DIRS
        .iter()
        .any(|system| first == system.to_lowercase())
}

/// The skip rules for one scan, with the excludes normalised once up front.
#[derive(Debug, Clone)]
pub(crate) struct Rules {
    skip_system: bool,
    exclude: Vec<String>,
    max_depth: Option<u32>,
}

impl Rules {
    /// Prepare the rules from the caller's options.
    pub(crate) fn new(options: &ScanOptions) -> Self {
        Self {
            skip_system: options.skip_system,
            exclude: options
                .exclude
                .iter()
                .map(|path| normalize(&path.to_string_lossy()))
                .filter(|path| !path.is_empty())
                .collect(),
            max_depth: options.max_depth,
        }
    }

    /// Decide whether to enter a directory `depth` levels below the root.
    ///
    /// The root itself is never skipped: the caller asked for it explicitly.
    pub(crate) fn skip_reason(&self, path: &Path, depth: u32) -> Option<SkipReason> {
        if depth == 0 {
            return None;
        }

        // Lossy is fine for matching: an unpaired surrogate never appears in a
        // system name or a user-typed exclude.
        let path = normalize(&path.to_string_lossy());

        if self.skip_system && is_system_path(&path) {
            return Some(SkipReason::SystemPath);
        }

        if self
            .exclude
            .iter()
            .any(|excluded| has_prefix(&path, excluded))
        {
            return Some(SkipReason::Excluded);
        }

        if self.max_depth.is_some_and(|max| depth > max) {
            return Some(SkipReason::Other);
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn rules(options: ScanOptions) -> Rules {
        Rules::new(&options)
    }

    #[test]
    fn normalisation_strips_prefixes_case_and_separators() {
        assert_eq!(normalize(r"\\?\C:\Users\Me\"), r"c:\users\me");
        assert_eq!(normalize(r"\\?\UNC\Server\Share"), r"\\server\share");
        assert_eq!(normalize("C:/Data/Dir/"), r"c:\data\dir");
        assert_eq!(normalize(r"C:\"), "c:");
        assert_eq!(normalize(r"D:\Фото"), r"d:\фото");
    }

    #[test]
    fn volume_relative_parts_handle_drives_and_shares() {
        assert_eq!(volume_relative(r"c:\a\b"), Some(r"a\b"));
        assert_eq!(volume_relative("c:"), None);
        assert_eq!(volume_relative(r"\\srv\share\a\b"), Some(r"a\b"));
        assert_eq!(volume_relative(r"\\srv\share"), None);
        assert_eq!(volume_relative("relative"), None);
    }

    #[test]
    fn system_directories_are_skipped_only_at_a_volume_root() {
        for path in [
            r"c:\$recycle.bin",
            r"c:\$recycle.bin\s-1-5-21",
            r"d:\system volume information",
            r"\\srv\share\$recycle.bin",
        ] {
            assert!(is_system_path(path), "{path} is a system path");
        }
        for path in [
            r"c:\windows",
            r"c:\program files",
            r"c:\users\me\$recycle.bin",
            r"c:\$recycle.bin.old",
            "c:",
        ] {
            assert!(!is_system_path(path), "{path} is not a system path");
        }
    }

    #[test]
    fn the_root_is_never_skipped() {
        let rules = rules(ScanOptions {
            exclude: vec![PathBuf::from(r"C:\$Recycle.Bin")],
            max_depth: Some(0),
            ..ScanOptions::default()
        });
        assert_eq!(rules.skip_reason(Path::new(r"C:\$Recycle.Bin"), 0), None);
    }

    #[test]
    fn rules_apply_in_the_documented_order() {
        let rules = rules(ScanOptions {
            exclude: vec![
                PathBuf::from(r"C:\Data\Skip"),
                PathBuf::from(r"C:\$Recycle.Bin"),
            ],
            max_depth: Some(2),
            ..ScanOptions::default()
        });

        // System beats exclude.
        assert_eq!(
            rules.skip_reason(Path::new(r"C:\$Recycle.Bin"), 1),
            Some(SkipReason::SystemPath)
        );
        // Exclude matching is case-insensitive and verbatim-agnostic.
        assert_eq!(
            rules.skip_reason(Path::new(r"\\?\c:\data\SKIP\inner"), 3),
            Some(SkipReason::Excluded)
        );
        // Component-aware: a shared prefix is not a match.
        assert_eq!(rules.skip_reason(Path::new(r"C:\Data\Skipper"), 2), None);
        // Too deep.
        assert_eq!(
            rules.skip_reason(Path::new(r"C:\Data\a\b"), 3),
            Some(SkipReason::Other)
        );
        assert_eq!(rules.skip_reason(Path::new(r"C:\Data\a"), 2), None);
    }

    #[test]
    fn skip_system_off_enters_system_directories() {
        let rules = rules(ScanOptions {
            skip_system: false,
            ..ScanOptions::default()
        });
        assert_eq!(rules.skip_reason(Path::new(r"C:\$Recycle.Bin"), 1), None);
    }

    #[test]
    fn an_empty_exclude_does_not_exclude_everything() {
        let rules = rules(ScanOptions {
            exclude: vec![PathBuf::new()],
            ..ScanOptions::default()
        });
        assert_eq!(rules.skip_reason(Path::new(r"C:\Data"), 1), None);
    }
}
