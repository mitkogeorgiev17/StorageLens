//! Synthetic trees for the six grouping views (PLAN §9.6, #25).
//!
//! The treemap draws "a tree of sized nodes"; this module builds that tree six
//! ways from the same [`Arena`], with no SQL:
//!
//! | Grouping      | Levels                                               |
//! | ------------- | ---------------------------------------------------- |
//! | `Folder`      | the directory tree itself                            |
//! | `Type`        | category → extension → files                         |
//! | `App`         | app → its satellites (bundle, caches, …) → files     |
//! | `Age`         | modified-time bucket → files                         |
//! | `Size`        | size bucket → files                                  |
//! | `Cleanliness` | rule risk (safe, review, keep, never) → files        |
//!
//! # Invariants (tested)
//!
//! - Every counted file (a regular file that is not a hard-link duplicate, the
//!   rule the scan totals use) that passes the filter appears in exactly one
//!   group, so the root's total equals the scan's total when unfiltered.
//! - Output is deterministic: groups are created in arena order, and children
//!   are ordered by size (descending) then label — except bucketed views
//!   (age, size, cleanliness), which keep their natural bucket order.
//! - A group's index is always greater than its parent's, so one reverse pass
//!   sums the totals.
//! - `Type` always has an `Other` group, even when it is empty.
//!
//! Files are not nodes here: a group lists its files (`files`), unsorted. A
//! view picks the biggest few when it needs them, so building stays O(n).

use std::collections::HashMap;
use std::path::Path;

use storagelens_catalog::{Arena, NodeIndex, UNKNOWN_TIME};
use storagelens_core::{Category, EntryKind};

/// The six views.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Grouping {
    Folder,
    Type,
    App,
    Age,
    Size,
    Cleanliness,
}

impl Grouping {
    pub const ALL: [Self; 6] = [
        Self::Folder,
        Self::Type,
        Self::App,
        Self::Age,
        Self::Size,
        Self::Cleanliness,
    ];
}

/// A cleanup rule's verdict on a file (PLAN §10). Supplied by the rules
/// engine (#31); until then every file is unclassified.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Risk {
    Safe,
    Review,
    Keep,
    Never,
}

/// Which files take part. The default admits every counted file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct FileFilter {
    /// Only these categories; empty means all.
    pub categories: Vec<Category>,
    /// At least this many allocated bytes.
    pub min_size: u64,
    /// Modified at or before this unix time. Files with no known time fail.
    pub modified_before: Option<i64>,
    /// Only files inside this folder; `None` means the whole scan.
    pub within: Option<NodeIndex>,
}

/// Everything a build reads. Only `arena` is required.
pub struct GroupingInput<'a> {
    pub arena: &'a Arena,
    /// The scanned root's absolute path; lets `App` recognise a scan that
    /// starts inside `~/Library` or `C:\Users\me\AppData`.
    pub root_path: Option<&'a Path>,
    /// Modified time per node (`catalog::modified_times`), for `Age` and the
    /// age filter. Without it every time is unknown.
    pub modified: Option<&'a [i64]>,
    /// "Now", unix seconds, for age buckets. A parameter so builds are
    /// deterministic.
    pub now: i64,
    /// Rule verdicts, for `Cleanliness`.
    pub risk: Option<&'a dyn Fn(NodeIndex) -> Option<Risk>>,
    pub filter: &'a FileFilter,
}

/// One node of a synthetic tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub label: String,
    /// The arena directory this group stands for, if any (Folder groups, App
    /// satellites): it can be revealed, and its id addresses it.
    pub entry: Option<NodeIndex>,
    /// Set for Type's category and extension groups.
    pub category: Option<Category>,
    /// Child group indices, ordered (see the module docs).
    pub children: Vec<u32>,
    /// Files directly in this group, in arena order.
    pub files: Vec<NodeIndex>,
    /// Allocated bytes of every file in the subtree.
    pub size: u64,
    pub file_count: u64,
}

impl Group {
    fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            entry: None,
            category: None,
            children: Vec::new(),
            files: Vec::new(),
            size: 0,
            file_count: 0,
        }
    }
}

