//! `rule_hit` rows (#31): what the cleanup rules found in a scan.
//!
//! The rules engine (`storagelens-analysis`) produces them; they are stored
//! per scan and replaced as a whole whenever the rules are re-run, so they
//! always reflect one evaluation of one catalog.

use rusqlite::{params, Connection};

use crate::error::Result;
use crate::util::{to_i64, to_u64};

/// One stored hit.
#[derive(Debug, Clone, PartialEq)]
pub struct RuleHitRow {
    pub rule_id: String,
    /// Absolute path of the matched file or folder.
    pub path: String,
    /// The matched entry, when it is part of the scan.
    pub entry_id: Option<i64>,
    /// Allocated bytes of the whole subtree.
    pub bytes: u64,
    /// `safe`, `review`, `keep`, or `never`.
    pub risk: String,
    pub confidence: f64,
    /// Starts selected on the Cleanup screen.
    pub selected: bool,
}

/// Replace every hit of `scan_id` with `rows`, in one transaction.
pub fn replace(conn: &Connection, scan_id: i64, rows: &[RuleHitRow]) -> Result<()> {
    let transaction = conn.unchecked_transaction()?;
    transaction.execute("DELETE FROM rule_hit WHERE scan_id = ?1", params![scan_id])?;
    {
        let mut insert = transaction.prepare(
            "INSERT INTO rule_hit (scan_id, rule_id, path, entry_id, bytes, risk, confidence, selected) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        )?;
        for row in rows {
            insert.execute(params![
                scan_id,
                row.rule_id,
                row.path,
                row.entry_id,
                to_i64(row.bytes),
                row.risk,
                row.confidence,
                row.selected,
            ])?;
        }
    }
    transaction.commit()?;
    Ok(())
}

/// Every hit of `scan_id`, biggest first.
pub fn list(conn: &Connection, scan_id: i64) -> Result<Vec<RuleHitRow>> {
    let mut statement = conn.prepare(
        "SELECT rule_id, path, entry_id, bytes, risk, confidence, selected FROM rule_hit \
         WHERE scan_id = ?1 ORDER BY bytes DESC, rule_id, path",
    )?;
    let rows = statement.query_map(params![scan_id], |row| {
        Ok(RuleHitRow {
            rule_id: row.get(0)?,
            path: row.get(1)?,
            entry_id: row.get(2)?,
            bytes: to_u64(row.get(3)?),
            risk: row.get(4)?,
            confidence: row.get(5)?,
            selected: row.get(6)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
