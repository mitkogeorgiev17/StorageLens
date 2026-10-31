//! Duplicate files (#39, PLAN §12): byte-identical files without reading the
//! whole disk.
//!
//! 1. **Group by exact logical size** (from the arena: no I/O). Identical
//!    content always has identical logical size; `size_alloc` can differ for
//!    the same bytes (sparse, compressed), so it would miss real duplicates.
//! 2. **Partial hash** of the first 64 KiB of each file in a group.
//! 3. **Full BLAKE3** of the files whose partial hashes still collide.
//!
//! A group is reported only when full hashes of the whole expected length
//! match, so non-identical files are never reported. Hard-link duplicates are
//! not candidates (the arena counts an inode once: trashing a second name
//! frees nothing).
//!
//! # Being a background task
//!
//! Hashing runs sequentially on the calling thread. The caller sets that
//! thread's I/O throttle first (`setiopolicy_np` / background mode live in
//! the scanner crates, since this crate never calls OS APIs). CPU is capped by
//! [`Options::duty`]: after each 1 MiB chunk the thread sleeps long enough that
//! work is at most that fraction of wall time. Cancellation is checked between
//! chunks, so a cancel lands mid-file, not after the group.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use storagelens_catalog::{Arena, NodeIndex};
use storagelens_core::{CancelToken, EntryKind};
use storagelens_safety::Guard;

/// Smaller files are not worth the I/O (PLAN §12).
pub const MIN_SIZE: u64 = 1 << 20;
/// Bytes read for the partial hash.
pub const PARTIAL_BYTES: u64 = 64 * 1024;
/// Read size; also the granularity of cancellation and CPU pacing.
const CHUNK: usize = 1 << 20;

/// A file that may have a twin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub node: NodeIndex,
    pub path: PathBuf,
    pub size_logical: u64,
    pub size_alloc: u64,
}

/// Byte-identical files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// BLAKE3 of the content, hex.
    pub hash: String,
    pub size_logical: u64,
    /// At least two, in path order.
    pub files: Vec<Candidate>,
}

impl Group {
    /// Space freed by keeping one copy: everything but the largest allocation.
    pub fn reclaimable(&self) -> u64 {
        let total: u64 = self.files.iter().map(|file| file.size_alloc).sum();
        let largest = self
            .files
            .iter()
            .map(|file| file.size_alloc)
            .max()
            .unwrap_or(0);
        total - largest
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    /// Files below this logical size are ignored.
    pub min_size: u64,
    /// Upper bound on the fraction of wall time spent working, `(0, 1]`.
    /// `1.0` never sleeps; `0.1` keeps a background run under ~10% of a core.
    pub duty: f64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            min_size: MIN_SIZE,
            duty: 0.1,
        }
    }
}

/// Where a run is, reported after each file is hashed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Progress {
    /// Size groups finished, of `groups_total`.
    pub groups_done: u64,
    pub groups_total: u64,
    pub files_hashed: u64,
    pub bytes_read: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Largest reclaimable first.
    pub groups: Vec<Group>,
    /// Files that could not be read, or no longer had the scanned size.
    pub unreadable: u64,
    /// Stopped early: `groups` holds only the groups completed before that.
    pub cancelled: bool,
}

/// Files worth hashing: regular, counted (not a hard-link duplicate), at
/// least `min_size`, sharing their exact size with another candidate, and
/// neither refused by `guard` (system folders, bundle internals, outside the
/// root) nor under `~/Library` (app data is risky to dedupe).
///
/// `root` is the scanned root's absolute path; the arena stores names only.
pub fn candidates(
    arena: &Arena,
    root: &Path,
    guard: &Guard,
    home: Option<&Path>,
    min_size: u64,
) -> Vec<Candidate> {
    // Sizes first: no paths are built for files with a unique size.
    let mut by_size: HashMap<u64, Vec<NodeIndex>> = HashMap::new();
    for (index, node) in arena.nodes().iter().enumerate() {
        let Ok(index) = NodeIndex::try_from(index) else {
            break;
        };
        if node.kind() != EntryKind::File || !node.counts() {
            continue;
        }
        let size = arena.size_logical(index).unwrap_or(0);
        if size >= min_size {
            by_size.entry(size).or_default().push(index);
        }
    }

    let library = home.map(|home| home.join("Library"));
    let mut out = Vec::new();
    for (size, nodes) in by_size {
        if nodes.len() < 2 {
            continue;
        }
        let kept: Vec<Candidate> = nodes
            .into_iter()
            .filter_map(|index| {
                let mut path = root.to_path_buf();
                path.extend(arena.path_names(index));
                let refused = library.as_deref().is_some_and(|lib| path.starts_with(lib))
                    || path
                        .to_str()
                        .is_none_or(|text| guard.check_resolved(text).is_err());
                (!refused).then(|| Candidate {
                    node: index,
                    path,
                    size_logical: size,
                    size_alloc: arena.node(index).map_or(0, |node| node.size_alloc),
                })
            })
            .collect();
        if kept.len() >= 2 {
            out.extend(kept);
        }
    }
    out
}

