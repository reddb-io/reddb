//! Borrowed aggregate scans must resolve the schema of native positional rows.
use reddb_server::{RedDBOptions, RedDBRuntime, StorageDeployPreset};

fn reopened_fixture() -> (tempfile::TempDir, RedDBRuntime) {
    let directory = tempfile::tempdir().expect("owned native fixture");
    let options = RedDBOptions::persistent(directory.path().join("database.rdb"))
        .with_memory_budget(256 * 1024 * 1024)
        .with_storage_profile(StorageDeployPreset::Serverless.selection())
        .expect("native paged profile");
    let runtime = RedDBRuntime::with_options(options.clone()).expect("runtime");
    runtime
        .execute_query("CREATE TABLE probe (id INTEGER PRIMARY KEY,bucket INTEGER,payload TEXT)")
        .expect("table");
    runtime
        .execute_query("CREATE INDEX probe_bucket ON probe (bucket)")
        .expect("secondary index");
    let rows = (0..20)
        .map(|id| format!("({id},{},'value-{id}')", id % 10))
        .collect::<Vec<_>>()
        .join(",");
    runtime
        .execute_query(&format!(
            "INSERT INTO probe (id,bucket,payload) VALUES {rows}"
        ))
        .expect("fixture rows");
    assert_eq!(
        scalar(
            &runtime,
            "SELECT COUNT(*) AS n FROM probe WHERE bucket=7",
            "n"
        ),
        "2"
    );
    runtime.checkpoint().expect("publish positional rows");
    drop(runtime);
    let runtime = RedDBRuntime::with_options(options).expect("cold reopen");
    (directory, runtime)
}

fn scalar(runtime: &RedDBRuntime, sql: &str, column: &str) -> String {
    let result = runtime.execute_query(sql).expect(sql);
    assert_eq!(result.result.records.len(), 1, "{sql}");
    result.result.records[0]
        .get(column)
        .unwrap_or_else(|| panic!("missing {column}: {sql}"))
        .to_string()
}

#[test]
fn native_table_read_views_preserve_indexed_and_scanned_payloads_after_mutation() {
    let (directory, runtime) = reopened_fixture();
    runtime
        .execute_query("CREATE INDEX probe_ordered_id ON probe (id) USING BTREE")
        .expect("ordered lookup over reopened positional rows");
    let queries = [
        "SELECT id,payload FROM probe WHERE id BETWEEN 7 AND 17 AND bucket=7 ORDER BY id LIMIT 2",
        "SELECT * FROM probe WHERE id BETWEEN 7 AND 17 AND bucket=7 ORDER BY id LIMIT 2",
        "SELECT id,payload FROM probe WHERE bucket=7 ORDER BY id LIMIT 2",
        "SELECT * FROM probe WHERE bucket=7 ORDER BY id LIMIT 2",
        "SELECT id,payload FROM probe WHERE LENGTH(payload)>0 AND bucket+0=7 ORDER BY id LIMIT 2",
        "SELECT * FROM probe WHERE LENGTH(payload)>0 AND bucket+0=7 ORDER BY id LIMIT 2",
    ];
    let check = |runtime: &RedDBRuntime, changed: bool| {
        for sql in queries {
            let result = runtime.execute_query(sql).expect(sql);
            let expected: &[i64] = if changed { &[7] } else { &[7, 17] };
            assert_eq!(result.result.records.len(), expected.len(), "{sql}");
            for (row, id) in result.result.records.iter().zip(expected) {
                assert_eq!(
                    row.get("id").expect("physical payload id").to_string(),
                    id.to_string(),
                    "{sql}"
                );
                let payload = if changed {
                    "changed".to_string()
                } else {
                    format!("value-{id}")
                };
                assert_eq!(
                    row.get("payload"),
                    Some(&reddb_types::Value::text(payload)),
                    "{sql}"
                );
            }
        }
    };
    check(&runtime, false);
    for sql in [
        "SELECT payload FROM probe WHERE bucket=7 ORDER BY id DESC LIMIT 1",
        "SELECT payload FROM probe WHERE bucket=7 ORDER BY id LIMIT 1 OFFSET 1",
        "SELECT payload FROM probe WHERE bucket=7 AND id BETWEEN 7 AND 17 ORDER BY id DESC LIMIT 1",
        "SELECT payload FROM probe WHERE bucket=7 AND id BETWEEN 7 AND 17 ORDER BY id LIMIT 1 OFFSET 1",
    ] {
        let result = runtime.execute_query(sql).expect(sql);
        assert_eq!(result.result.records.len(), 1, "{sql}");
        let row = &result.result.records[0];
        assert_eq!(row.get("payload"), Some(&reddb_types::Value::text("value-17")), "{sql}");
        assert!(row.get("id").is_none(), "sort-only columns must not leak: {sql}");
    }
    runtime
        .execute_query("UPDATE probe SET payload='changed' WHERE id=7")
        .expect("replace payload");
    runtime
        .execute_query("DELETE FROM probe WHERE id=17")
        .expect("delete second candidate");
    check(&runtime, true);
    runtime
        .checkpoint()
        .expect("publish changed source and indexes");
    drop(runtime);
    let options = RedDBOptions::persistent(directory.path().join("database.rdb"))
        .with_memory_budget(256 * 1024 * 1024)
        .with_storage_profile(StorageDeployPreset::Serverless.selection())
        .expect("same native paged profile");
    let reopened = RedDBRuntime::with_options(options).expect("reopen changed view");
    check(&reopened, true);
}

