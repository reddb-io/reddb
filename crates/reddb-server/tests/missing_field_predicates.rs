//! Missing nullable fields must behave like SQL NULL in every read path, and
//! fields added by UPDATE must immediately become visible to predicates.

use std::path::Path;

use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

fn open(path: &Path) -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::persistent(path)).expect("runtime opens")
}

fn matching_ids(runtime: &RedDBRuntime, predicate: &str) -> Vec<String> {
    runtime
        .execute_query(&format!(
            "SELECT id FROM probes WHERE {predicate} ORDER BY id"
        ))
        .expect("predicate executes")
        .result
        .records
        .iter()
        .map(|row| row.get("id").expect("id").plain_text())
        .collect()
}

fn assert_updated_row_is_queryable(runtime: &RedDBRuntime) {
    assert_eq!(matching_ids(runtime, "marker = 'set-later'"), ["omitted"]);

    let count = runtime
        .execute_query("SELECT COUNT(*) AS n FROM probes WHERE marker = 'set-later'")
        .expect("aggregate predicate executes");
    assert_eq!(
        count.result.records[0].get("n").expect("count").to_string(),
        "1"
    );
}

#[test]
fn omitted_nullable_field_and_later_update_survive_index_and_reopen() {
    let directory = tempfile::tempdir().expect("fixture directory");
    let path = directory.path().join("missing-field.rdb");

    {
        let runtime = open(&path);
        runtime
            .execute_query("CREATE TABLE probes (id TEXT, marker TEXT)")
            .expect("table");
        runtime
            .execute_query("CREATE INDEX probes_marker ON probes (marker) USING HASH")
            .expect("marker index");
        runtime
            .execute_query("INSERT INTO probes (id) VALUES ('omitted')")
            .expect("omitted marker row");
        runtime
            .execute_query("INSERT INTO probes (id, marker) VALUES ('explicit-null', NULL)")
            .expect("explicit null row");

        let projected = runtime
            .execute_query("SELECT id, marker FROM probes ORDER BY id")
            .expect("projection");
        assert_eq!(projected.result.records.len(), 2);
        assert!(projected
            .result
            .records
            .iter()
            .all(|row| row.get("marker") == Some(&Value::Null)));
        assert_eq!(
            matching_ids(&runtime, "marker IS NULL"),
            ["explicit-null", "omitted"]
        );

        let updated = runtime
            .execute_query("UPDATE probes SET marker = 'set-later' WHERE id = 'omitted'")
            .expect("materialize omitted field");
        assert_eq!(updated.affected_rows, 1);
        assert_updated_row_is_queryable(&runtime);
        runtime.checkpoint().expect("checkpoint updated row");
    }

    let reopened = open(&path);
    assert_eq!(matching_ids(&reopened, "marker IS NULL"), ["explicit-null"]);
    assert_updated_row_is_queryable(&reopened);
}
