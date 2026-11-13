//! Versioned GRAPH collections retain MVCC history (Phase 3 of the
//! multi-model versioning rollout; KV was Phase 1, documents Phase 2).
//!
//! A graph collection that has opted into versioning
//! (`vcs.set_versioned(collection, true)`) must keep prior node
//! versions physically alive so time-travel (`AS OF`) reads resolve the
//! snapshot-visible version per logical node — exactly like a versioned
//! table row / document. Graph nodes are stored as their own
//! `EntityKind::GraphNode`, but they carry the same `logical_id` /
//! `xmin` / `xmax` MVCC fields a table row does, so the table-row
//! versioning machinery applies once the node carries an explicit
//! logical id.
//!
//! A non-versioned graph collection keeps last-writer-wins semantics
//! (in-place mutation, no history accumulation).

use std::sync::Arc;

use reddb::application::{
    Author, CheckoutInput, CheckoutTarget, CreateBranchInput, CreateCommitInput, CreateEdgeInput,
    CreateNodeInput, CreateTagInput, DiffChange, DiffInput, MergeInput, MergeOpts, StatusInput,
    VcsUseCases,
};
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::{EntityUseCases, RedDBOptions, RedDBRuntime};
use reddb_types::Value;

fn rt() -> Arc<RedDBRuntime> {
    Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime"))
}

fn author() -> Author {
    Author {
        name: "test".to_string(),
        email: "test@reddb.io".to_string(),
    }
}

fn vcs(rt: &RedDBRuntime) -> VcsUseCases<'_, RedDBRuntime> {
    VcsUseCases::new(rt)
}

fn commit(rt: &RedDBRuntime, conn: u64, msg: &str) -> String {
    vcs(rt)
        .commit(CreateCommitInput {
            connection_id: conn,
            message: msg.to_string(),
            author: author(),
            committer: None,
            amend: false,
            allow_empty: true,
        })
        .expect("commit")
        .hash
}

/// Insert a node `(label, node_type, score)` and return its stable `rid`.
fn insert_node(rt: &RedDBRuntime, collection: &str, label: &str, score: i64) -> u64 {
    let sql = format!(
        "INSERT INTO {collection} NODE (label, node_type, score) \
         VALUES ('{label}', 'person', {score}) RETURNING rid"
    );
    let result = rt
        .execute_query(&sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    assert_eq!(result.result.records.len(), 1, "{sql}");
    match result.result.records[0].get("rid") {
        Some(Value::UnsignedInteger(rid)) => *rid,
        Some(Value::Integer(rid)) => *rid as u64,
        Some(Value::Text(rid)) => rid.parse().expect("rid text -> u64"),
        other => panic!("expected rid, got {other:?}"),
    }
}

fn insert_edge(rt: &RedDBRuntime, collection: &str, from: u64, to: u64) -> u64 {
    let result = rt
        .execute_query(&format!(
            "INSERT INTO {collection} EDGE (label, from_rid, to_rid, weight) \
             VALUES ('knows', {from}, {to}, 1.0) RETURNING rid"
        ))
        .expect("insert edge");
    match result.result.records[0].get("rid") {
        Some(Value::UnsignedInteger(rid)) => *rid,
        Some(Value::Integer(rid)) => *rid as u64,
        other => panic!("expected rid, got {other:?}"),
    }
}

/// UPDATE the node's `score` to `value`.
fn update_score(rt: &RedDBRuntime, collection: &str, label: &str, value: i64) {
    let sql = format!("UPDATE {collection} NODES SET score = {value} WHERE label = '{label}'");
    rt.execute_query(&sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

/// Read `score` for the node with `label` at a historical commit via
/// SQL `AS OF`. Returns `None` when not visible.
fn as_of_score(rt: &RedDBRuntime, collection: &str, label: &str, commit_hash: &str) -> Option<i64> {
    let sql = format!(
        "SELECT score FROM {collection} AS OF COMMIT '{commit_hash}' WHERE label = '{label}'"
    );
    read_single_score(rt, &sql)
}

/// Read `score` for the node with `label` against the live snapshot.
fn live_score(rt: &RedDBRuntime, collection: &str, label: &str) -> Option<i64> {
    let sql = format!("SELECT score FROM {collection} WHERE label = '{label}'");
    read_single_score(rt, &sql)
}

fn read_single_score(rt: &RedDBRuntime, sql: &str) -> Option<i64> {
    let result = rt
        .execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
    assert!(
        result.result.records.len() <= 1,
        "read must resolve at most one visible version, got {} for `{sql}`",
        result.result.records.len()
    );
    result.result.records.first().map(|record| {
        let value = record.get("score").or_else(|| record.get("SCORE"));
        match value {
            Some(Value::Integer(v)) => *v,
            Some(Value::UnsignedInteger(v)) => *v as i64,
            Some(Value::Float(v)) => *v as i64,
            other => panic!("expected int score, got {other:?} in {record:?} for `{sql}`"),
        }
    })
}

fn assert_conflict(err: &reddb::RedDBError) {
    let msg = format!("{err:?}");
    assert!(
        msg.contains("serialization conflict"),
        "expected serialization conflict, got: {msg}"
    );
}

fn assert_main_ref(rt: &RedDBRuntime, expected: &str) {
    let refs = vcs(rt).branch_list().unwrap();
    let main = refs
        .iter()
        .find(|item| item.name == "refs/heads/main")
        .unwrap();
    assert_eq!(main.target, expected);
}

#[test]
fn graph_branch_checkout_isolates_writes_from_main() {
    let rt = rt();
    let conn = 70008;
    set_current_connection_id(conn);
    rt.execute_query("CREATE GRAPH graph_branch_isolation")
        .expect("create graph");
    vcs(&rt)
        .set_versioned("graph_branch_isolation", true)
        .unwrap();
    insert_node(&rt, "graph_branch_isolation", "alice", 1);
    commit(&rt, conn, "base");
    vcs(&rt)
        .branch_create(CreateBranchInput {
            name: "agent".to_string(),
            from: None,
            connection_id: conn,
        })
        .unwrap();
    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: conn,
            target: CheckoutTarget::Branch("agent".to_string()),
            force: false,
        })
        .unwrap();
    insert_node(&rt, "graph_branch_isolation", "agent-node", 2);
    commit(&rt, conn, "agent write");
    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: conn,
            target: CheckoutTarget::Branch("main".to_string()),
            force: false,
        })
        .unwrap();

    assert_eq!(live_score(&rt, "graph_branch_isolation", "alice"), Some(1));
    assert_eq!(
        live_score(&rt, "graph_branch_isolation", "agent-node"),
        None
    );
    clear_current_connection_id();
}

#[test]
fn graph_branches_isolate_updates_deletes_rollback_and_recovery() {
    check_graph_branch_recovery(false);
}

