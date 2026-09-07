use std::sync::Arc;

use reddb::auth::{AuthConfig, AuthStore, Role};
use reddb::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

fn fixture(turbo: bool) -> (tempfile::TempDir, RedDBRuntime) {
    let directory = tempfile::tempdir().expect("temporary database directory");
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(directory.path().join("db.rdb")))
        .expect("runtime");
    rt.execute_query(if turbo {
        "CREATE COLLECTION embeddings KIND vector.turbo DIM 2 METRIC cosine"
    } else {
        "CREATE VECTOR template DIM 2 METRIC cosine"
    })
    .expect("vector collection");
    if !turbo {
        // Legacy collections predate the TurboQuant marker installed by CREATE VECTOR.
        let db = rt.db();
        let mut contract = db.collection_contract("template").expect("vector contract");
        contract.name = "embeddings".to_string();
        db.store()
            .create_collection("embeddings")
            .expect("legacy collection");
        db.save_collection_contract(contract)
            .expect("legacy vector contract");
    }
    rt.execute_query("INSERT INTO embeddings VECTOR (dense) VALUES ([0.8,0.6])")
        .expect("fractional vector");
    (directory, rt)
}

#[test]
fn vector_analysis_executes_instead_of_reusing_cached_measurements() {
    let (_directory, rt) = fixture(false);
    let sql = "VECTOR SEARCH embeddings SIMILAR TO [1.0,0.0] LIMIT 1";
    let first = rt.execute_query(sql).expect("first search");
    assert!(
        !first
            .result
            .stats
            .vector
            .as_ref()
            .expect("metrics")
            .cache_hit
    );
    let second = rt.execute_query(sql).expect("cached search");
    assert!(
        second
            .result
            .stats
            .vector
            .as_ref()
            .expect("metrics")
            .cache_hit
    );
    let analyzed = rt
        .execute_query(&format!("EXPLAIN ANALYZE {sql}"))
        .expect("fresh analysis");
    let metrics = analyzed.result.stats.vector.expect("fresh metrics");
    assert!(!metrics.cache_hit);
    assert_eq!(metrics.access_path, "vector_exact_scan");
    assert_eq!(metrics.candidates_examined, 1);
    assert_eq!(metrics.exact_distance_evaluations, 1);
    assert_eq!(metrics.rows_returned, 1);
}

#[test]
fn turbo_plan_and_measurement_name_the_executed_path() {
    let (_directory, rt) = fixture(true);
    let sql = "VECTOR SEARCH embeddings SIMILAR TO [1.0,0.0] LIMIT 1";
    let plan = rt.execute_query(&format!("EXPLAIN {sql}")).expect("plan");
    assert!(plan
        .result
        .records
        .iter()
        .any(|row| { row.get("op") == Some(&Value::text("vector_turbo_search")) }));
    let analyzed = rt
        .execute_query(&format!("EXPLAIN ANALYZE {sql}"))
        .expect("analysis");
    let metrics = analyzed.result.stats.vector.expect("metrics");
    assert_eq!(metrics.access_path, "vector_turbo_search");
    assert!(metrics.index_used);
    assert_eq!(metrics.rows_returned, 1);
    assert_eq!(metrics.exact_distance_evaluations, 1);
}

#[test]
fn vector_analysis_keeps_the_target_privilege_gate() {
    let (_directory, rt) = fixture(false);
    let auth = Arc::new(AuthStore::new(AuthConfig::default()));
    auth.set_enforcement_mode(reddb::auth::enforcement_mode::PolicyEnforcementMode::PolicyOnly);
    auth.create_user("alice", "fixture-password", Role::Read)
        .expect("test user");
    // IAM evaluation activates when a policy exists; grant an unrelated verb.
    auth.put_policy(reddb::auth::policies::Policy::from_json_str(
        r#"{"id":"read-only","version":1,"statements":[{"effect":"allow","actions":["vector:read"],"resources":["vector:embeddings"]}]}"#,
    ).expect("test policy")).expect("store policy");
    auth.attach_policy(
        reddb::auth::store::PrincipalRef::User(reddb::auth::UserId::platform("alice")),
        "read-only",
    )
    .expect("attach policy");
    rt.set_auth_store(auth);
    struct IdentityGuard;
    impl Drop for IdentityGuard {
        fn drop(&mut self) {
            clear_current_auth_identity();
        }
    }
    let _guard = IdentityGuard;
    set_current_auth_identity("alice".to_string(), Role::Read);
    let sql = "VECTOR SEARCH embeddings SIMILAR TO [1.0,0.0] LIMIT 1";
    assert!(rt.execute_query(sql).is_err());
    let error = rt
        .execute_query(&format!("EXPLAIN ANALYZE {sql}"))
        .expect_err("no grant");
    assert!(error.to_string().contains("permission denied"), "{error}");
}

#[test]
fn registered_hnsw_does_not_turn_an_exact_runtime_search_into_ann() {
    let (directory, rt) = fixture(false);
    let db = rt.db();
    let mut metadata = reddb::physical::PhysicalMetadataFile::from_state(
        db.options().clone(),
        db.catalog_snapshot(),
        Default::default(),
        db.physical_indexes(),
        None,
    );
    let index = metadata
        .indexes
        .iter_mut()
        .find(|index| index.kind == reddb::index::IndexKind::VectorHnsw)
        .expect("HNSW registry entry");
    index.collection = Some("embeddings".to_string());
    index.enabled = true;
    metadata
        .save_for_data_path(&directory.path().join("db.rdb"))
        .expect("test registry association");
    let status = db
        .index_statuses()
        .into_iter()
        .find(|index| {
            index.kind == "vector.hnsw" && index.collection.as_deref() == Some("embeddings")
        })
        .expect("associated index");
    assert!(status.declared && status.operational && status.enabled);
    let plan = rt
        .execute_query("EXPLAIN VECTOR SEARCH embeddings SIMILAR TO [1.0,0.0] LIMIT 1")
        .expect("plan");
    assert!(plan.result.records.iter().any(|row| {
        row.get("op") == Some(&Value::text("vector_exact_scan"))
            && matches!(row.get("access_path_reason"), Some(Value::Text(reason)) if reason.contains("not used by this route"))
    }), "{:?}", plan.result.records);
}

#[test]
fn vector_analysis_does_not_end_the_callers_transaction() {
    let (_directory, rt) = fixture(true);
    rt.execute_query("CREATE TABLE changes (id INT)")
        .expect("table");
    rt.execute_query("BEGIN").expect("begin");
    rt.execute_query("INSERT INTO changes (id) VALUES (1)")
        .expect("staged write");
    rt.execute_query("EXPLAIN ANALYZE VECTOR SEARCH embeddings SIMILAR TO [1.0,0.0] LIMIT 1")
        .expect("vector analysis inside transaction");
    rt.execute_query("ROLLBACK")
        .expect("caller can still roll back");
    let rows = rt
        .execute_query("SELECT * FROM changes")
        .expect("read after rollback");
    assert!(rows.result.records.is_empty());
}
