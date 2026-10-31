//! Smart collections (#28, PLAN §9.7): saved Map queries that re-evaluate
//! against whatever scan is current.
//!
//! A collection is a name and a query. The query is stored as versioned JSON
//! (`{"v":1,"by":"type","categories":["video"],"minSize":524288000,...}`) so a
//! later build can add fields without a migration. It holds no scan ids: a
//! folder limit is an absolute **path**, resolved against each scan when the
//! collection is opened, so a result can never come from an older scan.
//!
//! Queries are validated when saved. A stored query that no longer parses (a
//! hand-edited database, a query from a newer build) is still listed — with
//! [`CollectionQuery::parse`]'s error — so it can be renamed or deleted rather
//! than breaking the list.

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use storagelens_core::Category;

use crate::error::{CatalogError, Result};
use crate::util::now;

/// The query format this build writes and reads.
pub const QUERY_VERSION: u32 = 1;

/// Longest name accepted, in characters.
pub const MAX_NAME_CHARS: usize = 120;

/// Groupings a query may name (the Map's selector).
pub const GROUPINGS: [&str; 6] = ["folder", "type", "app", "age", "size", "cleanliness"];
/// Charts a query may name.
pub const CHARTS: [&str; 2] = ["treemap", "sunburst"];

/// What a collection shows: the Map's grouping, chart, and filters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CollectionQuery {
    /// Format version; see [`QUERY_VERSION`].
    pub v: u32,
    pub by: String,
    #[serde(default = "default_chart")]
    pub chart: String,
    /// Empty means every type.
    #[serde(default)]
    pub categories: Vec<Category>,
    /// Allocated bytes, at least.
    #[serde(default)]
    pub min_size: Option<u64>,
    #[serde(default)]
    pub older_than_days: Option<u32>,
    /// Only files under this absolute folder path.
    #[serde(default)]
    pub within: Option<String>,
}

fn default_chart() -> String {
    CHARTS[0].to_owned()
}

impl CollectionQuery {
    /// Parse and validate stored or submitted JSON. The error says what is
    /// wrong in words a person can act on.
    pub fn parse(text: &str) -> std::result::Result<Self, String> {
        let query: Self =
            serde_json::from_str(text).map_err(|error| format!("not a valid query: {error}"))?;
        query.validate()?;
        Ok(query)
    }

    /// Check every field is one this build understands.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if self.v != QUERY_VERSION {
            return Err(format!(
                "query version {} is not supported (this build reads version {QUERY_VERSION})",
                self.v
            ));
        }
        if !GROUPINGS.contains(&self.by.as_str()) {
            return Err(format!("unknown grouping \"{}\"", self.by));
        }
        if !CHARTS.contains(&self.chart.as_str()) {
            return Err(format!("unknown chart \"{}\"", self.chart));
        }
        if self.within.as_deref().is_some_and(str::is_empty) {
            return Err("the folder limit is an empty path".to_owned());
        }
        Ok(())
    }

    /// The JSON stored for this query.
    pub fn to_json(&self) -> Result<String> {
        serde_json::to_string(self).map_err(|error| CatalogError::Other(error.to_string()))
    }
}

/// A stored collection. `query` is the raw JSON: parse it with
/// [`CollectionQuery::parse`], which may fail for an old or edited row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    pub id: i64,
    pub name: String,
    pub query: String,
    pub created_at: i64,
    pub updated_at: i64,
}

fn clean_name(name: &str) -> Result<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(CatalogError::Other("a collection needs a name".to_owned()));
    }
    if trimmed.chars().count() > MAX_NAME_CHARS {
        return Err(CatalogError::Other(format!(
            "a collection name can be at most {MAX_NAME_CHARS} characters"
        )));
    }
    Ok(trimmed.to_owned())
}

fn row(row: &Row<'_>) -> rusqlite::Result<Collection> {
    Ok(Collection {
        id: row.get(0)?,
        name: row.get(1)?,
        query: row.get(2)?,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
    })
}

const COLUMNS: &str = "id, name, query, created_at, updated_at";

/// Save a new collection; returns it.
pub fn create(conn: &Connection, name: &str, query: &CollectionQuery) -> Result<Collection> {
    let name = clean_name(name)?;
    query.validate().map_err(CatalogError::Other)?;
    let at = now();
    conn.execute(
        "INSERT INTO collection (name, query, created_at, updated_at) VALUES (?1, ?2, ?3, ?3)",
        params![name, query.to_json()?, at],
    )?;
    get(conn, conn.last_insert_rowid())?
        .ok_or_else(|| CatalogError::Other("the new collection was not saved".to_owned()))
}

/// Every collection, by name (case-insensitive), then id.
pub fn list(conn: &Connection) -> Result<Vec<Collection>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM collection ORDER BY name COLLATE NOCASE, id"
    ))?;
    let rows = statement.query_map([], row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn get(conn: &Connection, id: i64) -> Result<Option<Collection>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM collection WHERE id = ?1"),
            params![id],
            row,
        )
        .optional()?)
}

fn missing(id: i64) -> CatalogError {
    CatalogError::Other(format!("collection {id} does not exist"))
}

pub fn rename(conn: &Connection, id: i64, name: &str) -> Result<Collection> {
    let name = clean_name(name)?;
    let changed = conn.execute(
        "UPDATE collection SET name = ?1, updated_at = ?2 WHERE id = ?3",
        params![name, now(), id],
    )?;
    if changed == 0 {
        return Err(missing(id));
    }
    get(conn, id)?.ok_or_else(|| missing(id))
}

pub fn delete(conn: &Connection, id: i64) -> Result<()> {
    if conn.execute("DELETE FROM collection WHERE id = ?1", params![id])? == 0 {
        return Err(missing(id));
    }
    Ok(())
}
