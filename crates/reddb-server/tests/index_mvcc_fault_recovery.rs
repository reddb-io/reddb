//! Derived index damage must never turn an authoritative table row into a miss.
//!
//! This reproduces the production failure mode rather than only testing index
//! helpers: a physical posting is first rewired to an obsolete MVCC version,
//! then to that same version after VACUUM has reclaimed it.  The first query in
//! each phase is the indexed query, so a table scan cannot warm or repair it.

use std::path::Path;

use reddb_server::storage::EntityId;
use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

const TABLE: &str = "platform_databases";
const KEY: &[u8] = b"org-a";

fn open(path: &Path) -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::persistent(path)).expect("open persistent runtime")
}

fn execute(runtime: &RedDBRuntime, sql: &str) {
    runtime
        .execute_query(sql)
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

fn physical_versions(runtime: &RedDBRuntime) -> (EntityId, EntityId) {
    let manager = runtime
        .db()
        .store()
        .get_collection(TABLE)
        .expect("platform table");
    let mut versions = manager.query_all(|_| true);
    versions.sort_unstable_by_key(|entity| entity.id.raw());
    assert_eq!(versions.len(), 2, "fixture must retain one MVCC update");
    let old = versions
        .iter()
        .find(|entity| entity.xmax != 0)
        .expect("obsolete physical version");
    let current = versions
        .iter()
        .find(|entity| entity.xmax == 0)
        .expect("current physical version");
    assert_eq!(old.logical_id(), current.logical_id());
    assert_ne!(old.id, current.id);
    (old.id, current.id)
}

fn equality_index_name(runtime: &RedDBRuntime) -> String {
    runtime
        .index_store_ref()
        .list_indices(TABLE)
        .into_iter()
        .find(|index| index.columns == ["org_id"])
        .expect("org_id index")
        .hash_lookup_name()
        .into_owned()
}

fn replace_posting(
    runtime: &RedDBRuntime,
    index_name: &str,
    remove: &[EntityId],
    insert: EntityId,
) {
    for entity_id in remove {
        let _ = runtime
            .index_store_ref()
            .hash
            .remove(TABLE, index_name, KEY, *entity_id)
            .expect("remove posting during fault injection");
    }
    runtime
        .index_store_ref()
        .hash
        .insert(TABLE, index_name, KEY.to_vec(), insert)
        .expect("insert posting during fault injection");

    let candidates = runtime
        .index_store_ref()
        .hash_lookup(TABLE, index_name, KEY)
        .expect("read injected posting");
    assert_eq!(
        candidates.iter().copied().collect::<Vec<_>>(),
        [insert],
        "fault fixture must replace the authoritative posting"
    );
}

fn assert_indexed_head(runtime: &RedDBRuntime, phase: &str) {
    runtime.invalidate_result_cache();
    let indexed = runtime
        .execute_query("SELECT id,org_id,payload FROM platform_databases WHERE org_id = 'org-a'")
        .unwrap_or_else(|error| panic!("{phase}: indexed lookup failed: {error}"));
    assert_eq!(indexed.result.records.len(), 1, "{phase}: indexed miss");
    assert_eq!(
        indexed.result.records[0].get("payload"),
        Some(&Value::text("current")),
        "{phase}: indexed lookup returned an obsolete version"
    );

    let scan = runtime
        .execute_query("SELECT id,org_id,payload FROM platform_databases")
        .unwrap_or_else(|error| panic!("{phase}: authoritative scan failed: {error}"));
    assert_eq!(scan.result.records.len(), 1, "{phase}: scan lost row");
    for column in ["id", "org_id", "payload"] {
        assert_eq!(
            scan.result.records[0].get(column),
            indexed.result.records[0].get(column),
            "{phase}: scan and index disagree on {column}"
        );
    }
}

fn exercise(method: &str) {
    let directory = tempfile::tempdir().expect("fault fixture directory");
    let path = directory.path().join(format!("index-{method}.rdb"));
    let runtime = open(&path);
    execute(
        &runtime,
        "SET CONFIG runtime.mvcc.autovacuum.enabled = false",
    );
    execute(
        &runtime,
        "CREATE TABLE platform_databases (id TEXT, org_id TEXT, payload TEXT)",
    );
    execute(
        &runtime,
        &format!(
            "CREATE INDEX platform_databases_org ON platform_databases (org_id) USING {method}"
        ),
    );
    execute(
        &runtime,
        "INSERT INTO platform_databases (id,org_id,payload) VALUES ('db-1','org-a','original')",
    );
    execute(&runtime, "BEGIN");
    execute(
        &runtime,
        "UPDATE platform_databases SET payload = 'current' WHERE org_id = 'org-a'",
    );
    execute(&runtime, "COMMIT");

    let (old_id, current_id) = physical_versions(&runtime);
    let index_name = equality_index_name(&runtime);

    replace_posting(&runtime, &index_name, &[current_id], old_id);
    assert_indexed_head(&runtime, "stale physical posting");

    execute(&runtime, "VACUUM platform_databases");
    assert!(
        runtime
            .db()
            .store()
            .get_collection(TABLE)
            .expect("platform table after vacuum")
            .get(old_id)
            .is_none(),
        "VACUUM must reclaim the obsolete posting target"
    );
    replace_posting(&runtime, &index_name, &[old_id, current_id], old_id);
    assert_indexed_head(&runtime, "dangling posting after vacuum");

    runtime
        .checkpoint()
        .expect("checkpoint must rebuild derived artifacts from current rows");
    drop(runtime);

    let reopened = open(&path);
    let restored = reopened
        .index_store_ref()
        .hash_lookup(TABLE, &equality_index_name(&reopened), KEY)
        .expect("read restored index postings");
    assert_eq!(
        restored.iter().copied().collect::<Vec<_>>(),
        [current_id],
        "cold restoration must contain the current posting, not the injected one"
    );
    assert_indexed_head(&reopened, "cold reopen after derived-index repair");
}

#[test]
fn hash_posting_faults_preserve_authoritative_mvcc_rows() {
    exercise("HASH");
}

#[test]
fn btree_equality_posting_faults_preserve_authoritative_mvcc_rows() {
    exercise("BTREE");
}