#[test]
fn graph_branches_isolate_updates_deletes_in_paged_recovery() {
    check_graph_branch_recovery(true);
}

fn check_graph_branch_recovery(paged: bool) {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("branches.rdb");
    let options = || {
        let options = RedDBOptions::persistent(&path);
        if paged {
            options
                .with_storage_profile(reddb::StorageDeployPreset::Serverless.selection())
                .expect("paged profile")
        } else {
            options
        }
    };
    let collection = "branch_graph";
    let checkout = |runtime: &RedDBRuntime, connection, branch: &str| {
        set_current_connection_id(connection);
        runtime
            .vcs_checkout(CheckoutInput {
                connection_id: connection,
                target: CheckoutTarget::Branch(branch.into()),
                force: false,
            })
            .expect("checkout");
    };
    let (base, main, agent);
    {
        let runtime = RedDBRuntime::with_options(options()).expect("runtime");
        set_current_connection_id(70101);
        runtime
            .execute_query("CREATE GRAPH branch_graph")
            .expect("graph");
        runtime
            .vcs_set_versioned(collection, true)
            .expect("versioned");
        let alice = insert_node(&runtime, collection, "alice", 10);
        let bob = insert_node(&runtime, collection, "bob", 10);
        base = commit(&runtime, 70101, "base");
        runtime
            .vcs_branch_create(CreateBranchInput {
                connection_id: 70101,
                name: "agent".into(),
                from: None,
            })
            .expect("branch");
        update_score(&runtime, collection, "alice", 20);
        runtime
            .execute_query(&format!("DELETE FROM {collection} WHERE rid = {bob}"))
            .expect("main delete");
        main = commit(&runtime, 70101, "main update and delete");

        checkout(&runtime, 70102, "agent");
        assert_eq!(live_score(&runtime, collection, "alice"), Some(10));
        assert_eq!(live_score(&runtime, collection, "bob"), Some(10));
        update_score(&runtime, collection, "bob", 30);
        runtime
            .execute_query(&format!("DELETE FROM {collection} WHERE rid = {alice}"))
            .expect("agent delete");
        assert_eq!(live_score(&runtime, collection, "alice"), None);
        assert_eq!(live_score(&runtime, collection, "bob"), Some(30));
        assert!(runtime
            .get_collection_entity(collection, reddb::storage::EntityId::new(alice))
            .expect("native delete")
            .is_none());

        runtime.execute_query("BEGIN").expect("begin");
        update_score(&runtime, collection, "bob", 40);
        assert_eq!(live_score(&runtime, collection, "bob"), Some(40));
        checkout(&runtime, 70103, "agent");
        assert_eq!(live_score(&runtime, collection, "bob"), Some(30));
        set_current_connection_id(70102);
        runtime.execute_query("ROLLBACK").expect("rollback");
        assert_eq!(live_score(&runtime, collection, "bob"), Some(30));
        agent = commit(&runtime, 70102, "agent update and delete");

        checkout(&runtime, 70101, "main");
        assert_eq!(live_score(&runtime, collection, "alice"), Some(20));
        assert_eq!(live_score(&runtime, collection, "bob"), None);
        let page = runtime
            .scan_collection(collection, None, 10)
            .expect("native scan");
        assert_eq!(page.total, 1);
        assert_eq!(page.items[0].logical_id().raw(), alice);
        runtime.checkpoint().expect("checkpoint");
    }
    let runtime = RedDBRuntime::with_options(options()).expect("reopen");
    checkout(&runtime, 70101, "main");
    assert_eq!(live_score(&runtime, collection, "alice"), Some(20));
    assert_eq!(live_score(&runtime, collection, "bob"), None);
    checkout(&runtime, 70102, "agent");
    assert_eq!(live_score(&runtime, collection, "alice"), None);
    assert_eq!(live_score(&runtime, collection, "bob"), Some(30));
    for (hash, alice, bob) in [
        (&base, Some(10), Some(10)),
        (&main, Some(20), None),
        (&agent, None, Some(30)),
    ] {
        assert_eq!(as_of_score(&runtime, collection, "alice", hash), alice);
        assert_eq!(as_of_score(&runtime, collection, "bob", hash), bob);
    }
    clear_current_connection_id();
}

#[test]
fn graph_native_entity_reads_hide_pending_writes() {
    let rt = rt();
    set_current_connection_id(70028);
    rt.execute_query("CREATE GRAPH graph_native_visibility")
        .unwrap();
    vcs(&rt)
        .set_versioned("graph_native_visibility", true)
        .unwrap();
    rt.execute_query("BEGIN").unwrap();
    let pending = insert_node(&rt, "graph_native_visibility", "pending", 2);
    assert!(rt
        .get_collection_entity(
            "graph_native_visibility",
            reddb::storage::EntityId::new(pending),
        )
        .expect("own point read")
        .is_some());
    assert_eq!(
        rt.scan_collection("graph_native_visibility", None, 1)
            .expect("own scan")
            .total,
        1
    );

    set_current_connection_id(70029);
    let entity = rt
        .get_collection_entity(
            "graph_native_visibility",
            reddb::storage::EntityId::new(pending),
        )
        .unwrap();
    let page = rt
        .scan_collection("graph_native_visibility", None, 10)
        .unwrap();
    assert!(entity.is_none(), "point read exposed an uncommitted node");
    assert!(
        page.items.is_empty(),
        "collection scan exposed an uncommitted node"
    );
    assert_eq!(page.total, 0);
    assert!(page.next.is_none());
    set_current_connection_id(70028);
    rt.execute_query("ROLLBACK").unwrap();
    assert!(rt
        .get_collection_entity(
            "graph_native_visibility",
            reddb::storage::EntityId::new(pending),
        )
        .expect("read after rollback")
        .is_none());
    clear_current_connection_id();
}

#[test]
fn graph_native_reads_resolve_logical_versions_and_transaction_snapshots() {
    let rt = rt();
    let writer = 70030;
    let reader = 70031;
    let collection = "graph_native_versions";
    set_current_connection_id(writer);
    rt.execute_query(&format!("CREATE GRAPH {collection}"))
        .expect("create graph");
    vcs(&rt).set_versioned(collection, true).expect("versioned");
    let id = insert_node(&rt, collection, "alice", 1);
    let read = || {
        let entity = rt
            .get_collection_entity(collection, reddb::storage::EntityId::new(id))
            .expect("point read")
            .expect("visible logical node");
        let page = rt.scan_collection(collection, None, 1).expect("scan");
        assert_eq!(page.total, 1);
        assert!(page.next.is_none());
        assert_eq!(page.items[0].id, entity.id);
        let reddb::storage::EntityData::Node(node) = entity.data else {
            panic!("expected graph node");
        };
        node.properties.get("score").cloned()
    };
    set_current_connection_id(reader);
    rt.execute_query("BEGIN ISOLATION LEVEL REPEATABLE READ")
        .expect("reader snapshot");
    assert_eq!(read(), Some(Value::Integer(1)));
    set_current_connection_id(writer);
    update_score(&rt, collection, "alice", 2);
    assert_eq!(read(), Some(Value::Integer(2)));
    set_current_connection_id(reader);
    assert_eq!(read(), Some(Value::Integer(1)));
    rt.execute_query("COMMIT").expect("finish reader");
    assert_eq!(read(), Some(Value::Integer(2)));
    set_current_connection_id(writer);
    rt.execute_query("BEGIN").expect("delete transaction");
    rt.execute_query(&format!("DELETE FROM {collection} WHERE rid = {id}"))
        .expect("delete current node");
    set_current_connection_id(reader);
    assert_eq!(read(), Some(Value::Integer(2)));
    set_current_connection_id(writer);
    rt.execute_query("ROLLBACK").expect("rollback delete");
    assert_eq!(read(), Some(Value::Integer(2)));
    clear_current_connection_id();
}