/// A grouping of one scan. `groups[0]` is the root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntheticTree {
    pub grouping: Grouping,
    pub groups: Vec<Group>,
}

impl SyntheticTree {
    pub fn root(&self) -> &Group {
        &self.groups[0]
    }

    /// The group standing for arena node `entry`, if there is one.
    pub fn group_of_entry(&self, entry: NodeIndex) -> Option<u32> {
        self.groups
            .iter()
            .position(|group| group.entry == Some(entry))
            .map(|index| index as u32)
    }
}

/// Build `grouping` over `input`.
pub fn build(grouping: Grouping, input: &GroupingInput<'_>) -> SyntheticTree {
    let mut groups = match grouping {
        Grouping::Folder => folder(input),
        Grouping::Type => by_type(input),
        Grouping::App => by_app(input),
        Grouping::Age => bucketed(input, &AGE_LABELS, |index| age_bucket(input, index)),
        Grouping::Size => bucketed(input, &SIZE_LABELS, |index| {
            size_bucket(input.arena.nodes()[index as usize].size_alloc)
        }),
        Grouping::Cleanliness => bucketed(input, &RISK_LABELS, |index| {
            risk_bucket(input.risk.and_then(|risk| risk(index)))
        }),
    };
    finish(&mut groups, input.arena, grouping);
    SyntheticTree { grouping, groups }
}

// --- Shared steps ---------------------------------------------------------------------

/// Whether `index` is a counted file that passes the filter.
fn admitted(input: &GroupingInput<'_>, index: NodeIndex) -> bool {
    let node = &input.arena.nodes()[index as usize];
    if !node.counts() || node.size_alloc < input.filter.min_size {
        return false;
    }
    if input
        .filter
        .within
        .is_some_and(|top| !input.arena.is_within(index, top))
    {
        return false;
    }
    if !input.filter.categories.is_empty() && !input.filter.categories.contains(&node.category()) {
        return false;
    }
    match input.filter.modified_before {
        None => true,
        Some(cutoff) => modified(input, index).is_some_and(|time| time <= cutoff),
    }
}

fn modified(input: &GroupingInput<'_>, index: NodeIndex) -> Option<i64> {
    input
        .modified
        .and_then(|times| times.get(index as usize).copied())
        .filter(|&time| time != UNKNOWN_TIME)
}

/// The admitted files, in arena order.
fn files<'a>(input: &'a GroupingInput<'_>) -> impl Iterator<Item = NodeIndex> + 'a {
    (0..input.arena.len() as NodeIndex).filter(move |&index| admitted(input, index))
}

/// Sum totals bottom-up, then order and prune children.
fn finish(groups: &mut [Group], arena: &Arena, grouping: Grouping) {
    for group in groups.iter_mut() {
        group.size = group
            .files
            .iter()
            .map(|&file| arena.nodes()[file as usize].size_alloc)
            .sum();
        group.file_count = group.files.len() as u64;
    }
    // Children always come after their parent, so a reverse pass sees every
    // child's total before its parent's.
    for index in (0..groups.len()).rev() {
        let (size, count) = groups[index]
            .children
            .iter()
            .map(|&child| {
                (
                    groups[child as usize].size,
                    groups[child as usize].file_count,
                )
            })
            .fold((0, 0), |(size, count), (s, c)| (size + s, count + c));
        groups[index].size += size;
        groups[index].file_count += count;
    }

    let bucketed = matches!(
        grouping,
        Grouping::Age | Grouping::Size | Grouping::Cleanliness
    );
    for index in 0..groups.len() {
        let mut children = std::mem::take(&mut groups[index].children);
        children.retain(|&child| {
            let group = &groups[child as usize];
            // Type keeps its Other group whatever it holds.
            group.file_count > 0
                || (grouping == Grouping::Type
                    && group.category == Some(Category::Other)
                    && index == 0)
        });
        if !bucketed {
            children.sort_by(|&a, &b| {
                let (a, b) = (&groups[a as usize], &groups[b as usize]);
                b.size.cmp(&a.size).then_with(|| a.label.cmp(&b.label))
            });
        }
        groups[index].children = children;
    }
}

