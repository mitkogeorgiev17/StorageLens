//! `trash_log` and the single-entry lookup (#34).

use storagelens_catalog::Catalog;

#[test]
fn deletions_are_logged_and_restores_marked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let catalog = Catalog::open(dir.path().join("catalog.db")).expect("open");
    let items = vec![
        ("/a".to_owned(), 10, Some("rule.a".to_owned())),
        ("/b".to_owned(), 20, None),
    ];
    catalog
        .record_trashed(100, "/m/trash-1.json", &items)
        .expect("record");
    catalog
        .record_trashed(200, "/m/trash-2.json", &[("/c".to_owned(), 5, None)])
        .expect("record");
    catalog
        .mark_restored(
            "/m/trash-1.json",
            &["/b".to_owned(), "/missing".to_owned()],
            150,
        )
        .expect("restore");
    let log = catalog.trash_log().expect("log");
    let summary: Vec<_> = log
        .iter()
        .map(|row| (row.original_path.as_str(), row.bytes, row.restored_at))
        .collect();
    assert_eq!(
        summary,
        [("/c", 5, None), ("/a", 10, None), ("/b", 20, Some(150))]
    );
    assert_eq!(log[1].rule_id.as_deref(), Some("rule.a"));
    assert_eq!(log[1].manifest_path.as_deref(), Some("/m/trash-1.json"));
    assert_eq!(log[1].performed_at, 100);
    assert!(catalog.entry(1, 1).expect("lookup").is_none());
}