#[test]
fn native_filtered_count_resolves_reopened_column_names() {
    let (_directory, runtime) = reopened_fixture();
    let rows = runtime
        .execute_query("SELECT id FROM probe WHERE bucket=7")
        .expect("indexed filter");
    assert_eq!(rows.result.records.len(), 2, "index and payload are intact");
    for sql in [
        "SELECT COUNT(*) AS n FROM probe WHERE bucket=7",
        "SELECT COUNT(*) AS n FROM probe p WHERE p.bucket=7",
        "SELECT COUNT(*) AS n FROM probe WHERE bucket+0=7",
        "SELECT COUNT(*) AS n FROM probe WHERE LENGTH(payload)>0 AND bucket=7",
    ] {
        assert_eq!(scalar(&runtime, sql, "n"), "2", "{sql}");
    }
}

#[test]
fn native_scalar_aggregate_arguments_resolve_reopened_column_names() {
    let (_directory, runtime) = reopened_fixture();
    for (sql, column, expected) in [
        ("SELECT COUNT(bucket) AS n FROM probe", "n", "20"),
        ("SELECT SUM(id) AS total FROM probe", "total", "190"),
        (
            "SELECT SUM(id) AS total FROM probe WHERE bucket=7",
            "total",
            "24",
        ),
        ("SELECT MIN(id) AS n FROM probe WHERE bucket=7", "n", "7"),
        ("SELECT MAX(id) AS n FROM probe WHERE bucket=7", "n", "17"),
    ] {
        assert_eq!(scalar(&runtime, sql, column), expected, "{sql}");
    }
}

#[test]
fn native_pushdown_groups_resolve_reopened_column_names() {
    let (_directory, runtime) = reopened_fixture();
    let sql = "SELECT bucket,COUNT(*),SUM(id) FROM probe WHERE bucket>=7 GROUP BY bucket";
    let result = runtime.execute_query(sql).expect("grouped pushdown");
    assert_eq!(result.result.records.len(), 3);
    for row in &result.result.records {
        let bucket = row
            .get("bucket")
            .expect("group key")
            .to_string()
            .parse::<i64>()
            .expect("integer group");
        assert!((7..=9).contains(&bucket));
        assert_eq!(row.get("COUNT(*)").expect("count").to_string(), "2");
        assert_eq!(
            row.get("SUM(id)").expect("sum").to_string(),
            (2 * bucket + 10).to_string()
        );
    }
}

#[test]
fn native_parallel_groups_read_complete_reopened_payloads() {
    let (_directory, runtime) = reopened_fixture();
    let result = runtime
        .execute_query("SELECT bucket,COUNT(*) AS n,SUM(id) AS total FROM probe GROUP BY bucket ORDER BY bucket")
        .expect("parallel group fold over native positional rows");
    assert_eq!(result.result.records.len(), 10);
    for (row, bucket) in result.result.records.iter().zip(0..10) {
        assert_eq!(
            row.get("bucket").expect("group").to_string(),
            bucket.to_string()
        );
        assert_eq!(row.get("n").expect("count").to_string(), "2");
        assert_eq!(
            row.get("total").expect("sum").to_string(),
            (2 * bucket + 10).to_string()
        );
    }
}

#[test]
fn native_fallback_groups_resolve_reopened_column_names() {
    let (_directory, runtime) = reopened_fixture();
    let sql = "SELECT bucket+0 AS b,COUNT(*) AS n,SUM(id+0) AS total FROM probe WHERE bucket+0>=7 GROUP BY bucket+0 HAVING COUNT(*)=2 ORDER BY b";
    let result = runtime.execute_query(sql).expect("fallback expressions");
    assert_eq!(result.result.records.len(), 3);
    for (row, bucket) in result.result.records.iter().zip(7..=9) {
        assert_eq!(
            row.get("b").expect("group key").to_string(),
            bucket.to_string()
        );
        assert_eq!(row.get("n").expect("count").to_string(), "2");
        assert_eq!(
            row.get("total").expect("sum").to_string(),
            (2 * bucket + 10).to_string()
        );
    }
}

#[test]
fn aggregate_collection_schema_does_not_override_explicit_row_schema() {
    use reddb_server::storage::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};
    use reddb_types::Value;
    use std::sync::Arc;

    let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    runtime
        .execute_query("CREATE TABLE probe (id INTEGER,bucket INTEGER)")
        .expect("table");
    runtime
        .execute_query("INSERT INTO probe (id,bucket) VALUES (1,7)")
        .expect("seed schema");
    let manager = runtime
        .db()
        .store()
        .get_collection("probe")
        .expect("collection");
    let mut schema = manager
        .column_schema()
        .expect("collection schema")
        .as_ref()
        .clone();
    schema.reverse();
    let columns = schema
        .iter()
        .map(|name| match name.as_str() {
            "id" => Value::Integer(2),
            "bucket" => Value::Integer(7),
            other => panic!("unexpected {other}"),
        })
        .collect();
    runtime
        .db()
        .store()
        .insert_auto(
            "probe",
            UnifiedEntity::new(
                EntityId::new(0),
                EntityKind::TableRow {
                    table: Arc::from("probe"),
                    row_id: 0,
                },
                EntityData::Row(RowData {
                    columns,
                    named: None,
                    schema: Some(Arc::new(schema)),
                }),
            ),
        )
        .expect("explicit row schema");
    assert_eq!(
        scalar(&runtime, "SELECT SUM(id) AS total FROM probe", "total"),
        "3"
    );
    assert_eq!(
        scalar(
            &runtime,
            "SELECT COUNT(*) AS n FROM probe WHERE bucket=7",
            "n"
        ),
        "2"
    );
}
