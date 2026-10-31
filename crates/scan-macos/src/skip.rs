//! Which directories the walker refuses to enter, and why.
//!
//! The rules are pure functions of the path, the path's device, and
//! [`ScanOptions`] — no syscalls — so they are unit-tested in isolation and the
//! walker only has to report the answer.
//!
//! # System paths
//!
//! The big one is `/System/Volumes`. On a modern Mac the writable Data volume is
//! both firmlinked into `/` (`/Users`, `/Applications`, `/private`, …) and mounted
//! at `/System/Volumes/Data`. Walking either path reaches the same files, so
//! walking both counts the disk twice. The default skip list stops at the mount
//! directory and lets the firmlinks supply the data exactly once.
//!
//! `/.DocumentRevisions-V100` and `/private/var/vm` are deliberately **not** on the
//! skip list. They are large (`/private/var/vm` holds the sleep image), they are
//! real space the user should see in the treemap, and they must never be offered
//! for deletion. The safety layer refuses to delete them; the scanner's job is to
//! count and report them.
//!
//! # Mounts
//!
//! With `cross_filesystems` off (the default), a directory on another device is
//! reported as [`SkipReason::MountPoint`] and not entered. Each volume is scanned
//! as its own root, so nothing is counted twice and no scan silently wanders onto
//! a network share.

use std::path::Path;

use storagelens_core::{ScanOptions, SkipReason};

/// Prefixes skipped when [`ScanOptions::skip_system`] is set.
///
/// Matched with [`Path::starts_with`], which is component-aware: `/dev` matches
/// `/dev` and `/dev/disk3`, never `/devices`.
pub(crate) const SYSTEM_PREFIXES: &[&str] = &[
    // Firmlinks and the system/VM/Preboot volumes. Everything under `/System` is
    // read-only and sealed; the only reason to reach it is double counting.
    "/System/Volumes",
    // Device nodes are not files and have no meaningful size.
    "/dev",
    // Mounted volumes are scanned individually. This also stops an external disk
    // from being counted as part of the boot volume.
    "/Volumes",
    // Index and event stores: billions of tiny files, no user data.
    "/.Spotlight-V100",
    "/.fseventsd",
    // Deleted files still occupying space. The Trash UI is the right place for
    // them; a size scan would just confuse the total.
    "/.Trashes",
];

/// Whether `path` is on the default system skip list.
pub(crate) fn is_system_path(path: &Path) -> bool {
    SYSTEM_PREFIXES
        .iter()
        .any(|prefix| path.starts_with(prefix))
}

/// Decide whether to enter a directory.
///
/// `depth` is the root-relative depth (`0` for the scan root), `dev` the
/// directory's device, and `root_dev` the scan root's device.
///
/// The root itself is never skipped: the caller asked for it explicitly, even if
/// it happens to sit on the system skip list or another filesystem. Everything
/// below it obeys the rules in the documented order.
pub(crate) fn skip_reason(
    path: &Path,
    depth: u32,
    dev: u32,
    root_dev: u32,
    options: &ScanOptions,
) -> Option<SkipReason> {
    if depth == 0 {
        return None;
    }

    if options.skip_system && is_system_path(path) {
        return Some(SkipReason::SystemPath);
    }

    if !options.cross_filesystems && dev != root_dev {
        return Some(SkipReason::MountPoint);
    }

    if options
        .exclude
        .iter()
        .any(|excluded| path.starts_with(excluded))
    {
        return Some(SkipReason::Excluded);
    }

    if options.max_depth.is_some_and(|max| depth > max) {
        return Some(SkipReason::Other);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> ScanOptions {
        ScanOptions {
            skip_system: true,
            cross_filesystems: false,
            threads: 1,
            exclude: Vec::new(),
            max_depth: None,
        }
    }

    #[test]
    fn every_documented_prefix_is_skipped() {
        for path in [
            "/System/Volumes",
            "/System/Volumes/Data",
            "/System/Volumes/Data/Users",
            "/dev",
            "/dev/disk3s1",
            "/Volumes",
            "/Volumes/External/Photos",
            "/.Spotlight-V100",
            "/.Spotlight-V100/Store-V2",
            "/.fseventsd",
            "/.Trashes/501",
        ] {
            assert!(
                is_system_path(Path::new(path)),
                "{path} should be on the skip list"
            );
        }
    }

    #[test]
    fn matching_is_component_aware_not_a_prefix_string() {
        for path in [
            "/devices",
            "/Volume",
            "/Volumes-old",
            "/System/VolumesX",
            "/.Spotlight-V1000",
            "/Users/me/System/Volumes",
        ] {
            assert!(
                !is_system_path(Path::new(path)),
                "{path} must not match a skipped component"
            );
        }
    }

    #[test]
    fn the_root_is_always_entered() {
        let options = options();
        assert_eq!(
            skip_reason(Path::new("/System/Volumes/Data"), 0, 2, 1, &options),
            None
        );
    }

    #[test]
    fn system_paths_win_over_the_mount_rule() {
        let options = options();
        assert_eq!(
            skip_reason(Path::new("/System/Volumes/VM"), 1, 9, 1, &options),
            Some(SkipReason::SystemPath)
        );
    }

    #[test]
    fn another_device_is_a_mount_point_unless_crossing_is_allowed() {
        let mut options = options();
        let path = Path::new("/Users/me/external");

        assert_eq!(
            skip_reason(path, 2, 9, 1, &options),
            Some(SkipReason::MountPoint)
        );

        options.cross_filesystems = true;
        assert_eq!(skip_reason(path, 2, 9, 1, &options), None);
        assert_eq!(skip_reason(path, 2, 1, 1, &options), None);
    }

    #[test]
    fn skip_system_can_be_turned_off() {
        let mut options = options();
        options.skip_system = false;

        assert_eq!(
            skip_reason(Path::new("/System/Volumes/Data"), 1, 1, 1, &options),
            None
        );
    }

    #[test]
    fn excluded_paths_and_their_children_are_reported_as_excluded() {
        let mut options = options();
        options.exclude = vec!["/Users/me/Downloads".into()];

        assert_eq!(
            skip_reason(Path::new("/Users/me/Downloads"), 1, 1, 1, &options),
            Some(SkipReason::Excluded)
        );
        assert_eq!(
            skip_reason(Path::new("/Users/me/Downloads/tmp"), 2, 1, 1, &options),
            Some(SkipReason::Excluded)
        );
        assert_eq!(
            skip_reason(Path::new("/Users/me/Downloads-old"), 1, 1, 1, &options),
            None
        );
    }

    #[test]
    fn max_depth_skips_only_deeper_directories() {
        let mut options = options();
        options.max_depth = Some(2);

        assert_eq!(skip_reason(Path::new("/root/a"), 1, 1, 1, &options), None);
        assert_eq!(skip_reason(Path::new("/root/a/b"), 2, 1, 1, &options), None);
        assert_eq!(
            skip_reason(Path::new("/root/a/b/c"), 3, 1, 1, &options),
            Some(SkipReason::Other)
        );
    }

    #[test]
    fn document_revisions_and_swap_are_walked() {
        // They are reported in the tree and protected by the safety layer, so they
        // must not appear in the skip list.
        let options = options();
        assert_eq!(
            skip_reason(Path::new("/.DocumentRevisions-V100"), 1, 1, 1, &options),
            None
        );
        assert_eq!(
            skip_reason(Path::new("/private/var/vm"), 1, 1, 1, &options),
            None
        );
    }
}
