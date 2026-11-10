//! Public contract coverage for statement execution context and DML
//! routing. These tests intentionally drive `execute_query` and the
//! application use case surface instead of private frame modules.

#[allow(dead_code)]
#[path = "../../support/mod.rs"]
mod support;

use reddb::application::ExecuteQueryInput;
use reddb::auth::Role;
use reddb::runtime::mvcc::{
    clear_current_auth_identity, clear_current_tenant, set_current_auth_identity,
    set_current_tenant,
};
use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime")
}

fn persistent_runtime(path: &support::TempDbFile) -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::persistent(path)).expect("persistent runtime")
}

fn temp_db_path(label: &str) -> support::TempDbFile {
    support::temp_db_file(label)
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

fn text_values(rt: &RedDBRuntime, sql: &str, column: &str) -> Vec<String> {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"))
        .result
        .records
        .iter()
        .map(|record| match record.get(column) {
            Some(Value::Text(text)) => text.to_string(),
            other => panic!("expected text column {column}, got {other:?} in {record:?}"),
        })
        .collect()
}

fn uint_value(rt: &RedDBRuntime, sql: &str, column: &str) -> u64 {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    let record = result
        .result
        .records
        .first()
        .unwrap_or_else(|| panic!("{sql}: expected one row"));
    match record.get(column) {
        Some(Value::UnsignedInteger(value)) => *value,
        Some(Value::Integer(value)) if *value >= 0 => *value as u64,
        other => panic!("expected unsigned integer column {column}, got {other:?} in {record:?}"),
    }
}

fn bool_value(rt: &RedDBRuntime, sql: &str, column: &str) -> bool {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    let record = result
        .result
        .records
        .first()
        .unwrap_or_else(|| panic!("{sql}: expected one row"));
    match record.get(column) {
        Some(Value::Boolean(value)) => *value,
        other => panic!("expected boolean column {column}, got {other:?} in {record:?}"),
    }
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
fn table_command_matrix_executes_ddl_dml_index_alter_truncate_and_drop() {
    let rt = runtime();

    exec(
        &rt,
        "CREATE TABLE table_matrix (id INT, status TEXT, score INT)",
    );
    exec(
        &rt,
        "INSERT INTO table_matrix (id, status, score) VALUES \
         (1, 'new', 10), (2, 'new', 20), (3, 'stale', 30)",
    );
    assert_eq!(
        selected_ids(
            &rt,
            "SELECT id FROM table_matrix WHERE status = 'new' ORDER BY id"
        ),
        vec![1, 2]
    );

    exec(
        &rt,
        "CREATE INDEX idx_table_matrix_status ON table_matrix (status) USING HASH",
    );
    assert_eq!(
        uint_value(
            &rt,
            "SHOW INDEXES ON table_matrix WHERE name = 'idx_table_matrix_status'",
            "entries_indexed",
        ),
        3
    );
    let explain_ops = text_values(
        &rt,
        "EXPLAIN SELECT id FROM table_matrix WHERE status = 'new'",
        "op",
    );
    assert!(
        explain_ops.iter().any(|op| op == "index_seek"),
        "expected table status predicate to use index_seek, got {explain_ops:?}"
    );

    exec(
        &rt,
        "UPDATE table_matrix SET status = 'done', score = 25 WHERE id = 2",
    );
    assert_eq!(
        selected_ids(
            &rt,
            "SELECT id FROM table_matrix WHERE status = 'done' ORDER BY id"
        ),
        vec![2]
    );

    exec(&rt, "DELETE FROM table_matrix WHERE status = 'stale'");
    assert_eq!(
        selected_ids(&rt, "SELECT id FROM table_matrix ORDER BY id"),
        vec![1, 2]
    );

    exec(&rt, "ALTER TABLE table_matrix ADD COLUMN urgency INT");
    let columns = || {
        text_values(
            &rt,
            "SELECT name FROM red.columns WHERE collection = 'table_matrix' ORDER BY name",
            "name",
        )
    };
    assert!(columns().contains(&"urgency".to_string()));

    exec(
        &rt,
        "ALTER TABLE table_matrix RENAME COLUMN urgency TO rank",
    );
    let renamed = columns();
    assert!(renamed.contains(&"rank".to_string()));
    assert!(!renamed.contains(&"urgency".to_string()));

    exec(&rt, "ALTER TABLE table_matrix DROP COLUMN rank");
    assert!(!columns().contains(&"rank".to_string()));

    exec(&rt, "DROP INDEX idx_table_matrix_status ON table_matrix");
    let indexes_after_drop = text_values(&rt, "SHOW INDEXES ON table_matrix", "name");
    assert!(
        !indexes_after_drop.contains(&"idx_table_matrix_status".to_string()),
        "DROP INDEX should remove the named index, got {indexes_after_drop:?}"
    );

    exec(&rt, "TRUNCATE TABLE table_matrix");
    let ids_after_truncate = selected_ids(&rt, "SELECT id FROM table_matrix ORDER BY id");
    assert_eq!(ids_after_truncate, Vec::<i64>::new());

    exec(&rt, "DROP TABLE table_matrix");
    assert_eq!(
        row_count(
            &rt,
            "SELECT name FROM red.collections WHERE name = 'table_matrix'"
        ),
        0
    );
}

#[test]
fn graph_command_matrix_executes_core_runtime_surface() {
    let rt = runtime();

    exec(&rt, "CREATE GRAPH graph_matrix");
    assert_eq!(
        row_count(
            &rt,
            "SELECT name FROM red.graphs WHERE name = 'graph_matrix'"
        ),
        1
    );
    exec(
        &rt,
        "INSERT INTO graph_matrix NODE (label, node_type, name) VALUES ('alpha', 'Service', 'Alpha')",
    );
    exec(
        &rt,
        "INSERT INTO graph_matrix NODE (label, node_type, name) VALUES ('beta', 'Service', 'Beta')",
    );
    exec(
        &rt,
        "INSERT INTO graph_matrix NODE (label, node_type, name) VALUES ('gamma', 'Database', 'Gamma')",
    );
    exec(
        &rt,
        "INSERT INTO graph_matrix EDGE (label, from_rid, to_rid, weight) VALUES ('CONNECTS', 'alpha', 'beta', 1.0)",
    );
    exec(
        &rt,
        "INSERT INTO graph_matrix EDGE (label, from_rid, to_rid, weight) VALUES ('CONNECTS', 'beta', 'gamma', 1.0)",
    );

    let matched = text_values(
        &rt,
        "MATCH (a)-[r:CONNECTS]->(b) \
         WHERE a.label = 'alpha' \
         RETURN a.name, b.name, r.label",
        "b.name",
    );
    assert_eq!(matched, vec!["Beta".to_string()]);

    assert_eq!(
        text_values(&rt, "GRAPH PROPERTIES 'alpha'", "label"),
        vec!["alpha".to_string()]
    );

    let neighborhood = text_values(
        &rt,
        "GRAPH NEIGHBORHOOD 'alpha' DEPTH 2 EDGES IN ('CONNECTS')",
        "label",
    );
    assert!(
        neighborhood.contains(&"gamma".to_string()),
        "neighborhood should reach gamma through beta, got {neighborhood:?}"
    );

    let traversal = text_values(
        &rt,
        "GRAPH TRAVERSE FROM 'alpha' STRATEGY bfs DIRECTION outgoing MAX_DEPTH 2 EDGES IN ('CONNECTS')",
        "label",
    );
    assert!(
        traversal.contains(&"gamma".to_string()),
        "traversal should reach gamma through beta, got {traversal:?}"
    );

    assert!(bool_value(
        &rt,
        "GRAPH SHORTEST_PATH FROM 'alpha' TO 'gamma' ALGORITHM dijkstra",
        "path_found",
    ));

    assert_eq!(
        row_count(&rt, "GRAPH CENTRALITY ALGORITHM degree LIMIT 3"),
        3
    );
    assert!(row_count(&rt, "GRAPH COMMUNITY ALGORITHM label_propagation LIMIT 5") >= 1);
    assert!(row_count(&rt, "GRAPH COMPONENTS MODE connected LIMIT 5") >= 1);
    assert!(row_count(&rt, "GRAPH CLUSTERING") >= 1);
    assert!(row_count(&rt, "GRAPH TOPOLOGICAL_SORT") >= 1);

    exec(
        &rt,
        "INSERT INTO graph_matrix EDGE (label, from_rid, to_rid, weight) VALUES ('CONNECTS', 'gamma', 'alpha', 1.0)",
    );
    assert!(row_count(&rt, "GRAPH CYCLES MAX_LENGTH 4") >= 1);

    exec(&rt, "DROP GRAPH graph_matrix");
    assert_eq!(
        row_count(
            &rt,
            "SELECT name FROM red.graphs WHERE name = 'graph_matrix'"
        ),
        0
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
fn show_config_as_json_reconstructs_config_subtree() {
    let rt = runtime();
    exec(
        &rt,
        "SET CONFIG runtime.result_cache.backend = 'blob_cache'",
    );
    exec(
        &rt,
        "SET CONFIG runtime.result_cache.capacity_entries = 128",
    );
    exec(&rt, "SET CONFIG runtime.result_cache.enabled = true");
    exec(&rt, "SET CONFIG runtime.result_cache.backend = 'shadow'");

    let result = rt
        .execute_query("SHOW CONFIG runtime.result_cache AS JSON")
        .expect("SHOW CONFIG AS JSON");
    assert_eq!(result.result.columns, vec!["key", "value"]);
    let record = result.result.records.first().expect("config json row");
    assert!(matches!(
        record.get("key"),
        Some(Value::Text(key)) if key.as_ref() == "runtime.result_cache"
    ));
    let Some(Value::Json(bytes)) = record.get("value") else {
        panic!("expected JSON value, got {record:?}");
    };
    let json: reddb::json::Value = reddb::json::from_slice(bytes).expect("valid config json");
    assert_eq!(json["backend"].as_str(), Some("shadow"));
    assert_eq!(json["capacity_entries"].as_u64(), Some(128));
    assert_eq!(json["enabled"].as_bool(), Some(true));
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
}

#[test]
fn blob_result_cache_recovers_table_result_after_restart() {
    let path = temp_db_path("result-cache-table-rehydrate");
    let query = "SELECT id FROM items ORDER BY id";
    let expected = {
        let rt = persistent_runtime(&path);
        exec(
            &rt,
            "SET CONFIG runtime.result_cache.backend = 'blob_cache'",
        );
        exec(&rt, "CREATE TABLE items (id INT)");
        exec(&rt, "INSERT INTO items (id) VALUES (1)");
        assert_eq!(selected_ids(&rt, query), vec![1]);
        let result = rt.execute_query(query).expect("cached result");
        let fields = result.result.records[0]
            .iter_fields()
            .map(|(name, value)| (name.to_string(), value.clone()))
            .collect::<std::collections::BTreeMap<_, _>>();
        rt.checkpoint().expect("checkpoint result cache");
        fields
    };
    {
        let rt = persistent_runtime(&path);
        let before = rt.result_cache_metrics();
        let result = rt.execute_query(query).expect("rehydrated result");
        let fields = result.result.records[0]
            .iter_fields()
            .map(|(name, value)| (name.to_string(), value.clone()))
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(fields, expected);
        let after = rt.result_cache_metrics();
        assert_eq!(after.0 - before.0, 1, "the result must rehydrate from L2");
        assert_eq!(after.1 - before.1, 0);
    }
}

#[test]
fn blob_result_cache_recovers_join_result_after_restart() {
    let path = temp_db_path("result-cache-join-rehydrate");
    let query = "SELECT a.id AS id FROM items a JOIN items b ON a.id = b.id";
    {
        let rt = persistent_runtime(&path);
        exec(
            &rt,
            "SET CONFIG runtime.result_cache.backend = 'blob_cache'",
        );
        exec(&rt, "CREATE TABLE items (id INT)");
        exec(&rt, "INSERT INTO items (id) VALUES (1)");
        assert_eq!(selected_ids(&rt, query), vec![1]);
        let before = rt.result_cache_metrics();
        assert_eq!(selected_ids(&rt, query), vec![1]);
        assert_eq!(rt.result_cache_metrics().0 - before.0, 1);
        rt.checkpoint().expect("checkpoint join result");
    }
    {
        let rt = persistent_runtime(&path);
        let before = rt.result_cache_metrics();
        assert_eq!(selected_ids(&rt, query), vec![1]);
        let after = rt.result_cache_metrics();
        assert_eq!(after.0 - before.0, 1, "the join result must rehydrate");
    }
}

#[test]
fn blob_cache_recovers_new_entry_after_namespace_flush() {
    use reddb::storage::cache::{BlobCachePolicy, BlobCachePut, L1Admission};
    let path = temp_db_path("blob-cache-flush-rehydrate");
    {
        let rt = persistent_runtime(&path);
        let cache = rt.result_blob_cache();
        let put = || {
            BlobCachePut::new(b"new value".to_vec())
                .with_policy(BlobCachePolicy::default().l1_admission(L1Admission::Never))
        };
        cache.put("ns", "old", put()).expect("old entry");
        cache.put("ns", "key", put()).expect("initial entry");
        assert!(cache.invalidate_namespace("ns"));
        cache.put("ns", "key", put()).expect("replacement entry");
        assert_eq!(
            cache
                .get("ns", "key")
                .expect("entry before restart")
                .value(),
            b"new value"
        );
        assert!(cache.get("ns", "old").is_none());
        rt.checkpoint().expect("checkpoint blob cache");
    }
    {
        let rt = persistent_runtime(&path);
        assert_eq!(
            rt.result_blob_cache()
                .get("ns", "key")
                .expect("entry after restart")
                .value(),
            b"new value"
        );
        assert!(rt.result_blob_cache().get("ns", "old").is_none());
    }
}

#[test]
fn blob_cache_standalone_l2_recovers_after_namespace_flush() {
    use reddb::storage::cache::{BlobCache, BlobCacheConfig, BlobCachePut};
    let path = temp_db_path("blob-cache-standalone-flush");
    let open = || {
        BlobCache::open_with_l2(BlobCacheConfig::default().with_l2_path(path.path()))
            .expect("standalone blob cache")
    };
    {
        let cache = open();
        cache
            .put("ns", "old", BlobCachePut::new(b"old".to_vec()))
            .expect("old entry");
        assert!(cache.invalidate_namespace("ns"));
        cache
            .put("ns", "new", BlobCachePut::new(b"new".to_vec()))
            .expect("new entry");
    }
    let cache = open();
    assert!(cache.get("ns", "old").is_none());
    assert_eq!(
        cache
            .get("ns", "new")
            .expect("new entry after restart")
            .value(),
        b"new"
    );
}

#[test]
fn blob_result_cache_keeps_unrelated_entries_after_table_write() {
    for backend in ["blob_cache", "shadow"] {
        let rt = runtime();
        exec(
            &rt,
            &format!("SET CONFIG runtime.result_cache.backend = '{backend}'"),
        );
        exec(&rt, "CREATE TABLE items (id INT)");
        exec(&rt, "CREATE TABLE noise (id INT)");
        exec(&rt, "INSERT INTO items (id) VALUES (1)");
        exec(&rt, "INSERT INTO noise (id) VALUES (1)");
        let query = "SELECT id FROM items ORDER BY id";
        assert_eq!(selected_ids(&rt, query), vec![1]);
        exec(&rt, "UPDATE noise SET id = 2 WHERE id = 1");
        let before = rt.result_cache_metrics();
        let blob_hits = rt.result_blob_cache().stats().hits();
        assert_eq!(selected_ids(&rt, query), vec![1]);
        let after = rt.result_cache_metrics();
        assert_eq!(
            after.0 - before.0,
            1,
            "unrelated writes should preserve the result"
        );
        assert_eq!(after.1 - before.1, 0);
        assert_eq!(rt.result_blob_cache().stats().hits() - blob_hits, 1);
        exec(&rt, "INSERT INTO items (id) VALUES (2)");
        assert_eq!(selected_ids(&rt, query), vec![1, 2]);
    }
}

#[test]
fn blob_result_cache_defers_l2_for_frequently_written_tables() {
    let path = temp_db_path("result-cache-changing-table");
    let query = "SELECT id FROM items ORDER BY id";
    {
        let rt = persistent_runtime(&path);
        exec(
            &rt,
            "SET CONFIG runtime.result_cache.backend = 'blob_cache'",
        );
        exec(&rt, "CREATE TABLE items (id INT)");
        exec(&rt, "INSERT INTO items (id) VALUES (1)");
        assert_eq!(selected_ids(&rt, query), vec![1]);
        exec(&rt, "INSERT INTO items (id) VALUES (2)");
        let stats = rt.result_blob_cache().stats();
        assert_eq!(selected_ids(&rt, query), vec![1, 2]);
        assert_eq!(
            rt.result_blob_cache().stats().insertions(),
            stats.insertions(),
            "a result immediately following invalidation should stay in memory"
        );
        exec(&rt, "INSERT INTO items (id) VALUES (3)");
        assert_eq!(
            rt.result_blob_cache().stats().namespace_flushes(),
            stats.namespace_flushes(),
            "an in-memory-only entry should not trigger another L2 flush"
        );
        assert_eq!(selected_ids(&rt, query), vec![1, 2, 3]);
        rt.checkpoint()
            .expect("checkpoint while result is volatile");
    }
    {
        let rt = persistent_runtime(&path);
        let before = rt.result_cache_metrics();
        assert_eq!(
            selected_ids(&rt, query),
            vec![1, 2, 3],
            "restart must recompute instead of returning an older L2 result"
        );
        let after = rt.result_cache_metrics();
        assert_eq!(
            after.1 - before.1,
            1,
            "volatile result must not survive restart"
        );
    }
}

#[test]
fn blob_result_cache_promotes_volatile_result_after_writes_stop() {
    let path = temp_db_path("result-cache-quiet-table");
    let query = "SELECT id FROM items ORDER BY id";
    {
        let rt = persistent_runtime(&path);
        exec(
            &rt,
            "SET CONFIG runtime.result_cache.backend = 'blob_cache'",
        );
        exec(&rt, "CREATE TABLE items (id INT)");
        exec(&rt, "INSERT INTO items (id) VALUES (1)");
        assert_eq!(selected_ids(&rt, query), vec![1]);
        exec(&rt, "INSERT INTO items (id) VALUES (2)");
        assert_eq!(selected_ids(&rt, query), vec![1, 2]);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let before = rt.result_blob_cache().stats().insertions();
        assert_eq!(selected_ids(&rt, query), vec![1, 2]);
        assert_eq!(
            rt.result_blob_cache().stats().insertions(),
            before + 1,
            "a hit after the quiet period should publish to L2"
        );
        rt.checkpoint().expect("checkpoint promoted result");
    }
    {
        let rt = persistent_runtime(&path);
        let before = rt.result_cache_metrics();
        assert_eq!(selected_ids(&rt, query), vec![1, 2]);
        assert_eq!(
            rt.result_cache_metrics().0 - before.0,
            1,
            "promoted result should rehydrate after restart"
        );
    }
}

#[test]
fn blob_result_cache_promotion_preserves_remaining_ttl() {
    let rt = runtime();
    exec(
        &rt,
        "SET CONFIG runtime.result_cache.backend = 'blob_cache'",
    );
    exec(&rt, "SET CONFIG runtime.result_cache.ttl_seconds = 5");
    exec(&rt, "CREATE TABLE items (id INT)");
    exec(&rt, "INSERT INTO items (id) VALUES (1)");
    let query = "SELECT id FROM items ORDER BY id";
    assert_eq!(selected_ids(&rt, query), vec![1]);
    exec(&rt, "INSERT INTO items (id) VALUES (2)");
    assert_eq!(selected_ids(&rt, query), vec![1, 2]);
    std::thread::sleep(std::time::Duration::from_millis(1200));
    let before = rt.result_blob_cache().stats().insertions();
    assert_eq!(selected_ids(&rt, query), vec![1, 2]);
    assert_eq!(rt.result_blob_cache().stats().insertions(), before + 1);
    std::thread::sleep(std::time::Duration::from_millis(4100));
    let before = rt.result_cache_metrics();
    assert_eq!(selected_ids(&rt, query), vec![1, 2]);
    assert_eq!(
        rt.result_cache_metrics().1 - before.1,
        1,
        "promotion must not reset the TTL of the cached result"
    );
}

#[test]
fn blob_result_cache_invalidates_entries_evicted_from_tracking() {
    let rt = runtime();
    exec(
        &rt,
        "SET CONFIG runtime.result_cache.backend = 'blob_cache'",
    );
    exec(&rt, "SET CONFIG runtime.result_cache.capacity_entries = 1");
    exec(&rt, "CREATE TABLE items (id INT)");
    exec(&rt, "CREATE TABLE other (id INT)");
    exec(&rt, "INSERT INTO items (id) VALUES (1)");
    exec(&rt, "INSERT INTO other (id) VALUES (1)");
    let query = "SELECT id FROM items ORDER BY id";
    assert_eq!(selected_ids(&rt, query), vec![1]);
    assert_eq!(
        selected_ids(&rt, "SELECT id FROM other ORDER BY id"),
        vec![1]
    );
    exec(&rt, "INSERT INTO items (id) VALUES (2)");
    assert_eq!(selected_ids(&rt, query), vec![1, 2]);
}

#[test]
fn collection_contract_enforces_insert_and_mutation_paths_through_application_api() {
    let rt = runtime();
    let q = &rt;

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
