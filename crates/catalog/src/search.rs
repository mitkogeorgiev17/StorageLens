//! Filename search (#24) over the `entries_fts` index (schema v3).
//!
//! # Queries
//!
//! The user's text is never handed to FTS5 as syntax. Each whitespace-separated
//! term becomes a quoted phrase with a prefix star — `img_20` → `"img_20"*` —
//! and the terms are ANDed. FTS5 tokenizes the phrase like the names, so
//! `img_20` matches the adjacent tokens `img` and `20…` in `IMG_2024.jpg`,
//! case- and accent-insensitively. Quotes in the input are escaped; terms with
//! no indexable character (`-`, `.`) are dropped, and a query with none left
//! matches nothing.
//!
//! # Indexing
//!
//! Each scan is indexed in bulk by [`index_names`] when it finishes (schema v3
//! explains why not by trigger). Searching an unindexed scan indexes it first.
//!
//! # Ranking and paging
//!
//! Biggest first (a folder by its subtree total from `dir_rollup`), then most
//! recently modified, then id — a total order, paged with a keyset cursor on
//! all three. No `OFFSET`. Hard-link duplicates are left out: their bytes are
//! counted under their first name.

use rusqlite::{params, Connection, OptionalExtension};
use storagelens_core::{Category, EntryKind};

use crate::error::Result;
use crate::util::{to_i64, to_u64};

/// Add every name of `scan_id` to the index, once. Called when a scan
/// finishes; [`search`] also calls it, so a scan whose app quit before it
/// was indexed becomes searchable on first use. A no-op for an indexed scan.
pub fn index_names(conn: &Connection, scan_id: i64) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    let pending: bool = tx
        .query_row(
            "SELECT names_indexed = 0 FROM scan_session WHERE id = ?1",
            params![scan_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or(false);
    if pending {
        tx.execute(
            "INSERT INTO entries_fts(rowid, name) SELECT id, name FROM entries WHERE scan_id = ?1",
            params![scan_id],
        )?;
        tx.execute(
            "UPDATE scan_session SET names_indexed = 1 WHERE id = ?1",
            params![scan_id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Where the next page starts: the last row of the previous one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchCursor {
    pub size: u64,
    pub modified: i64,
    pub id: i64,
}

/// One search result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub id: i64,
    pub name: String,
    pub kind: EntryKind,
    pub category: Category,
    /// Allocated bytes: own size for a file, subtree total for a directory.
    pub size: u64,
    pub size_logical: u64,
    /// Unix seconds; 0 when unknown.
    pub modified: i64,
}

/// Whether FTS5's tokenizer (as configured in schema v3) keeps `c` in a token.
/// Mirrors the categories `L* N* Co M* So Sk` closely enough to drop terms that
/// would tokenize to nothing.
fn indexable(c: char) -> bool {
    c.is_alphanumeric() || (!c.is_ascii() && !c.is_whitespace() && !c.is_control())
}

/// The FTS5 expression for a user's query, or `None` if it has nothing to match.
pub fn match_expression(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split_whitespace()
        .filter(|term| term.chars().any(indexable))
        .map(|term| format!("\"{}\"*", term.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

const SEARCH_SQL: &str = "
SELECT id, name, kind, category, size, size_logical, modified FROM (
  SELECT e.id AS id, e.name AS name, e.kind AS kind, e.category AS category,
         CASE WHEN e.kind = 2 THEN COALESCE(
           (SELECT total_alloc FROM dir_rollup WHERE scan_id = e.scan_id AND dir_id = e.id), 0)
         ELSE e.size_alloc END AS size,
         CASE WHEN e.kind = 2 THEN COALESCE(
           (SELECT total_logical FROM dir_rollup WHERE scan_id = e.scan_id AND dir_id = e.id), 0)
         ELSE e.size_logical END AS size_logical,
         COALESCE(e.modified, 0) AS modified
  FROM entries_fts
  JOIN entries e ON e.id = entries_fts.rowid
  WHERE entries_fts MATCH ?1 AND e.scan_id = ?2 AND e.hardlink_dup = 0
)
WHERE (size, modified, id) < (?3, ?4, ?5)
ORDER BY size DESC, modified DESC, id DESC
LIMIT ?6";

/// Up to `limit` entries of `scan_id` whose name matches `query`, after
/// `after` in ranking order.
pub fn search(
    conn: &Connection,
    scan_id: i64,
    query: &str,
    after: Option<SearchCursor>,
    limit: u32,
) -> Result<Vec<SearchHit>> {
    let Some(expression) = match_expression(query) else {
        return Ok(Vec::new());
    };
    index_names(conn, scan_id)?;
    let (size, modified, id) = after.map_or((i64::MAX, i64::MAX, i64::MAX), |cursor| {
        (to_i64(cursor.size), cursor.modified, cursor.id)
    });
    let mut statement = conn.prepare_cached(SEARCH_SQL)?;
    let rows = statement.query_map(
        params![expression, scan_id, size, modified, id, i64::from(limit)],
        |row| {
            let kind: i64 = row.get(2)?;
            let category: Option<String> = row.get(3)?;
            Ok(SearchHit {
                id: row.get(0)?,
                name: row.get(1)?,
                kind: u8::try_from(kind)
                    .ok()
                    .and_then(EntryKind::from_u8)
                    .unwrap_or(EntryKind::Other),
                category: category
                    .as_deref()
                    .and_then(Category::from_name)
                    .unwrap_or(Category::Other),
                size: to_u64(row.get(4)?),
                size_logical: to_u64(row.get(5)?),
                modified: row.get(6)?,
            })
        },
    )?;
    let mut hits = Vec::new();
    for row in rows {
        hits.push(row?);
    }
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_become_quoted_prefix_phrases() {
        assert_eq!(match_expression("report"), Some("\"report\"*".to_owned()));
        assert_eq!(
            match_expression("  tax  2024 "),
            Some("\"tax\"* \"2024\"*".to_owned())
        );
        assert_eq!(
            match_expression("say \"hi\""),
            Some("\"say\"* \"\"\"hi\"\"\"*".to_owned())
        );
        assert_eq!(
            match_expression("OR NOT"),
            Some("\"OR\"* \"NOT\"*".to_owned())
        );
        assert_eq!(match_expression(""), None);
        assert_eq!(match_expression(" - . "), None);
        assert_eq!(match_expression("📷"), Some("\"📷\"*".to_owned()));
    }
}