#[test]
fn graph_fast_forward_applies_data_and_preserves_independent_writers() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("fast-forward.rdb");
    let connection = 70018;
    let collection = "graph_ff";
    let checkout = |runtime: &RedDBRuntime, branch: &str| {
        runtime
            .vcs_checkout(CheckoutInput {
                connection_id: connection,
                target: CheckoutTarget::Branch(branch.into()),
                force: false,
            })
            .expect("checkout");
    };
    let (base, source);
    {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime");
        set_current_connection_id(connection);
        runtime
            .execute_query("CREATE GRAPH graph_ff")
            .expect("graph");
        runtime
            .vcs_set_versioned(collection, true)
            .expect("versioned");
        insert_node(&runtime, collection, "base", 1);
        base = commit(&runtime, connection, "base");
        runtime
            .vcs_branch_create(CreateBranchInput {
                name: "agent".into(),
                from: None,
                connection_id: connection,
            })
            .expect("branch");
        checkout(&runtime, "agent");
        update_score(&runtime, collection, "base", 2);
        insert_node(&runtime, collection, "new", 2);
        source = commit(&runtime, connection, "source");
        checkout(&runtime, "main");
        assert_eq!(live_score(&runtime, collection, "base"), Some(1));
        assert_eq!(live_score(&runtime, collection, "new"), None);
        let outcome = runtime
            .vcs_merge(MergeInput {
                connection_id: connection,
                from: "agent".into(),
                opts: MergeOpts::default(),
                author: author(),
            })
            .expect("merge");
        assert!(outcome.fast_forward);
        assert_eq!(outcome.merge_commit.expect("commit").hash, source);
        assert_main_ref(&runtime, &source);
        assert_eq!(live_score(&runtime, collection, "base"), Some(2));
        assert_eq!(live_score(&runtime, collection, "new"), Some(2));
        update_score(&runtime, collection, "base", 3);
        checkout(&runtime, "agent");
        assert_eq!(live_score(&runtime, collection, "base"), Some(2));
        update_score(&runtime, collection, "base", 4);
        checkout(&runtime, "main");
        assert_eq!(live_score(&runtime, collection, "base"), Some(3));
        runtime.checkpoint().expect("checkpoint");
    }
    let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen");
    checkout(&runtime, "main");
    assert_eq!(live_score(&runtime, collection, "base"), Some(3));
    checkout(&runtime, "agent");
    assert_eq!(live_score(&runtime, collection, "base"), Some(4));
    assert_eq!(as_of_score(&runtime, collection, "base", &base), Some(1));
    assert_eq!(as_of_score(&runtime, collection, "base", &source), Some(2));
    clear_current_connection_id();
}

#[test]
fn graph_clean_divergent_merge_applies_source_and_preserves_target() {
    let rt = rt();
    let conn = 70019;
    set_current_connection_id(conn);
    rt.execute_query("CREATE GRAPH graph_merge_guard").unwrap();
    vcs(&rt).set_versioned("graph_merge_guard", true).unwrap();
    insert_node(&rt, "graph_merge_guard", "base", 1);
    commit(&rt, conn, "base");
    vcs(&rt)
        .branch_create(CreateBranchInput {
            name: "agent".to_string(),
            from: None,
            connection_id: conn,
        })
        .unwrap();
    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: conn,
            target: CheckoutTarget::Branch("agent".to_string()),
            force: false,
        })
        .unwrap();
    insert_node(&rt, "graph_merge_guard", "agent", 2);
    commit(&rt, conn, "agent write");
    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: conn,
            target: CheckoutTarget::Branch("main".to_string()),
            force: false,
        })
        .unwrap();
    insert_node(&rt, "graph_merge_guard", "main", 3);
    let main_head = commit(&rt, conn, "main write");

    let outcome = vcs(&rt)
        .merge(MergeInput {
            connection_id: conn,
            from: "agent".into(),
            opts: MergeOpts::default(),
            author: author(),
        })
        .expect("divergent merge applies data");
    assert!(!outcome.fast_forward);
    let merged = outcome.merge_commit.expect("merge commit");
    assert_eq!(merged.parents[0], main_head);
    assert_main_ref(&rt, &merged.hash);
    assert_eq!(live_score(&rt, "graph_merge_guard", "base"), Some(1));
    assert_eq!(live_score(&rt, "graph_merge_guard", "agent"), Some(2));
    assert_eq!(live_score(&rt, "graph_merge_guard", "main"), Some(3));
    assert_eq!(
        as_of_score(&rt, "graph_merge_guard", "agent", &main_head),
        None
    );
    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: conn,
            target: CheckoutTarget::Branch("agent".into()),
            force: false,
        })
        .expect("source branch");
    assert_eq!(live_score(&rt, "graph_merge_guard", "main"), None);
    clear_current_connection_id();
}

