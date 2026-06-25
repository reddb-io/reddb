//! Versioned VECTOR collections — SCOPED FOLLOW-UP for Phase 3 of the
//! multi-model versioning rollout (KV Phase 1, documents Phase 2, graph
//! Phase 3a SHIPPED). Vector versioning is NOT shipped in this pass; the
//! `#[ignore]`d tests below are an executable specification of the
//! remaining work so a follow-up can drive them red→green.
//!
//! WHY VECTOR IS DEFERRED (investigated, not assumed):
//!
//! 1. No snapshot-honoring read surface. A vector's only read surface is
//!    `VECTOR SEARCH`. Its read path (runtime/query_exec/vector.rs)
//!    fetches each index hit via `store.get` and filters with
//!    `entity_visible_with_context(capture_current_snapshot(), entity)`.
//!    In autocommit (the common case) there is no active snapshot, so
//!    `capture_current_snapshot()` is `None` and the visibility check
//!    returns `true` unconditionally — a tombstoned vector stays in the
//!    results. `VECTOR SEARCH` also has no `AS OF` clause, so historical
//!    versions can never be read back. `SELECT ... FROM <vector>` does
//!    not surface vectors as rows at all (they live in `EntityData::
//!    Vector`, not a queryable row), so there is no row-scan fallback.
//!
//! 2. Derived index is not pruned. The TurboQuant index
//!    (runtime/vector_turbo_kind.rs) is appended on insert
//!    (application/ports_impls_entity.rs `create_vector`) but never has
//!    entries removed on delete (`delete_entities_batch` →
//!    `store.delete_batch` does not touch the turbo index). So even a
//!    physically-removed vector lingers in the index and is only hidden
//!    if the per-hit `store.get` returns `None` AND a snapshot is
//!    active.
//!
//! Shipping write-side vector tombstoning WITHOUT (1) would make a
//! deleted versioned vector stay searchable in autocommit — strictly
//! worse than today — so vector versioning is held until the search read
//! path honors snapshot visibility (always capture a read snapshot) and
//! the turbo index is pruned/version-filtered. The graph half of Phase 3
//! ships in this same change set (e2e_vcs_graph_mvcc_history.rs).

use std::sync::Arc;

use reddb::application::VcsUseCases;
use reddb::storage::schema::Value;
use reddb::{RedDBOptions, RedDBRuntime};

fn rt() -> Arc<RedDBRuntime> {
    Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime"))
}

fn rid_of(value: &Value) -> u64 {
    match value {
        Value::UnsignedInteger(v) => *v,
        Value::Integer(v) => *v as u64,
        Value::Text(v) => v.parse().expect("rid text -> u64"),
        other => panic!("expected rid, got {other:?}"),
    }
}

fn insert_vector(rt: &RedDBRuntime, collection: &str, dense: &str, content: &str) -> u64 {
    let sql = format!(
        "INSERT INTO {collection} VECTOR (dense, content) VALUES ({dense}, '{content}') RETURNING rid"
    );
    let result = rt
        .execute_query(&sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    let record = result
        .result
        .records
        .first()
        .unwrap_or_else(|| panic!("no RETURNING record for `{sql}`"));
    rid_of(
        record
            .get("rid")
            .unwrap_or_else(|| panic!("no rid in `{sql}`")),
    )
}

fn search_contents(rt: &RedDBRuntime, collection: &str, dense: &str, limit: usize) -> Vec<String> {
    let sql = format!("VECTOR SEARCH {collection} SIMILAR TO {dense} LIMIT {limit}");
    let result = rt
        .execute_query(&sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    result
        .result
        .records
        .iter()
        .filter_map(|record| match record.get("content") {
            Some(Value::Text(value)) => Some(value.to_string()),
            _ => None,
        })
        .collect()
}

/// FOLLOW-UP SPEC (currently failing — vector versioning not shipped):
/// after versioning, a live `VECTOR SEARCH` must return only the live
/// version, not the tombstoned old one. Requires the search read path to
/// (a) always capture a read snapshot so `entity_visible_with_context`
/// can hide tombstones in autocommit, and (b) tombstone (not physically
/// drop) versioned vector deletes while pruning/version-filtering the
/// TurboQuant index. See module docs for the precise blockers.
#[test]
#[ignore = "vector versioning deferred: VECTOR SEARCH read path does not honor snapshot visibility in autocommit; see module docs"]
fn versioned_vector_search_excludes_tombstoned_version() {
    let rt = rt();
    rt.execute_query("CREATE VECTOR vvec_search DIM 2 METRIC cosine")
        .expect("CREATE VECTOR");
    VcsUseCases::new(&*rt)
        .set_versioned("vvec_search", true)
        .unwrap();

    let rid_v1 = insert_vector(&rt, "vvec_search", "[1.0, 0.0]", "v1");
    rt.execute_query(&format!("DELETE FROM vvec_search WHERE rid = {rid_v1}"))
        .expect("DELETE v1");
    insert_vector(&rt, "vvec_search", "[1.0, 0.0]", "v2");

    let hits = search_contents(&rt, "vvec_search", "[1.0, 0.0]", 5);
    assert_eq!(
        hits,
        vec!["v2".to_string()],
        "live VECTOR SEARCH must return only the live version, not the tombstoned 'v1'"
    );
}
