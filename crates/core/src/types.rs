//! The vocabulary every crate shares: entries, volumes, options, results.

use std::fmt;
use std::ops::{Add, AddAssign};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::ScanError;

/// Index of a node in the in-memory arena. `0` is reserved for the scan root.
pub type NodeId = u32;

/// A byte count.
///
/// Newtyped so that allocated and logical sizes cannot be swapped by accident at a
/// call site. [`Entry`] keeps raw `u64` fields on purpose: it is the hot struct that
/// maps 1:1 onto the SQLite row and the 40-byte arena node.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Bytes(u64);

impl Bytes {
    /// Zero bytes.
    pub const ZERO: Self = Self(0);

    /// Construct from a raw count.
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    /// The raw count.
    pub const fn get(self) -> u64 {
        self.0
    }

    /// Addition that clamps instead of wrapping. Disk totals cannot overflow `u64`,
    /// but a corrupt FFI value must not panic a release build.
    pub const fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    /// Subtraction that clamps at zero.
    pub const fn saturating_sub(self, other: Self) -> Self {
        Self(self.0.saturating_sub(other.0))
    }
}

impl Add for Bytes {
    type Output = Self;

    fn add(self, rhs: Self) -> Self::Output {
        self.saturating_add(rhs)
    }
}

impl AddAssign for Bytes {
    fn add_assign(&mut self, rhs: Self) {
        *self = self.saturating_add(rhs);
    }
}

impl std::iter::Sum for Bytes {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::ZERO, Add::add)
    }
}

impl From<u64> for Bytes {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

impl From<Bytes> for u64 {
    fn from(value: Bytes) -> Self {
        value.0
    }
}

impl fmt::Display for Bytes {
    /// Raw integer. Human-readable formatting belongs to `ui/src/lib/format.ts`
    /// (and to `scanbench` for CLI output).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What an entry is, as stored in the `kind` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum EntryKind {
    /// A regular file.
    File = 1,
    /// A directory.
    Dir = 2,
    /// A symbolic link. Never followed.
    Symlink = 3,
    /// Sockets, FIFOs, devices, and anything else that is not a file/dir/link.
    Other = 4,
}

impl EntryKind {
    /// The value written to SQLite.
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    /// Parse a value read from SQLite.
    pub const fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::File),
            2 => Some(Self::Dir),
            3 => Some(Self::Symlink),
            4 => Some(Self::Other),
            _ => None,
        }
    }
}

impl TryFrom<u8> for EntryKind {
    type Error = ScanError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        Self::from_u8(value).ok_or(ScanError::InvalidEntryKind(i64::from(value)))
    }
}

/// One filesystem entry, exactly as the scanner saw it.
///
/// `parent` is an index into the node arena, not a database id. The root of each
/// scanned volume has `parent == 0` and is itself stored at index 0.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Arena index of the containing directory. `0` = volume root.
    pub parent: NodeId,
    /// Final path component, as reported by the OS (not normalised).
    pub name: String,
    /// File, directory, symlink, or other.
    pub kind: EntryKind,
    /// Bytes actually occupied on disk. **This is the number the app displays.**
    pub size_alloc: u64,
    /// Apparent file size. Shown only when explicitly labelled.
    pub size_logical: u64,
    /// Volume identity (`dev_t` on macOS, volume serial on Windows).
    pub dev: u32,
    /// Inode number on macOS, file index on Windows.
    pub file_id: u64,
    /// Hard-link count. Values above 1 are deduped on `(dev, file_id)`.
    pub nlink: u32,
    /// Raw platform attributes (`st_flags` / `dwFileAttributes`).
    pub attrs: u32,
    /// Creation time, unix seconds. `0` when the platform has no answer.
    pub created: i64,
    /// Last modification time, unix seconds.
    pub modified: i64,
    /// Last access time, unix seconds. Not a usage signal on Windows.
    pub accessed: i64,
    /// True when this path is an additional hard link to an inode the scan already
    /// counted. The row is kept — the path is real and browsable — but it must not
    /// contribute to rollups or to [`ScanResult`](crate::ScanResult) totals.
    ///
    /// Scanners set this only for regular files with `nlink > 1`; "first path seen"
    /// owns the bytes.
    #[serde(default)]
    pub hardlink_dup: bool,
}

impl Entry {
    /// True for a regular file.
    pub fn is_file(&self) -> bool {
        self.kind == EntryKind::File
    }

    /// True for a directory.
    pub fn is_dir(&self) -> bool {
        self.kind == EntryKind::Dir
    }

    /// True for a symbolic link.
    pub fn is_symlink(&self) -> bool {
        self.kind == EntryKind::Symlink
    }

    /// True when this entry may be a hard link to another path.
    pub fn is_hard_link(&self) -> bool {
        self.nlink > 1
    }
}