#[test]
fn graph_commit_excludes_transactions_still_open_at_checkpoint() {
    let rt = rt();
    let checkpoint_conn = 70011;
    let writer_conn = 70012;
    set_current_connection_id(checkpoint_conn);
    rt.execute_query("CREATE GRAPH graph_checkpoint_snapshot")
        .expect("create graph");
    vcs(&rt)
        .set_versioned("graph_checkpoint_snapshot", true)
        .unwrap();
    insert_node(&rt, "graph_checkpoint_snapshot", "existing", 1);
    commit(&rt, checkpoint_conn, "base");

    set_current_connection_id(writer_conn);
    rt.execute_query("BEGIN").expect("begin writer");
    insert_node(&rt, "graph_checkpoint_snapshot", "pending", 1);
    update_score(&rt, "graph_checkpoint_snapshot", "existing", 2);
    set_current_connection_id(checkpoint_conn);
    let checkpoint = commit(&rt, checkpoint_conn, "checkpoint");
    vcs(&rt)
        .tag(CreateTagInput {
            name: "checkpoint".to_string(),
            target: checkpoint.clone(),
            annotation: None,
        })
        .expect("tag checkpoint");
    set_current_connection_id(writer_conn);
    rt.execute_query("COMMIT").expect("commit writer");
    set_current_connection_id(checkpoint_conn);

    assert_eq!(
        as_of_score(&rt, "graph_checkpoint_snapshot", "pending", &checkpoint),
        None
    );
    assert_eq!(
        as_of_score(&rt, "graph_checkpoint_snapshot", "existing", &checkpoint),
        Some(1)
    );
    for spec in ["BRANCH 'main'", "TAG 'checkpoint'"] {
        assert_eq!(
            read_single_score(
                &rt,
                &format!(
                    "SELECT score FROM graph_checkpoint_snapshot AS OF {spec} WHERE label = 'existing'"
                )
            ),
            Some(1)
        );
    }
    assert_eq!(
        live_score(&rt, "graph_checkpoint_snapshot", "pending"),
        Some(1)
    );
    let after = commit(&rt, checkpoint_conn, "writer committed");
    let diff = vcs(&rt)
        .diff(DiffInput {
            from: checkpoint.clone(),
            to: after,
            collection: Some("graph_checkpoint_snapshot".to_string()),
            summary_only: true,
        })
        .expect("diff after writer commit");
    assert_eq!((diff.added, diff.removed, diff.modified), (1, 0, 1));
    set_current_connection_id(writer_conn);
    rt.execute_query("BEGIN").expect("begin second writer");
    update_score(&rt, "graph_checkpoint_snapshot", "existing", 3);
    assert_eq!(
        as_of_score(&rt, "graph_checkpoint_snapshot", "existing", &checkpoint),
        Some(1)
    );
    rt.execute_query("ROLLBACK")
        .expect("rollback second writer");
    clear_current_connection_id();
}

#[test]
fn graph_autocommit_insert_is_absent_from_earlier_checkpoint() {
    let rt = rt();
    let conn = 70020;
    set_current_connection_id(conn);
    rt.execute_query("CREATE GRAPH graph_insert_history")
        .unwrap();
    vcs(&rt)
        .set_versioned("graph_insert_history", true)
        .unwrap();
    let before = commit(&rt, conn, "before insert");
    insert_node(&rt, "graph_insert_history", "later", 2);
    commit(&rt, conn, "after insert");
    assert_eq!(
        as_of_score(&rt, "graph_insert_history", "later", &before),
        None
    );
    clear_current_connection_id();
}

#[test]
fn graph_autocommit_insert_invalidates_cached_reads() {
    let rt = rt();
    set_current_connection_id(70030);
    rt.execute_query("SET CONFIG runtime.result_cache.enabled = true")
        .unwrap();
    rt.execute_query("CREATE GRAPH graph_insert_cache").unwrap();
    vcs(&rt).set_versioned("graph_insert_cache", true).unwrap();
    assert_eq!(live_score(&rt, "graph_insert_cache", "later"), None);
    assert_eq!(live_score(&rt, "graph_insert_cache", "later"), None);
    insert_node(&rt, "graph_insert_cache", "later", 2);
    assert_eq!(live_score(&rt, "graph_insert_cache", "later"), Some(2));
    clear_current_connection_id();
}

#[test]
fn graph_autocommit_node_and_edge_history_survives_reopen() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("graph-insert.rdb");
    let conn = 70021;
    let before;
    let after;
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime");
        set_current_connection_id(conn);
        rt.execute_query("CREATE GRAPH graph_insert_reopen")
            .unwrap();
        vcs(&rt).set_versioned("graph_insert_reopen", true).unwrap();
        let first = insert_node(&rt, "graph_insert_reopen", "first", 1);
        before = commit(&rt, conn, "before insert");
        let second = insert_node(&rt, "graph_insert_reopen", "second", 2);
        insert_edge(&rt, "graph_insert_reopen", first, second);
        after = commit(&rt, conn, "after insert");
        clear_current_connection_id();
    }
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen");
    set_current_connection_id(conn);
    assert_eq!(
        as_of_score(&rt, "graph_insert_reopen", "second", &before),
        None
    );
    assert_eq!(
        as_of_score(&rt, "graph_insert_reopen", "second", &after),
        Some(2)
    );
    let edge_at = |commit: &str| {
        rt.execute_query(&format!(
            "SELECT weight FROM graph_insert_reopen AS OF COMMIT '{commit}' WHERE label = 'knows'"
        ))
        .expect("edge history")
        .result
        .records
        .len()
    };
    assert_eq!(edge_at(&before), 0);
    assert_eq!(edge_at(&after), 1);
    let diff = vcs(&rt)
        .diff(DiffInput {
            from: before,
            to: after,
            collection: Some("graph_insert_reopen".into()),
            summary_only: true,
        })
        .unwrap();
    assert_eq!((diff.added, diff.removed, diff.modified), (2, 0, 0));
    clear_current_connection_id();
}

#[test]
fn graph_native_node_and_edge_history_survives_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("graph-native.rdb");
    let conn = 70024;
    let before;
    let after;
    let after_transaction;
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
        set_current_connection_id(conn);
        rt.execute_query("CREATE GRAPH graph_native_history")
            .unwrap();
        vcs(&rt)
            .set_versioned("graph_native_history", true)
            .unwrap();
        let entities = EntityUseCases::new(&rt);
        let node = |label: &str, score: i64| {
            entities
                .create_node(CreateNodeInput {
                    collection: "graph_native_history".into(),
                    label: label.into(),
                    node_type: Some("person".into()),
                    properties: vec![("score".into(), Value::Integer(score))],
                    metadata: vec![],
                    embeddings: vec![],
                    table_links: vec![],
                    node_links: vec![],
                })
                .unwrap()
                .id
        };
        let first = node("first", 1);
        before = commit(&rt, conn, "before native insert");
        let second = node("second", 2);
        entities
            .create_edge(CreateEdgeInput {
                collection: "graph_native_history".into(),
                label: "knows".into(),
                from: first,
                to: second,
                weight: Some(1.0),
                properties: vec![],
                metadata: vec![],
            })
            .unwrap();
        after = commit(&rt, conn, "after native insert");
        rt.execute_query("BEGIN").unwrap();
        node("third", 3);
        rt.execute_query("COMMIT").unwrap();
        after_transaction = commit(&rt, conn, "after native transaction");
        clear_current_connection_id();
    }
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
    set_current_connection_id(conn);
    assert_eq!(
        as_of_score(&rt, "graph_native_history", "second", &before),
        None
    );
    assert_eq!(
        as_of_score(&rt, "graph_native_history", "second", &after),
        Some(2)
    );
    assert_eq!(
        as_of_score(&rt, "graph_native_history", "third", &after),
        None
    );
    assert_eq!(
        as_of_score(&rt, "graph_native_history", "third", &after_transaction),
        Some(3)
    );
    let diff = vcs(&rt)
        .diff(DiffInput {
            from: before,
            to: after,
            collection: Some("graph_native_history".into()),
            summary_only: true,
        })
        .unwrap();
    assert_eq!((diff.added, diff.removed, diff.modified), (2, 0, 0));
    clear_current_connection_id();
}

