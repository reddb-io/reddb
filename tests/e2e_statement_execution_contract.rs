//! Public contract coverage for statement execution context and DML
//! routing. These tests intentionally drive `execute_query` and the
//! application use case surface instead of private frame modules.

use reddb::application::ExecuteQueryInput;
use reddb::auth::Role;
use reddb::runtime::mvcc::{
    clear_current_auth_identity, clear_current_tenant, set_current_auth_identity,
    set_current_tenant,
};
use reddb::storage::schema::Value;
use reddb::{QueryUseCases, RedDBOptions, RedDBRuntime};
use std::path::PathBuf;

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime")
}

fn persistent_runtime(path: &PathBuf) -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::persistent(path)).expect("persistent runtime")
}

fn temp_db_path(label: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    path.push(format!("reddb-{label}-{}-{nanos}.rdb", std::process::id()));
    path
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
        .map(|record| {
            match record
                .get("id")
                .or_else(|| record.get("docs.id"))
                .or_else(|| record.get("items.id"))
                .or_else(|| record.get("c0"))
            {
                Some(Value::Integer(id)) => *id,
                other => panic!("expected integer id, got {other:?} in record {record:?}"),
            }
        })
        .collect()
}

fn selected_text(rt: &RedDBRuntime, sql: &str) -> String {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    let record = result
        .result
        .records
        .first()
        .unwrap_or_else(|| panic!("{sql}: expected one row"));
    match record
        .get("c0")
        .or_else(|| record.get("tenant"))
        .or_else(|| record.get("CURRENT_TENANT"))
        .or_else(|| record.get("CURRENT_TENANT()"))
        .or_else(|| record.get("CURRENT_USER"))
        .or_else(|| record.get("CURRENT_USER()"))
    {
        Some(Value::Text(text)) => text.to_string(),
        other => panic!("expected text scalar, got {other:?} in record {record:?}"),
    }
}

fn cleanup_persistent_path(path: &PathBuf) {
    let _ = std::fs::remove_file(path);
    let l2 = path.with_extension("result-cache.l2");
    let _ = std::fs::remove_file(&l2);
    let _ = std::fs::remove_file(reddb_file::blob_cache_control_path(&l2));
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
fn blob_result_cache_rehydrates_after_restart_with_tenant_and_auth_isolation() {
    clear_current_tenant();
    clear_current_auth_identity();
    let path = temp_db_path("result-cache-warm-restart");

    {
        let rt = persistent_runtime(&path);
        exec(
            &rt,
            "SET CONFIG runtime.result_cache.backend = 'blob_cache'",
        );
        set_current_auth_identity("alice".to_string(), Role::Read);
        set_current_tenant("acme".to_string());
        assert_eq!(selected_text(&rt, "SELECT CURRENT_TENANT()"), "acme");
        set_current_tenant("globex".to_string());
        assert_eq!(selected_text(&rt, "SELECT CURRENT_TENANT()"), "globex");
        clear_current_tenant();
        assert_eq!(selected_text(&rt, "SELECT CURRENT_USER()"), "alice");
        clear_current_tenant();
        clear_current_auth_identity();
        rt.checkpoint().expect("checkpoint before restart");
    }

    {
        let rt = persistent_runtime(&path);

        set_current_auth_identity("alice".to_string(), Role::Read);
        set_current_tenant("acme".to_string());
        assert_eq!(selected_text(&rt, "SELECT CURRENT_TENANT()"), "acme");
        set_current_tenant("globex".to_string());
        assert_eq!(selected_text(&rt, "SELECT CURRENT_TENANT()"), "globex");
        clear_current_tenant();
        set_current_auth_identity("bob".to_string(), Role::Read);
        assert_eq!(selected_text(&rt, "SELECT CURRENT_USER()"), "bob");
        set_current_auth_identity("alice".to_string(), Role::Read);
        assert_eq!(selected_text(&rt, "SELECT CURRENT_USER()"), "alice");

        let stats = rt.stats().result_blob_cache;
        assert_eq!(
            stats.hits(),
            3,
            "alice/acme, alice/globex, and alice's user scalar should be served from durable L2"
        );
        assert_eq!(
            stats.misses(),
            1,
            "bob must not reuse alice's result-cache entry"
        );
    }

    clear_current_tenant();
    clear_current_auth_identity();
    cleanup_persistent_path(&path);
}

#[test]
fn blob_result_cache_write_after_restart_invalidates_unrehydrated_l2_entries() {
    clear_current_tenant();
    clear_current_auth_identity();
    let path = temp_db_path("result-cache-restart-invalidation");

    {
        let rt = persistent_runtime(&path);
        exec(
            &rt,
            "SET CONFIG runtime.result_cache.backend = 'blob_cache'",
        );
        exec(&rt, "CREATE TABLE items (id INT)");
        exec(&rt, "INSERT INTO items (id) VALUES (1)");
        assert_eq!(
            selected_ids(&rt, "SELECT id FROM items ORDER BY id"),
            vec![1]
        );
        rt.checkpoint().expect("checkpoint before restart");
    }

    {
        let rt = persistent_runtime(&path);
        exec(&rt, "INSERT INTO items (id) VALUES (2)");
        assert_eq!(
            selected_ids(&rt, "SELECT id FROM items ORDER BY id"),
            vec![1, 2],
            "a table write after restart must not leave stale result-cache L2 visible"
        );
        assert_eq!(
            rt.stats().result_blob_cache.hits(),
            0,
            "the stale pre-restart result should have been invalidated before rehydrate"
        );
    }

    cleanup_persistent_path(&path);
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