/// What a scan recorded about one path — or what a fresh `lstat` sees now — in
/// the scanner's units. The safety layer's preflight (#29) compares the two
/// immediately before deleting: if they differ, the file was changed or
/// replaced after the scan and is skipped.
///
/// Each platform crate provides `stamp(path)`, which reads it for a path
/// without following a final symlink, exactly as its scanner does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStamp {
    pub kind: EntryKind,
    /// Apparent size (`st_size` / end of file).
    pub size_logical: u64,
    /// Last modification, unix seconds, rounded down.
    pub modified: i64,
    /// Volume identity, as in [`Entry::dev`].
    pub dev: u32,
    /// Inode / NTFS file id, as in [`Entry::file_id`].
    pub file_id: u64,
    /// Raw platform attributes, as in [`Entry::attrs`].
    pub attrs: u32,
}

impl FileStamp {
    /// The stamp the scan recorded for `entry`.
    pub fn of(entry: &Entry) -> Self {
        Self {
            kind: entry.kind,
            size_logical: entry.size_logical,
            modified: entry.modified,
            dev: entry.dev,
            file_id: entry.file_id,
            attrs: entry.attrs,
        }
    }
}

/// A mounted filesystem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Volume {
    /// Platform volume identity, matching [`Entry::dev`].
    pub id: u32,
    /// Human-readable name, e.g. `Macintosh HD`.
    pub name: String,
    /// Where it is mounted.
    pub mount_point: PathBuf,
    /// Filesystem type, e.g. `apfs` or `NTFS`.
    pub fs_type: String,
    /// Total capacity.
    pub total: Bytes,
    /// Space the OS reports as available to the user.
    pub available: Bytes,
    /// macOS purgeable space (0 on Windows). Part of `available` in the Finder's
    /// arithmetic, which is why capacity gauges can disagree.
    ///
    /// 0 on macOS until the Foundation bridge in the app layer lands; `available`
    /// from statfs already includes purgeable space as the OS reports it.
    pub purgeable: Bytes,
    /// Removable media.
    pub removable: bool,
}

impl Volume {
    /// Space in use, computed the way the app presents it: total − available.
    pub fn used(&self) -> Bytes {
        self.total.saturating_sub(self.available)
    }
}

/// Knobs a caller can turn when starting a scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanOptions {
    /// Apply the platform's default skip list (`/System/Volumes/*`, `C:\Windows\...`).
    pub skip_system: bool,
    /// Descend into other mounted filesystems. Off: each volume is scanned once.
    pub cross_filesystems: bool,
    /// Worker threads. `0` means "choose automatically".
    pub threads: usize,
    /// Absolute paths to skip, matched by prefix.
    pub exclude: Vec<PathBuf>,
    /// Maximum directory depth below each root. `None` = unlimited.
    pub max_depth: Option<u32>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            skip_system: true,
            cross_filesystems: false,
            threads: 0,
            exclude: Vec::new(),
            max_depth: None,
        }
    }
}

/// A throttled progress snapshot, emitted at most ~10 times per second.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Progress {
    /// Files seen so far.
    pub files: u64,
    /// Directories seen so far.
    pub dirs: u64,
    /// Allocated bytes seen so far.
    pub bytes_alloc: Bytes,
    /// Milliseconds since the scan started.
    pub elapsed_ms: u64,
    /// Directory currently being enumerated, when known.
    pub current_path: Option<PathBuf>,
}

/// How a scan ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScanStatus {
    /// Still running. Only appears in progress events, never in a final result.
    Running,
    /// Walked every reachable entry.
    Complete,
    /// Stopped on request. Rows already written are consistent.
    Cancelled,
    /// Finished, but some directories were skipped.
    Partial,
}

impl ScanStatus {
    /// The value written to `scan_session.status`.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Complete => "complete",
            Self::Cancelled => "cancelled",
            Self::Partial => "partial",
        }
    }

    /// Parse a value read from SQLite.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "running" => Self::Running,
            "complete" => Self::Complete,
            "cancelled" => Self::Cancelled,
            "partial" => Self::Partial,
            _ => return None,
        })
    }
}

/// Why a directory was not descended into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// The OS refused access.
    PermissionDenied,
    /// Matched the platform skip list (system volume, `C:\Windows`, …).
    SystemPath,
    /// A different filesystem is mounted here and `cross_filesystems` is off.
    MountPoint,
    /// Reparse point, junction, or symlink that must not be followed.
    Symlink,
    /// Matched [`ScanOptions::exclude`].
    Excluded,
    /// A specific I/O error.
    IoError,
    /// Anything else.
    Other,
}

impl SkipReason {
    /// Stable snake_case name, suitable for logs and `ScanResult` JSON.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PermissionDenied => "permission_denied",
            Self::SystemPath => "system_path",
            Self::MountPoint => "mount_point",
            Self::Symlink => "symlink",
            Self::Excluded => "excluded",
            Self::IoError => "io_error",
            Self::Other => "other",
        }
    }
}