#[test]
fn graph_transaction_inserts_keep_history_after_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("graph-transaction.rdb");
    let before;
    let after;
    let conn = 70025;
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
        set_current_connection_id(conn);
        rt.execute_query("CREATE GRAPH graph_transaction_history")
            .unwrap();
        vcs(&rt)
            .set_versioned("graph_transaction_history", true)
            .unwrap();
        let first = insert_node(&rt, "graph_transaction_history", "first", 1);
        before = commit(&rt, conn, "before transaction");
        rt.execute_query("BEGIN").unwrap();
        let second = insert_node(&rt, "graph_transaction_history", "second", 2);
        insert_edge(&rt, "graph_transaction_history", first, second);
        rt.execute_query("COMMIT").unwrap();
        after = commit(&rt, conn, "after transaction");
        clear_current_connection_id();
    }
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
    set_current_connection_id(conn);
    assert_eq!(
        as_of_score(&rt, "graph_transaction_history", "second", &before),
        None
    );
    assert_eq!(
        as_of_score(&rt, "graph_transaction_history", "second", &after),
        Some(2)
    );
    let diff = vcs(&rt)
        .diff(DiffInput {
            from: before,
            to: after,
            collection: Some("graph_transaction_history".into()),
            summary_only: true,
        })
        .unwrap();
    assert_eq!((diff.added, diff.removed, diff.modified), (2, 0, 0));
    clear_current_connection_id();
}

#[test]
fn graph_checkpoint_excludes_uncommitted_insert_and_update_after_recovery() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("graph-live.rdb");
    let copy = directory.path().join("graph-copy.rdb");
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime");
    set_current_connection_id(70022);
    rt.execute_query("CREATE GRAPH graph_checkpoint_pending")
        .unwrap();
    vcs(&rt)
        .set_versioned("graph_checkpoint_pending", true)
        .unwrap();
    let existing = insert_node(&rt, "graph_checkpoint_pending", "existing", 1);
    let other = insert_node(&rt, "graph_checkpoint_pending", "other", 1);
    let existing_edge = insert_edge(&rt, "graph_checkpoint_pending", existing, other);
    rt.execute_query("BEGIN").unwrap();
    let pending = insert_node(&rt, "graph_checkpoint_pending", "pending", 2);
    let edge = insert_edge(&rt, "graph_checkpoint_pending", existing, pending);
    update_score(&rt, "graph_checkpoint_pending", "existing", 3);
    rt.execute_query("UPDATE graph_checkpoint_pending EDGES SET weight = 3 WHERE label = 'knows'")
        .unwrap();
    let changed = rt
        .execute_query("SELECT weight FROM graph_checkpoint_pending WHERE label = 'knows'")
        .unwrap();
    assert_eq!(changed.result.records.len(), 2);
    assert!(changed
        .result
        .records
        .iter()
        .all(|record| record.get("weight") == Some(&Value::Float(3.0))));
    rt.checkpoint().expect("checkpoint with active writer");
    std::fs::copy(&path, &copy).expect("copy checkpoint");
    let wal = reddb_file::unified_wal_path(&path);
    if wal.exists() {
        std::fs::copy(wal, reddb_file::unified_wal_path(&copy)).expect("copy WAL");
    }
    let recovered =
        RedDBRuntime::with_options(RedDBOptions::persistent(&copy)).expect("recover copy");
    set_current_connection_id(70023);
    assert_eq!(
        live_score(&recovered, "graph_checkpoint_pending", "existing"),
        Some(1)
    );
    assert_eq!(
        live_score(&recovered, "graph_checkpoint_pending", "pending"),
        None
    );
    let store = recovered.db().store();
    let manager = store.get_collection("graph_checkpoint_pending").unwrap();
    assert!(manager.get(reddb::storage::EntityId::new(edge)).is_none());
    assert_eq!(
        manager
            .get(reddb::storage::EntityId::new(existing_edge))
            .unwrap()
            .xmax,
        0
    );
    let result = recovered
        .execute_query("SELECT weight FROM graph_checkpoint_pending WHERE label = 'knows'")
        .expect("read recovered existing edge");
    assert_eq!(result.result.records.len(), 1);
    assert_eq!(
        result.result.records[0].get("weight"),
        Some(&Value::Float(1.0))
    );
    set_current_connection_id(70022);
    rt.execute_query("ROLLBACK").unwrap();
    clear_current_connection_id();
}

#[test]
fn graph_checkpoint_does_not_recover_references_to_uncommitted_native_edge() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("graph-refs-live.rdb");
    let copy = directory.path().join("graph-refs-copy.rdb");
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
    set_current_connection_id(70026);
    rt.execute_query("CREATE GRAPH graph_pending_refs").unwrap();
    vcs(&rt).set_versioned("graph_pending_refs", true).unwrap();
    let first = insert_node(&rt, "graph_pending_refs", "first", 1);
    let second = insert_node(&rt, "graph_pending_refs", "second", 2);
    rt.execute_query("BEGIN").unwrap();
    let edge = EntityUseCases::new(&rt)
        .create_edge(CreateEdgeInput {
            collection: "graph_pending_refs".into(),
            label: "knows".into(),
            from: reddb::storage::EntityId::new(first),
            to: reddb::storage::EntityId::new(second),
            weight: Some(1.0),
            properties: vec![],
            metadata: vec![],
        })
        .unwrap()
        .id;
    let source = reddb::storage::EntityId::new(first);
    assert!(rt
        .db()
        .store()
        .get("graph_pending_refs", source)
        .unwrap()
        .cross_refs()
        .iter()
        .any(|reference| reference.target == edge));
    rt.checkpoint().unwrap();
    std::fs::copy(&path, &copy).unwrap();
    let wal = reddb_file::unified_wal_path(&path);
    if wal.exists() {
        std::fs::copy(wal, reddb_file::unified_wal_path(&copy)).unwrap();
    }
    let recovered = RedDBRuntime::with_options(RedDBOptions::persistent(&copy)).unwrap();
    let store = recovered.db().store();
    assert!(store.get("graph_pending_refs", edge).is_none());
    assert!(!store
        .get("graph_pending_refs", source)
        .unwrap()
        .cross_refs()
        .iter()
        .any(|reference| reference.target == edge));
    assert!(!store
        .get_refs_from(source)
        .iter()
        .any(|(target, _, _)| *target == edge));
    rt.execute_query("ROLLBACK").unwrap();
    clear_current_connection_id();
}