/// Root plus one child per label; `bucket` picks a label index per file.
fn bucketed(
    input: &GroupingInput<'_>,
    labels: &[&str],
    bucket: impl Fn(NodeIndex) -> usize,
) -> Vec<Group> {
    let mut groups = vec![Group::new("All files")];
    for (position, label) in labels.iter().enumerate() {
        groups.push(Group::new(*label));
        groups[0].children.push(position as u32 + 1);
    }
    for file in files(input) {
        let position = bucket(file).min(labels.len() - 1);
        groups[position + 1].files.push(file);
    }
    groups
}

// --- Folder -----------------------------------------------------------------------------

fn folder(input: &GroupingInput<'_>) -> Vec<Group> {
    let arena = input.arena;
    let nodes = arena.nodes();
    // Arena order is pre-order: a directory's parent always has its group.
    let mut group_of = vec![u32::MAX; nodes.len()];
    let mut groups: Vec<Group> = Vec::new();
    for (index, node) in nodes.iter().enumerate() {
        if node.kind() != EntryKind::Dir {
            continue;
        }
        let mut group = Group::new(arena.name(index as NodeIndex).unwrap_or_default());
        group.entry = Some(index as NodeIndex);
        let position = groups.len() as u32;
        if let Some(&parent) = group_of.get(node.parent as usize) {
            if parent != u32::MAX {
                groups[parent as usize].children.push(position);
            }
        }
        group_of[index] = position;
        groups.push(group);
    }
    if groups.is_empty() {
        groups.push(Group::new("Scan"));
    }
    for file in files(input) {
        let parent = nodes[file as usize].parent;
        let position = group_of.get(parent as usize).copied().unwrap_or(0);
        groups[if position == u32::MAX {
            0
        } else {
            position as usize
        }]
        .files
        .push(file);
    }
    groups
}

// --- Type -------------------------------------------------------------------------------

/// Lower-cased extension of `name`, or `None` for no extension (`Makefile`,
/// `.zshrc`, `archive.`).
pub fn extension_of(name: &str) -> Option<String> {
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty() && !ext.is_empty() && ext.len() <= 16).then(|| ext.to_lowercase())
}

fn by_type(input: &GroupingInput<'_>) -> Vec<Group> {
    let arena = input.arena;
    let mut groups = vec![Group::new("All files")];
    // Categories first, in Category::ALL order, so Other always exists.
    for category in Category::ALL {
        let mut group = Group::new(category.label());
        group.category = Some(category);
        groups.push(group);
        let position = groups.len() as u32 - 1;
        groups[0].children.push(position);
    }
    let category_group = |category: Category| {
        1 + Category::ALL
            .iter()
            .position(|known| *known == category)
            .unwrap_or(Category::ALL.len() - 1)
    };

    let mut extensions: HashMap<(usize, Option<String>), u32> = HashMap::new();
    for file in files(input) {
        let node = &arena.nodes()[file as usize];
        let parent = category_group(node.category());
        let ext = extension_of(arena.name(file).unwrap_or_default());
        let next = groups.len() as u32;
        let position = *extensions.entry((parent, ext.clone())).or_insert(next);
        if position == next {
            let mut group =
                Group::new(ext.map_or_else(|| "No extension".to_owned(), |ext| format!(".{ext}")));
            group.category = Some(node.category());
            groups.push(group);
            groups[parent].children.push(position);
        }
        groups[position as usize].files.push(file);
    }
    groups
}

// --- Age, size, cleanliness ---------------------------------------------------------------

const DAY: i64 = 86_400;

/// Age buckets by last modification, newest first; then unknown.
pub const AGE_LABELS: [&str; 6] = [
    "Under 1 month",
    "1–6 months",
    "6–12 months",
    "1–2 years",
    "Over 2 years",
    "Unknown date",
];

fn age_bucket(input: &GroupingInput<'_>, index: NodeIndex) -> usize {
    let Some(time) = modified(input, index) else {
        return 5;
    };
    // A time in the future (clock skew, odd archives) counts as new.
    let age = input.now.saturating_sub(time).max(0);
    match age {
        age if age < 30 * DAY => 0,
        age if age < 182 * DAY => 1,
        age if age < 365 * DAY => 2,
        age if age < 730 * DAY => 3,
        _ => 4,
    }
}

