//! Shared extension → category table.
//!
//! One curated table, used identically on macOS and Windows. Asking the OS
//! (`UTType`, `SHGetFileInfo`) per file is a syscall disaster and produces different
//! answers on the two machines; see PLAN §9.3.
//!
//! Lookup is a single hash probe and does not allocate: the extension is lowercased
//! into a 32-byte stack buffer, then matched against the static table.
//!
//! # Platform enrichment
//!
//! PLAN §9.3 allows an OS source (the Windows registry today) to **fill gaps** in
//! the table, never to replace it. [`Category::install_overrides`] takes such a
//! map once, at startup, before any scan; it is consulted only when the static
//! table has no entry, so every extension the table knows still classifies the
//! same on both platforms (AGENTS.md invariant 6). An extension that would
//! otherwise be `Other` may become more specific on one machine — the documented
//! cost of enrichment.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

/// Longest extension we bother lowercasing on the stack. Anything longer is
/// certainly not in the table.
const MAX_EXTENSION_LEN: usize = 32;

/// The 18 top-level buckets from PLAN §9.5: 14 by extension, 4 by location
/// (#95). Keep this list small and stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    /// Raster and vector images.
    Images,
    /// Video files.
    Video,
    /// Audio files.
    Audio,
    /// Text documents, PDFs, e-books.
    Documents,
    /// Spreadsheets and tabular data files meant for humans.
    Spreadsheets,
    /// Slide decks.
    Presentations,
    /// Compressed archives and installable packages.
    Archives,
    /// Source code, scripts, and configuration.
    Code,
    /// Structured data: JSON, SQLite, Parquet, scientific formats.
    Data,
    /// Font files.
    Fonts,
    /// 3D scenes and CAD.
    ThreeD,
    /// Executables, bundles, and libraries.
    Apps,
    /// Disk images.
    DiskImages,
    /// Location (#95, [`crate::location`]): app and system caches, temp files.
    Caches,
    /// Location: apps' own data (`~/Library/Application Support`, `AppData`).
    AppData,
    /// Location: the operating system's own files.
    System,
    /// Location: build output, dependencies and toolchains.
    Developer,
    /// Everything unclassified. Always visible in the UI.
    Other,
}

impl Category {
    /// Every category, in display order. `Other` last, always.
    pub const ALL: [Self; 18] = [
        Self::Images,
        Self::Video,
        Self::Audio,
        Self::Documents,
        Self::Spreadsheets,
        Self::Presentations,
        Self::Archives,
        Self::Code,
        Self::Data,
        Self::Fonts,
        Self::ThreeD,
        Self::Apps,
        Self::DiskImages,
        Self::Caches,
        Self::AppData,
        Self::System,
        Self::Developer,
        Self::Other,
    ];

