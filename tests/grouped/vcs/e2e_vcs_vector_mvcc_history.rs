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
use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

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

/// Historical payload reads must resolve the retained vector at the commit,
/// while live search excludes it after deletion.
#[test]
fn versioned_vector_as_of_resolves_prior_version() {
    use reddb::application::{Author, CreateCommitInput};
    let rt = rt();
    rt.execute_query("CREATE VECTOR vvec_asof DIM 2 METRIC cosine")
        .expect("CREATE VECTOR");
    VcsUseCases::new(&*rt)
        .set_versioned("vvec_asof", true)
        .unwrap();

    let rid_v1 = insert_vector(&rt, "vvec_asof", "[1.0, 0.0]", "v1");
    let before = rt
        .vcs_commit(CreateCommitInput {
            connection_id: 0,
            message: "before vector deletion".to_string(),
            author: Author {
                name: "test".into(),
                email: "test@reddb.io".into(),
            },
            committer: None,
            amend: false,
            allow_empty: true,
        })
        .expect("commit vector");
    rt.execute_query(&format!("DELETE FROM vvec_asof WHERE rid = {rid_v1}"))
        .expect("DELETE v1");

    let historical = rt
        .execute_query(&format!(
            "SELECT content FROM vvec_asof AS OF COMMIT '{}'",
            before.hash,
        ))
        .expect("historical vector payload");
    assert_eq!(historical.result.records.len(), 1);
    assert_eq!(
        historical.result.records[0].get("content"),
        Some(&Value::text("v1"))
    );
    assert!(search_contents(&rt, "vvec_asof", "[1.0, 0.0]", 5).is_empty());
}

#[test]
fn vector_autocommit_insert_history_survives_reopen() {
    use reddb::application::{Author, CreateCommitInput, CreateVectorInput};
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("vector-history.rdb");
    let before;
    let after;
    {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime");
        runtime
            .execute_query("CREATE VECTOR vvec_recovery DIM 2 METRIC cosine")
            .expect("create");
        runtime
            .vcs_set_versioned("vvec_recovery", true)
            .expect("versioned");
        let commit = |message: &str| {
            runtime
                .vcs_commit(CreateCommitInput {
                    connection_id: 0,
                    message: message.into(),
                    author: Author {
                        name: "test".into(),
                        email: "test@reddb.io".into(),
                    },
                    committer: None,
                    amend: false,
                    allow_empty: true,
                })
                .expect("commit")
                .hash
        };
        before = commit("before inserts");
        insert_vector(&runtime, "vvec_recovery", "[1.0, 0.0]", "sql");
        reddb::EntityUseCases::new(&runtime)
            .create_vector(CreateVectorInput {
                collection: "vvec_recovery".into(),
                dense: vec![0.0, 1.0],
                content: Some("native".into()),
                metadata: Vec::new(),
                link_row: None,
                link_node: None,
            })
            .expect("native vector");
        after = commit("after inserts");
        runtime.checkpoint().expect("checkpoint");
    }
    let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen");
    for (hash, count) in [(&before, 0), (&after, 2)] {
        let result = runtime
            .execute_query(&format!(
                "SELECT content FROM vvec_recovery AS OF COMMIT '{hash}'",
            ))
            .expect("historical vector read");
        assert_eq!(result.result.records.len(), count);
    }
}

#[test]
fn vector_checkpoint_does_not_commit_pending_delete() {
    check_vector_checkpoint_pending_delete(false, false);
}

#[test]
fn vector_paged_checkpoint_does_not_commit_pending_delete() {
    if let Some(path) = std::env::var_os("REDDB_TEST_VECTOR_CHECKPOINT_PATH") {
        let warm = std::env::var_os("REDDB_TEST_VECTOR_CHECKPOINT_WARM").is_some();
        let runtime = prepare_vector_pending_delete(std::path::Path::new(&path), true, warm);
        // Exit without destructors: recovery must use the checkpoint made with
        // an active transaction, not a shutdown checkpoint or rollback.
        std::mem::forget(runtime);
        std::process::exit(0);
    }
    check_vector_checkpoint_pending_delete(true, false);
    check_vector_checkpoint_pending_delete(true, true);
}

fn vector_checkpoint_options(path: &std::path::Path, paged: bool) -> RedDBOptions {
    let options = RedDBOptions::persistent(path);
    if paged {
        options
            .with_storage_profile(reddb::StorageDeployPreset::Serverless.selection())
            .expect("paged profile")
    } else {
        options
    }
}

fn prepare_vector_pending_delete(
    path: &std::path::Path,
    paged: bool,
    warm_checkpoint: bool,
) -> RedDBRuntime {
    let runtime =
        RedDBRuntime::with_options(vector_checkpoint_options(path, paged)).expect("runtime");
    set_current_connection_id(80004);
    runtime
        .execute_query("CREATE VECTOR vvec_recovery DIM 2 METRIC cosine")
        .expect("create");
    runtime
        .vcs_set_versioned("vvec_recovery", true)
        .expect("versioned");
    let id = insert_vector(&runtime, "vvec_recovery", "[1.0, 0.0]", "keep");
    if warm_checkpoint {
        runtime.checkpoint().expect("publish checkpoint base");
    }
    runtime.execute_query("BEGIN").expect("begin");
    runtime
        .execute_query(&format!("DELETE FROM vvec_recovery WHERE rid = {id}"))
        .expect("pending delete");
    runtime.checkpoint().expect("checkpoint with active delete");
    runtime
}

fn check_vector_checkpoint_pending_delete(paged: bool, warm_checkpoint: bool) {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("vector-live.rdb");
    let copy = directory.path().join("vector-copy.rdb");
    if paged {
        // Windows prevents copying a live pager's exclusively locked file.
        // A child process also exercises recovery after an actual abrupt exit.
        let mut child =
            std::process::Command::new(std::env::current_exe().expect("test executable"));
        child.args([
            "--exact",
            "e2e_vcs_vector_mvcc_history::vector_paged_checkpoint_does_not_commit_pending_delete",
            "--nocapture",
        ]);
        child.env("REDDB_TEST_VECTOR_CHECKPOINT_PATH", &path);
        child.env_remove("REDDB_TEST_VECTOR_CHECKPOINT_WARM");
        if warm_checkpoint {
            child.env("REDDB_TEST_VECTOR_CHECKPOINT_WARM", "1");
        }
        assert!(child.status().expect("checkpoint child").success());
        let recovered = RedDBRuntime::with_options(vector_checkpoint_options(&path, true))
            .expect("recover after abrupt exit");
        assert_eq!(
            search_contents(&recovered, "vvec_recovery", "[1.0, 0.0]", 5),
            ["keep"]
        );
        return;
    }
    let runtime = prepare_vector_pending_delete(&path, paged, warm_checkpoint);
    std::fs::copy(&path, &copy).expect("copy checkpoint");
    let wal = reddb_file::unified_wal_path(&path);
    if wal.exists() {
        std::fs::copy(wal, reddb_file::unified_wal_path(&copy)).expect("copy WAL");
    }
    let recovered =
        RedDBRuntime::with_options(vector_checkpoint_options(&copy, paged)).expect("recover");
    set_current_connection_id(80005);
    let hits = search_contents(&recovered, "vvec_recovery", "[1.0, 0.0]", 5);
    set_current_connection_id(80004);
    runtime.execute_query("ROLLBACK").expect("rollback");
    clear_current_connection_id();
    assert_eq!(
        hits,
        ["keep".to_string()],
        "checkpoint must preserve committed data"
    );
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
