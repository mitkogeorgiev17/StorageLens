//! Deterministic fixture trees for scanner tests.
//!
//! One tree that breaks every naive scanner: hard links, symlink loops, a sparse
//! file, unicode and RTL names, a 255-byte name, 200-level nesting, an unreadable
//! directory, and one file per category from [`crate::categories`].
//!
//! The generator computes the expected manifest **as it writes**, so the manifest
//! never depends on re-walking the tree. Compare a scanner's `ScanResult` against
//! it directly.
//!
//! Enabled by the `fixtures` feature, which is off by default — it never ships in
//! the app.
//!
//! # Platform notes
//!
//! - **Allocated bytes** are exact on Unix (`st_blocks * 512`). Windows `std`
//!   exposes no allocation size, so [`allocated_bytes`] returns the logical size
//!   there. On NTFS the two differ only for compressed and sparse files, and the
//!   sparse case is absent on Windows, so manifest totals still match a correct
//!   scan.
//! - The **sparse file** and the **permission-denied directory** are Unix-only:
//!   creating a sparse file or a deny ACL on Windows needs unsafe Win32 calls,
//!   and this crate forbids unsafe code.
//! - The **junction** is Windows-only and is created with `mklink /J`, which needs
//!   no privilege. Symlinks are attempted on both platforms and skipped when the
//!   OS refuses (Windows needs Developer Mode or SeCreateSymbolicLinkPrivilege).
//! - A path longer than 1,024 characters cannot be created on macOS/APFS at all
//!   (`PATH_MAX`); the deep tree is 200 levels and exceeds 1,024 characters on
//!   Windows only.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::categories::Category;

/// The sparse file's logical size: 4 GiB.
pub const SPARSE_LOGICAL_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// Nesting depth of the deep tree.
pub const DEEP_DEPTH: u32 = 200;

/// Length of the long filename in bytes, the maximum both APFS and NTFS allow.
pub const LONG_NAME_BYTES: usize = 255;

/// Everything that can go wrong while building a fixture tree.
#[derive(Debug, Error)]
pub enum FixtureError {
    /// A filesystem call failed.
    #[error("i/o error at {}: {source}", path.display())]
    Io {
        /// Path being operated on.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: io::Error,
    },

    /// The shared category table and the fixture disagree.
    #[error("{0}")]
    Other(String),
}

/// Convenience alias.
pub type FixtureResult<T> = std::result::Result<T, FixtureError>;

/// Extension used for the sample file of each category.
///
/// One file per [`Category::ALL`] entry. The generator verifies that the shared
/// table maps each extension back to the same category.
const CATEGORY_SAMPLES: [(Category, &str); 14] = [
    (Category::Images, "jpg"),
    (Category::Video, "mp4"),
    (Category::Audio, "flac"),
    (Category::Documents, "pdf"),
    (Category::Spreadsheets, "xlsx"),
    (Category::Presentations, "pptx"),
    (Category::Archives, "zip"),
    (Category::Code, "rs"),
    (Category::Data, "sqlite"),
    (Category::Fonts, "ttf"),
    (Category::ThreeD, "obj"),
    (Category::Apps, "dll"),
    (Category::DiskImages, "dmg"),
    (Category::Other, "blob"),
];

/// One category sample in the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CategorySample {
    /// File path.
    pub path: PathBuf,
    /// Category the shared table must assign.
    pub category: Category,
    /// Bytes written.
    pub bytes_logical: u64,
}

/// Named paths inside the tree, for tests that need a specific trap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixturePaths {
    /// A small regular file.
    pub plain_file: PathBuf,
    /// A zero-byte regular file.
    pub empty_file: PathBuf,
    /// The three hard-link names, all pointing at one inode.
    pub hardlinks: [PathBuf; 3],
    /// First half of the symlink loop, when the OS allowed it.
    pub symlink_loop: Option<[PathBuf; 2]>,
    /// A symlink pointing outside the tree, when the OS allowed it.
    pub symlink_outside: Option<PathBuf>,
    /// The sparse file (Unix only).
    pub sparse: Option<PathBuf>,
    /// A unicode filename.
    pub unicode: PathBuf,
    /// An emoji filename.
    pub emoji: PathBuf,
    /// A right-to-left filename.
    pub rtl: PathBuf,
    /// A filename of exactly [`LONG_NAME_BYTES`] bytes.
    pub long_name: PathBuf,
    /// The leaf of the 200-level tree.
    pub deep_path: PathBuf,
    /// A directory the process cannot read (Unix only).
    pub denied_dir: Option<PathBuf>,
    /// Empty directories.
    pub empty_dirs: Vec<PathBuf>,
    /// A junction pointing at the tree root (Windows only).
    pub junction: Option<PathBuf>,
}