/// A directory the scanner could not or would not enter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedDir {
    /// Absolute path of the directory.
    pub path: PathBuf,
    /// Why it was skipped.
    pub reason: SkipReason,
}

/// The summary a completed scan returns.
///
/// Entries themselves are streamed to an
/// [`EntrySink`](crate::EntrySink); they are deliberately not collected here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanResult {
    /// How the scan ended.
    pub status: ScanStatus,
    /// Regular files counted (hard links counted once).
    pub files: u64,
    /// Directories counted.
    pub dirs: u64,
    /// Allocated bytes. The number the app shows.
    pub bytes_alloc: Bytes,
    /// Logical bytes. Diagnostics only.
    pub bytes_logical: Bytes,
    /// Wall-clock duration.
    pub elapsed_ms: u64,
    /// Every directory that was skipped, with a reason.
    pub skipped: Vec<SkippedDir>,
    /// Whether the scan ran with administrator rights, where that matters.
    ///
    /// `Some(false)` on an unelevated Windows scan labels every
    /// [`SkipReason::PermissionDenied`] entry in `skipped` as a gap an elevated
    /// relaunch may fill (PLAN §7.3). `None` where the platform has no such
    /// notion (macOS uses Full Disk Access instead) or the state is unknown.
    #[serde(default)]
    pub elevated: Option<bool>,
}

impl ScanResult {
    /// An empty result with the given status. Useful for early exits and tests.
    pub fn empty(status: ScanStatus) -> Self {
        Self {
            status,
            files: 0,
            dirs: 0,
            bytes_alloc: Bytes::ZERO,
            bytes_logical: Bytes::ZERO,
            elapsed_ms: 0,
            skipped: Vec::new(),
            elevated: None,
        }
    }

    /// Number of skipped directories.
    pub fn skipped_count(&self) -> usize {
        self.skipped.len()
    }

    /// Number of skipped directories with `reason`.
    ///
    /// The UI groups skips by reason ("12 permission denied, 3 mount points"), so
    /// the count is exposed here instead of making every caller re-implement it.
    pub fn skipped_by(&self, reason: SkipReason) -> usize {
        self.skipped
            .iter()
            .filter(|skipped| skipped.reason == reason)
            .count()
    }
}

/// A platform-specific cleanup location reported by the scanner.
///
/// The rules engine (#31) turns these into risk-scored `rule_hit` rows; the struct
/// stays intentionally thin until M4.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformTarget {
    /// Stable identifier, e.g. `macos.xcode.derived-data`.
    pub id: String,
    /// Human-readable name.
    pub label: String,
    /// Absolute path.
    pub path: PathBuf,
    /// Bytes currently occupied.
    pub bytes: Bytes,
}

/// How confident we are that an app is unused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppStatus {
    /// Used within 90 days, or protected by running / autostart / default-handler
    /// / system evidence (PLAN §11).
    InUse,
    /// Last used 90–180 days ago, with no supporting evidence of use.
    LikelyUnused,
    /// Last used more than 180 days ago, and nothing else says it is in use.
    Unused,
    /// Metadata was missing or unreadable. Never treated as unused.
    Unknown,
}

/// What the platform says about an app's code signature.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Signature {
    /// Signed, and the signature verifies.
    Valid,
    /// Signed, but verification failed (modified or damaged bundle).
    Invalid,
    /// Not signed at all.
    Unsigned,
    /// Could not be checked.
    #[default]
    Unknown,
}

/// Data an app left outside its bundle, matched by heuristic (PLAN §11).
///
/// Always presented as "associated data (estimated)".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Satellite {
    /// The satellite directory or file.
    pub path: PathBuf,
    /// Allocated bytes below `path`.
    pub bytes: Bytes,
}

/// Something that starts at login (#114): a launchd job on macOS, a `Run`
/// value or Startup-folder shortcut on Windows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupItem {
    /// Stable id the platform toggles it by.
    pub id: String,
    /// Display name.
    pub name: String,
    /// What runs: a command line, program path or shortcut.
    pub command: String,
    /// Where it is registered, for display.
    pub source: String,
    /// It will start at the next login.
    pub enabled: bool,
    /// This app can switch it; `false` when that needs admin rights or the
    /// system's own settings.
    pub can_toggle: bool,
}