#[test]
fn graph_native_autocommit_edge_recovers_from_wal_before_checkpoint() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("graph-wal-live.rdb");
    let copy = directory.path().join("graph-wal-copy.rdb");
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
    set_current_connection_id(70027);
    rt.execute_query("CREATE GRAPH graph_wal_edge").unwrap();
    vcs(&rt).set_versioned("graph_wal_edge", true).unwrap();
    let first = insert_node(&rt, "graph_wal_edge", "first", 1);
    let second = insert_node(&rt, "graph_wal_edge", "second", 2);
    rt.checkpoint().unwrap();
    let edge = EntityUseCases::new(&rt)
        .create_edge(CreateEdgeInput {
            collection: "graph_wal_edge".into(),
            label: "knows".into(),
            from: reddb::storage::EntityId::new(first),
            to: reddb::storage::EntityId::new(second),
            weight: Some(1.0),
            properties: vec![],
            metadata: vec![],
        })
        .unwrap()
        .id;
    std::fs::copy(&path, &copy).unwrap();
    let wal = reddb_file::unified_wal_path(&path);
    if wal.exists() {
        std::fs::copy(wal, reddb_file::unified_wal_path(&copy)).unwrap();
    }
    let recovered = RedDBRuntime::with_options(RedDBOptions::persistent(&copy)).unwrap();
    let store = recovered.db().store();
    let source = reddb::storage::EntityId::new(first);
    assert!(store.get("graph_wal_edge", edge).is_some());
    assert!(store
        .get("graph_wal_edge", source)
        .unwrap()
        .cross_refs()
        .iter()
        .any(|reference| reference.target == edge));
    assert!(store
        .get_refs_from(source)
        .iter()
        .any(|(target, _, _)| *target == edge));
    clear_current_connection_id();
}

#[test]
fn graph_checkpoint_snapshot_survives_reopen() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("graph-vcs.rdb");
    let checkpoint;
    let writer_xid;
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime");
        set_current_connection_id(70014);
        rt.execute_query("CREATE GRAPH graph_reopen_snapshot")
            .expect("create graph");
        vcs(&rt)
            .set_versioned("graph_reopen_snapshot", true)
            .unwrap();
        insert_node(&rt, "graph_reopen_snapshot", "existing", 1);
        commit(&rt, 70014, "base");
        set_current_connection_id(70015);
        rt.execute_query("BEGIN").expect("begin writer");
        writer_xid = rt
            .snapshot_manager()
            .oldest_active_xid()
            .expect("writer xid");
        update_score(&rt, "graph_reopen_snapshot", "existing", 2);
        set_current_connection_id(70014);
        checkpoint = commit(&rt, 70014, "checkpoint");
        set_current_connection_id(70015);
        rt.execute_query("COMMIT").expect("commit writer");
        clear_current_connection_id();
    }
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen");
    set_current_connection_id(70014);
    assert!(rt.snapshot_manager().is_pinned(writer_xid));
    assert_eq!(
        as_of_score(&rt, "graph_reopen_snapshot", "existing", &checkpoint),
        Some(1)
    );
    assert_eq!(
        live_score(&rt, "graph_reopen_snapshot", "existing"),
        Some(2)
    );
    clear_current_connection_id();
}

#[test]
fn graph_as_of_branch_observes_ref_changes_with_result_cache_enabled() {
    let rt = rt();
    let conn = 70016;
    set_current_connection_id(conn);
    rt.execute_query("SET CONFIG runtime.result_cache.enabled = true")
        .expect("enable result cache");
    rt.execute_query("CREATE GRAPH graph_branch_cache")
        .expect("create graph");
    vcs(&rt).set_versioned("graph_branch_cache", true).unwrap();
    insert_node(&rt, "graph_branch_cache", "alice", 1);
    commit(&rt, conn, "base");
    update_score(&rt, "graph_branch_cache", "alice", 2);

    let query = "SELECT score FROM graph_branch_cache AS OF BRANCH 'main' WHERE label = 'alice'";
    assert_eq!(read_single_score(&rt, query), Some(1));
    assert_eq!(read_single_score(&rt, query), Some(1));
    commit(&rt, conn, "advance main");
    assert_eq!(read_single_score(&rt, query), Some(2));
    clear_current_connection_id();
}

#[test]
fn graph_diff_reports_node_update_as_one_modified_item() {
    let rt = rt();
    let conn = 70009;
    set_current_connection_id(conn);
    rt.execute_query("CREATE GRAPH graph_diff_update")
        .expect("create graph");
    vcs(&rt).set_versioned("graph_diff_update", true).unwrap();
    let rid = insert_node(&rt, "graph_diff_update", "alice", 1);
    let before = commit(&rt, conn, "before");
    update_score(&rt, "graph_diff_update", "alice", 2);
    let after = commit(&rt, conn, "after");

    let diff = vcs(&rt)
        .diff(DiffInput {
            from: before,
            to: after,
            collection: Some("graph_diff_update".to_string()),
            summary_only: false,
        })
        .expect("diff");
    assert_eq!((diff.added, diff.removed, diff.modified), (0, 0, 1));
    assert_eq!(diff.entries.len(), 1);
    assert_eq!(diff.entries[0].entity_id, rid.to_string());
    let DiffChange::Modified { before, after } = &diff.entries[0].change else {
        panic!("expected modified node");
    };
    let rendered_before = reddb::json::to_string(before).expect("before JSON");
    let rendered_after = reddb::json::to_string(after).expect("after JSON");
    assert!(rendered_before.contains("\"score\":1"), "{rendered_before}");
    assert!(rendered_after.contains("\"score\":2"), "{rendered_after}");
    assert!(rendered_after.contains("\"node_type\":\"person\""));
    clear_current_connection_id();
}

#[test]
fn graph_diff_reports_edge_update_as_one_modified_item() {
    let rt = rt();
    let conn = 70010;
    set_current_connection_id(conn);
    rt.execute_query("CREATE GRAPH graph_edge_diff")
        .expect("create graph");
    vcs(&rt).set_versioned("graph_edge_diff", true).unwrap();
    let alice = insert_node(&rt, "graph_edge_diff", "alice", 1);
    let bob = insert_node(&rt, "graph_edge_diff", "bob", 1);
    let rid = insert_edge(&rt, "graph_edge_diff", alice, bob);
    let before = commit(&rt, conn, "before");
    rt.execute_query("UPDATE graph_edge_diff EDGES SET weight = 2.0 WHERE label = 'knows'")
        .expect("update edge");
    let after = commit(&rt, conn, "after");

    let diff = vcs(&rt)
        .diff(DiffInput {
            from: before,
            to: after,
            collection: Some("graph_edge_diff".to_string()),
            summary_only: false,
        })
        .expect("diff");
    assert_eq!((diff.added, diff.removed, diff.modified), (0, 0, 1));
    assert_eq!(diff.entries.len(), 1);
    assert_eq!(diff.entries[0].entity_id, rid.to_string());
    let DiffChange::Modified { before, after } = &diff.entries[0].change else {
        panic!("expected modified edge");
    };
    let rendered_before = reddb::json::to_string(before).expect("before JSON");
    let rendered_after = reddb::json::to_string(after).expect("after JSON");
    assert!(
        rendered_before.contains("\"weight\":1"),
        "{rendered_before}"
    );
    assert!(rendered_after.contains("\"weight\":2"), "{rendered_after}");
    assert!(rendered_after.contains("\"from_node\""));
    assert!(rendered_after.contains("\"to_node\""));
    clear_current_connection_id();
}

