//! File-type enrichment from the Windows registry, read once at startup.
//!
//! PLAN §9.3: the shared static table in `core/categories.rs` is the primary and
//! only per-file classifier. The registry is an **optional enrichment layer** that
//! fills its gaps — extensions the table maps to [`Category::Other`] — and never
//! overrides an extension the table knows. That keeps AGENTS.md invariant 6 (same
//! file, same category on both OSes) for every extension the table covers, and
//! only ever turns an `Other` into something more specific.
//!
//! # One pass, never per file
//!
//! [`load`] enumerates `HKEY_CLASSES_ROOT` once, opens each `.ext` key, and reads
//! two values:
//!
//! - `PerceivedType` — `image`, `video`, `audio`, `text`, `document`,
//!   `compressed`, `application`, … — preferred, because it is a category.
//! - `Content Type` — a MIME type, used when `PerceivedType` is absent.
//!
//! The result is a plain map handed to the classifier before any scan starts
//! (`Category::install_overrides`). The scan loop never touches the registry.
//!
//! Everything except [`load`] is pure and unit-tested on every runner.

use std::collections::HashMap;
use std::time::Duration;

use storagelens_core::Category;

/// Longest extension kept, matching the classifier's own key limit.
const MAX_EXTENSION_LEN: usize = 32;

/// What one startup pass found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RegistryOverrides {
    /// Gap-filling categories, keyed by lowercased extension without the dot.
    pub categories: HashMap<String, Category>,
    /// `.ext` keys examined.
    pub extensions_seen: usize,
    /// How long the pass took. PLAN budget: under 50 ms.
    pub elapsed: Duration,
}

/// Map a `PerceivedType` value to a category.
///
/// `system` and `unknown` (and anything unrecognised) carry no useful signal and
/// return `None`, leaving the extension in `Other`.
pub fn category_from_perceived_type(value: &str) -> Option<Category> {
    Some(match value.trim().to_ascii_lowercase().as_str() {
        "image" => Category::Images,
        "video" => Category::Video,
        "audio" => Category::Audio,
        "text" | "document" => Category::Documents,
        "compressed" => Category::Archives,
        "application" => Category::Apps,
        _ => return None,
    })
}

/// Map a MIME `Content Type` to a category.
///
/// Top-level types decide most cases; a few `application/*` subtypes are common
/// enough on Windows to name. Anything else returns `None`.
pub fn category_from_content_type(mime: &str) -> Option<Category> {
    let mime = mime.trim().to_ascii_lowercase();
    let (top, sub) = mime.split_once('/')?;
    let sub = sub.split(';').next().unwrap_or(sub).trim();

    Some(match top {
        "image" => Category::Images,
        "video" => Category::Video,
        "audio" => Category::Audio,
        "font" => Category::Fonts,
        "model" => Category::ThreeD,
        "text" => Category::Documents,
        "application" => match sub {
            "pdf" | "rtf" | "msword" | "epub+zip" => Category::Documents,
            "zip" | "gzip" | "x-gzip" | "x-tar" | "x-7z-compressed" | "x-rar-compressed"
            | "vnd.rar" | "x-bzip2" | "x-xz" | "x-zip-compressed" | "x-compressed" => {
                Category::Archives
            }
            "json" | "xml" | "x-sqlite3" | "vnd.sqlite3" => Category::Data,
            "x-msdownload" | "x-msdos-program" | "x-ms-installer" | "x-msi" => Category::Apps,
            "x-iso9660-image" | "x-raw-disk-image" => Category::DiskImages,
            other if other.contains("spreadsheet") || other.contains("ms-excel") => {
                Category::Spreadsheets
            }
            other if other.contains("presentation") || other.contains("ms-powerpoint") => {
                Category::Presentations
            }
            other if other.contains("wordprocessing") || other.contains("opendocument.text") => {
                Category::Documents
            }
            _ => return None,
        },
        _ => return None,
    })
}

/// Combine the two values, `PerceivedType` first.
pub fn classify(perceived: Option<&str>, content_type: Option<&str>) -> Option<Category> {
    perceived
        .and_then(category_from_perceived_type)
        .or_else(|| content_type.and_then(category_from_content_type))
}

/// `.JPG` → `jpg`. `None` for a key that is not a usable single extension.
pub fn normalize_extension(key: &str) -> Option<String> {
    let extension = key.strip_prefix('.')?;
    let valid = !extension.is_empty()
        && extension.len() <= MAX_EXTENSION_LEN
        && extension
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'+' | b'~'));
    valid.then(|| extension.to_ascii_lowercase())
}