    /// Stable identifier, written to the `category` column and sent over IPC.
    /// Set by where a file lives (#95), never by its extension.
    pub const fn by_location(self) -> bool {
        matches!(
            self,
            Self::Caches | Self::AppData | Self::System | Self::Developer
        )
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Images => "images",
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Documents => "documents",
            Self::Spreadsheets => "spreadsheets",
            Self::Presentations => "presentations",
            Self::Archives => "archives",
            Self::Code => "code",
            Self::Data => "data",
            Self::Fonts => "fonts",
            Self::ThreeD => "3d",
            Self::Apps => "apps",
            Self::DiskImages => "disk-images",
            Self::Caches => "caches",
            Self::AppData => "app-data",
            Self::System => "system",
            Self::Developer => "developer",
            Self::Other => "other",
        }
    }

    /// Human-readable name for the UI.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Images => "Images",
            Self::Video => "Video",
            Self::Audio => "Audio",
            Self::Documents => "Documents",
            Self::Spreadsheets => "Spreadsheets",
            Self::Presentations => "Presentations",
            Self::Archives => "Archives",
            Self::Code => "Code",
            Self::Data => "Data",
            Self::Fonts => "Fonts",
            Self::ThreeD => "3D / CAD",
            Self::Apps => "Apps",
            Self::DiskImages => "Disk images",
            Self::Caches => "Caches",
            Self::AppData => "App data",
            Self::System => "System",
            Self::Developer => "Developer",
            Self::Other => "Other",
        }
    }

    /// Parse a value read from SQLite or the TSV table.
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "images" => Self::Images,
            "video" => Self::Video,
            "audio" => Self::Audio,
            "documents" => Self::Documents,
            "spreadsheets" => Self::Spreadsheets,
            "presentations" => Self::Presentations,
            "archives" => Self::Archives,
            "code" => Self::Code,
            "data" => Self::Data,
            "fonts" => Self::Fonts,
            "3d" => Self::ThreeD,
            "apps" => Self::Apps,
            "disk-images" => Self::DiskImages,
            "caches" => Self::Caches,
            "app-data" => Self::AppData,
            "system" => Self::System,
            "developer" => Self::Developer,
            "other" => Self::Other,
            _ => return None,
        })
    }

    /// Classify a bare extension (`"jpg"`, `"JPG"`, `".JPG"`, `"tar.gz"`).
    ///
    /// Only the **last** dot segment matters, so `.tar.gz` → `gz` → Archives.
    /// Unknown extensions return [`Category::Other`] — never a guess.
    pub fn for_extension(extension: &str) -> Self {
        let last = extension.rsplit('.').next().unwrap_or(extension);
        let last = last.trim_start_matches('.');

        let bytes = last.as_bytes();
        if bytes.is_empty() || bytes.len() > MAX_EXTENSION_LEN {
            return Self::Other;
        }

        let mut buffer = [0u8; MAX_EXTENSION_LEN];
        for (slot, byte) in buffer.iter_mut().zip(bytes) {
            *slot = byte.to_ascii_lowercase();
        }

        // ASCII lowercasing preserves UTF-8 validity, so this never fails; the
        // fallback is `Other` rather than a panic.
        let key = std::str::from_utf8(&buffer[..bytes.len()]).unwrap_or("");
        table()
            .get(key)
            .copied()
            .or_else(|| OVERRIDES.get()?.get(key).copied())
            .unwrap_or(Self::Other)
    }

    /// Install platform gap-fillers for extensions the static table does not know.
    ///
    /// Call once at startup, before scanning. Entries for extensions the table
    /// already maps, entries whose category is `Other`, and keys that are not a
    /// lowercased extension are dropped, so a caller cannot override the shared
    /// table even by mistake. Returns the number of overrides installed, or `None`
    /// if overrides were already installed (the first set wins; classification
    /// must not change mid-session).
    pub fn install_overrides(overrides: HashMap<String, Category>) -> Option<usize> {
        let filtered: HashMap<String, Category> = overrides
            .into_iter()
            .filter(|(extension, category)| {
                *category != Self::Other
                    && !extension.is_empty()
                    && extension.len() <= MAX_EXTENSION_LEN
                    && !extension.contains('.')
                    && *extension == extension.to_ascii_lowercase()
                    && !table().contains_key(extension.as_str())
            })
            .collect();
        let installed = filtered.len();
        OVERRIDES.set(filtered).ok().map(|()| installed)
    }

    /// Classify a path by its final extension, falling back to the whole filename.
    ///
    /// The fallback covers dotfiles (`.gitignore` → `gitignore`) and extensionless
    /// names (`Makefile`, `Dockerfile`, `README`). Allocation-free.
    pub fn for_path(path: &Path) -> Self {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return Self::Other;
        };

        if let Some((_, extension)) = name.rsplit_once('.') {
            if !extension.is_empty() {
                return Self::for_extension(extension);
            }
        }

        Self::for_name(name)
    }

    /// Classify a whole filename, for extensionless names and dotfiles.
    fn for_name(name: &str) -> Self {
        let bytes = name.as_bytes();
        if bytes.is_empty() || bytes.len() > MAX_EXTENSION_LEN {
            return Self::Other;
        }

        let mut buffer = [0u8; MAX_EXTENSION_LEN];
        for (slot, byte) in buffer.iter_mut().zip(bytes) {
            *slot = byte.to_ascii_lowercase();
        }

        let key = std::str::from_utf8(&buffer[..bytes.len()]).unwrap_or("");
        table().get(key).copied().unwrap_or(Self::Other)
    }
}

/// Lowercased final extension, suitable for the `ext` column. `None` when the
/// filename has no extension.
pub fn extension_of(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let (_, extension) = name.rsplit_once('.')?;
    if extension.is_empty() {
        return None;
    }
    Some(extension.to_ascii_lowercase())
}

