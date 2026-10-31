//! Schema definition and the migration runner.
//!
//! The schema is PLAN §5, unchanged. Migrations are append-only: once a version has
//! shipped, its SQL is frozen. Add a new entry to [`MIGRATIONS`] instead of editing
//! an old one.

use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};

use crate::error::{CatalogError, Result};

/// Highest schema version this build knows how to apply.
pub const SCHEMA_VERSION: u32 = 2;

/// The version table itself. Created outside the first migration so the runner can
/// always read the current version.
const VERSION_TABLE: &str = "
CREATE TABLE IF NOT EXISTS schema_version (
  version    INTEGER PRIMARY KEY,
  applied_at INTEGER NOT NULL
);
";

/// Append-only list of migrations. Index `n` upgrades the schema to version `n + 1`.
const MIGRATIONS: &[&str] = &[
    // v1 — PLAN §5.
    "
CREATE TABLE scan_session (
  id           INTEGER PRIMARY KEY,
  platform     TEXT    NOT NULL,
  started_at   INTEGER NOT NULL,
  finished_at  INTEGER,
  status       TEXT    NOT NULL,
  files_seen   INTEGER NOT NULL DEFAULT 0,
  bytes_seen   INTEGER NOT NULL DEFAULT 0,
  dirs_skipped INTEGER NOT NULL DEFAULT 0,
  elevated     INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE entries (
  id           INTEGER PRIMARY KEY,
  scan_id      INTEGER NOT NULL REFERENCES scan_session(id),
  parent_id    INTEGER NOT NULL,
  name         TEXT    NOT NULL,
  ext          TEXT,
  kind         INTEGER NOT NULL,
  size_alloc   INTEGER NOT NULL,
  size_logical INTEGER NOT NULL,
  dev          INTEGER NOT NULL,
  file_id      INTEGER NOT NULL,
  nlink        INTEGER NOT NULL,
  attrs        INTEGER NOT NULL,
  created      INTEGER,
  modified     INTEGER,
  accessed     INTEGER,
  category     TEXT,
  tags         INTEGER NOT NULL DEFAULT 0,
  hardlink_dup INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX idx_entries_parent ON entries(scan_id, parent_id);
CREATE INDEX idx_entries_size   ON entries(scan_id, size_alloc DESC);
CREATE INDEX idx_entries_cat    ON entries(scan_id, category, size_alloc DESC);
CREATE INDEX idx_entries_ext    ON entries(scan_id, ext);

CREATE TABLE dir_rollup (
  scan_id       INTEGER NOT NULL,
  dir_id        INTEGER NOT NULL,
  total_alloc   INTEGER NOT NULL,
  total_logical INTEGER NOT NULL,
  file_count    INTEGER NOT NULL,
  PRIMARY KEY (scan_id, dir_id)
);

CREATE TABLE rule_hit (
  id         INTEGER PRIMARY KEY,
  scan_id    INTEGER NOT NULL,
  rule_id    TEXT    NOT NULL,
  path       TEXT    NOT NULL,
  entry_id   INTEGER,
  bytes      INTEGER NOT NULL,
  risk       TEXT    NOT NULL,
  confidence REAL    NOT NULL,
  selected   INTEGER NOT NULL DEFAULT 0,
  UNIQUE(scan_id, rule_id, path)
);

CREATE TABLE trash_log (
  id            INTEGER PRIMARY KEY,
  performed_at  INTEGER NOT NULL,
  original_path TEXT    NOT NULL,
  trashed_name  TEXT,
  bytes         INTEGER NOT NULL,
  rule_id       TEXT,
  manifest_path TEXT,
  restored_at   INTEGER
);
",
    // v2 — the scanned root's absolute path (#61). Nullable: sessions recorded
    // before v2 have no root, and inventing one would be worse than admitting it.
    "
ALTER TABLE scan_session ADD COLUMN root TEXT;
",
];

/// Create the version table if needed and apply every missing migration.
///
/// Returns the version the database is at afterwards. Applying an already-current
/// schema is a no-op.
pub fn migrate(conn: &Connection) -> Result<u32> {
    conn.execute_batch(VERSION_TABLE)?;

    let current = current_version(conn)?;
    if current > SCHEMA_VERSION {
        return Err(CatalogError::SchemaTooNew {
            found: current,
            supported: SCHEMA_VERSION,
        });
    }

    for version in (current + 1)..=SCHEMA_VERSION {
        let sql = MIGRATIONS
            .get((version - 1) as usize)
            .ok_or_else(|| CatalogError::Corrupt(format!("missing migration for v{version}")))?;

        let tx = conn.unchecked_transaction()?;
        tx.execute_batch(sql)?;
        tx.execute(
            "INSERT INTO schema_version (version, applied_at) VALUES (?1, ?2)",
            params![version, now()],
        )?;
        tx.commit()?;
    }

    current_version(conn)
}

/// Highest applied migration, or `0` for an empty database.
pub fn current_version(conn: &Connection) -> Result<u32> {
    let version: i64 = conn.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_version",
        [],
        |row| row.get(0),
    )?;
    u32::try_from(version)
        .map_err(|_| CatalogError::Corrupt(format!("negative schema version {version}")))
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_from_empty_to_latest() {
        let conn = Connection::open_in_memory().expect("open");
        let version = migrate(&conn).expect("migrate");
        assert_eq!(version, SCHEMA_VERSION);
        assert_eq!(current_version(&conn).expect("version"), SCHEMA_VERSION);
    }

    #[test]
    fn migrating_twice_is_a_no_op() {
        let conn = Connection::open_in_memory().expect("open");
        migrate(&conn).expect("first");
        let version = migrate(&conn).expect("second");
        assert_eq!(version, SCHEMA_VERSION);

        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM schema_version", [], |row| row.get(0))
            .expect("count");
        assert_eq!(rows, i64::from(SCHEMA_VERSION));
    }

    #[test]
    fn a_v1_database_upgrades_without_losing_sessions() {
        let conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(VERSION_TABLE).expect("version table");
        conn.execute_batch(MIGRATIONS[0]).expect("v1 schema");
        conn.execute(
            "INSERT INTO schema_version (version, applied_at) VALUES (1, 0)",
            [],
        )
        .expect("mark v1");
        conn.execute(
            "INSERT INTO scan_session (platform, started_at, status) VALUES ('macos', 1, 'complete')",
            [],
        )
        .expect("v1 session");

        assert_eq!(migrate(&conn).expect("migrate"), 2);
        let (count, root): (i64, Option<String>) = conn
            .query_row("SELECT COUNT(*), MAX(root) FROM scan_session", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .expect("read back");
        assert_eq!(count, 1, "the v1 session survives");
        assert_eq!(root, None, "and honestly has no root");
    }

    #[test]
    fn refuses_a_database_from_a_newer_build() {
        let conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(VERSION_TABLE).expect("version table");
        conn.execute(
            "INSERT INTO schema_version (version, applied_at) VALUES (?1, ?2)",
            params![SCHEMA_VERSION + 1, 0],
        )
        .expect("insert future version");

        let error = migrate(&conn).expect_err("must refuse");
        assert!(matches!(error, CatalogError::SchemaTooNew { .. }));
    }

    #[test]
    fn every_table_from_the_plan_exists() {
        let conn = Connection::open_in_memory().expect("open");
        migrate(&conn).expect("migrate");

        for table in [
            "scan_session",
            "entries",
            "dir_rollup",
            "rule_hit",
            "trash_log",
        ] {
            let found: i64 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1",
                    params![table],
                    |row| row.get(0),
                )
                .expect("lookup table");
            assert_eq!(found, 1, "missing table {table}");
        }
    }
}
