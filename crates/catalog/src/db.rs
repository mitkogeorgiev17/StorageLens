//! Opening the database, applying pragmas, and the session/query surface.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension, Row};

use storagelens_core::ScanStatus;

use crate::error::{CatalogError, Result};
use crate::model::{CategoryTotal, Platform, ScanSession, SizeCursor, StoredEntry};
use crate::query;
use crate::schema;
use crate::util::{now, to_i64, to_u64};
use crate::writer::{EntryWriter, DEFAULT_BATCH_SIZE};

/// The application catalog.
///
/// Holds one read connection. Writes go through [`writer`](Self::writer), which
/// opens its own connection so that a long batch insert never blocks a read — this
/// is what WAL buys us, and the Large Files screen depends on it.
#[derive(Debug)]
pub struct Catalog {
    conn: Connection,
    path: PathBuf,
}

impl Catalog {
    /// Open (or create) the catalog at `path`, apply pragmas, and migrate.
    ///
    /// Missing parent directories are created. An existing database is migrated
    /// forward; a database from a newer build is refused with
    /// [`CatalogError::SchemaTooNew`].
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();

        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)?;
            }
        }

        let conn = Connection::open(&path)?;
        configure(&conn)?;
        schema::migrate(&conn)?;

        Ok(Self { conn, path })
    }

    /// The file backing this catalog.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The schema version currently applied.
    pub fn schema_version(&self) -> Result<u32> {
        schema::current_version(&self.conn)
    }

    /// The read connection, for callers that need a query this module does not
    /// expose yet (the analysis crate in M4).
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Start a scan session and return its id.
    ///
    /// The session is created in `running` state with zero counters; call
    /// [`finish_session`](Self::finish_session) when the scan ends, including when
    /// it is cancelled.
    pub fn begin_session(&self, platform: Platform, elevated: bool) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO scan_session (platform, started_at, status, elevated) \
             VALUES (?1, ?2, ?3, ?4)",
            params![
                platform.as_str(),
                now(),
                ScanStatus::Running.as_str(),
                i64::from(elevated)
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Record how a scan ended and what it counted.
    pub fn finish_session(
        &self,
        scan_id: i64,
        status: ScanStatus,
        files_seen: u64,
        bytes_seen: u64,
        dirs_skipped: u64,
    ) -> Result<()> {
        let updated = self.conn.execute(
            "UPDATE scan_session \
             SET finished_at = ?2, status = ?3, files_seen = ?4, bytes_seen = ?5, \
                 dirs_skipped = ?6 \
             WHERE id = ?1",
            params![
                scan_id,
                now(),
                status.as_str(),
                to_i64(files_seen),
                to_i64(bytes_seen),
                to_i64(dirs_skipped),
            ],
        )?;

        if updated == 0 {
            return Err(CatalogError::SessionNotFound(scan_id));
        }
        Ok(())
    }

    /// One session by id.
    pub fn session(&self, scan_id: i64) -> Result<ScanSession> {
        self.conn
            .query_row(
                "SELECT id, platform, started_at, finished_at, status, files_seen, \
                        bytes_seen, dirs_skipped, elevated \
                 FROM scan_session WHERE id = ?1",
                params![scan_id],
                map_session,
            )
            .optional()?
            .ok_or(CatalogError::SessionNotFound(scan_id))
    }

    /// The most recently started session, if any.
    pub fn latest_session(&self) -> Result<Option<ScanSession>> {
        let session = self
            .conn
            .query_row(
                "SELECT id, platform, started_at, finished_at, status, files_seen, \
                        bytes_seen, dirs_skipped, elevated \
                 FROM scan_session ORDER BY id DESC LIMIT 1",
                [],
                map_session,
            )
            .optional()?;
        Ok(session)
    }

    /// A batched writer for `scan_id` on its own connection.
    ///
    /// The session must exist. Drop the writer (or call
    /// [`EntryWriter::flush`]) before finishing the session.
    pub fn writer(&self, scan_id: i64) -> Result<EntryWriter> {
        // Fail early with a clear error instead of a foreign-key violation at the
        // first flush.
        let exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM scan_session WHERE id = ?1)",
            params![scan_id],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(CatalogError::SessionNotFound(scan_id));
        }

        EntryWriter::open(&self.path, scan_id, DEFAULT_BATCH_SIZE)
    }

    /// Direct children of `parent_id`, largest first. `parent_id == 0` returns the
    /// scanned volume roots.
    pub fn children(&self, scan_id: i64, parent_id: i64) -> Result<Vec<StoredEntry>> {
        query::children(&self.conn, scan_id, parent_id)
    }

    /// The largest regular files, keyset-paginated.
    pub fn largest(
        &self,
        scan_id: i64,
        min_size: u64,
        after: Option<SizeCursor>,
        limit: u32,
    ) -> Result<Vec<StoredEntry>> {
        query::largest(&self.conn, scan_id, min_size, after, limit)
    }

    /// Allocated bytes and file counts per primary category.
    pub fn category_totals(&self, scan_id: i64) -> Result<Vec<CategoryTotal>> {
        query::category_totals(&self.conn, scan_id)
    }

    /// Number of entries recorded for a scan.
    pub fn entry_count(&self, scan_id: i64) -> Result<u64> {
        query::entry_count(&self.conn, scan_id)
    }
}

