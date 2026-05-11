//! Public contract coverage for statement execution context and DML
//! routing. These tests intentionally drive `execute_query` and the
//! application use case surface instead of private frame modules.

use reddb::application::ExecuteQueryInput;
use reddb::auth::Role;
use reddb::runtime::mvcc::{
    clear_current_auth_identity, clear_current_tenant, set_current_auth_identity,
};
use reddb::storage::schema::Value;
use reddb::{QueryUseCases, RedDBOptions, RedDBRuntime};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

fn row_count(rt: &RedDBRuntime, sql: &str) -> usize {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
        .result
        .records
        .len()
}

fn show_config_value(rt: &RedDBRuntime, key: &str) -> String {
    let result = rt
        .execute_query(&format!("SHOW CONFIG {key}"))
        .unwrap_or_else(|err| panic!("SHOW CONFIG {key}: {err:?}"));
    result
        .result
        .records
        .first()
        .and_then(|record| record.get("value"))
        .map(|value| format!("{value:?}"))
        .unwrap_or_else(|| "Null".to_string())
}

fn selected_ids(rt: &RedDBRuntime, sql: &str) -> Vec<i64> {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    result
        .result
        .records
        .iter()
        .map(|record| match record.get("id") {
            Some(Value::Integer(id)) => *id,
            other => panic!("expected integer id, got {other:?}"),
        })
        .collect()
}

fn seed_target_scan_fixture(rt: &RedDBRuntime) {
    exec(rt, "CREATE TABLE items (id INT, score INT, touched INT)");
    for id in 0..5 {
        exec(
            rt,
            &format!(
                "INSERT INTO items (id, score, touched) VALUES ({id}, {}, 0)",
                id * 10
            ),
        );
    }
    exec(rt, "CREATE INDEX idx_items_id ON items (id) USING HASH");
}

fn assert_update_and_delete_target_same_rows(predicate: &str, expected_ids: &[i64]) {
    let update_rt = runtime();
    let delete_rt = runtime();
    seed_target_scan_fixture(&update_rt);
    seed_target_scan_fixture(&delete_rt);

    let updated = update_rt
        .execute_query(&format!("UPDATE items SET touched = 1 WHERE {predicate}"))
        .unwrap_or_else(|err| panic!("UPDATE predicate {predicate}: {err:?}"));
    let deleted = delete_rt
        .execute_query(&format!("DELETE FROM items WHERE {predicate}"))
        .unwrap_or_else(|err| panic!("DELETE predicate {predicate}: {err:?}"));

    assert_eq!(updated.affected_rows, expected_ids.len() as u64);
    assert_eq!(deleted.affected_rows, expected_ids.len() as u64);
    assert_eq!(
        selected_ids(
            &update_rt,
            "SELECT id FROM items WHERE touched = 1 ORDER BY id"
        ),
        expected_ids
    );
    assert_eq!(
        selected_ids(&delete_rt, "SELECT id FROM items ORDER BY id"),
        (0..5)
            .filter(|id| !expected_ids.contains(id))
            .collect::<Vec<_>>()
    );
}

#[test]
fn read_statement_context_observes_tenant_config_auth_and_policy_state() {
    clear_current_tenant();
    clear_current_auth_identity();
    let rt = runtime();

    exec(&rt, "CREATE TABLE docs (id INT, tenant_id TEXT, body TEXT)");
    exec(
        &rt,
        "INSERT INTO docs (id, tenant_id, body) VALUES \
         (1, 'acme', 'a'), (2, 'acme', 'b'), (3, 'globex', 'g')",
    );
    exec(&rt, "ALTER TABLE docs ENABLE ROW LEVEL SECURITY");

    assert_eq!(row_count(&rt, "SELECT * FROM docs"), 0);
    assert_eq!(row_count(&rt, "WITHIN TENANT 'acme' SELECT * FROM docs"), 0);

    exec(
        &rt,
        "CREATE POLICY scoped_read ON docs FOR SELECT \
         USING (tenant_id = CURRENT_TENANT())",
    );
    assert_eq!(row_count(&rt, "WITHIN TENANT 'acme' SELECT * FROM docs"), 2);
    assert_eq!(
        row_count(&rt, "WITHIN TENANT 'globex' SELECT * FROM docs"),
        1
    );

    exec(
        &rt,
        "SET CONFIG runtime.result_cache.backend = 'blob_cache'",
    );
    assert!(
        show_config_value(&rt, "runtime.result_cache.backend").contains("blob_cache"),
        "SHOW CONFIG should expose the current runtime config"
    );

    set_current_auth_identity("reader".to_string(), Role::Read);
    let read = rt.execute_query("SELECT 1");
    assert!(read.is_ok(), "Role::Read should execute SELECT: {read:?}");
    let write = rt.execute_query("INSERT INTO docs (id, tenant_id, body) VALUES (4, 'acme', 'x')");
    let err = write.expect_err("Role::Read must not execute INSERT");
    assert!(
        err.to_string().contains("permission denied") && err.to_string().contains("Write"),
        "expected write privilege denial, got {err:?}"
    );
    clear_current_auth_identity();
}

#[test]
fn collection_contract_enforces_insert_and_mutation_paths_through_application_api() {
    let rt = runtime();
    let q = QueryUseCases::new(&rt);

    q.execute(ExecuteQueryInput {
        query: "CREATE TABLE audit_log (id INT, body TEXT) APPEND ONLY".into(),
    })
    .unwrap();
    let inserted = q
        .execute(ExecuteQueryInput {
            query: "INSERT INTO audit_log (id, body) VALUES (1, 'created')".into(),
        })
        .unwrap();
    assert_eq!(inserted.affected_rows, 1);

    let update = q.execute(ExecuteQueryInput {
        query: "UPDATE audit_log SET body = 'mutated' WHERE id = 1".into(),
    });
    let update_err = update.expect_err("APPEND ONLY should reject UPDATE");
    assert!(update_err.to_string().contains("APPEND ONLY"));

    let delete = q.execute(ExecuteQueryInput {
        query: "DELETE FROM audit_log WHERE id = 1".into(),
    });
    let delete_err = delete.expect_err("APPEND ONLY should reject DELETE");
    assert!(delete_err.to_string().contains("APPEND ONLY"));

    let surviving = q
        .execute(ExecuteQueryInput {
            query: "SELECT id FROM audit_log".into(),
        })
        .unwrap();
    assert_eq!(surviving.result.records.len(), 1);
}

#[test]
fn update_and_delete_share_observable_target_scan_semantics() {
    assert_update_and_delete_target_same_rows("id = 3", &[3]);
    assert_update_and_delete_target_same_rows("score > 25", &[3, 4]);
}
