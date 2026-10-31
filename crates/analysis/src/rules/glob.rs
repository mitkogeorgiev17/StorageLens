//! Path globs for cleanup rules, matched one path component at a time.
//!
//! A glob is split into components. `**` matches any number of components
//! (including none); within a component, `*` matches any run of characters
//! and `?` exactly one byte. `{a,b}` alternatives are expanded when the glob is
//! compiled, so `**/{.venv,venv}` becomes two globs. Matching ignores ASCII
//! case: both macOS (by default) and Windows file systems do.
//!
//! The engine walks the scan tree from the root, so a glob is a small state
//! machine: each node's state set is its parent's, stepped by the node's name.
//! That makes a whole-tree evaluation one pass, with no path strings built.

/// One component of a compiled glob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// `**`: zero or more components.
    Any,
    /// A component pattern, with `*` and `?`.
    Name(String),
}

/// A compiled glob: absolute, components only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Glob {
    pub segments: Vec<Segment>,
}

/// Why a glob was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GlobError {
    /// Not absolute and not starting with `**`.
    Relative(String),
    /// `{` without a matching `}`, or a `/` inside braces.
    Braces(String),
    /// A `.` or `..` component.
    Traversal(String),
}

impl std::fmt::Display for GlobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Relative(glob) => {
                write!(
                    f,
                    "\"{glob}\" must be absolute (or start with **/) after expansion"
                )
            }
            Self::Braces(glob) => write!(f, "\"{glob}\" has unbalanced or multi-folder braces"),
            Self::Traversal(glob) => write!(f, "\"{glob}\" contains . or .."),
        }
    }
}

/// The components of a path or glob: `\` and `/` both separate, empty parts
/// are dropped. `C:\Users` gives `["C:", "Users"]`, `/Users` gives `["Users"]`.
pub fn components(path: &str) -> impl Iterator<Item = &str> + Clone {
    path.split(['/', '\\']).filter(|part| !part.is_empty())
}