/// Size buckets, biggest first, in the binary units the UI shows.
pub const SIZE_LABELS: [&str; 4] = ["Over 10 GB", "1–10 GB", "100 MB–1 GB", "Under 100 MB"];

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

fn size_bucket(size: u64) -> usize {
    match size {
        size if size >= 10 * GIB => 0,
        size if size >= GIB => 1,
        size if size >= 100 * MIB => 2,
        _ => 3,
    }
}

/// Risk buckets (PLAN §10), then files no rule has judged.
pub const RISK_LABELS: [&str; 5] = [
    "Safe to remove",
    "Review",
    "Keep",
    "Never touch",
    "Unclassified",
];

fn risk_bucket(risk: Option<Risk>) -> usize {
    match risk {
        Some(Risk::Safe) => 0,
        Some(Risk::Review) => 1,
        Some(Risk::Keep) => 2,
        Some(Risk::Never) => 3,
        None => 4,
    }
}

// --- App --------------------------------------------------------------------------------

/// Where a directory sits, for recognising app data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Place {
    Plain,
    /// Inside an `Applications` folder: `*.app` children are apps.
    Applications,
    /// Inside a `Library` folder: some children hold per-app data.
    Library,
    /// Inside `AppData`.
    AppData,
    /// Children are per-app satellites (`~/Library/Caches`, `Program Files`).
    Satellites(SatelliteKind),
}

/// The kind of place an app's data lives in; labels its satellite groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SatelliteKind {
    Bundle,
    ApplicationSupport,
    Caches,
    Containers,
    GroupContainers,
    Preferences,
    Logs,
    SavedState,
    WebData,
    ProgramFiles,
    AppDataLocal,
    AppDataRoaming,
    ProgramData,
}

impl SatelliteKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Bundle => "App",
            Self::ApplicationSupport => "Application Support",
            Self::Caches => "Caches",
            Self::Containers => "Containers",
            Self::GroupContainers => "Group Containers",
            Self::Preferences => "Preferences",
            Self::Logs => "Logs",
            Self::SavedState => "Saved state",
            Self::WebData => "Web data",
            Self::ProgramFiles => "Program Files",
            Self::AppDataLocal => "AppData (Local)",
            Self::AppDataRoaming => "AppData (Roaming)",
            Self::ProgramData => "ProgramData",
        }
    }
}

fn library_child(name: &str) -> Option<SatelliteKind> {
    Some(match name.to_lowercase().as_str() {
        "application support" => SatelliteKind::ApplicationSupport,
        "caches" => SatelliteKind::Caches,
        "containers" => SatelliteKind::Containers,
        "group containers" => SatelliteKind::GroupContainers,
        "preferences" => SatelliteKind::Preferences,
        "logs" => SatelliteKind::Logs,
        "saved application state" => SatelliteKind::SavedState,
        "httpstorages" | "webkit" | "cookies" => SatelliteKind::WebData,
        _ => return None,
    })
}

/// Where a directory named `name` sits, given its parent's place.
fn place_of(parent: Place, name: &str) -> Place {
    let lower = name.to_lowercase();
    match (parent, lower.as_str()) {
        (_, "applications") => Place::Applications,
        (_, "library") => Place::Library,
        (_, "appdata") => Place::AppData,
        (_, "program files" | "program files (x86)") => {
            Place::Satellites(SatelliteKind::ProgramFiles)
        }
        (_, "programdata") => Place::Satellites(SatelliteKind::ProgramData),
        (Place::AppData, "local" | "locallow") => Place::Satellites(SatelliteKind::AppDataLocal),
        (Place::AppData, "roaming") => Place::Satellites(SatelliteKind::AppDataRoaming),
        (Place::Library, _) => library_child(name).map_or(Place::Plain, Place::Satellites),
        // `/Applications/Utilities/*.app`.
        (Place::Applications, _) => Place::Applications,
        _ => Place::Plain,
    }
}

