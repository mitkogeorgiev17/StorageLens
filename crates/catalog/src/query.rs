//! Read queries over a completed (or in-progress) scan.

use std::collections::HashMap;

use rusqlite::{params, Connection, Row};

use storagelens_core::{Category, EntryKind, Tags};

use crate::error::Result;
use crate::model::{CategoryTotal, SizeCursor, StoredEntry};
use crate::util::{to_i64, to_u64};

const ENTRY_COLUMNS: &str = "id, parent_id, name, ext, kind, size_alloc, size_logical, dev, \
                             file_id, nlink, attrs, created, modified, accessed, category, tags, \
                             hardlink_dup";

fn column_error(index: usize, message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message,
        )),
    )
}

fn map_entry(row: &Row<'_>) -> rusqlite::Result<StoredEntry> {
    let kind_value: i64 = row.get(4)?;
    let kind = u8::try_from(kind_value)
        .ok()
        .and_then(EntryKind::from_u8)
        .ok_or_else(|| column_error(4, format!("invalid entry kind {kind_value}")))?;

    let category_value: Option<String> = row.get(14)?;
    let category = category_value
        .as_deref()
        .and_then(Category::from_name)
        .unwrap_or(Category::Other);

    let tags_value: i64 = row.get(15)?;

    Ok(StoredEntry {
        id: row.get(0)?,
        parent_id: row.get(1)?,
        name: row.get(2)?,
        ext: row.get(3)?,
        kind,
        size_alloc: to_u64(row.get(5)?),
        size_logical: to_u64(row.get(6)?),
        dev: u32::try_from(row.get::<_, i64>(7)?).unwrap_or(0),
        file_id: to_u64(row.get(8)?),
        nlink: u32::try_from(row.get::<_, i64>(9)?).unwrap_or(0),
        attrs: u32::try_from(row.get::<_, i64>(10)?).unwrap_or(0),
        created: row.get(11)?,
        modified: row.get(12)?,
        accessed: row.get(13)?,
        category,
        tags: Tags::from_bits(tags_value as u64),
        hardlink_dup: row.get::<_, i64>(16)? != 0,
    })
}

/// One entry of `scan_id` by id.
pub fn entry(conn: &Connection, scan_id: i64, id: i64) -> Result<Option<StoredEntry>> {
    use rusqlite::OptionalExtension;
    let sql = format!("SELECT {ENTRY_COLUMNS} FROM entries WHERE scan_id = ?1 AND id = ?2");
    Ok(conn
        .prepare_cached(&sql)?
        .query_row(params![scan_id, id], map_entry)
        .optional()?)
}

/// Direct children of `parent_id`, largest first.
pub fn children(conn: &Connection, scan_id: i64, parent_id: i64) -> Result<Vec<StoredEntry>> {
    let sql = format!(
        "SELECT {ENTRY_COLUMNS} FROM entries \
         WHERE scan_id = ?1 AND parent_id = ?2 \
         ORDER BY size_alloc DESC, name ASC"
    );
    let mut statement = conn.prepare_cached(&sql)?;
    let rows = statement.query_map(params![scan_id, parent_id], map_entry)?;

    let mut entries = Vec::new();
    for row in rows {
        entries.push(row?);
    }
    Ok(entries)
}

/// The largest regular files, using keyset pagination.
///
/// Pass the last row of a page as `after` to get the next page. There is no
/// `OFFSET` anywhere: it degrades linearly and this feeds a million-row table.
pub fn largest(
    conn: &Connection,
    scan_id: i64,
    min_size: u64,
    after: Option<SizeCursor>,
    limit: u32,
) -> Result<Vec<StoredEntry>> {
    let (cursor_size, cursor_id) = after
        .map(|cursor| (to_i64(cursor.size_alloc), cursor.id))
        .unwrap_or((i64::MAX, i64::MAX));

    let sql = format!(
        "SELECT {ENTRY_COLUMNS} FROM entries \
         WHERE scan_id = ?1 \
           AND kind = 1 \
           AND size_alloc >= ?2 \
           AND (size_alloc < ?3 OR (size_alloc = ?3 AND id < ?4)) \
         ORDER BY size_alloc DESC, id DESC \
         LIMIT ?5"
    );
    let mut statement = conn.prepare_cached(&sql)?;
    let rows = statement.query_map(
        params![
            scan_id,
            to_i64(min_size),
            cursor_size,
            cursor_id,
            i64::from(limit)
        ],
        map_entry,
    )?;

    let mut entries = Vec::new();
    for row in rows {
        entries.push(row?);
    }
    Ok(entries)
}

/// Allocated bytes and file counts per primary category.
pub fn category_totals(conn: &Connection, scan_id: i64) -> Result<Vec<CategoryTotal>> {
    let mut statement = conn.prepare_cached(
        // Hard-link duplicates keep their row (and their true size) so the path
        // stays browsable, but they were already counted at their first path.
        "SELECT category, SUM(size_alloc), COUNT(*) FROM entries \
         WHERE scan_id = ?1 AND kind = 1 AND hardlink_dup = 0 \
         GROUP BY category",
    )?;
    let rows = statement.query_map(params![scan_id], |row| {
        let category: Option<String> = row.get(0)?;
        let bytes: i64 = row.get(1)?;
        let count: i64 = row.get(2)?;
        Ok((
            category
                .as_deref()
                .and_then(Category::from_name)
                .unwrap_or(Category::Other),
            to_u64(bytes),
            to_u64(count),
        ))
    })?;

    // Unknown category strings collapse into `Other`, so merge by category rather
    // than trusting the SQL grouping to be one row per bucket.
    let mut merged: HashMap<Category, CategoryTotal> = HashMap::new();
    for row in rows {
        let (category, bytes, count) = row?;
        let total = merged.entry(category).or_insert(CategoryTotal {
            category,
            bytes_alloc: 0,
            file_count: 0,
        });
        total.bytes_alloc = total.bytes_alloc.saturating_add(bytes);
        total.file_count = total.file_count.saturating_add(count);
    }

    let mut totals: Vec<CategoryTotal> = merged.into_values().collect();
    totals.sort_by(|a, b| {
        b.bytes_alloc
            .cmp(&a.bytes_alloc)
            .then_with(|| a.category.cmp(&b.category))
    });
    Ok(totals)
}

/// Number of entries recorded for a scan.
pub fn entry_count(conn: &Connection, scan_id: i64) -> Result<u64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM entries WHERE scan_id = ?1",
        params![scan_id],
        |row| row.get(0),
    )?;
    Ok(to_u64(count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saturating_conversions_do_not_wrap() {
        assert_eq!(to_i64(u64::MAX), i64::MAX);
        assert_eq!(to_i64(42), 42);
        assert_eq!(to_u64(-1), 0);
        assert_eq!(to_u64(42), 42);
    }
}