/// The expected result of scanning a fixture tree.
///
/// Counts and totals exclude the root directory itself: `dirs` is the number of
/// directories *below* `root`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Tree root.
    pub root: PathBuf,
    /// Unique regular files. The three hard-link names count once.
    pub files: u64,
    /// Directories below the root.
    pub dirs: u64,
    /// Symlinks (never followed).
    pub symlinks: u64,
    /// Junctions (Windows only; never followed).
    pub junctions: u64,
    /// Allocated bytes over unique regular files.
    pub bytes_alloc: u64,
    /// Logical bytes over unique regular files.
    pub bytes_logical: u64,
    /// Number of directory entries that are hard links (3 in the default tree).
    pub hardlink_names: u64,
    /// Logical size of the sparse file, when present.
    pub sparse_logical: u64,
    /// Allocated size of the sparse file, when present.
    pub sparse_alloc: u64,
    /// Depth of the deep tree.
    pub deep_depth: u32,
    /// One sample per category.
    pub category_samples: Vec<CategorySample>,
    /// Named paths.
    pub paths: FixturePaths,
}

impl Manifest {
    /// Allocated bytes per category over the category samples, for cross-checks.
    pub fn sample_bytes_by_category(&self) -> BTreeMap<Category, u64> {
        let mut totals = BTreeMap::new();
        for sample in &self.category_samples {
            *totals.entry(sample.category).or_insert(0) += sample.bytes_logical;
        }
        totals
    }
}

/// A generated tree, with its expected manifest.
///
/// When created with [`generate`](Self::generate), the tree owns a temporary
/// directory and removes it — including the read-only directory — on drop.
/// [`generate_in`](Self::generate_in) leaves removal to the caller.
#[derive(Debug)]
pub struct FixtureTree {
    manifest: Manifest,
    temp: Option<tempfile::TempDir>,
    cleanup: Vec<PathBuf>,
}

impl FixtureTree {
    /// Create a tree in a fresh temporary directory.
    pub fn generate() -> FixtureResult<Self> {
        let temp = tempfile::Builder::new()
            .prefix("storagelens-fixtures-")
            .tempdir()
            .map_err(|source| FixtureError::Io {
                path: std::env::temp_dir(),
                source,
            })?;

        let mut tree = Self::generate_in(temp.path())?;
        tree.temp = Some(temp);
        Ok(tree)
    }

    /// Create a tree under `root`, which must be empty or absent.
    ///
    /// The caller owns cleanup, but read-only permissions are still restored on
    /// drop.
    pub fn generate_in(root: impl AsRef<Path>) -> FixtureResult<Self> {
        let root = root.as_ref();
        fs::create_dir_all(root).map_err(|source| FixtureError::Io {
            path: root.to_path_buf(),
            source,
        })?;

        let mut builder = Builder::new(root.to_path_buf());
        builder.build()?;

        Ok(Self {
            manifest: builder.manifest,
            temp: None,
            cleanup: builder.cleanup,
        })
    }

    /// The expected manifest.
    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    /// The tree root.
    pub fn root(&self) -> &Path {
        &self.manifest.root
    }
}

impl Drop for FixtureTree {
    fn drop(&mut self) {
        // Restore permissions before `temp` removes the tree; otherwise the
        // unreadable directory makes `remove_dir_all` fail and leak the tree.
        for path in &self.cleanup {
            restore_permissions(path);
        }
    }
}

/// Measure a regular file's on-disk allocation.
///
/// Exact on Unix. On Windows `std` exposes no allocation size, so this returns the
/// logical size; see the module docs.
pub fn allocated_bytes(path: &Path) -> io::Result<u64> {
    let metadata = fs::metadata(path)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(metadata.blocks().saturating_mul(512))
    }

    #[cfg(not(unix))]
    {
        Ok(metadata.len())
    }
}

