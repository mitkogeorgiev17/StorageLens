//! Batched, streaming inserts into `entries`.
//!
//! A scan produces millions of rows. Inserting them one transaction per row is
//! ~100× slower than batching, so [`EntryWriter`] buffers up to
//! [`DEFAULT_BATCH_SIZE`] rows and commits each batch in one transaction.
//!
//! # Arena order
//!
//! [`Entry`] has no field for its own arena index — only [`Entry::parent`], which
//! points at one. The writer therefore assigns SQLite row ids in **acceptance
//! order**, treating each accepted entry as the next arena index. Scanners must
//! stream entries in pre-order: a parent is always accepted before its children,
//! and the first entry is the scanned root (arena index `0`, SQL `parent_id` `0`).

use std::path::Path;

use rusqlite::{params, Connection};

use storagelens_core::{
    extension_of, Category, Entry, EntrySink, Result as CoreResult, ScanError, Tags,
};

use crate::error::{CatalogError, Result};
use crate::util::to_i64;

/// Rows buffered before a transaction is committed. PLAN §5's "one transaction per
/// ~10k rows".
pub const DEFAULT_BATCH_SIZE: usize = 10_000;

const INSERT_SQL: &str = "INSERT INTO entries (\
     id, scan_id, parent_id, name, ext, kind, size_alloc, size_logical, dev, file_id, \
     nlink, attrs, created, modified, accessed, category, tags, hardlink_dup\
 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)";

struct PendingRow {
    id: i64,
    parent_id: i64,
    name: String,
    ext: Option<String>,
    kind: i64,
    size_alloc: i64,
    size_logical: i64,
    dev: i64,
    file_id: i64,
    nlink: i64,
    attrs: i64,
    created: i64,
    modified: i64,
    accessed: i64,
    category: &'static str,
    tags: i64,
    hardlink_dup: bool,
}

/// A streaming [`EntrySink`] over a scan's `entries` table.
///
/// Owns its own SQLite connection so that writes never block the catalog's read
/// connection (WAL). Drop flushes any buffered rows, logging rather than panicking
/// if the flush fails.
pub struct EntryWriter {
    conn: Connection,
    scan_id: i64,
    batch_size: usize,
    pending: Vec<PendingRow>,
    next_id: i64,
    id_by_index: Vec<i64>,
    written: u64,
}

impl EntryWriter {
    /// Open a writer for `scan_id` against the database at `path`.
    ///
    /// Only one writer may exist per database at a time: ids are allocated from
    /// `MAX(id) + 1` at open. Scans are serialized by the app, so this holds.
    pub fn open(path: &Path, scan_id: i64, batch_size: usize) -> Result<Self> {
        let conn = Connection::open(path)?;
        crate::db::configure(&conn)?;

        // Explicit ids let the writer map arena indices to row ids without a
        // second query per entry. A single writer exists per scan, so this is
        // race-free.
        let next_id: i64 =
            conn.query_row("SELECT COALESCE(MAX(id), 0) + 1 FROM entries", [], |row| {
                row.get(0)
            })?;

        let batch_size = batch_size.max(1);
        Ok(Self {
            conn,
            scan_id,
            batch_size,
            pending: Vec::with_capacity(batch_size.min(65_536)),
            next_id,
            id_by_index: Vec::new(),
            written: 0,
        })
    }

    /// The scan this writer belongs to.
    pub fn scan_id(&self) -> i64 {
        self.scan_id
    }

    /// Rows committed so far.
    pub fn written(&self) -> u64 {
        self.written
    }