/// Guess a category from the first bytes of a file, for files with no usable
/// extension or a lying one.
///
/// Pure function over a slice — no I/O, no allocation. The caller decides how much
/// of the file to read (PLAN §9.4 suggests the first 4 KB).
pub fn sniff_category(bytes: &[u8]) -> Option<Category> {
    if bytes.starts_with(b"%PDF") {
        return Some(Category::Documents);
    }
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Some(Category::Images);
    }
    if bytes.starts_with(b"PK\x03\x04")
        || bytes.starts_with(b"PK\x05\x06")
        || bytes.starts_with(b"PK\x07\x08")
    {
        return Some(Category::Archives);
    }
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        return Some(iso_media_category(&bytes[8..12]));
    }
    if bytes.starts_with(b"\xff\xd8\xff")
        || bytes.starts_with(b"GIF87a")
        || bytes.starts_with(b"GIF89a")
    {
        return Some(Category::Images);
    }
    if bytes.starts_with(b"\x1f\x8b")
        || bytes.starts_with(b"7z\xbc\xaf\x27\x1c")
        || bytes.starts_with(b"Rar!\x1a\x07")
    {
        return Some(Category::Archives);
    }
    if bytes.starts_with(b"SQLite format 3\0") {
        return Some(Category::Data);
    }
    if bytes.starts_with(b"\x7fELF") || bytes.starts_with(b"MZ") {
        return Some(Category::Apps);
    }
    if bytes.starts_with(b"ID3") || bytes.starts_with(b"OggS") || bytes.starts_with(b"fLaC") {
        return Some(Category::Audio);
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") {
        return Some(match &bytes[8..12] {
            b"WAVE" => Category::Audio,
            b"AVI " => Category::Video,
            b"WEBP" => Category::Images,
            _ => return None,
        });
    }
    None
}

/// `ftyp` brands. HEIF/AVIF are images; everything else in the ISO base media
/// family is treated as video (mp4, mov, m4v, 3gp).
fn iso_media_category(brand: &[u8]) -> Category {
    match brand {
        b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"mif1" | b"msf1" | b"avif"
        | b"avis" => Category::Images,
        _ => Category::Video,
    }
}

/// Gap-filling categories from the platform, installed once at startup.
static OVERRIDES: OnceLock<HashMap<String, Category>> = OnceLock::new();