/// Run stages 2 and 3 over `candidates` (stage 1 is the size grouping).
pub fn find(
    candidates: Vec<Candidate>,
    options: Options,
    cancel: &CancelToken,
    progress: &mut dyn FnMut(Progress),
) -> Report {
    let mut by_size: HashMap<u64, Vec<Candidate>> = HashMap::new();
    for candidate in candidates {
        if candidate.size_logical >= options.min_size {
            by_size
                .entry(candidate.size_logical)
                .or_default()
                .push(candidate);
        }
    }
    let mut sizes: Vec<(u64, Vec<Candidate>)> = by_size
        .into_iter()
        .filter(|(_, files)| files.len() >= 2)
        .collect();
    // Biggest first: the most space per byte read, if the run is cancelled.
    sizes.sort_by_key(|(size, _)| std::cmp::Reverse(*size));

    let mut hasher = Hasher {
        duty: options.duty.clamp(f64::MIN_POSITIVE, 1.0),
        cancel,
        buffer: vec![0; CHUNK],
        state: Progress {
            groups_total: sizes.len() as u64,
            ..Progress::default()
        },
        unreadable: 0,
    };
    let mut groups = Vec::new();
    let mut cancelled = false;

    'sizes: for (size, files) in sizes {
        let partial_len = size.min(PARTIAL_BYTES);
        let Some(partials) = hasher.split(files, partial_len, progress) else {
            cancelled = true;
            break;
        };
        for (partial, files) in partials {
            let fulls = if partial_len == size {
                // The partial hash already covered every byte.
                vec![(partial, files)]
            } else {
                match hasher.split(files, size, progress) {
                    Some(fulls) => fulls,
                    None => {
                        cancelled = true;
                        break 'sizes;
                    }
                }
            };
            for (hash, mut files) in fulls {
                files.sort_by(|a, b| a.path.cmp(&b.path));
                groups.push(Group {
                    hash: hash.to_hex().to_string(),
                    size_logical: size,
                    files,
                });
            }
        }
        hasher.state.groups_done += 1;
        progress(hasher.state);
    }

    groups.sort_by(|a, b| {
        b.reclaimable()
            .cmp(&a.reclaimable())
            .then_with(|| a.hash.cmp(&b.hash))
    });
    Report {
        groups,
        unreadable: hasher.unreadable,
        cancelled,
    }
}

struct Hasher<'a> {
    duty: f64,
    cancel: &'a CancelToken,
    buffer: Vec<u8>,
    state: Progress,
    unreadable: u64,
}

impl Hasher<'_> {
    /// Hash the first `len` bytes of each file and keep the hashes shared by
    /// two or more. `None` when cancelled.
    fn split(
        &mut self,
        files: Vec<Candidate>,
        len: u64,
        progress: &mut dyn FnMut(Progress),
    ) -> Option<Vec<(blake3::Hash, Vec<Candidate>)>> {
        let mut by_hash: HashMap<blake3::Hash, Vec<Candidate>> = HashMap::new();
        for file in files {
            match self.hash(&file.path, len, file.size_logical) {
                Ok(Some(hash)) => by_hash.entry(hash).or_default().push(file),
                Ok(None) => return None,
                Err(_) => self.unreadable += 1,
            }
            self.state.files_hashed += 1;
            progress(self.state);
        }
        Some(
            by_hash
                .into_iter()
                .filter(|(_, files)| files.len() >= 2)
                .collect(),
        )
    }

    /// BLAKE3 of the first `len` bytes. An error if the file cannot be read or
    /// is not `size` bytes long any more; `Ok(None)` when cancelled.
    fn hash(&mut self, path: &Path, len: u64, size: u64) -> io::Result<Option<blake3::Hash>> {
        let mut file = open(path)?;
        if file.metadata()?.len() != size {
            return Err(io::Error::other("changed since the scan"));
        }
        let mut hasher = blake3::Hasher::new();
        let mut left = len;
        while left > 0 {
            if self.cancel.is_cancelled() {
                return Ok(None);
            }
            let started = Instant::now();
            let want = usize::try_from(left).unwrap_or(CHUNK).min(CHUNK);
            let read = file.read(&mut self.buffer[..want])?;
            if read == 0 {
                return Err(io::Error::other("shorter than the scan recorded"));
            }
            hasher.update(&self.buffer[..read]);
            left -= read as u64;
            self.state.bytes_read += read as u64;
            std::thread::sleep(pause(started.elapsed(), self.duty));
        }
        Ok(Some(hasher.finalize()))
    }
}

/// Sleep after `work` so that work is `duty` of the total.
fn pause(work: Duration, duty: f64) -> Duration {
    if duty >= 1.0 {
        return Duration::ZERO;
    }
    work.mul_f64((1.0 - duty) / duty)
}

/// Open for one sequential pass. On Windows, `FILE_FLAG_SEQUENTIAL_SCAN`
/// tells the cache manager not to keep what was read (AGENTS.md §6).
fn open(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_FLAG_SEQUENTIAL_SCAN: u32 = 0x0800_0000;
        options.custom_flags(FILE_FLAG_SEQUENTIAL_SCAN);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacing_keeps_work_to_the_duty_fraction() {
        let work = Duration::from_millis(10);
        assert_eq!(pause(work, 1.0), Duration::ZERO);
        assert_eq!(pause(work, 0.1), Duration::from_millis(90));
        assert_eq!(pause(work, 0.5), Duration::from_millis(10));
    }

    #[test]
    fn reclaimable_keeps_the_largest_copy() {
        let file = |alloc| Candidate {
            node: 0,
            path: PathBuf::new(),
            size_logical: 10,
            size_alloc: alloc,
        };
        let group = Group {
            hash: String::new(),
            size_logical: 10,
            files: vec![file(4_096), file(8_192), file(4_096)],
        };
        assert_eq!(group.reclaimable(), 8_192);
    }
}