    /// Commit everything buffered.
    pub fn flush_batch(&mut self) -> Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }

        let transaction = self.conn.unchecked_transaction()?;
        {
            let mut statement = transaction.prepare_cached(INSERT_SQL)?;
            for row in &self.pending {
                statement.execute(params![
                    row.id,
                    self.scan_id,
                    row.parent_id,
                    row.name,
                    row.ext,
                    row.kind,
                    row.size_alloc,
                    row.size_logical,
                    row.dev,
                    row.file_id,
                    row.nlink,
                    row.attrs,
                    row.created,
                    row.modified,
                    row.accessed,
                    row.category,
                    row.tags,
                    i64::from(row.hardlink_dup),
                ])?;
            }
        }
        transaction.commit()?;

        self.written = self.written.saturating_add(self.pending.len() as u64);
        self.pending.clear();
        Ok(())
    }

    fn parent_id_for(&self, parent: u32) -> Result<i64> {
        if self.id_by_index.is_empty() {
            // The first accepted entry is the root: arena index 0, SQL parent 0.
            if parent == 0 {
                return Ok(0);
            }
            return Err(CatalogError::Corrupt(format!(
                "first entry has parent index {parent}, expected 0"
            )));
        }

        self.id_by_index
            .get(parent as usize)
            .copied()
            .ok_or_else(|| {
                CatalogError::Corrupt(format!(
                    "parent index {parent} was not accepted before its child"
                ))
            })
    }

    fn push(&mut self, entry: Entry) -> Result<()> {
        let parent_id = self.parent_id_for(entry.parent)?;
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);

        let name_path = Path::new(&entry.name);
        let ext = extension_of(name_path);
        let category = Category::for_path(name_path);

        self.id_by_index.push(id);
        self.pending.push(PendingRow {
            id,
            parent_id,
            name: entry.name,
            ext,
            kind: i64::from(entry.kind.as_u8()),
            size_alloc: to_i64(entry.size_alloc),
            size_logical: to_i64(entry.size_logical),
            dev: to_i64(u64::from(entry.dev)),
            file_id: to_i64(entry.file_id),
            nlink: i64::from(entry.nlink),
            attrs: i64::from(entry.attrs),
            created: entry.created,
            modified: entry.modified,
            accessed: entry.accessed,
            category: category.as_str(),
            tags: Tags::from_category(category).bits() as i64,
            hardlink_dup: entry.hardlink_dup,
        });

        if self.pending.len() >= self.batch_size {
            self.flush_batch()?;
        }
        Ok(())
    }
}

impl EntrySink for EntryWriter {
    fn accept(&mut self, entry: Entry) -> CoreResult<()> {
        self.push(entry).map_err(core_error)
    }

    fn flush(&mut self) -> CoreResult<()> {
        self.flush_batch().map_err(core_error)
    }
}

impl Drop for EntryWriter {
    fn drop(&mut self) {
        if !self.pending.is_empty() {
            if let Err(error) = self.flush_batch() {
                tracing::warn!(%error, "failed to flush buffered catalog rows on drop");
            }
        }
    }
}