/// Keep only the entries that fill a gap in the static table.
///
/// An extension the table already classifies is dropped, whatever the registry
/// says, and so is any `Other`, which would add nothing.
pub fn gap_overrides(
    found: impl IntoIterator<Item = (String, Category)>,
) -> HashMap<String, Category> {
    found
        .into_iter()
        .filter(|(extension, category)| {
            *category != Category::Other && Category::for_extension(extension) == Category::Other
        })
        .collect()
}

#[cfg(windows)]
pub use sys::load;

#[cfg(windows)]
mod sys {
    use std::time::Instant;

    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegEnumKeyExW, RegGetValueW, RegOpenKeyExW, HKEY, HKEY_CLASSES_ROOT, KEY_READ,
        RRF_RT_REG_SZ,
    };

    use super::{classify, gap_overrides, normalize_extension, RegistryOverrides};

    /// Room for any value this module reads; longer values are not file types.
    const VALUE_UNITS: usize = 256;

    fn wide_c(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// A `REG_SZ` value of an open key, or `None` if absent or unreadable.
    fn read_string(key: HKEY, value: &[u16]) -> Option<String> {
        let mut buffer = [0u16; VALUE_UNITS];
        let mut bytes = (buffer.len() * 2) as u32;
        // SAFETY: `key` is open, `value` is NUL-terminated, and `buffer` is a
        // writable allocation of `bytes` bytes. `RRF_RT_REG_SZ` makes the API
        // NUL-terminate the result.
        let status = unsafe {
            RegGetValueW(
                key,
                PCWSTR::null(),
                PCWSTR(value.as_ptr()),
                RRF_RT_REG_SZ,
                None,
                Some(buffer.as_mut_ptr().cast()),
                Some(&mut bytes),
            )
        };
        if status != ERROR_SUCCESS {
            return None;
        }
        let units = (bytes as usize / 2).min(buffer.len());
        let end = buffer[..units]
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(units);
        Some(String::from_utf16_lossy(&buffer[..end]))
    }

    /// Enumerate `HKEY_CLASSES_ROOT\.ext` once and keep the gap-filling entries.
    ///
    /// Never fails: an unreadable registry simply yields no overrides, and the
    /// static table still classifies every file.
    pub fn load() -> RegistryOverrides {
        let started = Instant::now();
        let perceived_name = wide_c("PerceivedType");
        let content_name = wide_c("Content Type");

        let mut found = Vec::new();
        let mut extensions_seen = 0usize;
        let mut name = [0u16; 256];

        for index in 0u32.. {
            let mut length = name.len() as u32;
            // SAFETY: HKEY_CLASSES_ROOT is a predefined key that is always open;
            // `name` is writable and `length` holds its capacity in characters.
            let status = unsafe {
                RegEnumKeyExW(
                    HKEY_CLASSES_ROOT,
                    index,
                    Some(PWSTR(name.as_mut_ptr())),
                    &mut length,
                    None,
                    None,
                    None,
                    None,
                )
            };
            if status == ERROR_NO_MORE_ITEMS {
                break;
            }
            // A name longer than 255 characters is a ProgID or CLSID, never an
            // extension.
            if status == ERROR_MORE_DATA {
                continue;
            }
            if status != ERROR_SUCCESS {
                break;
            }

            let key_name = &name[..length as usize];
            if key_name.first() != Some(&u16::from(b'.')) {
                continue;
            }
            let Some(extension) = normalize_extension(&String::from_utf16_lossy(key_name)) else {
                continue;
            };
            extensions_seen += 1;

            let mut subkey_name = key_name.to_vec();
            subkey_name.push(0);
            let mut subkey = HKEY::default();
            // SAFETY: `subkey_name` is NUL-terminated and `subkey` is writable.
            let opened = unsafe {
                RegOpenKeyExW(
                    HKEY_CLASSES_ROOT,
                    PCWSTR(subkey_name.as_ptr()),
                    None,
                    KEY_READ,
                    &mut subkey,
                )
            };
            if opened != ERROR_SUCCESS {
                continue;
            }

            let perceived = read_string(subkey, &perceived_name);
            let content = read_string(subkey, &content_name);
            // SAFETY: `subkey` was opened above and is closed exactly once.
            let _ = unsafe { RegCloseKey(subkey) };

            if let Some(category) = classify(perceived.as_deref(), content.as_deref()) {
                found.push((extension, category));
            }
        }

        RegistryOverrides {
            categories: gap_overrides(found),
            extensions_seen,
            elapsed: started.elapsed(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perceived_types_map_to_categories() {
        assert_eq!(
            category_from_perceived_type("image"),
            Some(Category::Images)
        );
        assert_eq!(category_from_perceived_type("Video"), Some(Category::Video));
        assert_eq!(
            category_from_perceived_type(" audio "),
            Some(Category::Audio)
        );
        assert_eq!(
            category_from_perceived_type("text"),
            Some(Category::Documents)
        );
        assert_eq!(
            category_from_perceived_type("compressed"),
            Some(Category::Archives)
        );
        assert_eq!(
            category_from_perceived_type("application"),
            Some(Category::Apps)
        );
        assert_eq!(category_from_perceived_type("system"), None);
        assert_eq!(category_from_perceived_type("unknown"), None);
        assert_eq!(category_from_perceived_type(""), None);
    }

    #[test]
    fn content_types_map_by_top_level_then_subtype() {
        let cases = [
            ("image/x-icon", Some(Category::Images)),
            ("video/mp2t", Some(Category::Video)),
            ("audio/x-ms-wma", Some(Category::Audio)),
            ("font/woff2", Some(Category::Fonts)),
            ("model/gltf+json", Some(Category::ThreeD)),
            ("text/plain; charset=utf-8", Some(Category::Documents)),
            ("application/pdf", Some(Category::Documents)),
            ("application/x-7z-compressed", Some(Category::Archives)),
            ("application/json", Some(Category::Data)),
            ("application/x-msdownload", Some(Category::Apps)),
            ("application/x-iso9660-image", Some(Category::DiskImages)),
            (
                "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
                Some(Category::Spreadsheets),
            ),
            (
                "application/vnd.ms-powerpoint",
                Some(Category::Presentations),
            ),
            ("application/octet-stream", None),
            ("x-world/x-vrml", None),
            ("not a mime type", None),
        ];
        for (mime, expected) in cases {
            assert_eq!(category_from_content_type(mime), expected, "{mime}");
        }
    }

    #[test]
    fn perceived_type_wins_over_content_type() {
        assert_eq!(
            classify(Some("video"), Some("application/octet-stream")),
            Some(Category::Video)
        );
        assert_eq!(
            classify(Some("system"), Some("image/png")),
            Some(Category::Images),
            "an uninformative PerceivedType falls through to the MIME type"
        );
        assert_eq!(classify(None, None), None);
    }

    #[test]
    fn only_single_extension_keys_are_kept() {
        assert_eq!(normalize_extension(".JPG").as_deref(), Some("jpg"));
        assert_eq!(normalize_extension(".c++").as_deref(), Some("c++"));
        assert_eq!(normalize_extension("jpg"), None, "ProgIDs have no dot");
        assert_eq!(normalize_extension("."), None);
        assert_eq!(normalize_extension(".tar.gz"), None);
        assert_eq!(normalize_extension(".has space"), None);
        assert_eq!(normalize_extension(&format!(".{}", "x".repeat(33))), None);
    }

    #[test]
    fn overrides_only_fill_gaps_in_the_static_table() {
        let found = vec![
            // Known to the table: the registry must not change it.
            ("jpg".to_owned(), Category::Video),
            ("mp4".to_owned(), Category::Images),
            // An `Other` adds nothing.
            ("zzslunknown".to_owned(), Category::Other),
            // A genuine gap.
            ("zzslgap".to_owned(), Category::Audio),
        ];
        let overrides = gap_overrides(found);

        assert_eq!(overrides.len(), 1, "{overrides:?}");
        assert_eq!(overrides.get("zzslgap"), Some(&Category::Audio));
    }

    /// The real pass, against the runner's registry.
    #[cfg(windows)]
    #[test]
    fn the_registry_pass_is_fast_and_only_fills_gaps() {
        let overrides = load();
        eprintln!(
            "registry: {} .ext keys, {} gap overrides, {:?}",
            overrides.extensions_seen,
            overrides.categories.len(),
            overrides.elapsed
        );

        assert!(
            overrides.extensions_seen > 50,
            "every Windows install registers dozens of extensions"
        );
        for (extension, category) in &overrides.categories {
            assert_eq!(
                Category::for_extension(extension),
                Category::Other,
                "{extension} is in the static table and must not be overridden"
            );
            assert_ne!(*category, Category::Other);
        }
        // PLAN: < 50 ms on a desktop. CI runners are slower and this is a debug
        // build, so the hard limit here only catches a pathological regression.
        assert!(
            overrides.elapsed < Duration::from_millis(500),
            "registry pass took {:?}",
            overrides.elapsed
        );
    }
}