struct Builder {
    root: PathBuf,
    files: u64,
    dirs: u64,
    symlinks: u64,
    junctions: u64,
    bytes_alloc: u64,
    bytes_logical: u64,
    hardlink_names: u64,
    sparse_logical: u64,
    sparse_alloc: u64,
    category_samples: Vec<CategorySample>,
    cleanup: Vec<PathBuf>,
    manifest: Manifest,
}

impl Builder {
    fn new(root: PathBuf) -> Self {
        // The manifest is filled in at the end; this placeholder keeps the struct
        // simple without an `Option` dance.
        let placeholder = Manifest {
            root: root.clone(),
            files: 0,
            dirs: 0,
            symlinks: 0,
            junctions: 0,
            bytes_alloc: 0,
            bytes_logical: 0,
            hardlink_names: 0,
            sparse_logical: 0,
            sparse_alloc: 0,
            deep_depth: DEEP_DEPTH,
            category_samples: Vec::new(),
            paths: FixturePaths {
                plain_file: PathBuf::new(),
                empty_file: PathBuf::new(),
                hardlinks: [PathBuf::new(), PathBuf::new(), PathBuf::new()],
                symlink_loop: None,
                symlink_outside: None,
                sparse: None,
                unicode: PathBuf::new(),
                emoji: PathBuf::new(),
                rtl: PathBuf::new(),
                long_name: PathBuf::new(),
                deep_path: PathBuf::new(),
                denied_dir: None,
                empty_dirs: Vec::new(),
                junction: None,
            },
        };

        Self {
            root,
            files: 0,
            dirs: 0,
            symlinks: 0,
            junctions: 0,
            bytes_alloc: 0,
            bytes_logical: 0,
            hardlink_names: 0,
            sparse_logical: 0,
            sparse_alloc: 0,
            category_samples: Vec::new(),
            cleanup: Vec::new(),
            manifest: placeholder,
        }
    }

    fn build(&mut self) -> FixtureResult<()> {
        self.build_files()?;
        self.build_categories()?;
        self.build_hardlinks()?;
        self.build_symlinks()?;
        self.build_sparse()?;
        self.build_names()?;
        self.build_deep_tree()?;
        self.build_permissions()?;
        self.build_empty_dirs()?;
        self.build_junction()?;
        self.finish();
        Ok(())
    }

    fn build_files(&mut self) -> FixtureResult<()> {
        let files = self.dir("files")?;
        let plain = files.join("plain.txt");
        self.file(&plain, b"hello storage lens\n")?;
        let empty = files.join("empty.bin");
        self.file(&empty, b"")?;

        self.manifest.paths.plain_file = plain;
        self.manifest.paths.empty_file = empty;
        Ok(())
    }

    fn build_categories(&mut self) -> FixtureResult<()> {
        let dir = self.dir("categories")?;

        for (category, extension) in CATEGORY_SAMPLES {
            let mapped = Category::for_extension(extension);
            if mapped != category {
                return Err(FixtureError::Other(format!(
                    "shared table maps {extension:?} to {mapped:?}, fixture expects {category:?}"
                )));
            }

            let path = dir.join(format!("sample.{extension}"));
            let content = format!("sample for {}\n", category.as_str());
            self.file(&path, content.as_bytes())?;
            self.category_samples.push(CategorySample {
                path,
                category,
                bytes_logical: content.len() as u64,
            });
        }
        Ok(())
    }

    fn build_hardlinks(&mut self) -> FixtureResult<()> {
        let dir = self.dir("hardlinks")?;
        let first = dir.join("a.txt");
        self.file(&first, b"one inode, three names\n")?;

        let second = dir.join("b.txt");
        let third = dir.join("c.txt");
        fs::hard_link(&first, &second).ctx(&second)?;
        fs::hard_link(&first, &third).ctx(&third)?;

        self.hardlink_names = 3;
        self.manifest.paths.hardlinks = [first, second, third];
        Ok(())
    }

    fn build_symlinks(&mut self) -> FixtureResult<()> {
        let dir = self.dir("symlinks")?;
        let first = dir.join("a");
        let second = dir.join("b");

        // Relative targets make the loop independent of where the tree lives.
        if self.symlink(&first, Path::new("b"), true).is_ok()
            && self.symlink(&second, Path::new("a"), true).is_ok()
        {
            self.manifest.paths.symlink_loop = Some([first, second]);
        }

        let outside = dir.join("outside");
        if self
            .symlink(&outside, Path::new("../files/plain.txt"), false)
            .is_ok()
        {
            self.manifest.paths.symlink_outside = Some(outside);
        }
        Ok(())
    }