/// `ScanError` carries catalog failures as text, because the core crate must not
/// depend on the catalog crate.
fn core_error(error: CatalogError) -> ScanError {
    ScanError::Catalog(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use storagelens_core::{EntryKind, ScanStatus};

    use crate::db::Catalog;
    use crate::model::Platform;

    fn entry(parent: u32, name: &str, kind: EntryKind, size: u64) -> Entry {
        Entry {
            parent,
            name: name.into(),
            kind,
            size_alloc: size,
            size_logical: size,
            dev: 1,
            file_id: size + 1,
            nlink: 1,
            attrs: 0,
            created: 1_700_000_000,
            modified: 1_700_000_100,
            accessed: 1_700_000_200,
            hardlink_dup: false,
        }
    }

    fn catalog_with_session(dir: &tempfile::TempDir) -> (Catalog, i64) {
        let catalog = Catalog::open(dir.path().join("catalog.db")).expect("open");
        let scan_id = catalog
            .begin_session(Platform::Macos, false, Path::new("/scanned"))
            .expect("begin");
        (catalog, scan_id)
    }

    #[test]
    fn parents_map_to_row_ids() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (catalog, scan_id) = catalog_with_session(&dir);

        let mut writer = catalog.writer(scan_id).expect("writer");
        writer
            .push(entry(0, "Users", EntryKind::Dir, 0))
            .expect("root");
        writer
            .push(entry(0, "mitko", EntryKind::Dir, 0))
            .expect("child dir");
        writer
            .push(entry(1, "photo.jpg", EntryKind::File, 4_096))
            .expect("file");
        writer.flush_batch().expect("flush");

        let roots = catalog.children(scan_id, 0).expect("roots");
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].name, "Users");
        assert_eq!(roots[0].parent_id, 0);

        let children = catalog.children(scan_id, roots[0].id).expect("children");
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name, "mitko");

        let files = catalog.children(scan_id, children[0].id).expect("files");
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "photo.jpg");
        assert_eq!(files[0].size_alloc, 4_096);
        assert_eq!(files[0].ext.as_deref(), Some("jpg"));
        assert_eq!(files[0].category, Category::Images);
        assert!(files[0].tags.contains(Tags::IMAGE));
    }

    #[test]
    fn batching_commits_incrementally() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (catalog, scan_id) = catalog_with_session(&dir);

        let mut writer = EntryWriter::open(catalog.path(), scan_id, 2).expect("writer");
        writer
            .push(entry(0, "root", EntryKind::Dir, 0))
            .expect("root");
        for index in 0..5 {
            writer
                .push(entry(0, &format!("file{index}.txt"), EntryKind::File, 10))
                .expect("file");
        }
        assert_eq!(writer.written(), 6);

        writer.flush_batch().expect("final flush");
        assert_eq!(catalog.entry_count(scan_id).expect("count"), 6);
    }

    #[test]
    fn out_of_order_parents_are_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (catalog, scan_id) = catalog_with_session(&dir);

        let mut writer = catalog.writer(scan_id).expect("writer");
        let error = writer
            .push(entry(7, "orphan.txt", EntryKind::File, 1))
            .expect_err("must fail");
        assert!(matches!(error, CatalogError::Corrupt(_)));
    }

    #[test]
    fn a_first_child_after_the_root_uses_the_root_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (catalog, scan_id) = catalog_with_session(&dir);

        let mut writer = catalog.writer(scan_id).expect("writer");
        writer
            .push(entry(0, "root", EntryKind::Dir, 0))
            .expect("root");
        writer
            .push(entry(0, "child", EntryKind::File, 3))
            .expect("child");
        writer.flush_batch().expect("flush");

        let roots = catalog.children(scan_id, 0).expect("roots");
        assert_eq!(roots.len(), 1);
        let children = catalog.children(scan_id, roots[0].id).expect("children");
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].name, "child");
    }

    #[test]
    fn trait_flush_persists_and_drop_does_not_panic() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (catalog, scan_id) = catalog_with_session(&dir);

        {
            let mut writer = catalog.writer(scan_id).expect("writer");
            EntrySink::accept(&mut writer, entry(0, "root", EntryKind::Dir, 0)).expect("accept");
            EntrySink::flush(&mut writer).expect("flush");
        }

        assert_eq!(catalog.entry_count(scan_id).expect("count"), 1);
    }

    #[test]
    fn unflushed_rows_are_flushed_on_drop() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (catalog, scan_id) = catalog_with_session(&dir);

        {
            let mut writer = catalog.writer(scan_id).expect("writer");
            writer
                .push(entry(0, "root", EntryKind::Dir, 0))
                .expect("root");
            writer
                .push(entry(0, "child.txt", EntryKind::File, 1))
                .expect("child");
        }

        assert_eq!(catalog.entry_count(scan_id).expect("count"), 2);
    }

    #[test]
    fn categories_and_tags_are_derived_from_the_name() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (catalog, scan_id) = catalog_with_session(&dir);

        let mut writer = catalog.writer(scan_id).expect("writer");
        writer
            .push(entry(0, "root", EntryKind::Dir, 0))
            .expect("root");
        writer
            .push(entry(0, "movie.MKV", EntryKind::File, 10))
            .expect("video");
        writer
            .push(entry(0, "Makefile", EntryKind::File, 10))
            .expect("code");
        writer
            .push(entry(0, "mystery.blob", EntryKind::File, 10))
            .expect("other");
        writer.flush_batch().expect("flush");

        let children = catalog.children(scan_id, 1).expect("children");
        let video = children
            .iter()
            .find(|row| row.name == "movie.MKV")
            .expect("video");
        assert_eq!(video.ext.as_deref(), Some("mkv"));
        assert_eq!(video.category, Category::Video);
        assert!(video.tags.contains(Tags::VIDEO));

        let code = children
            .iter()
            .find(|row| row.name == "Makefile")
            .expect("code");
        assert_eq!(code.category, Category::Code);

        let other = children
            .iter()
            .find(|row| row.name == "mystery.blob")
            .expect("other");
        assert_eq!(other.category, Category::Other);
    }

    #[test]
    fn category_totals_and_largest_pagination_work() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (catalog, scan_id) = catalog_with_session(&dir);

        let mut writer = catalog.writer(scan_id).expect("writer");
        writer
            .push(entry(0, "root", EntryKind::Dir, 0))
            .expect("root");
        writer
            .push(entry(0, "a.jpg", EntryKind::File, 300))
            .expect("a");
        writer
            .push(entry(0, "b.png", EntryKind::File, 200))
            .expect("b");
        writer
            .push(entry(0, "c.mp4", EntryKind::File, 100))
            .expect("c");
        writer.flush_batch().expect("flush");

        let totals = catalog.category_totals(scan_id).expect("totals");
        let images = totals
            .iter()
            .find(|total| total.category == Category::Images)
            .expect("images");
        assert_eq!(images.bytes_alloc, 500);
        assert_eq!(images.file_count, 2);

        let first = catalog.largest(scan_id, 0, None, 2).expect("page 1");
        assert_eq!(first.len(), 2);
        assert_eq!(first[0].name, "a.jpg");
        assert_eq!(first[1].name, "b.png");

        let cursor = crate::model::SizeCursor {
            size_alloc: first[1].size_alloc,
            id: first[1].id,
        };
        let second = catalog
            .largest(scan_id, 0, Some(cursor), 2)
            .expect("page 2");
        assert_eq!(second.len(), 1);
        assert_eq!(second[0].name, "c.mp4");

        let none = catalog.largest(scan_id, 1_000, None, 10).expect("filtered");
        assert!(none.is_empty());
    }

    #[test]
    fn hardlink_duplicates_are_stored_but_excluded_from_totals() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (catalog, scan_id) = catalog_with_session(&dir);

        let mut writer = catalog.writer(scan_id).expect("writer");
        writer
            .push(entry(0, "root", EntryKind::Dir, 0))
            .expect("root");

        let owner = entry(0, "photo.jpg", EntryKind::File, 300);
        let mut duplicate = entry(0, "photo-copy.jpg", EntryKind::File, 300);
        duplicate.dev = owner.dev;
        duplicate.file_id = owner.file_id;
        duplicate.nlink = 2;
        duplicate.hardlink_dup = true;

        writer.push(owner).expect("owner");
        writer.push(duplicate).expect("duplicate");
        writer.flush_batch().expect("flush");

        let totals = catalog.category_totals(scan_id).expect("totals");
        let images = totals
            .iter()
            .find(|total| total.category == Category::Images)
            .expect("images");
        assert_eq!(images.bytes_alloc, 300, "only the first name is counted");
        assert_eq!(images.file_count, 1);

        let children = catalog.children(scan_id, 1).expect("children");
        let stored = children
            .iter()
            .find(|row| row.name == "photo-copy.jpg")
            .expect("duplicate row is kept for browsing");
        assert!(stored.hardlink_dup);
        assert_eq!(stored.size_alloc, 300, "the row keeps its true size");
        let owner_row = children
            .iter()
            .find(|row| row.name == "photo.jpg")
            .expect("owner");
        assert!(!owner_row.hardlink_dup);
    }

    #[test]
    fn sessions_can_be_finished_after_writing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (catalog, scan_id) = catalog_with_session(&dir);

        let mut writer = catalog.writer(scan_id).expect("writer");
        writer
            .push(entry(0, "root", EntryKind::Dir, 0))
            .expect("root");
        writer.flush_batch().expect("flush");
        drop(writer);

        catalog
            .finish_session(scan_id, ScanStatus::Complete, 1, 0, 0)
            .expect("finish");

        let session = catalog.session(scan_id).expect("session");
        assert_eq!(session.status, ScanStatus::Complete);
        assert_eq!(session.files_seen, 1);
    }

    #[test]
    fn two_sessions_do_not_share_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (catalog, first) = catalog_with_session(&dir);
        let second = catalog
            .begin_session(Platform::Macos, false, Path::new("/scanned"))
            .expect("begin");

        for scan_id in [first, second] {
            let mut writer = catalog.writer(scan_id).expect("writer");
            writer
                .push(entry(0, "root", EntryKind::Dir, 0))
                .expect("root");
            writer
                .push(entry(0, "file.txt", EntryKind::File, 5))
                .expect("file");
            writer.flush_batch().expect("flush");
        }

        assert_eq!(catalog.entry_count(first).expect("count"), 2);
        assert_eq!(catalog.entry_count(second).expect("count"), 2);
    }

    /// Acceptance criterion: 1M rows in under 30 s.
    ///
    /// Ignored by default — run with
    /// `cargo test -p storagelens-catalog --release -- --ignored million_rows`.
    #[test]
    #[ignore = "perf test; run explicitly in release mode"]
    fn million_rows_under_thirty_seconds() {
        const ROWS: usize = 1_000_000;

        let dir = tempfile::tempdir().expect("tempdir");
        let (catalog, scan_id) = catalog_with_session(&dir);

        let started = std::time::Instant::now();
        let mut writer = catalog.writer(scan_id).expect("writer");
        writer
            .push(entry(0, "root", EntryKind::Dir, 0))
            .expect("root");
        for index in 0..ROWS {
            writer
                .push(entry(
                    0,
                    &format!("file{index}.jpg"),
                    EntryKind::File,
                    4_096,
                ))
                .expect("file");
        }
        writer.flush_batch().expect("flush");
        let elapsed = started.elapsed();
        drop(writer);

        assert_eq!(
            catalog.entry_count(scan_id).expect("count"),
            ROWS as u64 + 1
        );
        assert!(
            elapsed < std::time::Duration::from_secs(30),
            "1M rows took {elapsed:?}"
        );
    }
}