fn table() -> &'static HashMap<&'static str, Category> {
    static TABLE: OnceLock<HashMap<&'static str, Category>> = OnceLock::new();

    TABLE.get_or_init(|| {
        let mut map = HashMap::with_capacity(768);

        for line in include_str!("../data/extensions.tsv").lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let (extension, category) = line
                .split_once('\t')
                .unwrap_or_else(|| panic!("extensions.tsv: missing tab in {line:?}"));
            let category = Category::from_name(category.trim()).unwrap_or_else(|| {
                panic!("extensions.tsv: unknown category {category:?} for {extension:?}")
            });

            let previous = map.insert(extension.trim(), category);
            assert!(
                previous.is_none(),
                "extensions.tsv: duplicate extension {:?}",
                extension.trim()
            );
        }

        map
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The only test that installs overrides: `OVERRIDES` is process-global, so
    /// every key here is unique to this test.
    #[test]
    fn overrides_fill_gaps_and_never_replace_the_table() {
        assert_eq!(Category::for_extension("zzslgapcore"), Category::Other);

        let overrides = HashMap::from([
            // A genuine gap: installed.
            ("zzslgapcore".to_owned(), Category::Audio),
            // Known to the table: dropped.
            ("jpg".to_owned(), Category::Video),
            // Adds nothing: dropped.
            ("zzslothercore".to_owned(), Category::Other),
            // Not a normalised extension: dropped.
            ("ZZSLUPPERCORE".to_owned(), Category::Video),
            ("zz.slcore".to_owned(), Category::Video),
        ]);
        assert_eq!(Category::install_overrides(overrides), Some(1));

        assert_eq!(Category::for_extension("zzslgapcore"), Category::Audio);
        assert_eq!(Category::for_extension("ZZSLGAPCORE"), Category::Audio);
        assert_eq!(
            Category::for_path(Path::new("song.zzslgapcore")),
            Category::Audio
        );
        assert_eq!(Category::for_extension("jpg"), Category::Images);
        assert_eq!(Category::for_extension("zzslupperco"), Category::Other);

        // The first set wins for the whole session.
        let again = HashMap::from([("zzslgapcore2".to_owned(), Category::Video)]);
        assert_eq!(Category::install_overrides(again), None);
        assert_eq!(Category::for_extension("zzslgapcore2"), Category::Other);
    }

    #[test]
    fn case_does_not_matter() {
        assert_eq!(Category::for_extension("JPG"), Category::Images);
        assert_eq!(Category::for_extension("jpg"), Category::Images);
        assert_eq!(Category::for_extension(".JpG"), Category::Images);
    }

    #[test]
    fn compound_extensions_use_the_last_segment() {
        assert_eq!(Category::for_extension("tar.gz"), Category::Archives);
        assert_eq!(Category::for_extension("tar.bz2"), Category::Archives);
        assert_eq!(
            Category::for_path(Path::new("backup.tar.gz")),
            Category::Archives
        );
    }

    #[test]
    fn unknown_is_other_and_never_a_guess() {
        assert_eq!(Category::for_extension("nope"), Category::Other);
        assert_eq!(Category::for_extension(""), Category::Other);
        assert_eq!(Category::for_extension("."), Category::Other);
        assert_eq!(
            Category::for_path(Path::new("mystery.blob")),
            Category::Other
        );
    }

    #[test]
    fn long_extensions_do_not_allocate_or_match() {
        let long = "x".repeat(MAX_EXTENSION_LEN + 1);
        assert_eq!(Category::for_extension(&long), Category::Other);
    }

    #[test]
    fn dotfiles_and_extensionless_names_classify() {
        assert_eq!(Category::for_path(Path::new(".gitignore")), Category::Code);
        assert_eq!(Category::for_path(Path::new(".env")), Category::Code);
        assert_eq!(Category::for_path(Path::new(".zshrc")), Category::Code);
        assert_eq!(Category::for_path(Path::new("Makefile")), Category::Code);
        assert_eq!(Category::for_path(Path::new("Dockerfile")), Category::Code);
        assert_eq!(Category::for_path(Path::new("README")), Category::Documents);
    }

    #[test]
    fn extension_of_normalises_and_handles_absence() {
        assert_eq!(
            extension_of(Path::new("/a/B/Photo.JPG")).as_deref(),
            Some("jpg")
        );
        assert_eq!(
            extension_of(Path::new("/a/archive.tar.GZ")).as_deref(),
            Some("gz")
        );
        assert_eq!(extension_of(Path::new("/a/Makefile")), None);
        assert_eq!(extension_of(Path::new("/a/trailing.")), None);
    }

    #[test]
    fn every_table_entry_maps_to_its_declared_category() {
        let table = table();
        assert!(
            table.len() >= 400,
            "expected a curated table of at least 400 entries, found {}",
            table.len()
        );

        let source = include_str!("../data/extensions.tsv");
        let mut counted = 0;
        for line in source.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (extension, category) = line.split_once('\t').expect("missing tab");
            let expected = Category::from_name(category.trim()).expect("unknown category");
            assert_eq!(
                Category::for_extension(extension),
                expected,
                "extension {extension:?} should be {expected:?}"
            );
            counted += 1;
        }
        assert_eq!(counted, table.len());
    }

    #[test]
    fn categories_have_stable_names_and_round_trip() {
        for category in Category::ALL {
            assert_eq!(Category::from_name(category.as_str()), Some(category));
            assert!(!category.label().is_empty());
        }
        assert_eq!(Category::from_name("nope"), None);
    }

    #[test]
    fn sniff_identifies_the_required_signatures() {
        assert_eq!(sniff_category(b"%PDF-1.7\n"), Some(Category::Documents));
        assert_eq!(
            sniff_category(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"),
            Some(Category::Images)
        );
        assert_eq!(sniff_category(b"PK\x03\x04rest"), Some(Category::Archives));
        assert_eq!(
            sniff_category(b"\0\0\0\x18ftypheic\0\0\0\0"),
            Some(Category::Images)
        );
        assert_eq!(
            sniff_category(b"\0\0\0\x18ftypmp42\0\0\0\0"),
            Some(Category::Video)
        );
    }

    #[test]
    fn sniff_is_conservative() {
        assert_eq!(sniff_category(b""), None);
        assert_eq!(sniff_category(b"hello world"), None);
        assert_eq!(sniff_category(b"\0\0\0\x18fty"), None);
    }

    #[test]
    fn sniff_handles_common_extras() {
        assert_eq!(sniff_category(b"GIF89a..."), Some(Category::Images));
        assert_eq!(sniff_category(b"\x1f\x8b\x08"), Some(Category::Archives));
        assert_eq!(sniff_category(b"SQLite format 3\0"), Some(Category::Data));
        assert_eq!(sniff_category(b"\x7fELF\x02\x01"), Some(Category::Apps));
        assert_eq!(sniff_category(b"ID3\x04"), Some(Category::Audio));
        assert_eq!(
            sniff_category(b"RIFF\0\0\0\0WEBPVP8 "),
            Some(Category::Images)
        );
    }
}