#[test]
fn graph_merge_detects_conflicting_updates_to_nodes_and_edges() {
    let rt = rt();
    let conn = 70013;
    set_current_connection_id(conn);
    rt.execute_query("CREATE GRAPH graph_merge_conflict")
        .expect("create graph");
    vcs(&rt)
        .set_versioned("graph_merge_conflict", true)
        .unwrap();
    let rid = insert_node(&rt, "graph_merge_conflict", "alice", 1);
    let bob = insert_node(&rt, "graph_merge_conflict", "bob", 1);
    let edge_rid = insert_edge(&rt, "graph_merge_conflict", rid, bob);
    commit(&rt, conn, "base");
    vcs(&rt)
        .branch_create(CreateBranchInput {
            name: "agent".to_string(),
            from: None,
            connection_id: conn,
        })
        .unwrap();
    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: conn,
            target: CheckoutTarget::Branch("agent".to_string()),
            force: false,
        })
        .unwrap();
    update_score(&rt, "graph_merge_conflict", "alice", 2);
    rt.execute_query(
        "UPDATE graph_merge_conflict NODES SET node_type = 'engineer' WHERE label = 'alice'",
    )
    .expect("agent updates node type");
    rt.execute_query("UPDATE graph_merge_conflict EDGES SET weight = 2.0 WHERE label = 'knows'")
        .expect("agent updates edge");
    commit(&rt, conn, "agent update");
    vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: conn,
            target: CheckoutTarget::Branch("main".to_string()),
            force: false,
        })
        .unwrap();
    update_score(&rt, "graph_merge_conflict", "alice", 3);
    rt.execute_query(
        "UPDATE graph_merge_conflict NODES SET node_type = 'manager' WHERE label = 'alice'",
    )
    .expect("main updates node type");
    rt.execute_query("UPDATE graph_merge_conflict EDGES SET weight = 3.0 WHERE label = 'knows'")
        .expect("main updates edge");
    let main_head = commit(&rt, conn, "main update");

    let outcome = vcs(&rt)
        .merge(MergeInput {
            connection_id: conn,
            from: "agent".to_string(),
            opts: MergeOpts::default(),
            author: author(),
        })
        .expect("merge");
    assert_eq!(outcome.conflicts.len(), 2);
    let node_conflict = outcome
        .conflicts
        .iter()
        .find(|c| c.entity_id == rid.to_string())
        .expect("node conflict");
    assert_eq!(node_conflict.conflicting_paths.len(), 2);
    assert!(node_conflict
        .conflicting_paths
        .iter()
        .any(|path| path == "properties.score"));
    assert!(node_conflict
        .conflicting_paths
        .iter()
        .any(|path| path == "node_type"));
    let edge_conflict = outcome
        .conflicts
        .iter()
        .find(|c| c.entity_id == edge_rid.to_string())
        .expect("edge conflict");
    assert_eq!(edge_conflict.conflicting_paths, ["weight"]);
    assert!(
        outcome.merge_commit.is_none(),
        "conflicts must pause the merge"
    );
    let status = vcs(&rt)
        .status(StatusInput {
            connection_id: conn,
        })
        .expect("status");
    assert_eq!(status.head_commit.as_deref(), Some(main_head.as_str()));
    assert_eq!(status.merge_state_id, outcome.merge_state_id);
    assert_eq!(status.unresolved_conflicts, 2);
    let error = vcs(&rt)
        .conflict_resolve(
            &node_conflict.id,
            reddb::json::Value::String("invalid choice".to_string()),
        )
        .expect_err("resolution must not discard a conflict without applying its data");
    assert!(error.to_string().contains("conflict retained"), "{error}");
    assert_eq!(
        vcs(&rt)
            .status(StatusInput {
                connection_id: conn,
            })
            .expect("status after rejected resolution")
            .unresolved_conflicts,
        2
    );
    let error = vcs(&rt)
        .commit(CreateCommitInput {
            connection_id: conn,
            message: "must not advance".to_string(),
            author: author(),
            committer: None,
            amend: false,
            allow_empty: true,
        })
        .expect_err("commit must not bypass the pending merge");
    assert!(error.to_string().contains("pending"), "{error}");
    let error = vcs(&rt)
        .checkout(CheckoutInput {
            connection_id: conn,
            target: CheckoutTarget::Branch("agent".to_string()),
            force: false,
        })
        .expect_err("checkout must not discard a pending merge");
    assert!(error.to_string().contains("pending"), "{error}");
    vcs(&rt)
        .conflict_resolve(
            &node_conflict.id,
            reddb::json::Value::String("theirs".into()),
        )
        .expect("choose source node");
    assert_eq!(
        vcs(&rt)
            .status(StatusInput {
                connection_id: conn
            })
            .expect("status")
            .unresolved_conflicts,
        1
    );
    vcs(&rt)
        .conflict_resolve(&edge_conflict.id, reddb::json::Value::String("ours".into()))
        .expect("choose target edge");
    assert_eq!(
        live_score(&rt, "graph_merge_conflict", "alice"),
        Some(3),
        "resolution is staged until commit"
    );
    let merged = commit(&rt, conn, "resolve merge");
    assert_main_ref(&rt, &merged);
    assert_eq!(live_score(&rt, "graph_merge_conflict", "alice"), Some(2));
    let status = vcs(&rt)
        .status(StatusInput {
            connection_id: conn,
        })
        .expect("finished status");
    assert_eq!(status.unresolved_conflicts, 0);
    assert!(status.merge_state_id.is_none());
    clear_current_connection_id();
}