fn is_bundle(name: &str) -> bool {
    name.len() > 4 && name.to_lowercase().ends_with(".app")
}

/// Words of a name, lower-cased, for matching satellites to apps.
fn words(name: &str) -> Vec<String> {
    name.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect()
}

/// Reverse-DNS prefixes that never name an app (`com.spotify.client`).
const DOMAIN_WORDS: [&str; 8] = ["com", "org", "net", "io", "app", "dev", "co", "de"];

/// Suffixes that are part of the file, not of the app's name.
const NAME_SUFFIXES: [&str; 4] = [".plist", ".savedstate", ".binarycookies", ".app"];

/// The key an app is known by: its name's words joined (`Google Chrome.app`
/// → `googlechrome`).
pub fn app_key(name: &str) -> String {
    let mut lower = name.to_lowercase();
    for suffix in NAME_SUFFIXES {
        if let Some(stripped) = lower.strip_suffix(suffix) {
            lower = stripped.to_owned();
        }
    }
    words(&lower).concat()
}

/// The app a satellite named `name` belongs to, among `apps` (keyed by
/// [`app_key`]). Tries the whole name, then every run of its words, longest
/// first, skipping reverse-DNS domain words — so `com.google.Chrome` finds
/// `Google Chrome` and `com.spotify.client` finds `Spotify`.
pub fn resolve_owner<'a, V>(name: &str, apps: &'a HashMap<String, V>) -> Option<&'a V> {
    if let Some(app) = apps.get(&app_key(name)) {
        return Some(app);
    }
    let mut lower = name.to_lowercase();
    for suffix in NAME_SUFFIXES {
        if let Some(stripped) = lower.strip_suffix(suffix) {
            lower = stripped.to_owned();
        }
    }
    let all = words(&lower);
    let start = all
        .iter()
        .take_while(|word| DOMAIN_WORDS.contains(&word.as_str()))
        .count();
    let words = &all[start..];
    for length in (1..=words.len()).rev() {
        for begin in 0..=words.len() - length {
            if let Some(app) = apps.get(&words[begin..begin + length].concat()) {
                return Some(app);
            }
        }
    }
    None
}

/// One place an app's data lives: its bundle, a cache folder, a plist.
struct Satellite {
    kind: SatelliteKind,
    name: String,
    entry: NodeIndex,
}

const NONE: u32 = u32::MAX;

