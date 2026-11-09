#![cfg(feature = "embedded")]

use std::time::Duration;

use reddb_client::{
    bookmark_routing::{BookmarkTarget, CausalReadOptions},
    ErrorCode, QueryOptions, Reddb, ValueOut,
};

#[tokio::test]
async fn embedded_single_node_satisfies_strong_and_bounded_reads() {
    let db = Reddb::connect("memory://")
        .await
        .expect("embedded database");
    db.query("CREATE TABLE readings (id INTEGER)")
        .await
        .expect("create table");
    db.query("INSERT INTO readings (id) VALUES (1)")
        .await
        .expect("insert row");

    for options in [
        QueryOptions::strong(),
        QueryOptions::bounded_staleness(Duration::ZERO),
        QueryOptions::local(),
    ] {
        let result = db
            .query_with_options("SELECT id FROM readings", options)
            .await
            .expect("single node satisfies the requested consistency");
        assert_eq!(result.rows.len(), 1);
    }
}

#[tokio::test]
async fn embedded_rejects_external_causal_bookmark() {
    let db = Reddb::connect("memory://")
        .await
        .expect("embedded database");
    let error = db
        .query_with_options(
            "SELECT 1",
            QueryOptions::causal(
                BookmarkTarget::new(3, 42),
                CausalReadOptions::with_deadline(Duration::from_millis(10)),
            ),
        )
        .await
        .expect_err("embedded runtime cannot validate an external cluster bookmark");
    assert_eq!(error.code, ErrorCode::ConsistencyUnsupported);
}

#[tokio::test]
async fn embedded_single_file_reopens_with_committed_data() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("driver-contract.rdb");
    let normalized = path.to_string_lossy().replace('\\', "/");
    let uri = if normalized.starts_with('/') {
        format!("file://{normalized}")
    } else {
        format!("file:///{normalized}")
    };

    let first = Reddb::connect(&uri)
        .await
        .expect("open single-file database");
    first
        .query("CREATE TABLE durable_items (id INTEGER, name TEXT)")
        .await
        .expect("create table");
    first
        .query("INSERT INTO durable_items (id, name) VALUES (1, 'persisted')")
        .await
        .expect("insert row");
    first.close().await.expect("checkpoint database");
    drop(first);

    let second = Reddb::connect(&uri).await.expect("reopen same database");
    let result = second
        .query("SELECT name FROM durable_items WHERE id = 1")
        .await
        .expect("read committed row after reopen");
    assert_eq!(result.rows.len(), 1);
    assert!(result.rows[0].iter().any(|(column, value)| column == "name"
        && matches!(value, ValueOut::String(name) if name == "persisted")));
    second.close().await.expect("checkpoint reopened database");
    drop(second);

    let artifacts = std::fs::read_dir(directory.path())
        .expect("list database directory")
        .map(|entry| entry.expect("directory entry").path())
        .collect::<Vec<_>>();
    assert_eq!(
        artifacts,
        [path],
        "embedded persistence must leave exactly one canonical database file"
    );
}