/// Whether `text` starts like an absolute path: `/`, `\\`, or a drive letter.
fn is_absolute(text: &str) -> bool {
    let bytes = text.as_bytes();
    text.starts_with('/')
        || text.starts_with('\\')
        || (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
}

/// Expand every `{a,b}` group into separate strings.
fn expand_braces(glob: &str) -> Result<Vec<String>, GlobError> {
    let Some(open) = glob.find('{') else {
        if glob.contains('}') {
            return Err(GlobError::Braces(glob.to_owned()));
        }
        return Ok(vec![glob.to_owned()]);
    };
    let close = glob[open..]
        .find('}')
        .map(|offset| open + offset)
        .ok_or_else(|| GlobError::Braces(glob.to_owned()))?;
    let inner = &glob[open + 1..close];
    if inner.contains(['{', '/', '\\']) {
        return Err(GlobError::Braces(glob.to_owned()));
    }
    let mut out = Vec::new();
    for choice in inner.split(',') {
        let joined = format!("{}{choice}{}", &glob[..open], &glob[close + 1..]);
        out.extend(expand_braces(&joined)?);
    }
    Ok(out)
}

impl Glob {
    /// Compile `glob` (already variable-expanded) into one glob per brace
    /// alternative.
    pub fn compile(glob: &str) -> Result<Vec<Glob>, GlobError> {
        expand_braces(glob)?
            .into_iter()
            .map(|text| {
                if !is_absolute(&text) && !text.starts_with("**") {
                    return Err(GlobError::Relative(glob.to_owned()));
                }
                let mut segments = Vec::new();
                for part in components(&text) {
                    if part == "." || part == ".." {
                        return Err(GlobError::Traversal(glob.to_owned()));
                    }
                    let segment = if part == "**" {
                        Segment::Any
                    } else {
                        Segment::Name(part.to_owned())
                    };
                    // `**/**` is `**`.
                    if !(segment == Segment::Any && segments.last() == Some(&Segment::Any)) {
                        segments.push(segment);
                    }
                }
                Ok(Glob { segments })
            })
            .collect()
    }

    /// The number of segments; a state equal to it accepts.
    pub fn len(&self) -> usize {
        self.segments.len()
    }

    /// Whether the glob has no segments (matches only the filesystem root).
    pub fn is_empty(&self) -> bool {
        self.segments.is_empty()
    }

    /// Add `state` and every state reachable from it without consuming a
    /// component (skipping `**`s) to `out`.
    pub fn close(&self, mut state: usize, out: &mut Vec<usize>) {
        loop {
            out.push(state);
            match self.segments.get(state) {
                Some(Segment::Any) => state += 1,
                _ => return,
            }
        }
    }

    /// Step `state` (already closed) by one component `name`, adding the
    /// closed results to `out`.
    pub fn step(&self, state: usize, name: &str, out: &mut Vec<usize>) {
        match self.segments.get(state) {
            Some(Segment::Any) => self.close(state, out),
            Some(Segment::Name(pattern)) if wildcard(pattern, name) => self.close(state + 1, out),
            _ => {}
        }
    }
}

/// `*` and `?` matching on bytes, ignoring ASCII case.
pub fn wildcard(pattern: &str, text: &str) -> bool {
    let (pattern, text) = (pattern.as_bytes(), text.as_bytes());
    let (mut p, mut t) = (0, 0);
    // Where the last `*` was, and the text position it is currently covering up to.
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some(b'*') => {
                star = Some((p, t));
                p += 1;
            }
            Some(&c) if c == b'?' || c.eq_ignore_ascii_case(&text[t]) => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((star_p, star_t)) => {
                    p = star_p + 1;
                    t = star_t + 1;
                    star = Some((star_p, star_t + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == b'*')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(glob: &str, path: &str) -> bool {
        Glob::compile(glob).expect("glob").iter().any(|glob| {
            let mut states = Vec::new();
            glob.close(0, &mut states);
            for name in components(path) {
                let mut next = Vec::new();
                for &state in &states {
                    glob.step(state, name, &mut next);
                }
                states = next;
            }
            states.contains(&glob.len())
        })
    }

    #[test]
    fn wildcards_match_within_a_component() {
        assert!(wildcard("*.dmg", "Installer.DMG"));
        assert!(wildcard("thumbcache_*.db", "thumbcache_256.db"));
        assert!(wildcard("a?c", "abc"));
        assert!(!wildcard("a?c", "ac"));
        assert!(wildcard("*", ""));
        assert!(wildcard("**", "x"));
        assert!(!wildcard("*.dmg", "dmg"));
        assert!(wildcard("a*b*c", "aXXbYYc"));
        assert!(!wildcard("a*b*c", "aXXbYY"));
        assert!(!wildcard("abc", "abcd"));
        assert!(!wildcard("abcd", "abc"));
    }

    #[test]
    fn double_star_spans_any_number_of_folders() {
        assert!(matches("**/node_modules", "/Users/me/app/node_modules"));
        assert!(matches("**/node_modules", "/node_modules"));
        assert!(!matches("**/node_modules", "/Users/me/app/node_modules/x"));
        assert!(matches(
            "/Users/*/Library/Caches/*",
            "/Users/me/Library/Caches/com.x"
        ));
        assert!(!matches(
            "/Users/*/Library/Caches/*",
            "/Users/me/Library/Caches"
        ));
        assert!(matches("/a/**/b", "/a/b"));
        assert!(matches("/a/**/b", "/a/x/y/b"));
        assert!(matches("/a/**/**/b", "/a/x/b"));
        assert!(matches(r"C:\Windows\Temp", "C:/windows/temp"));
        assert!(
            matches("/a/**", "/a"),
            "a trailing ** also matches the folder itself"
        );
    }

    #[test]
    fn braces_expand_into_alternatives() {
        assert_eq!(Glob::compile("/d/*.{dmg,pkg,zip}").expect("glob").len(), 3);
        assert!(matches("/d/*.{dmg,pkg}", "/d/x.pkg"));
        assert!(!matches("/d/*.{dmg,pkg}", "/d/x.zip"));
        assert_eq!(Glob::compile("/{a,b}/{c,d}").expect("glob").len(), 4);
    }

    #[test]
    fn bad_globs_are_rejected() {
        for (glob, error) in [
            ("node_modules", GlobError::Relative("node_modules".into())),
            ("/a/{b", GlobError::Braces("/a/{b".into())),
            ("/a/b}", GlobError::Braces("/a/b}".into())),
            ("/a/{b/c}", GlobError::Braces("/a/{b/c}".into())),
            ("/a/{b,{c}}", GlobError::Braces("/a/{b,{c}}".into())),
            ("/a/../b", GlobError::Traversal("/a/../b".into())),
            ("/a/./b", GlobError::Traversal("/a/./b".into())),
        ] {
            assert_eq!(Glob::compile(glob), Err(error.clone()), "{glob}");
            assert!(!error.to_string().is_empty());
        }
        let root = Glob::compile("/").expect("root");
        assert!(root[0].is_empty());
    }
}