/// An installed application: its footprint and the raw usage evidence.
///
/// Scanners fill the evidence and leave `status` as [`AppStatus::Unknown`];
/// `storagelens-analysis` classifies it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppInfo {
    /// Bundle identifier on macOS, uninstall key name on Windows.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Version string, when the platform exposes one.
    pub version: Option<String>,
    /// Bundle or install directory.
    pub path: PathBuf,
    /// Size of the app itself, not its satellites.
    pub bytes: Bytes,
    /// Last-used time, unix seconds, when known.
    pub last_used: Option<i64>,
    /// Usage classification.
    pub status: AppStatus,
    /// Launch count, where the platform records one.
    pub use_count: Option<u64>,
    /// Code signature state.
    pub signature: Signature,
    /// A process is running from inside the app right now.
    pub running: bool,
    /// Referenced by a launch agent / daemon / autostart entry.
    pub autostart: bool,
    /// The current default handler for a file type or web/mail scheme.
    pub default_handler: bool,
    /// Ships with the OS (`/System/Applications`, `com.apple.*`). Never flagged.
    pub system: bool,
    /// Associated data outside the bundle.
    pub satellites: Vec<Satellite>,
    /// The app's own uninstaller (#115): on Windows the command "Add or
    /// remove apps" runs, on macOS a vendor `Uninstall….app` shipped with it.
    #[serde(default)]
    pub uninstaller: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry() -> Entry {
        Entry {
            parent: 0,
            name: "photo.jpg".into(),
            kind: EntryKind::File,
            size_alloc: 4_096,
            size_logical: 5_000,
            dev: 16_777_217,
            file_id: 42,
            nlink: 2,
            attrs: 0,
            created: 1_700_000_000,
            modified: 1_700_000_100,
            accessed: 1_700_000_200,
            hardlink_dup: false,
        }
    }

    #[test]
    fn bytes_arithmetic_saturates() {
        assert_eq!(Bytes::new(2) + Bytes::new(3), Bytes::new(5));
        assert_eq!(Bytes::new(3).saturating_sub(Bytes::new(9)), Bytes::ZERO);
        assert_eq!(Bytes::new(u64::MAX) + Bytes::new(1), Bytes::new(u64::MAX));
        let total: Bytes = [Bytes::new(1), Bytes::new(2), Bytes::new(3)]
            .into_iter()
            .sum();
        assert_eq!(total, Bytes::new(6));
    }

    #[test]
    fn bytes_display_is_raw() {
        assert_eq!(Bytes::new(1_500).to_string(), "1500");
    }

    #[test]
    fn entry_kind_round_trips() {
        for kind in [
            EntryKind::File,
            EntryKind::Dir,
            EntryKind::Symlink,
            EntryKind::Other,
        ] {
            assert_eq!(EntryKind::from_u8(kind.as_u8()), Some(kind));
            assert_eq!(EntryKind::try_from(kind.as_u8()).ok(), Some(kind));
        }
        assert_eq!(EntryKind::from_u8(0), None);
        assert!(matches!(
            EntryKind::try_from(9),
            Err(ScanError::InvalidEntryKind(9))
        ));
    }

    #[test]
    fn entry_helpers_agree_with_kind() {
        let mut entry = sample_entry();
        assert!(entry.is_file());
        assert!(!entry.is_dir());
        assert!(!entry.is_symlink());
        assert!(entry.is_hard_link());

        entry.kind = EntryKind::Dir;
        assert!(entry.is_dir());
        entry.nlink = 1;
        assert!(!entry.is_hard_link());
    }

    #[test]
    fn scan_options_defaults_are_conservative() {
        let opts = ScanOptions::default();
        assert!(opts.skip_system);
        assert!(!opts.cross_filesystems);
        assert_eq!(opts.threads, 0);
        assert!(opts.exclude.is_empty());
        assert_eq!(opts.max_depth, None);
    }

    #[test]
    fn volume_used_is_total_minus_available() {
        let volume = Volume {
            id: 1,
            name: "Macintosh HD".into(),
            mount_point: PathBuf::from("/"),
            fs_type: "apfs".into(),
            total: Bytes::new(1_000),
            available: Bytes::new(250),
            purgeable: Bytes::new(100),
            removable: false,
        };
        assert_eq!(volume.used(), Bytes::new(750));
    }

    #[test]
    fn scan_result_empty_has_no_skips() {
        let result = ScanResult::empty(ScanStatus::Complete);
        assert_eq!(result.files, 0);
        assert_eq!(result.bytes_alloc, Bytes::ZERO);
        assert_eq!(result.skipped_count(), 0);
    }

    #[test]
    fn status_and_skip_reasons_have_stable_names() {
        for status in [
            ScanStatus::Running,
            ScanStatus::Complete,
            ScanStatus::Cancelled,
            ScanStatus::Partial,
        ] {
            assert_eq!(ScanStatus::from_name(status.as_str()), Some(status));
        }
        assert_eq!(ScanStatus::from_name("nope"), None);
        assert_eq!(ScanStatus::Partial.as_str(), "partial");
        assert_eq!(SkipReason::PermissionDenied.as_str(), "permission_denied");
        assert_eq!(SkipReason::SystemPath.as_str(), "system_path");
    }
}