#[test]
fn pending_graph_merge_status_survives_reopen() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("graph-merge.rdb");
    let merge_state_id;
    let main_head;
    let conn = 70017;
    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime");
        set_current_connection_id(conn);
        rt.execute_query("CREATE GRAPH graph_pending_merge")
            .expect("create graph");
        vcs(&rt).set_versioned("graph_pending_merge", true).unwrap();
        insert_node(&rt, "graph_pending_merge", "alice", 1);
        commit(&rt, conn, "base");
        vcs(&rt)
            .branch_create(CreateBranchInput {
                name: "agent".to_string(),
                from: None,
                connection_id: conn,
            })
            .unwrap();
        vcs(&rt)
            .checkout(CheckoutInput {
                connection_id: conn,
                target: CheckoutTarget::Branch("agent".to_string()),
                force: false,
            })
            .unwrap();
        update_score(&rt, "graph_pending_merge", "alice", 2);
        commit(&rt, conn, "agent update");
        vcs(&rt)
            .checkout(CheckoutInput {
                connection_id: conn,
                target: CheckoutTarget::Branch("main".to_string()),
                force: false,
            })
            .unwrap();
        update_score(&rt, "graph_pending_merge", "alice", 3);
        main_head = commit(&rt, conn, "main update");
        let outcome = vcs(&rt)
            .merge(MergeInput {
                connection_id: conn,
                from: "agent".to_string(),
                opts: MergeOpts::default(),
                author: author(),
            })
            .expect("merge");
        assert_eq!(outcome.conflicts.len(), 1);
        merge_state_id = outcome.merge_state_id.expect("merge state");
        clear_current_connection_id();
    }

    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen");
    set_current_connection_id(conn);
    let status = vcs(&rt)
        .status(StatusInput {
            connection_id: conn,
        })
        .unwrap();
    assert_eq!(status.head_commit.as_deref(), Some(main_head.as_str()));
    assert_eq!(
        status.merge_state_id.as_deref(),
        Some(merge_state_id.as_str())
    );
    assert_eq!(status.unresolved_conflicts, 1);
    let conflicts = vcs(&rt)
        .conflicts_list(&merge_state_id)
        .expect("recovered conflict");
    vcs(&rt)
        .conflict_resolve(
            &conflicts[0].id,
            reddb::json::Value::String("theirs".into()),
        )
        .expect("resolve recovered conflict");
    rt.checkpoint().expect("persist resolution");
    drop(rt);
    let rt =
        RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen resolved merge");
    assert_eq!(
        vcs(&rt)
            .status(StatusInput {
                connection_id: conn
            })
            .expect("resolved status")
            .unresolved_conflicts,
        0
    );
    commit(&rt, conn, "finish recovered merge");
    assert_eq!(live_score(&rt, "graph_pending_merge", "alice"), Some(2));
    clear_current_connection_id();
}

/// Time-travel: versioned graph node, insert score=1 @P1, UPDATE to 2,
/// AS OF P1 must still see score=1.
#[test]
fn versioned_graph_node_update_retains_prior_version_for_as_of() {
    let rt = rt();
    set_current_connection_id(70001);
    rt.execute_query("CREATE GRAPH vgraph_hist")
        .expect("CREATE GRAPH");
    vcs(&rt).set_versioned("vgraph_hist", true).unwrap();

    let _rid = insert_node(&rt, "vgraph_hist", "alice", 1);
    let p1 = commit(&rt, 70001, "p1");

    update_score(&rt, "vgraph_hist", "alice", 2);
    let _p2 = commit(&rt, 70001, "p2");

    assert_eq!(
        live_score(&rt, "vgraph_hist", "alice"),
        Some(2),
        "live is newest"
    );
    assert_eq!(
        as_of_score(&rt, "vgraph_hist", "alice", &p1),
        Some(1),
        "AS OF P1 must resolve the prior node version"
    );
    clear_current_connection_id();
}

/// Delete-tombstone: versioned graph node, insert score=1 @P1, DELETE,
/// current read absent, AS OF P1 still resolves the node.
#[test]
fn versioned_graph_node_delete_keeps_history_for_as_of() {
    let rt = rt();
    set_current_connection_id(70002);
    rt.execute_query("CREATE GRAPH vgraph_del")
        .expect("CREATE GRAPH");
    vcs(&rt).set_versioned("vgraph_del", true).unwrap();

    let _rid = insert_node(&rt, "vgraph_del", "alice", 1);
    let p1 = commit(&rt, 70002, "p1");

    rt.execute_query("DELETE FROM vgraph_del WHERE label = 'alice'")
        .expect("DELETE node");
    let _p2 = commit(&rt, 70002, "p2");

    assert_eq!(
        live_score(&rt, "vgraph_del", "alice"),
        None,
        "deleted node must be absent from the live snapshot"
    );
    assert_eq!(
        as_of_score(&rt, "vgraph_del", "alice", &p1),
        Some(1),
        "AS OF P1 must still resolve the pre-delete node version"
    );
    clear_current_connection_id();
}

/// First-committer-wins: two txns both snapshot score=1, both UPDATE,
/// T1 commits, T2 commit must fail with a serialization conflict.
#[test]
fn concurrent_versioned_graph_node_update_conflicts_on_second_commit() {
    let rt = rt();
    set_current_connection_id(70003);
    rt.execute_query("CREATE GRAPH vgraph_wc")
        .expect("CREATE GRAPH");
    vcs(&rt).set_versioned("vgraph_wc", true).unwrap();
    let _rid = insert_node(&rt, "vgraph_wc", "alice", 1);
    commit(&rt, 70003, "seed");

    set_current_connection_id(70004);
    rt.execute_query("BEGIN").expect("T1 begin");
    set_current_connection_id(70005);
    rt.execute_query("BEGIN").expect("T2 begin");

    set_current_connection_id(70004);
    update_score(&rt, "vgraph_wc", "alice", 2);
    set_current_connection_id(70005);
    update_score(&rt, "vgraph_wc", "alice", 3);

    set_current_connection_id(70004);
    rt.execute_query("COMMIT").expect("T1 commit");
    set_current_connection_id(70005);
    let err = rt
        .execute_query("COMMIT")
        .expect_err("T2 commit must conflict");
    assert_conflict(&err);

    set_current_connection_id(70006);
    assert_eq!(
        live_score(&rt, "vgraph_wc", "alice"),
        Some(2),
        "winner's value survives"
    );
    clear_current_connection_id();
}

/// Regression: a NON-versioned graph collection keeps last-writer-wins —
/// sequential UPDATEs do not conflict, the live read sees the latest
/// value, and no prior versions accumulate.
#[test]
fn non_versioned_graph_node_is_last_writer_wins() {
    let rt = rt();
    set_current_connection_id(70007);
    rt.execute_query("CREATE GRAPH plain_graph")
        .expect("CREATE GRAPH");

    let _rid = insert_node(&rt, "plain_graph", "alice", 1);
    commit(&rt, 70007, "p1");

    update_score(&rt, "plain_graph", "alice", 2);
    update_score(&rt, "plain_graph", "alice", 3);
    commit(&rt, 70007, "p2");

    assert_eq!(
        live_score(&rt, "plain_graph", "alice"),
        Some(3),
        "last writer wins"
    );
    let count = rt
        .execute_query("SELECT score FROM plain_graph WHERE label = 'alice'")
        .expect("scan plain_graph")
        .result
        .records
        .len();
    assert_eq!(
        count, 1,
        "non-versioned graph node must keep no history; got {count} live rows"
    );
    clear_current_connection_id();
}