fn by_app(input: &GroupingInput<'_>) -> Vec<Group> {
    let arena = input.arena;
    let nodes = arena.nodes();

    // Where the scan root's parent sits: walk the root path's components
    // (all but the root itself) like directories. If one of them is already an
    // app's satellite or bundle, the whole scan belongs to it.
    let mut root_place = Place::Plain;
    let mut root_owner: Option<(SatelliteKind, String)> = None;
    if let Some(path) = input.root_path {
        let names: Vec<String> = path
            .components()
            .map(|component| component.as_os_str().to_string_lossy().into_owned())
            .collect();
        for name in names.iter().take(names.len().saturating_sub(1)) {
            if let Place::Satellites(kind) = root_place {
                root_owner = Some((kind, name.clone()));
                break;
            }
            if is_bundle(name) {
                root_owner = Some((SatelliteKind::Bundle, name.clone()));
                break;
            }
            root_place = place_of(root_place, name);
        }
    }

    // Pass 1, pre-order: each directory's place and owning satellite.
    let mut place = vec![Place::Plain; nodes.len()];
    let mut owner = vec![NONE; nodes.len()];
    let mut satellites: Vec<Satellite> = Vec::new();
    let mut new_satellite = |kind, entry: NodeIndex, name: Option<String>| {
        satellites.push(Satellite {
            kind,
            name: name.unwrap_or_else(|| arena.name(entry).unwrap_or_default().to_owned()),
            entry,
        });
        satellites.len() as u32 - 1
    };
    let root = arena.root();
    for (index, node) in nodes.iter().enumerate() {
        let index = index as NodeIndex;
        let name = arena.name(index).unwrap_or_default();
        let kind = node.kind();
        if index == root {
            if kind == EntryKind::Dir {
                owner[index as usize] = match root_owner.take() {
                    Some((kind, ancestor)) => new_satellite(kind, index, Some(ancestor)),
                    None if is_bundle(name) => new_satellite(SatelliteKind::Bundle, index, None),
                    None => match root_place {
                        Place::Satellites(kind) => new_satellite(kind, index, None),
                        _ => NONE,
                    },
                };
                place[index as usize] = place_of(root_place, name);
            }
            continue;
        }
        let parent_place = place
            .get(node.parent as usize)
            .copied()
            .unwrap_or(Place::Plain);
        let parent_owner = owner.get(node.parent as usize).copied().unwrap_or(NONE);
        if kind == EntryKind::Dir {
            owner[index as usize] = if parent_owner != NONE {
                parent_owner
            } else if is_bundle(name) {
                new_satellite(SatelliteKind::Bundle, index, None)
            } else if let Place::Satellites(kind) = parent_place {
                new_satellite(kind, index, None)
            } else {
                NONE
            };
            place[index as usize] = place_of(parent_place, name);
        } else if kind == EntryKind::File && parent_owner == NONE {
            // A file straight inside a satellite root (Preferences/com.x.plist)
            // is a satellite of its own.
            if let Place::Satellites(kind) = parent_place {
                if admitted(input, index) {
                    owner[index as usize] = new_satellite(kind, index, None);
                }
            }
        }
    }

    // Pass 2: apps are the bundles; every other satellite joins the app it
    // names, or forms a group named after itself.
    let mut groups = vec![Group::new("All files"), Group::new("Not app data")];
    groups[0].children.push(1);
    let mut app_groups: HashMap<String, u32> = HashMap::new();
    fn add_app(
        groups: &mut Vec<Group>,
        app_groups: &mut HashMap<String, u32>,
        key: String,
        label: String,
    ) -> u32 {
        *app_groups.entry(key).or_insert_with(|| {
            groups.push(Group::new(label));
            let position = groups.len() as u32 - 1;
            groups[0].children.push(position);
            position
        })
    }
    let mut satellite_app = vec![0u32; satellites.len()];
    for (position, satellite) in satellites.iter().enumerate() {
        if satellite.kind == SatelliteKind::Bundle {
            let stem = satellite.name.len().saturating_sub(4);
            let label = if is_bundle(&satellite.name) {
                satellite.name[..stem].to_owned()
            } else {
                satellite.name.clone()
            };
            satellite_app[position] = add_app(
                &mut groups,
                &mut app_groups,
                app_key(&satellite.name),
                label,
            );
        }
    }
    let bundles: HashMap<String, u32> = app_groups.clone();
    for (position, satellite) in satellites.iter().enumerate() {
        if satellite.kind != SatelliteKind::Bundle {
            satellite_app[position] = match resolve_owner(&satellite.name, &bundles) {
                Some(&app) => app,
                None => {
                    let label = satellite
                        .name
                        .strip_suffix(".plist")
                        .unwrap_or(&satellite.name)
                        .to_owned();
                    add_app(
                        &mut groups,
                        &mut app_groups,
                        app_key(&satellite.name),
                        label,
                    )
                }
            };
        }
    }

    // One group per satellite, under its app.
    let mut satellite_group = vec![0u32; satellites.len()];
    for (position, satellite) in satellites.iter().enumerate() {
        let mut group = Group::new(format!("{} · {}", satellite.kind.label(), satellite.name));
        if nodes[satellite.entry as usize].kind() == EntryKind::Dir {
            group.entry = Some(satellite.entry);
        }
        groups.push(group);
        let index = groups.len() as u32 - 1;
        groups[satellite_app[position] as usize]
            .children
            .push(index);
        satellite_group[position] = index;
    }

    for file in files(input) {
        let own = owner[file as usize];
        let parent = owner
            .get(nodes[file as usize].parent as usize)
            .copied()
            .unwrap_or(NONE);
        let satellite = if own != NONE { own } else { parent };
        let group = if satellite == NONE {
            1
        } else {
            satellite_group[satellite as usize] as usize
        };
        groups[group].files.push(file);
    }
    groups
}
