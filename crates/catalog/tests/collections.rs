//! Smart collections (#28): save, list, rename, delete, persistence across
//! reopening, and graceful handling of queries this build cannot read.

use rusqlite::params;
use storagelens_catalog::{Catalog, CollectionQuery, QUERY_VERSION};
use storagelens_core::Category;

fn videos() -> CollectionQuery {
    CollectionQuery {
        v: QUERY_VERSION,
        by: "type".to_owned(),
        chart: "treemap".to_owned(),
        categories: vec![Category::Video],
        min_size: Some(500 * 1024 * 1024),
        older_than_days: Some(365),
        within: Some("/Users/me/Movies".to_owned()),
    }
}

#[test]
fn collections_survive_reopening_the_catalog() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("catalog.db");
    let saved = {
        let catalog = Catalog::open(&path).expect("open");
        catalog
            .create_collection("  Old big videos ", &videos())
            .expect("create")
    };
    assert_eq!(saved.name, "Old big videos", "names are trimmed");

    let catalog = Catalog::open(&path).expect("reopen");
    let listed = catalog.collections().expect("list");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, saved.id);
    assert_eq!(
        CollectionQuery::parse(&listed[0].query).expect("parse"),
        videos()
    );
}

#[test]
fn rename_and_delete() {
    let dir = tempfile::tempdir().expect("tempdir");
    let catalog = Catalog::open(dir.path().join("catalog.db")).expect("open");
    let b = catalog.create_collection("b", &videos()).expect("b");
    let a = catalog.create_collection("A", &videos()).expect("a");
    let names: Vec<String> = catalog
        .collections()
        .expect("list")
        .into_iter()
        .map(|c| c.name)
        .collect();
    assert_eq!(names, ["A", "b"], "listed by name, ignoring case");

    let renamed = catalog.rename_collection(b.id, "Zed").expect("rename");
    assert_eq!(renamed.name, "Zed");
    assert!(
        catalog.rename_collection(b.id, "   ").is_err(),
        "names are required"
    );
    assert!(catalog.rename_collection(b.id, &"x".repeat(121)).is_err());

    catalog.delete_collection(a.id).expect("delete");
    assert!(catalog.collection(a.id).expect("get").is_none());
    assert!(catalog.delete_collection(a.id).is_err(), "already gone");
    assert!(catalog.rename_collection(9999, "x").is_err());
}

#[test]
fn invalid_queries_are_refused_on_save_and_reported_on_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    let catalog = Catalog::open(dir.path().join("catalog.db")).expect("open");

    let bad_grouping = CollectionQuery {
        by: "colour".to_owned(),
        ..videos()
    };
    assert!(catalog.create_collection("x", &bad_grouping).is_err());
    let future = CollectionQuery { v: 99, ..videos() };
    assert!(catalog.create_collection("x", &future).is_err());

    // A row written by hand (or by a newer build) still lists; parsing says why.
    for (name, text) in [
        ("garbage", "{not json"),
        ("future", r#"{"v":2,"by":"type"}"#),
        ("extra", r#"{"v":1,"by":"type","sortBy":"name"}"#),
        (
            "category",
            r#"{"v":1,"by":"type","categories":["holograms"]}"#,
        ),
    ] {
        catalog
            .connection()
            .execute(
                "INSERT INTO collection (name, query, created_at, updated_at) VALUES (?1, ?2, 0, 0)",
                params![name, text],
            )
            .expect("insert");
    }
    let listed = catalog.collections().expect("list survives bad rows");
    assert_eq!(listed.len(), 4);
    for collection in &listed {
        let error = CollectionQuery::parse(&collection.query).expect_err(&collection.name);
        assert!(!error.is_empty());
    }
    let future = listed.iter().find(|c| c.name == "future").expect("future");
    assert!(CollectionQuery::parse(&future.query)
        .expect_err("v2")
        .contains("version 2"));

    // Defaults: a minimal query is a treemap with no filters.
    let minimal = CollectionQuery::parse(r#"{"v":1,"by":"age"}"#).expect("minimal");
    assert_eq!(minimal.chart, "treemap");
    assert!(minimal.categories.is_empty() && minimal.within.is_none());
}
