//! Reopened positional rows must retain untouched fields during SQL updates.
use std::path::Path;

use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};

fn open(path: &Path, initial: bool) -> RedDBRuntime {
    let options = RedDBOptions::persistent(path);
    RedDBRuntime::with_options(if initial {
        options
            .with_storage_profile(StorageDeployPreset::Serverless.selection())
            .expect("paged fixture")
    } else {
        options
    })
    .expect("runtime opens")
}

fn seed(path: &Path) {
    {
        let runtime = open(path, true);
        runtime.checkpoint().expect("initialize paged file");
    }
    let runtime = open(path, false);
    for query in [
        "CREATE TABLE updated_rows (id INTEGER, n INTEGER, payload TEXT)",
        "CREATE UNIQUE INDEX rows_id ON updated_rows (id)",
        "INSERT INTO updated_rows (id,n,payload) VALUES (0,0,'zero'),(1,0,'one'),(2,0,'two')",
        "UPDATE updated_rows SET n = 1",
        "DELETE FROM updated_rows WHERE id < 2",
        "INSERT INTO updated_rows (id,n,payload) VALUES (0,1,'zero'),(1,1,'one')",
    ] {
        runtime.execute_query(query).expect("seed fixture");
    }
    runtime.checkpoint().expect("checkpoint reinserted rows");
}

fn verify(runtime: &RedDBRuntime, revision: i64) {
    let rows = runtime
        .execute_query("SELECT id,n,payload FROM updated_rows")
        .expect("full row scan");
    assert_eq!(rows.result.records.len(), 3);
    for (id, payload) in [(0, "zero"), (1, "one"), (2, "two")] {
        let row = rows
            .result
            .records
            .iter()
            .find(|row| {
                row.get("id")
                    .is_some_and(|value| value.to_string() == id.to_string())
            })
            .expect("untouched row ID must remain present");
        assert_eq!(row.get("n").expect("n").to_string(), revision.to_string());
        assert_eq!(
            row.get("payload").expect("payload").as_text(),
            Some(payload)
        );
        let indexed = runtime
            .execute_query(&format!("SELECT payload FROM updated_rows WHERE id = {id}"))
            .expect("unique index lookup");
        assert_eq!(indexed.result.records.len(), 1);
        assert_eq!(
            indexed.result.records[0]
                .get("payload")
                .expect("indexed payload")
                .as_text(),
            Some(payload)
        );
    }
}

fn exercise(update: &str) {
    let directory = tempfile::tempdir().expect("fixture directory");
    let path = directory.path().join("data.rdb");
    seed(&path);
    {
        let runtime = open(&path, false);
        verify(&runtime, 1);
        runtime.execute_query(update).expect("update reopened rows");
        verify(&runtime, 2);
        runtime.checkpoint().expect("checkpoint updated rows");
    }
    verify(&open(&path, false), 2);
}

#[test]
fn constant_update_preserves_reopened_positional_fields_and_indexes() {
    exercise("UPDATE updated_rows SET n = 2");
}

#[test]
fn expression_update_preserves_reopened_positional_fields_and_indexes() {
    exercise("UPDATE updated_rows SET n = n + 1");
}
