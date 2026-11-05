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

#[test]
fn ordered_update_uses_reopened_positional_keys_and_preserves_other_fields() {
    let directory = tempfile::tempdir().expect("ordered fixture");
    let path = directory.path().join("ordered.rdb");
    seed(&path);
    let check = |runtime: &RedDBRuntime| {
        let rows = runtime
            .execute_query("SELECT id,n,payload FROM updated_rows ORDER BY id")
            .expect("ordered result");
        assert_eq!(rows.result.records.len(), 3);
        for (row, (id, n, payload)) in
            rows.result
                .records
                .iter()
                .zip([(0, 2, "zero"), (1, 1, "one"), (2, 1, "two")])
        {
            assert_eq!(row.get("id").expect("id").to_string(), id.to_string());
            assert_eq!(row.get("n").expect("n").to_string(), n.to_string());
            assert_eq!(
                row.get("payload").expect("payload").as_text(),
                Some(payload)
            );
        }
    };
    {
        let runtime = open(&path, false);
        verify(&runtime, 1);
        let manager = runtime
            .db()
            .store()
            .get_collection("updated_rows")
            .expect("rows");
        let mut positional = 0;
        manager.for_each_entity(|entity| {
            if entity.data.as_row().is_some_and(|row| row.named.is_none()) {
                positional += 1;
            }
            true
        });
        assert!(positional > 0, "fixture must exercise positional rows");
        // Reinserted id=0 has a newer logical identity than id=2. Missing
        // ordering keys would choose id=2 through the identity tie-break.
        let updated = runtime
            .execute_query(
                "UPDATE updated_rows SET n = 2 ORDER BY id ASC LIMIT 1 RETURNING id,payload",
            )
            .expect("ordered positional update");
        assert_eq!(updated.affected_rows, 1);
        assert_eq!(
            updated.result.records[0]
                .get("id")
                .expect("selected id")
                .to_string(),
            "0"
        );
        assert_eq!(
            updated.result.records[0]
                .get("payload")
                .expect("selected payload")
                .as_text(),
            Some("zero")
        );
        check(&runtime);
        runtime.checkpoint().expect("publish ordered update");
    }
    check(&open(&path, false));
}