/// WAL, NORMAL, 128 MB cache, and a bounded busy timeout. PLAN §5.
///
/// Applied to every connection: `journal_mode` is persistent, but `synchronous`
/// and `cache_size` are per-connection.
pub(crate) fn configure(conn: &Connection) -> Result<()> {
    // `journal_mode` returns the mode it settled on, so it cannot go through
    // `execute_batch`. On a plain in-memory database this returns "memory"; on a
    // file it returns "wal".
    let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "cache_size", -131_072_i64)?;
    conn.pragma_update(None, "foreign_keys", true)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(())
}

fn map_session(row: &Row<'_>) -> rusqlite::Result<ScanSession> {
    let platform_value: String = row.get(1)?;
    let platform = Platform::from_name(&platform_value).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown platform {platform_value:?}"),
            )),
        )
    })?;

    let status_value: String = row.get(4)?;
    let status = ScanStatus::from_name(&status_value).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            4,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown scan status {status_value:?}"),
            )),
        )
    })?;

    Ok(ScanSession {
        id: row.get(0)?,
        platform,
        started_at: row.get(2)?,
        finished_at: row.get(3)?,
        status,
        files_seen: to_u64(row.get(5)?),
        bytes_seen: to_u64(row.get(6)?),
        dirs_skipped: to_u64(row.get(7)?),
        elevated: row.get::<_, i64>(8)? != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use storagelens_core::EntrySink;

    fn catalog(dir: &tempfile::TempDir) -> Catalog {
        Catalog::open(dir.path().join("catalog.db")).expect("open catalog")
    }

    #[test]
    fn opening_a_fresh_file_migrates_and_uses_wal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog = catalog(&dir);

        assert_eq!(
            catalog.schema_version().expect("version"),
            schema::SCHEMA_VERSION
        );

        let mode: String = catalog
            .connection()
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("journal mode");
        assert_eq!(mode.to_ascii_lowercase(), "wal");
    }

    #[test]
    fn reopening_preserves_sessions_and_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("catalog.db");

        let scan_id = {
            let catalog = Catalog::open(&path).expect("open");
            let id = catalog
                .begin_session(Platform::Macos, false)
                .expect("begin");

            let mut writer = catalog.writer(id).expect("writer");
            writer
                .accept(storagelens_core::Entry {
                    parent: 0,
                    name: "keep.txt".into(),
                    kind: storagelens_core::EntryKind::File,
                    size_alloc: 4_096,
                    size_logical: 4_096,
                    dev: 1,
                    file_id: 1,
                    nlink: 1,
                    attrs: 0,
                    created: 0,
                    modified: 0,
                    accessed: 0,
                    hardlink_dup: false,
                })
                .expect("accept");
            writer.flush_batch().expect("flush");
            drop(writer);

            catalog
                .finish_session(id, ScanStatus::Complete, 10, 4_096, 1)
                .expect("finish");
            id
        };

        let catalog = Catalog::open(&path).expect("reopen");
        let session = catalog.session(scan_id).expect("session");
        assert_eq!(session.status, ScanStatus::Complete);
        assert_eq!(session.files_seen, 10);
        assert_eq!(session.bytes_seen, 4_096);
        assert_eq!(session.dirs_skipped, 1);
        assert!(session.finished_at.is_some());
        assert_eq!(catalog.latest_session().expect("latest"), Some(session));

        let roots = catalog.children(scan_id, 0).expect("roots");
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].name, "keep.txt");
        assert_eq!(roots[0].size_alloc, 4_096);
    }

    #[test]
    fn finishing_an_unknown_session_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog = catalog(&dir);
        let error = catalog
            .finish_session(999, ScanStatus::Complete, 0, 0, 0)
            .expect_err("must fail");
        assert!(matches!(error, CatalogError::SessionNotFound(999)));
    }

    #[test]
    fn writer_refuses_an_unknown_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let catalog = catalog(&dir);
        let error = catalog.writer(42).err().expect("must fail");
        assert!(matches!(error, CatalogError::SessionNotFound(42)));
    }

    #[test]
    fn concurrent_reads_see_the_old_snapshot_during_a_write_transaction() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("catalog.db");
        let catalog = Catalog::open(&path).expect("open");
        let scan_id = catalog
            .begin_session(Platform::Macos, false)
            .expect("begin");

        // A second connection holding an open write transaction, exactly like a
        // flush in progress.
        let mut writer = Connection::open(&path).expect("writer connection");
        writer
            .busy_timeout(Duration::from_secs(5))
            .expect("timeout");
        let tx = writer.transaction().expect("begin write");
        tx.execute(
            "INSERT INTO entries (scan_id, parent_id, name, kind, size_alloc, size_logical, \
                                  dev, file_id, nlink, attrs, category, tags) \
             VALUES (?1, 0, 'in-flight.bin', 1, 100, 100, 1, 1, 1, 0, 'other', 0)",
            params![scan_id],
        )
        .expect("insert");

        // The read connection must not block, and must not see the uncommitted row.
        assert_eq!(catalog.entry_count(scan_id).expect("count"), 0);

        tx.commit().expect("commit");
        assert_eq!(catalog.entry_count(scan_id).expect("count"), 1);
    }

    #[test]
    fn unicode_names_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("catalog.db");
        let catalog = Catalog::open(&path).expect("open");
        let scan_id = catalog
            .begin_session(Platform::Macos, false)
            .expect("begin");

        let mut writer = catalog.writer(scan_id).expect("writer");
        writer
            .accept(storagelens_core::Entry {
                parent: 0,
                name: "Фотографии 📷".into(),
                kind: storagelens_core::EntryKind::Dir,
                size_alloc: 0,
                size_logical: 0,
                dev: 1,
                file_id: 1,
                nlink: 1,
                attrs: 0,
                created: 0,
                modified: 0,
                accessed: 0,
                hardlink_dup: false,
            })
            .expect("accept");
        writer.flush().expect("flush");

        let roots = catalog.children(scan_id, 0).expect("children");
        assert_eq!(roots.len(), 1);
        assert_eq!(roots[0].name, "Фотографии 📷");
    }

    #[test]
    fn a_database_from_a_newer_build_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("catalog.db");

        {
            let conn = Connection::open(&path).expect("open");
            conn.execute_batch(
                "CREATE TABLE schema_version (version INTEGER PRIMARY KEY, applied_at INTEGER NOT NULL);",
            )
            .expect("version table");
            conn.execute(
                "INSERT INTO schema_version (version, applied_at) VALUES (?1, 0)",
                params![schema::SCHEMA_VERSION + 1],
            )
            .expect("insert version");
        }

        let error = Catalog::open(&path).expect_err("must refuse");
        assert!(matches!(error, CatalogError::SchemaTooNew { .. }));
    }

    #[test]
    fn database_file_is_created_on_disk() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("nested").join("deep").join("catalog.db");
        let catalog = Catalog::open(&path).expect("open");
        assert!(path.exists());
        assert_eq!(catalog.path(), path);
    }
}