    #[cfg(unix)]
    fn build_sparse(&mut self) -> FixtureResult<()> {
        use std::io::Write;

        let dir = self.dir("sparse")?;
        let path = dir.join("big.img");

        let mut file = fs::File::create(&path).ctx(&path)?;
        file.set_len(SPARSE_LOGICAL_BYTES).ctx(&path)?;
        // Touch the first 8 KiB so the allocation is real but tiny.
        file.write_all(&[0u8; 8 * 1024]).ctx(&path)?;
        file.sync_all().ctx(&path)?;
        drop(file);

        let alloc = allocated_bytes(&path).ctx(&path)?;
        self.files += 1;
        self.bytes_logical = self.bytes_logical.saturating_add(SPARSE_LOGICAL_BYTES);
        self.bytes_alloc = self.bytes_alloc.saturating_add(alloc);
        self.sparse_logical = SPARSE_LOGICAL_BYTES;
        self.sparse_alloc = alloc;
        self.manifest.paths.sparse = Some(path);
        Ok(())
    }

    #[cfg(not(unix))]
    fn build_sparse(&mut self) -> FixtureResult<()> {
        // Sparse files need FSCTL_SET_SPARSE, which is unsafe Win32. The manifest
        // records that the case is absent rather than lying about the size.
        Ok(())
    }

    fn build_names(&mut self) -> FixtureResult<()> {
        let dir = self.dir("names")?;

        let unicode = dir.join("Фотографии-日本語-Ünïcode.txt");
        self.file(&unicode, "unicode\n".as_bytes())?;

        let emoji = dir.join("📷-🎉-🚀.txt");
        self.file(&emoji, "emoji\n".as_bytes())?;

        let rtl = dir.join("مرحبا-بالعالم.txt");
        self.file(&rtl, "rtl\n".as_bytes())?;

        let mut long = String::with_capacity(LONG_NAME_BYTES);
        long.push_str(&"a".repeat(LONG_NAME_BYTES - ".txt".len()));
        long.push_str(".txt");
        debug_assert_eq!(long.len(), LONG_NAME_BYTES);
        let long_name = dir.join(&long);
        self.file(&long_name, "long name\n".as_bytes())?;

        self.manifest.paths.unicode = unicode;
        self.manifest.paths.emoji = emoji;
        self.manifest.paths.rtl = rtl;
        self.manifest.paths.long_name = long_name;
        Ok(())
    }

    fn build_deep_tree(&mut self) -> FixtureResult<()> {
        // On Unix the absolute path must stay under PATH_MAX (1024), so the
        // components are one character. Windows supports long paths, so there
        // they are four characters and the final path exceeds 1,024.
        let component = if cfg!(windows) { "deep" } else { "d" };

        let mut path = self.dir("deep")?;
        for _ in 0..DEEP_DEPTH {
            path.push(component);
            self.dir_path(&path)?;
        }

        let leaf = path.join("leaf.txt");
        self.file(&leaf, b"bottom\n")?;
        self.manifest.paths.deep_path = leaf;
        Ok(())
    }

