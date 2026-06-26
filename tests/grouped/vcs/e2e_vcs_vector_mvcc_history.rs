//! Versioned VECTOR collections retain MVCC history (Phase 3 of the
//! multi-model versioning rollout; KV Phase 1, documents Phase 2, graph
//! Phase 3a). A vector's only read surface is `VECTOR SEARCH`, so the
//! correctness bar here is: a deleted / superseded vector must NOT
//! appear in `VECTOR SEARCH` results.
//!
//! TWO bugs are fixed in this pass:
//!
//! 1. Pre-existing delete bug (ALL vector collections, independent of
//!    versioning): the derived TurboQuant index is append-only and is
//!    never pruned on delete, and the per-hit visibility filter used the
//!    `entity_visible_with_context(None, …)` path which returns `true`
//!    unconditionally in autocommit. So a physically-deleted vector
//!    could linger in search results. The search read path now filters
//!    each hit through `entity_visible_under_current_snapshot`, which in
//!    autocommit hides any superseded/tombstoned physical version
//!    (`xmax != 0`).
//!
//! 2. Versioned vector deletes now tombstone (xmax stamp) instead of
//!    physically dropping, so history is retained — but the same
//!    visibility post-filter keeps the tombstoned version out of live
//!    search. Non-versioned vectors keep the legacy physical delete.

use std::sync::Arc;

use reddb::application::VcsUseCases;
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
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

/// (a) Pre-existing delete bug — NON-versioned: a deleted vector must
/// not appear in `VECTOR SEARCH`. Fails on `e4c7a640` because the turbo
/// index is never pruned and the autocommit visibility filter is a
/// no-op.
#[test]
fn non_versioned_vector_delete_excludes_from_search() {
    let rt = rt();
    rt.execute_query("CREATE VECTOR pvec_del DIM 2 METRIC cosine")
        .expect("CREATE VECTOR");

    let rid_a = insert_vector(&rt, "pvec_del", "[1.0, 0.0]", "a");
    insert_vector(&rt, "pvec_del", "[0.0, 1.0]", "b");
    rt.execute_query(&format!("DELETE FROM pvec_del WHERE rid = {rid_a}"))
        .expect("DELETE a");

    let hits = search_contents(&rt, "pvec_del", "[1.0, 0.0]", 5);
    assert!(
        !hits.contains(&"a".to_string()),
        "deleted vector 'a' must not appear in VECTOR SEARCH; got {hits:?}"
    );
}

/// (b) Versioned search returns only the live version after a delete +
/// re-insert that supersedes: the tombstoned old version is excluded.
#[test]
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

/// (c) Versioned delete excludes from search, and the tombstone retains
/// history (the entity stays physically alive with xmax set).
#[test]
fn versioned_vector_delete_excludes_from_search() {
    let rt = rt();
    rt.execute_query("CREATE VECTOR vvec_del DIM 2 METRIC cosine")
        .expect("CREATE VECTOR");
    VcsUseCases::new(&*rt)
        .set_versioned("vvec_del", true)
        .unwrap();

    let rid = insert_vector(&rt, "vvec_del", "[1.0, 0.0]", "v1");
    insert_vector(&rt, "vvec_del", "[0.0, 1.0]", "keep");
    rt.execute_query(&format!("DELETE FROM vvec_del WHERE rid = {rid}"))
        .expect("DELETE v1");

    let hits = search_contents(&rt, "vvec_del", "[1.0, 0.0]", 5);
    assert!(
        !hits.contains(&"v1".to_string()),
        "tombstoned versioned vector 'v1' must not appear in search; got {hits:?}"
    );
}

/// DEFERRED — time-travel read surface for vectors. `VECTOR SEARCH` has
/// no `AS OF` clause and `SELECT ... FROM <vector>` surfaces zero rows
/// (vectors live in `EntityData::Vector`, not a queryable row), so a
/// historical (tombstoned) vector version cannot be read back today.
/// Versioned vector deletes already retain history physically (the
/// tombstoned version stays alive with `xmax` set), so the read surface
/// can be added later without a data-model change. Adding `AS OF` to the
/// vector grammar + planner + a historical-version resolver is a large
/// structural change and is intentionally out of scope for this pass;
/// live-search correctness (deleted/superseded versions excluded) is the
/// shipped guarantee. Un-ignore when the read surface lands.
#[test]
#[ignore = "time-travel AS OF read surface for vectors is deferred; history is retained physically — see module docs and this test's doc comment"]
fn versioned_vector_as_of_resolves_prior_version() {
    let rt = rt();
    rt.execute_query("CREATE VECTOR vvec_asof DIM 2 METRIC cosine")
        .expect("CREATE VECTOR");
    VcsUseCases::new(&*rt)
        .set_versioned("vvec_asof", true)
        .unwrap();

    let rid_v1 = insert_vector(&rt, "vvec_asof", "[1.0, 0.0]", "v1");
    rt.execute_query(&format!("DELETE FROM vvec_asof WHERE rid = {rid_v1}"))
        .expect("DELETE v1");

    // Intended contract once the read surface lands: an AS OF read taken
    // before the delete must still resolve "v1".
    let hits = search_contents(&rt, "vvec_asof", "[1.0, 0.0]", 5);
    assert_eq!(hits, vec!["v1".to_string()]);
}

/// (d) First-committer-wins on concurrent versioned vector delete: two
/// transactions both snapshot the same live vector and DELETE it; T1
/// commits, T2's commit must fail with a serialization conflict.
#[test]
fn concurrent_versioned_vector_delete_conflicts_on_second_commit() {
    let rt = rt();
    set_current_connection_id(80001);
    rt.execute_query("CREATE VECTOR vvec_wc DIM 2 METRIC cosine")
        .expect("CREATE VECTOR");
    VcsUseCases::new(&*rt)
        .set_versioned("vvec_wc", true)
        .unwrap();
    let rid = insert_vector(&rt, "vvec_wc", "[1.0, 0.0]", "v1");

    set_current_connection_id(80002);
    rt.execute_query("BEGIN").expect("T1 begin");
    set_current_connection_id(80003);
    rt.execute_query("BEGIN").expect("T2 begin");

    set_current_connection_id(80002);
    rt.execute_query(&format!("DELETE FROM vvec_wc WHERE rid = {rid}"))
        .expect("T1 delete");
    set_current_connection_id(80003);
    rt.execute_query(&format!("DELETE FROM vvec_wc WHERE rid = {rid}"))
        .expect("T2 delete");

    set_current_connection_id(80002);
    rt.execute_query("COMMIT").expect("T1 commit");
    set_current_connection_id(80003);
    let err = rt
        .execute_query("COMMIT")
        .expect_err("T2 commit must conflict");
    let msg = format!("{err:?}");
    assert!(
        msg.contains("serialization conflict"),
        "expected serialization conflict, got: {msg}"
    );
    clear_current_connection_id();
}
