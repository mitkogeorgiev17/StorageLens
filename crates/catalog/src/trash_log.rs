//! `trash_log` (#34): one row per item a deletion moved to the Trash, kept
//! for the History screen and for undo.

use rusqlite::{params, Connection};

use crate::error::Result;
use crate::util::{to_i64, to_u64};

/// One item moved to the Trash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashLogRow {
    pub id: i64,
    /// Unix seconds.
    pub performed_at: i64,
    pub original_path: String,
    pub bytes: u64,
    pub rule_id: Option<String>,
    /// The deletion's manifest.
    pub manifest_path: Option<String>,
    /// When undo put it back.
    pub restored_at: Option<i64>,
}

/// Record items moved by one deletion: `(path, bytes, rule id)`.
pub fn record(
    conn: &Connection,
    performed_at: i64,
    manifest_path: &str,
    items: &[(String, u64, Option<String>)],
) -> Result<()> {
    let transaction = conn.unchecked_transaction()?;
    {
        let mut insert = transaction.prepare(
            "INSERT INTO trash_log (performed_at, original_path, bytes, rule_id, manifest_path) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for (path, bytes, rule_id) in items {
            insert.execute(params![
                performed_at,
                path,
                to_i64(*bytes),
                rule_id,
                manifest_path
            ])?;
        }
    }
    transaction.commit()?;
    Ok(())
}

/// Mark `paths` of the deletion `manifest_path` restored at `restored_at`.
pub fn mark_restored(
    conn: &Connection,
    manifest_path: &str,
    paths: &[String],
    restored_at: i64,
) -> Result<()> {
    let transaction = conn.unchecked_transaction()?;
    {
        let mut update = transaction.prepare(
            "UPDATE trash_log SET restored_at = ?1 \
             WHERE manifest_path = ?2 AND original_path = ?3 AND restored_at IS NULL",
        )?;
        for path in paths {
            update.execute(params![restored_at, manifest_path, path])?;
        }
    }
    transaction.commit()?;
    Ok(())
}

/// Every logged item, newest first.
pub fn list(conn: &Connection) -> Result<Vec<TrashLogRow>> {
    let mut statement = conn.prepare(
        "SELECT id, performed_at, original_path, bytes, rule_id, manifest_path, restored_at \
         FROM trash_log ORDER BY performed_at DESC, id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(TrashLogRow {
            id: row.get(0)?,
            performed_at: row.get(1)?,
            original_path: row.get(2)?,
            bytes: to_u64(row.get(3)?),
            rule_id: row.get(4)?,
            manifest_path: row.get(5)?,
            restored_at: row.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