    #[cfg(unix)]
    fn build_permissions(&mut self) -> FixtureResult<()> {
        use std::os::unix::fs::PermissionsExt;

        let dir = self.dir("denied")?;
        // Deliberately not counted: a scanner cannot see inside, so including
        // these bytes would make the manifest disagree with every correct scan.
        self.file_uncounted(&dir.join("secret.txt"), b"cannot read me\n")?;

        fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).ctx(&dir)?;
        self.cleanup.push(dir.clone());
        self.manifest.paths.denied_dir = Some(dir);
        Ok(())
    }

    #[cfg(not(unix))]
    fn build_permissions(&mut self) -> FixtureResult<()> {
        // A deny ACL needs unsafe Win32 or `icacls`; skip it rather than shell
        // out. The manifest records that the case is absent.
        Ok(())
    }

    fn build_empty_dirs(&mut self) -> FixtureResult<()> {
        let parent = self.dir("empty")?;
        let first = parent.join("first");
        let second = parent.join("second");
        self.dir_path(&first)?;
        self.dir_path(&second)?;
        self.manifest.paths.empty_dirs = vec![first, second];
        Ok(())
    }

    #[cfg(windows)]
    fn build_junction(&mut self) -> FixtureResult<()> {
        let dir = self.dir("junctions")?;
        let link = dir.join("loop");

        // `mklink /J` creates a junction without needing SeCreateSymbolicLink.
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&self.root)
            .status();

        if let Ok(status) = status {
            if status.success() {
                self.junctions += 1;
                self.manifest.paths.junction = Some(link);
            }
        }
        Ok(())
    }

    #[cfg(not(windows))]
    fn build_junction(&mut self) -> FixtureResult<()> {
        Ok(())
    }

    fn finish(&mut self) {
        self.manifest.files = self.files;
        self.manifest.dirs = self.dirs;
        self.manifest.symlinks = self.symlinks;
        self.manifest.junctions = self.junctions;
        self.manifest.bytes_alloc = self.bytes_alloc;
        self.manifest.bytes_logical = self.bytes_logical;
        self.manifest.hardlink_names = self.hardlink_names;
        self.manifest.sparse_logical = self.sparse_logical;
        self.manifest.sparse_alloc = self.sparse_alloc;
        self.manifest.category_samples = std::mem::take(&mut self.category_samples);
    }

    fn dir(&mut self, name: &str) -> FixtureResult<PathBuf> {
        let path = self.root.join(name);
        self.dir_path(&path)?;
        Ok(path)
    }

    fn dir_path(&mut self, path: &Path) -> FixtureResult<()> {
        fs::create_dir(path).ctx(path)?;
        self.dirs += 1;
        Ok(())
    }

    fn file(&mut self, path: &Path, bytes: &[u8]) -> FixtureResult<()> {
        fs::write(path, bytes).ctx(path)?;
        let alloc = allocated_bytes(path).ctx(path)?;
        self.files += 1;
        self.bytes_logical = self.bytes_logical.saturating_add(bytes.len() as u64);
        self.bytes_alloc = self.bytes_alloc.saturating_add(alloc);
        Ok(())
    }

    /// Write a file that exists but must not appear in the expected totals.
    ///
    /// Only the Unix denied-directory case needs this; on Windows the deny ACL
    /// is skipped, so the helper would be dead code.
    #[cfg(unix)]
    fn file_uncounted(&mut self, path: &Path, bytes: &[u8]) -> FixtureResult<()> {
        fs::write(path, bytes).ctx(path)
    }

    #[cfg(unix)]
    fn symlink(&mut self, link: &Path, target: &Path, _is_dir: bool) -> FixtureResult<()> {
        std::os::unix::fs::symlink(target, link).ctx(link)?;
        self.symlinks += 1;
        Ok(())
    }

    #[cfg(windows)]
    fn symlink(&mut self, link: &Path, target: &Path, is_dir: bool) -> FixtureResult<()> {
        if is_dir {
            std::os::windows::fs::symlink_dir(target, link).ctx(link)?;
        } else {
            std::os::windows::fs::symlink_file(target, link).ctx(link)?;
        }
        self.symlinks += 1;
        Ok(())
    }

    #[cfg(not(any(unix, windows)))]
    fn symlink(&mut self, _link: &Path, _target: &Path, _is_dir: bool) -> FixtureResult<()> {
        Ok(())
    }
}

#[cfg(unix)]
fn restore_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn restore_permissions(_path: &Path) {}

/// Attach the failing path to an `io::Error`.
trait IoContext<T> {
    fn ctx(self, path: &Path) -> FixtureResult<T>;
}

impl<T> IoContext<T> for io::Result<T> {
    fn ctx(self, path: &Path) -> FixtureResult<T> {
        self.map_err(|source| FixtureError::Io {
            path: path.to_path_buf(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[derive(Default)]
    struct Walked {
        files: u64,
        dirs: u64,
        links: u64,
        bytes_alloc: u64,
        bytes_logical: u64,
    }

    #[cfg(windows)]
    fn is_link(metadata: &fs::Metadata) -> bool {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        metadata.file_type().is_symlink()
            || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }

    #[cfg(not(windows))]
    fn is_link(metadata: &fs::Metadata) -> bool {
        metadata.file_type().is_symlink()
    }

    fn walk(root: &Path) -> Walked {
        let mut walked = Walked::default();
        // Annotated because the non-Unix branch never inserts, leaving the type
        // unconstrained on Windows.
        let mut inodes: HashSet<(u64, u64)> = HashSet::new();
        let mut stack = vec![root.to_path_buf()];

        while let Some(path) = stack.pop() {
            let entries = match fs::read_dir(&path) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let child = entry.path();
                let metadata = match fs::symlink_metadata(&child) {
                    Ok(metadata) => metadata,
                    Err(_) => continue,
                };
                if is_link(&metadata) {
                    walked.links += 1;
                } else if metadata.is_dir() {
                    walked.dirs += 1;
                    stack.push(child);
                } else {
                    // Hard links share one inode, so count the inode once — the
                    // same rule a correct scanner follows. Stable `std` exposes
                    // no file identity on Windows, so the walker counts every
                    // name there and the test folds the extras back in.
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        if inodes.insert((metadata.dev(), metadata.ino())) {
                            walked.files += 1;
                            walked.bytes_logical += metadata.len();
                            walked.bytes_alloc += metadata.blocks() * 512;
                        }
                    }
                    #[cfg(not(unix))]
                    {
                        let _ = &mut inodes;
                        walked.files += 1;
                        walked.bytes_logical += metadata.len();
                        // `allocated_bytes` also falls back to the logical size
                        // where the OS exposes no allocation size.
                        walked.bytes_alloc += metadata.len();
                    }
                }
            }
        }

        walked
    }

    /// Hard-link names the walker counts but the manifest folds into one file.
    ///
    /// Zero on Unix, where the walker dedupes by inode. On Windows stable `std`
    /// has no file identity, so every name is counted and the comparison adds
    /// the extras back.
    fn hardlink_extras(manifest: &Manifest) -> (u64, u64) {
        #[cfg(unix)]
        {
            let _ = manifest;
            (0, 0)
        }

        #[cfg(not(unix))]
        {
            let extras = manifest.hardlink_names.saturating_sub(1);
            let size = fs::metadata(&manifest.paths.hardlinks[0])
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            (extras, extras.saturating_mul(size))
        }
    }

    #[test]
    fn manifest_matches_a_real_walk() {
        let tree = FixtureTree::generate().expect("generate");
        let manifest = tree.manifest();
        let walked = walk(tree.root());
        let (extra_files, extra_bytes) = hardlink_extras(manifest);

        assert_eq!(manifest.files + extra_files, walked.files, "file count");
        assert_eq!(manifest.dirs, walked.dirs, "dir count");
        assert_eq!(
            manifest.symlinks + manifest.junctions,
            walked.links,
            "link count"
        );
        assert_eq!(
            manifest.bytes_alloc + extra_bytes,
            walked.bytes_alloc,
            "allocated bytes"
        );
        assert_eq!(
            manifest.bytes_logical + extra_bytes,
            walked.bytes_logical,
            "logical bytes"
        );
    }

    #[test]
    fn hard_links_share_one_inode_and_count_once() {
        let tree = FixtureTree::generate().expect("generate");
        let manifest = tree.manifest();

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let ids: Vec<(u64, u64)> = manifest
                .paths
                .hardlinks
                .iter()
                .map(|path| {
                    let metadata = fs::metadata(path).expect("metadata");
                    (metadata.dev(), metadata.ino())
                })
                .collect();
            assert_eq!(ids[0], ids[1]);
            assert_eq!(ids[1], ids[2]);
        }

        assert_eq!(manifest.hardlink_names, 3);
    }

    #[test]
    fn every_category_has_a_sample_that_classifies_correctly() {
        let tree = FixtureTree::generate().expect("generate");
        let manifest = tree.manifest();

        assert_eq!(manifest.category_samples.len(), Category::ALL.len());
        let seen: HashSet<Category> = manifest
            .category_samples
            .iter()
            .map(|sample| sample.category)
            .collect();
        for category in Category::ALL {
            assert!(seen.contains(&category), "missing sample for {category:?}");
        }

        for sample in &manifest.category_samples {
            assert_eq!(Category::for_path(&sample.path), sample.category);
            assert_eq!(
                fs::metadata(&sample.path).expect("metadata").len(),
                sample.bytes_logical
            );
        }
    }

    #[test]
    fn the_deep_tree_is_two_hundred_levels() {
        let tree = FixtureTree::generate().expect("generate");
        let manifest = tree.manifest();

        assert_eq!(manifest.deep_depth, DEEP_DEPTH);
        let relative = manifest
            .paths
            .deep_path
            .strip_prefix(&manifest.root)
            .expect("inside root");
        // `deep` + 200 components + `leaf.txt`.
        assert_eq!(relative.components().count() as u32, DEEP_DEPTH + 2);
        assert!(manifest.paths.deep_path.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn the_sparse_file_is_sparse() {
        use std::os::unix::fs::MetadataExt;

        let tree = FixtureTree::generate().expect("generate");
        let manifest = tree.manifest();
        let path = manifest.paths.sparse.as_ref().expect("sparse file");

        let metadata = fs::metadata(path).expect("metadata");
        assert_eq!(metadata.len(), SPARSE_LOGICAL_BYTES);
        assert_eq!(manifest.sparse_logical, SPARSE_LOGICAL_BYTES);
        assert_eq!(manifest.sparse_alloc, metadata.blocks() * 512);
        assert!(
            manifest.sparse_alloc < 1024 * 1024,
            "sparse file allocated {} bytes",
            manifest.sparse_alloc
        );
        assert!(manifest.sparse_alloc < manifest.sparse_logical);
    }

    #[cfg(unix)]
    #[test]
    fn the_denied_directory_is_unreadable() {
        let tree = FixtureTree::generate().expect("generate");
        let manifest = tree.manifest();
        let path = manifest.paths.denied_dir.as_ref().expect("denied dir");

        assert!(
            fs::read_dir(path).is_err(),
            "directory should be unreadable"
        );
    }

    #[test]
    fn generation_is_deterministic() {
        let first = FixtureTree::generate().expect("first");
        let second = FixtureTree::generate().expect("second");

        let a = first.manifest();
        let b = second.manifest();
        assert_eq!(a.files, b.files);
        assert_eq!(a.dirs, b.dirs);
        assert_eq!(a.symlinks, b.symlinks);
        assert_eq!(a.junctions, b.junctions);
        assert_eq!(a.bytes_alloc, b.bytes_alloc);
        assert_eq!(a.bytes_logical, b.bytes_logical);
        assert_eq!(a.sparse_alloc, b.sparse_alloc);
        assert_eq!(a.category_samples.len(), b.category_samples.len());
    }

    #[test]
    fn drop_removes_the_whole_tree_including_read_only_dirs() {
        let root = {
            let tree = FixtureTree::generate().expect("generate");
            tree.root().to_path_buf()
        };

        assert!(
            !root.exists(),
            "temporary tree leaked at {}",
            root.display()
        );
    }

    #[test]
    fn generate_in_leaves_cleanup_to_the_caller() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("tree");

        let tree = FixtureTree::generate_in(&root).expect("generate");
        assert!(tree.root().join("files").is_dir());
        assert_eq!(tree.root(), root);

        // Dropping restores permissions but does not delete a caller-owned tree.
        drop(tree);
        assert!(root.exists());
        fs::remove_dir_all(&root).expect("caller cleanup");
    }

    #[test]
    fn long_name_is_exactly_255_bytes() {
        let tree = FixtureTree::generate().expect("generate");
        let name = tree
            .manifest()
            .paths
            .long_name
            .file_name()
            .expect("file name")
            .to_str()
            .expect("utf-8")
            .to_owned();
        assert_eq!(name.len(), LONG_NAME_BYTES);
    }

    #[cfg(unix)]
    #[test]
    fn deep_path_stays_under_path_max_on_unix() {
        let tree = FixtureTree::generate().expect("generate");
        let length = tree.manifest().paths.deep_path.as_os_str().len();
        assert!(length < 1024, "path is {length} bytes");
        assert!(length > 400, "path should still be deep, got {length}");
    }

    #[cfg(windows)]
    #[test]
    fn deep_path_exceeds_1024_on_windows() {
        let tree = FixtureTree::generate().expect("generate");
        let length = tree.manifest().paths.deep_path.as_os_str().len();
        assert!(length > 1024, "path is {length} bytes");
    }

    #[test]
    fn sample_totals_by_category_are_consistent() {
        let tree = FixtureTree::generate().expect("generate");
        let manifest = tree.manifest();
        let totals = manifest.sample_bytes_by_category();

        let expected: u64 = manifest
            .category_samples
            .iter()
            .map(|sample| sample.bytes_logical)
            .sum();
        assert_eq!(totals.values().sum::<u64>(), expected);
        assert_eq!(totals.len(), Category::ALL.len());
    }
}
