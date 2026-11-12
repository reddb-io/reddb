use super::index_store::MutationTestPhase;
use crate::application::entity::{
    CreateDocumentInput, CreateNodeInput, CreateRowInput, DeleteEntityInput, PatchEntityInput,
    PatchEntityOperation, PatchEntityOperationType,
};
use crate::application::ports::RuntimeEntityPort;
use crate::storage::EntityId;
use crate::{RedDBResult, RedDBRuntime};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

const COLLECTION: &str = "mutation_records";
const TIMEOUT: Duration = Duration::from_secs(10);
const BLOCKING_PROBE: Duration = Duration::from_millis(100);

#[test]
fn graph_autocommit_snapshot_excludes_insert_before_wal_publication() {
    use crate::application::vcs::{Author, CreateCommitInput};
    use crate::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};

    let runtime = RedDBRuntime::in_memory().unwrap();
    set_current_connection_id(89001);
    runtime
        .execute_query("CREATE GRAPH graph_publication")
        .unwrap();
    runtime
        .vcs_set_versioned("graph_publication", true)
        .unwrap();
    let (published_send, published_receive) = mpsc::channel();
    let (release_send, release_receive) = mpsc::channel();
    let release_receive = Mutex::new(release_receive);
    *runtime.index_store_ref().mutation_hook.lock() = Some(Arc::new(move |collection, phase| {
        if collection == "graph_publication" && phase == MutationTestPhase::StoragePublished {
            published_send.send(()).unwrap();
            release_receive
                .lock()
                .unwrap()
                .recv_timeout(TIMEOUT)
                .unwrap();
        }
    }));
    std::thread::scope(|scope| {
        let writer = scope.spawn(|| {
            set_current_connection_id(89002);
            let result = runtime.execute_query(
                "INSERT INTO graph_publication NODE (label, node_type, score) VALUES ('later', 'person', 2)",
            );
            clear_current_connection_id();
            result
        });
        published_receive.recv_timeout(TIMEOUT).unwrap();
        let snapshot = runtime
            .vcs_commit(CreateCommitInput {
                connection_id: 89001,
                message: "during insert".into(),
                author: Author {
                    name: "test".into(),
                    email: "test@reddb.io".into(),
                },
                committer: None,
                amend: false,
                allow_empty: true,
            })
            .unwrap();
        release_send.send(()).unwrap();
        writer.join().unwrap().unwrap();
        let result = runtime
            .execute_query(&format!(
                "SELECT score FROM graph_publication AS OF COMMIT '{}' WHERE label = 'later'",
                snapshot.hash
            ))
            .unwrap();
        assert!(result.result.records.is_empty());
        assert_eq!(
            runtime
                .execute_query("SELECT score FROM graph_publication WHERE label = 'later'")
                .unwrap()
                .result
                .records
                .len(),
            1
        );
    });
    *runtime.index_store_ref().mutation_hook.lock() = None;
    clear_current_connection_id();
}

#[test]
fn graph_autocommit_wal_failure_hides_insert_after_recovery() {
    use crate::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
    use crate::RedDBOptions;

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("graph-failed-insert.rdb");
    {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
        set_current_connection_id(89003);
        runtime
            .execute_query("CREATE GRAPH graph_failed_insert")
            .unwrap();
        runtime
            .vcs_set_versioned("graph_failed_insert", true)
            .unwrap();
        runtime
            .db()
            .store()
            .test_fail_deferred_wal_append
            .store(true, Ordering::SeqCst);
        let error = runtime.execute_query(
            "INSERT INTO graph_failed_insert NODE (label, node_type) VALUES ('failed', 'person')",
        ).expect_err("WAL failure must fail the insertion");
        assert!(error
            .to_string()
            .contains("injected deferred WAL append failure"));
        assert!(runtime
            .execute_query("SELECT * FROM graph_failed_insert WHERE label = 'failed'")
            .unwrap()
            .result
            .records
            .is_empty());
        assert_eq!(runtime.snapshot_manager().oldest_active_xid(), None);
        runtime.checkpoint().unwrap();
        clear_current_connection_id();
    }
    let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).unwrap();
    set_current_connection_id(89003);
    assert!(runtime
        .execute_query("SELECT * FROM graph_failed_insert WHERE label = 'failed'")
        .unwrap()
        .result
        .records
        .is_empty());
    clear_current_connection_id();
}

#[test]
fn graph_autocommit_joins_existing_wal_capture() {
    let runtime = RedDBRuntime::in_memory().unwrap();
    runtime
        .execute_query("CREATE GRAPH graph_nested_capture")
        .unwrap();
    runtime
        .vcs_set_versioned("graph_nested_capture", true)
        .unwrap();
    runtime.execute_query("SET CONFIG runtime.result_cache.enabled = true").unwrap();
    let query = "SELECT * FROM graph_nested_capture WHERE label = 'nested'";
    assert!(runtime.execute_query(query).unwrap().result.records.is_empty());
    assert!(runtime.execute_query(query).unwrap().result.records.is_empty());
    runtime
        .with_deferred_store_wal_for_dml(true, || {
            let result = runtime.execute_query(
                "INSERT INTO graph_nested_capture NODE (label, node_type) VALUES ('nested', 'person')",
            );
            let xid = runtime.graph_insert_xid().expect("graph writer joins the outer capture");
            assert!(runtime.snapshot_manager().is_active(xid));
            assert_eq!(
                runtime.execute_query(query)
                    .unwrap().result.records.len(),
                1,
                "the owning statement can read its graph insertion before publication"
            );
            result
        })
        .unwrap();
    assert!(runtime.graph_insert_xid().is_none());
    assert_eq!(
        runtime
            .execute_query(query)
            .unwrap()
            .result
            .records
            .len(),
        1
    );
}

#[test]
fn graph_autocommit_outer_wal_failure_rolls_back_graph_writer() {
    let runtime = RedDBRuntime::in_memory().unwrap();
    runtime
        .execute_query("CREATE GRAPH graph_outer_failure")
        .unwrap();
    runtime
        .vcs_set_versioned("graph_outer_failure", true)
        .unwrap();
    runtime
        .db()
        .store()
        .test_fail_deferred_wal_append
        .store(true, Ordering::SeqCst);
    let error = runtime
        .with_deferred_store_wal_for_dml(true, || {
            runtime.execute_query(
            "INSERT INTO graph_outer_failure NODE (label, node_type) VALUES ('failed', 'person')",
        )
        })
        .expect_err("outer WAL failure must roll back the graph writer");
    assert!(error
        .to_string()
        .contains("injected deferred WAL append failure"));
    assert!(runtime.graph_insert_xid().is_none());
    assert_eq!(runtime.snapshot_manager().oldest_active_xid(), None);
    assert!(runtime
        .execute_query("SELECT * FROM graph_outer_failure WHERE label = 'failed'")
        .unwrap()
        .result
        .records
        .is_empty());
    runtime
        .execute_query(
            "INSERT INTO graph_outer_failure NODE (label, node_type) VALUES ('retry', 'person')",
        )
        .unwrap();
    assert_eq!(
        runtime
            .execute_query("SELECT * FROM graph_outer_failure WHERE label = 'retry'")
            .unwrap()
            .result
            .records
            .len(),
        1
    );
}

#[test]
fn graph_autocommit_rejects_another_connections_wal_capture() {
    use crate::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
    let runtime = RedDBRuntime::in_memory().unwrap();
    set_current_connection_id(89004);
    runtime
        .execute_query("CREATE GRAPH graph_other_connection")
        .unwrap();
    runtime
        .vcs_set_versioned("graph_other_connection", true)
        .unwrap();
    let error = runtime
        .with_deferred_store_wal_for_dml(true, || {
            set_current_connection_id(89005);
            let result = runtime.execute_query(
            "INSERT INTO graph_other_connection NODE (label, node_type) VALUES ('other', 'person')",
        );
            set_current_connection_id(89004);
            result
        })
        .expect_err("a connection must not use another connection's WAL capture");
    assert!(error.to_string().contains("unrelated WAL capture"));
    assert!(runtime
        .execute_query("SELECT * FROM graph_other_connection WHERE label = 'other'")
        .unwrap()
        .result
        .records
        .is_empty());
    clear_current_connection_id();
}

#[test]
fn graph_transaction_can_join_existing_wal_capture() {
    let runtime = RedDBRuntime::in_memory().unwrap();
    runtime
        .execute_query("CREATE GRAPH graph_nested_transaction")
        .unwrap();
    runtime
        .vcs_set_versioned("graph_nested_transaction", true)
        .unwrap();
    runtime.execute_query("BEGIN").unwrap();
    runtime.with_deferred_store_wal_for_dml(true, || {
        runtime.execute_query(
            "INSERT INTO graph_nested_transaction NODE (label, node_type) VALUES ('nested', 'person')",
        )
    }).unwrap();
    runtime.execute_query("COMMIT").unwrap();
    assert_eq!(
        runtime
            .execute_query("SELECT * FROM graph_nested_transaction WHERE label = 'nested'")
            .unwrap()
            .result
            .records
            .len(),
        1
    );
}

#[test]
fn native_insert_keeps_indexed_reads_visible_during_publication() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    seed(&runtime, "table");
    create_index(&runtime, COLLECTION, "mutation_lookup", true).expect("index");
    let (published_send, published_receive) = mpsc::channel();
    let (release_send, release_receive) = mpsc::channel();
    let release_receive = Mutex::new(release_receive);
    *runtime.index_store_ref().mutation_hook.lock() = Some(Arc::new(move |collection, phase| {
        if collection == COLLECTION && phase == MutationTestPhase::StoragePublished {
            published_send.send(()).expect("storage published");
            release_receive
                .lock()
                .expect("release receiver")
                .recv_timeout(TIMEOUT)
                .expect("resume writer");
        }
    }));
    std::thread::scope(|scope| {
        let writer = scope.spawn(|| {
            runtime.create_row(CreateRowInput {
                collection: COLLECTION.to_string(),
                fields: vec![("record_key".to_string(), reddb_types::Value::Integer(2))],
                metadata: Vec::new(),
                node_links: Vec::new(),
                vector_links: Vec::new(),
            })
        });
        published_receive
            .recv_timeout(TIMEOUT)
            .expect("writer paused");
        let observed = runtime
            .execute_query("SELECT * FROM mutation_records WHERE record_key = 2")
            .expect("indexed query")
            .result
            .records
            .len();
        release_send.send(()).expect("resume writer");
        writer.join().expect("writer thread").expect("insert");
        assert_eq!(
            observed, 1,
            "published row must not disappear behind a stale index"
        );
    });
}

#[test]
fn native_patch_keeps_indexed_reads_visible_during_publication() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    let id = seed(&runtime, "table");
    create_index(&runtime, COLLECTION, "mutation_lookup", true).expect("index");
    let (published_send, published_receive) = mpsc::channel();
    let (release_send, release_receive) = mpsc::channel();
    let release_receive = Mutex::new(release_receive);
    *runtime.index_store_ref().mutation_hook.lock() = Some(Arc::new(move |collection, phase| {
        if collection == COLLECTION && phase == MutationTestPhase::StoragePublished {
            published_send.send(()).expect("storage published");
            release_receive
                .lock()
                .expect("release receiver")
                .recv_timeout(TIMEOUT)
                .expect("resume writer");
        }
    }));
    std::thread::scope(|scope| {
        let writer = scope.spawn(|| mutate(&runtime, "native_patch", id));
        published_receive
            .recv_timeout(TIMEOUT)
            .expect("writer paused");
        let observed = runtime
            .execute_query("SELECT * FROM mutation_records WHERE record_key = 2")
            .expect("indexed query")
            .result
            .records
            .len();
        release_send.send(()).expect("resume writer");
        writer.join().expect("writer thread").expect("patch");
        assert_eq!(
            observed, 1,
            "published row must not disappear behind a stale index"
        );
    });
}

#[test]
fn native_patch_marks_the_storage_index_publication_window() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    let id = seed(&runtime, "table");
    create_index(&runtime, COLLECTION, "mutation_lookup", true).expect("index");
    let seen = Arc::new(AtomicBool::new(false));
    let marked = Arc::clone(&seen);
    let probe = runtime.clone();
    *runtime.index_store_ref().mutation_hook.lock() = Some(Arc::new(move |collection, phase| {
        if collection == COLLECTION && phase == MutationTestPhase::StoragePublished {
            marked.store(
                probe.index_store_ref().indexed_mutation_active(),
                Ordering::SeqCst,
            );
        }
    }));
    mutate(&runtime, "native_patch", id).expect("patch");
    *runtime.index_store_ref().mutation_hook.lock() = None;
    assert!(
        seen.load(Ordering::SeqCst),
        "native patch publication must be guarded"
    );
}

#[test]
fn native_delete_marks_the_storage_index_publication_window() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    let id = seed(&runtime, "table");
    create_index(&runtime, COLLECTION, "mutation_lookup", true).expect("index");
    let seen = Arc::new(AtomicBool::new(false));
    let marked = Arc::clone(&seen);
    let probe = runtime.clone();
    *runtime.index_store_ref().mutation_hook.lock() = Some(Arc::new(move |collection, phase| {
        if collection == COLLECTION && phase == MutationTestPhase::StoragePublished {
            marked.store(
                probe.index_store_ref().indexed_mutation_active(),
                Ordering::SeqCst,
            );
        }
    }));
    mutate(&runtime, "native_delete", id).expect("delete");
    *runtime.index_store_ref().mutation_hook.lock() = None;
    assert!(
        seen.load(Ordering::SeqCst),
        "native delete publication must be guarded"
    );
}

#[test]
fn index_publication_guard_is_scoped_to_one_database() {
    let first = RedDBRuntime::in_memory().expect("first runtime");
    let second = RedDBRuntime::in_memory().expect("second runtime");
    let first_index = first.index_store_ref();
    let second_index = second.index_store_ref();
    let initial_generation = second_index.indexed_mutation_generation();
    {
        let _guard = first_index.begin_indexed_mutation();
        assert!(first_index.indexed_mutation_active());
        assert!(!second_index.indexed_mutation_active());
        assert_eq!(
            second_index.indexed_mutation_generation(),
            initial_generation
        );
    }
    assert!(!first_index.indexed_mutation_active());
    assert_eq!(
        second_index.indexed_mutation_generation(),
        initial_generation
    );
}

#[test]
fn index_rebuild_and_drop_guard_partial_topology_publication() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    seed(&runtime, "table");
    create_index(&runtime, COLLECTION, "mutation_lookup", true).expect("index");
    let rebuild_seen = Arc::new(AtomicBool::new(false));
    let drop_seen = Arc::new(AtomicBool::new(false));
    let rebuilding = Arc::clone(&rebuild_seen);
    let dropping = Arc::clone(&drop_seen);
    let probe = runtime.clone();
    *runtime.index_store_ref().mutation_hook.lock() = Some(Arc::new(move |collection, phase| {
        if collection == COLLECTION && phase == MutationTestPhase::IndexTopologyPublishing {
            rebuilding.store(
                probe.index_store_ref().indexed_mutation_active(),
                Ordering::SeqCst,
            );
        }
        if collection == COLLECTION && phase == MutationTestPhase::IndexTopologyDropping {
            dropping.store(
                probe.index_store_ref().indexed_mutation_active(),
                Ordering::SeqCst,
            );
        }
    }));
    runtime
        .rebuild_runtime_indexes_for_table(COLLECTION)
        .expect("rebuild");
    assert!(runtime
        .index_store_ref()
        .drop_index("mutation_lookup", COLLECTION));
    *runtime.index_store_ref().mutation_hook.lock() = None;
    assert!(
        rebuild_seen.load(Ordering::SeqCst),
        "index rebuild publication must be guarded"
    );
    assert!(
        drop_seen.load(Ordering::SeqCst),
        "index drop publication must be guarded"
    );
}

#[test]
fn rollback_guards_the_index_undo_window() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    let id = seed(&runtime, "table");
    create_index(&runtime, COLLECTION, "mutation_lookup", true).expect("index");
    runtime.execute_query("BEGIN").expect("begin");
    mutate(&runtime, "sql_update", id).expect("update");
    let seen = Arc::new(AtomicBool::new(false));
    let marked = Arc::clone(&seen);
    let probe = runtime.clone();
    *runtime.index_store_ref().mutation_hook.lock() = Some(Arc::new(move |collection, phase| {
        if collection == COLLECTION && phase == MutationTestPhase::UndoIndexRemoved {
            marked.store(
                probe.index_store_ref().indexed_mutation_active(),
                Ordering::SeqCst,
            );
        }
    }));
    runtime.execute_query("ROLLBACK").expect("rollback");
    *runtime.index_store_ref().mutation_hook.lock() = None;
    assert!(
        seen.load(Ordering::SeqCst),
        "rollback index undo must be guarded"
    );
}

#[test]
fn deferred_delete_guards_commit_and_index_publication() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    seed(&runtime, "table");
    create_index(&runtime, COLLECTION, "mutation_lookup", true).expect("index");
    let seen = Arc::new(AtomicBool::new(false));
    let marked = Arc::clone(&seen);
    let probe = runtime.clone();
    *runtime.index_store_ref().mutation_hook.lock() = Some(Arc::new(move |collection, phase| {
        if collection == COLLECTION && phase == MutationTestPhase::DeleteVisibilityCommitted {
            marked.store(
                probe.index_store_ref().indexed_mutation_active(),
                Ordering::SeqCst,
            );
        }
    }));
    runtime
        .with_deferred_store_wal_for_dml(true, || {
            runtime.execute_query("DELETE FROM mutation_records WHERE record_key = 1")
        })
        .expect("deferred delete");
    *runtime.index_store_ref().mutation_hook.lock() = None;
    assert!(
        seen.load(Ordering::SeqCst),
        "deferred delete commit must be guarded"
    );
}

fn seed(runtime: &RedDBRuntime, model: &str) -> EntityId {
    match model {
        "table" => {
            runtime
                .execute_query("CREATE TABLE mutation_records (record_key INT)")
                .expect("table");
            runtime
                .execute_query("INSERT INTO mutation_records (record_key) VALUES (1)")
                .expect("seed");
            runtime
                .db()
                .store()
                .get_collection(COLLECTION)
                .expect("collection")
                .query_all(|_| true)[0]
                .id
        }
        "document" => {
            runtime
                .execute_query("CREATE DOCUMENT mutation_records")
                .expect("documents");
            runtime
                .create_document(CreateDocumentInput {
                    collection: COLLECTION.to_string(),
                    body: crate::json::json!({"record_key": 1}),
                    metadata: Vec::new(),
                    node_links: Vec::new(),
                    vector_links: Vec::new(),
                })
                .expect("document")
                .id
        }
        "node" => {
            runtime
                .execute_query("CREATE GRAPH mutation_records")
                .expect("graph");
            runtime
                .create_node(CreateNodeInput {
                    collection: COLLECTION.to_string(),
                    label: "seed".to_string(),
                    node_type: None,
                    properties: vec![("record_key".to_string(), reddb_types::Value::Integer(1))],
                    metadata: Vec::new(),
                    embeddings: Vec::new(),
                    table_links: Vec::new(),
                    node_links: Vec::new(),
                })
                .expect("node")
                .id
        }
        _ => unreachable!(),
    }
}

fn create_index(
    runtime: &RedDBRuntime,
    collection: &str,
    name: &str,
    native: bool,
) -> RedDBResult<()> {
    let sql = format!("CREATE INDEX {name} ON {collection} (record_key) USING HASH");
    if native {
        runtime
            .execute_create_index(
                &sql,
                &reddb_rql::ast::CreateIndexQuery {
                    name: name.to_string(),
                    table: collection.to_string(),
                    columns: vec!["record_key".to_string()],
                    method: reddb_rql::ast::IndexMethod::Hash,
                    unique: false,
                    if_not_exists: false,
                },
            )
            .map(|_| ())
    } else {
        runtime.execute_query(&sql).map(|_| ())
    }
}

fn mutate(runtime: &RedDBRuntime, operation: &str, id: EntityId) -> RedDBResult<()> {
    match operation {
        "sql_update" => runtime
            .execute_query("UPDATE mutation_records SET record_key = 2 WHERE record_key = 1")
            .map(|_| ()),
        "sql_rmw" => runtime
            .execute_query(
                "UPDATE mutation_records SET record_key = record_key + 1 WHERE record_key = 1",
            )
            .map(|_| ()),
        "native_patch" => runtime
            .patch_entity(PatchEntityInput {
                collection: COLLECTION.to_string(),
                id,
                payload: crate::json::Value::Null,
                operations: vec![PatchEntityOperation {
                    op: PatchEntityOperationType::Set,
                    path: vec!["fields".to_string(), "record_key".to_string()],
                    value: Some(crate::json::json!(2)),
                }],
            })
            .map(|_| ()),
        "sql_delete" => runtime
            .execute_query("DELETE FROM mutation_records WHERE record_key = 1")
            .map(|_| ()),
        "native_delete" => runtime
            .delete_entity(DeleteEntityInput {
                collection: COLLECTION.to_string(),
                id,
            })
            .map(|_| ()),
        "rollback" => runtime.execute_query("ROLLBACK").map(|_| ()),
        _ => unreachable!(),
    }
}

fn assert_new_key(runtime: &RedDBRuntime, name: &str) {
    assert_eq!(
        runtime
            .db()
            .store()
            .get_collection(COLLECTION)
            .expect("collection")
            .query_all(
                |entity| crate::application::ports::entity_row_fields_snapshot(entity)
                    .contains(&("record_key".to_string(), reddb_types::Value::Integer(2)))
            )
            .len(),
        1,
        "mutation must actually store the new value before checking the index"
    );
    assert_eq!(
        runtime
            .index_store_ref()
            .hash_lookup(COLLECTION, name, &2i64.to_le_bytes())
            .expect("new key")
            .len(),
        1,
        "new value must be indexed exactly once"
    );
}

#[test]
fn index_backfill_cannot_miss_update_or_delete() {
    for native_ddl in [true, false] {
        for model in ["table", "document", "node"] {
            let operations: &[&str] = if model == "node" {
                &["native_patch", "native_delete"]
            } else {
                &[
                    "sql_update",
                    "sql_rmw",
                    "native_patch",
                    "sql_delete",
                    "native_delete",
                ]
            };
            for &operation in operations {
                let runtime = RedDBRuntime::in_memory().expect("runtime");
                let id = seed(&runtime, model);
                eprintln!("backfill: {model}/{operation}, native DDL={native_ddl}");
                let (snapshot_send, snapshot_receive) = mpsc::channel();
                let (release_send, release_receive) = mpsc::channel();
                let release_receive = Mutex::new(release_receive);
                *runtime.index_store_ref().before_build.lock() = Some(Arc::new(move || {
                    snapshot_send.send(()).expect("snapshot collected");
                    release_receive
                        .lock()
                        .expect("release receiver")
                        .recv_timeout(TIMEOUT)
                        .expect("resume index build");
                }));
                let completed_early = std::thread::scope(|scope| {
                    let ddl = scope.spawn(|| {
                        create_index(&runtime, COLLECTION, "mutation_lookup", native_ddl)
                    });
                    snapshot_receive
                        .recv_timeout(TIMEOUT)
                        .expect("builder paused");
                    let (started_send, started_receive) = mpsc::channel();
                    let (done_send, done_receive) = mpsc::channel();
                    let runtime = &runtime;
                    let writer = scope.spawn(move || {
                        started_send.send(()).expect("mutation started");
                        let result = mutate(runtime, operation, id);
                        done_send.send(()).expect("mutation finished");
                        result
                    });
                    started_receive
                        .recv_timeout(TIMEOUT)
                        .expect("mutation attempted");
                    let completed_early = done_receive.recv_timeout(BLOCKING_PROBE).is_ok();
                    release_send.send(()).expect("resume builder");
                    ddl.join().expect("DDL thread").expect("create index");
                    writer.join().expect("writer thread").expect("mutation");
                    completed_early
                });
                if !operation.ends_with("delete") {
                    assert_new_key(&runtime, "mutation_lookup");
                    if model != "node" {
                        assert_eq!(
                            runtime
                                .execute_query(
                                    "SELECT * FROM mutation_records WHERE record_key = 2"
                                )
                                .expect("indexed query")
                                .result
                                .records
                                .len(),
                            1
                        );
                    }
                } else if operation == "native_delete" {
                    assert!(
                        runtime
                            .index_store_ref()
                            .hash_lookup(COLLECTION, "mutation_lookup", &1i64.to_le_bytes())
                            .expect("old key")
                            .is_empty(),
                        "physically deleted row must leave no posting"
                    );
                } else {
                    assert!(runtime
                        .execute_query("SELECT * FROM mutation_records WHERE record_key = 1")
                        .expect("deleted value query")
                        .result
                        .records
                        .is_empty());
                }
                assert!(!completed_early, "{model}/{operation}: publication raced the index snapshot (native DDL={native_ddl})");
            }
        }
    }
}

#[test]
fn topology_changes_wait_between_mutation_storage_and_index_maintenance() {
    for operation in [
        "sql_update",
        "sql_rmw",
        "native_patch",
        "sql_delete",
        "native_delete",
    ] {
        for ddl_operation in ["create", "drop", "rebuild"] {
            let runtime = RedDBRuntime::in_memory().expect("runtime");
            let id = seed(&runtime, "table");
            create_index(&runtime, COLLECTION, "mutation_lookup", true).expect("initial index");
            let (published_send, published_receive) = mpsc::channel();
            let (release_send, release_receive) = mpsc::channel();
            let release_receive = Mutex::new(release_receive);
            *runtime.index_store_ref().mutation_hook.lock() =
                Some(Arc::new(move |collection, phase| {
                    if collection == COLLECTION && phase == MutationTestPhase::StoragePublished {
                        published_send.send(()).expect("storage published");
                        release_receive
                            .lock()
                            .expect("release receiver")
                            .recv_timeout(TIMEOUT)
                            .expect("resume maintenance");
                    }
                }));
            let completed_early = std::thread::scope(|scope| {
                let writer = scope.spawn(|| mutate(&runtime, operation, id));
                published_receive
                    .recv_timeout(TIMEOUT)
                    .expect("writer paused after storage");
                let (started_send, started_receive) = mpsc::channel();
                let (done_send, done_receive) = mpsc::channel();
                let runtime = &runtime;
                let ddl = scope.spawn(move || {
                    started_send.send(()).expect("DDL started");
                    let result = match ddl_operation {
                        "create" => create_index(runtime, COLLECTION, "second_lookup", true),
                        "drop" => runtime
                            .execute_drop_index(
                                "DROP INDEX mutation_lookup ON mutation_records",
                                &reddb_rql::ast::DropIndexQuery {
                                    name: "mutation_lookup".to_string(),
                                    table: COLLECTION.to_string(),
                                    if_exists: false,
                                },
                            )
                            .map(|_| ()),
                        "rebuild" => runtime.rebuild_runtime_indexes_for_table(COLLECTION),
                        _ => unreachable!(),
                    };
                    done_send.send(()).expect("DDL finished");
                    result
                });
                started_receive
                    .recv_timeout(TIMEOUT)
                    .expect("DDL attempted");
                let completed_early = done_receive.recv_timeout(BLOCKING_PROBE).is_ok();
                release_send.send(()).expect("resume writer");
                writer.join().expect("writer thread").expect("mutation");
                ddl.join().expect("DDL thread").expect("DDL operation");
                completed_early
            });
            assert!(
                !completed_early,
                "{ddl_operation} ran during {operation} index maintenance"
            );
            if !operation.ends_with("delete") && ddl_operation != "drop" {
                assert_new_key(
                    &runtime,
                    if ddl_operation == "create" {
                        "second_lookup"
                    } else {
                        "mutation_lookup"
                    },
                );
            }
        }
    }
}

#[test]
fn mutation_releases_topology_before_event_boundary() {
    for operation in [
        "sql_update",
        "sql_rmw",
        "native_patch",
        "sql_delete",
        "native_delete",
    ] {
        let runtime = Arc::new(RedDBRuntime::in_memory().expect("runtime"));
        let id = seed(&runtime, "table");
        let weak_runtime = Arc::downgrade(&runtime);
        let fired = Arc::new(AtomicBool::new(false));
        let hook_fired = Arc::clone(&fired);
        *runtime.index_store_ref().mutation_hook.lock() =
            Some(Arc::new(move |collection, phase| {
                if collection == COLLECTION && phase == MutationTestPhase::BeforeEvents {
                    let runtime = weak_runtime
                        .upgrade()
                        .expect("runtime alive during mutation");
                    let lock = runtime
                        .index_store_ref()
                        .collection_topology_lock(COLLECTION);
                    assert!(
                        lock.try_write().is_some(),
                        "events must not retain the shared topology guard"
                    );
                    create_index(&runtime, COLLECTION, "callback_lookup", true)
                        .expect("reentrant DDL at event boundary");
                    hook_fired.store(true, Ordering::SeqCst);
                }
            }));
        mutate(&runtime, operation, id).expect("mutation");
        assert!(fired.load(Ordering::SeqCst));
        if !operation.ends_with("delete") {
            assert_new_key(&runtime, "callback_lookup");
        }
    }
}

#[test]
fn native_patch_error_releases_topology_without_modifying_indexes() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    let id = seed(&runtime, "table");
    create_index(&runtime, COLLECTION, "mutation_lookup", true).expect("index");
    assert!(runtime
        .patch_entity(PatchEntityInput {
            collection: COLLECTION.to_string(),
            id,
            payload: crate::json::Value::Null,
            operations: vec![PatchEntityOperation {
                op: PatchEntityOperationType::Set,
                path: Vec::new(),
                value: Some(crate::json::json!(2)),
            }],
        })
        .is_err());
    let lock = runtime
        .index_store_ref()
        .collection_topology_lock(COLLECTION);
    assert!(lock.try_write().is_some(), "failed patch releases topology");
    assert_eq!(
        runtime
            .index_store_ref()
            .hash_lookup(COLLECTION, "mutation_lookup", &1i64.to_le_bytes())
            .expect("unchanged key"),
        vec![id]
    );
    mutate(&runtime, "native_patch", id).expect("retry valid patch");
    assert_new_key(&runtime, "mutation_lookup");
}

#[test]
fn shared_mutation_guard_allows_other_writers_and_unrelated_ddl() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    let id = seed(&runtime, "table");
    runtime
        .execute_query("INSERT INTO mutation_records (record_key) VALUES (3)")
        .expect("second row");
    runtime
        .execute_query("CREATE TABLE other_records (record_key INT)")
        .expect("other collection");
    let second_id = runtime
        .db()
        .store()
        .get_collection(COLLECTION)
        .expect("collection")
        .query_all(|entity| entity.id != id)[0]
        .id;
    create_index(&runtime, COLLECTION, "mutation_lookup", true).expect("index");
    let (published_send, published_receive) = mpsc::channel();
    let (release_send, release_receive) = mpsc::channel();
    let release_receive = Mutex::new(release_receive);
    let first = AtomicBool::new(true);
    *runtime.index_store_ref().mutation_hook.lock() = Some(Arc::new(move |collection, phase| {
        if collection == COLLECTION
            && phase == MutationTestPhase::StoragePublished
            && first.swap(false, Ordering::SeqCst)
        {
            published_send.send(()).expect("first writer paused");
            release_receive
                .lock()
                .expect("release receiver")
                .recv_timeout(TIMEOUT)
                .expect("resume first writer");
        }
    }));
    std::thread::scope(|scope| {
        let writer = scope.spawn(|| mutate(&runtime, "native_patch", id));
        published_receive
            .recv_timeout(TIMEOUT)
            .expect("storage published");
        runtime
            .patch_entity(PatchEntityInput {
                collection: COLLECTION.to_string(),
                id: second_id,
                payload: crate::json::json!({"fields": crate::json::json!({"record_key": 4})}),
                operations: Vec::new(),
            })
            .expect("independent writer on same collection");
        create_index(&runtime, "other_records", "other_lookup", true).expect("unrelated DDL");
        release_send.send(()).expect("release first writer");
        writer
            .join()
            .expect("writer thread")
            .expect("first mutation");
    });
    assert_new_key(&runtime, "mutation_lookup");
    assert_eq!(
        runtime
            .index_store_ref()
            .hash_lookup(COLLECTION, "mutation_lookup", &4i64.to_le_bytes())
            .expect("second updated key"),
        vec![second_id]
    );
}

#[test]
fn rollback_restores_indexes_after_waiting_for_backfill() {
    rollback_backfill_with_gc(false);
}

#[test]
fn savepoint_rollback_restores_indexes_after_waiting_for_backfill() {
    rollback_backfill_with_gc(true);
}

fn rollback_backfill_with_gc(savepoint: bool) {
    let mut runtime = RedDBRuntime::in_memory().expect("runtime");
    // Drive the real collector at a controlled point, without a timer race.
    drop(runtime.background_tasks.take());
    let id = seed(&runtime, "table");
    runtime.execute_query("BEGIN").expect("begin");
    if savepoint {
        runtime
            .execute_query("SAVEPOINT before_update")
            .expect("savepoint");
    }
    mutate(&runtime, "sql_update", id).expect("pending update");
    let (undo_send, undo_receive) = mpsc::channel();
    let (collect_send, collect_receive) = mpsc::channel();
    let collect_receive = Mutex::new(collect_receive);
    *runtime.index_store_ref().mutation_hook.lock() = Some(Arc::new(move |collection, phase| {
        if collection == COLLECTION && phase == MutationTestPhase::BeforeUndoTopology {
            undo_send.send(()).expect("undo reached topology");
            collect_receive
                .lock()
                .expect("collector receiver")
                .recv_timeout(TIMEOUT)
                .expect("collector finished");
        }
    }));
    let (snapshot_send, snapshot_receive) = mpsc::channel();
    let (release_send, release_receive) = mpsc::channel();
    let release_receive = Mutex::new(release_receive);
    *runtime.index_store_ref().before_build.lock() = Some(Arc::new(move || {
        snapshot_send.send(()).expect("snapshot collected");
        release_receive
            .lock()
            .expect("release receiver")
            .recv_timeout(TIMEOUT)
            .expect("resume builder");
    }));
    let (completed_early, reclaimed) = std::thread::scope(|scope| {
        let ddl = scope.spawn(|| create_index(&runtime, COLLECTION, "mutation_lookup", true));
        snapshot_receive
            .recv_timeout(TIMEOUT)
            .expect("builder paused");
        let (started_send, started_receive) = mpsc::channel();
        let (done_send, done_receive) = mpsc::channel();
        let runtime = &runtime;
        let rollback = scope.spawn(move || {
            started_send.send(()).expect("rollback started");
            let result = if savepoint {
                runtime
                    .execute_query("ROLLBACK TO SAVEPOINT before_update")
                    .map(|_| ())
            } else {
                mutate(runtime, "rollback", id)
            };
            done_send.send(()).expect("rollback complete");
            result
        });
        started_receive
            .recv_timeout(TIMEOUT)
            .expect("rollback attempted");
        undo_receive.recv_timeout(TIMEOUT).expect("undo paused");
        let store = runtime.db().store();
        let mut cursor = store
            .mvcc_reclaim_cursor(COLLECTION.into())
            .expect("GC cursor");
        let reclaimed = store
            .reclaim_mvcc_tick(
                &mut cursor,
                &runtime.snapshot_manager(),
                runtime.mvcc_vacuum_cutoff_xid(),
                4096,
                256,
            )
            .expect("controlled GC");
        assert!(cursor.finished(), "fixture fits one collector tick");
        collect_send.send(()).expect("resume undo");
        let completed_early = done_receive.recv_timeout(BLOCKING_PROBE).is_ok();
        release_send.send(()).expect("release backfill");
        ddl.join().expect("DDL thread").expect("index");
        rollback.join().expect("rollback thread").expect("rollback");
        (completed_early, reclaimed)
    });
    assert_eq!(
        reclaimed, 0,
        "GC cannot reclaim row fields needed by pending index undo"
    );
    assert!(!completed_early, "undo must not race index backfill");
    assert!(runtime
        .index_store_ref()
        .hash_lookup(COLLECTION, "mutation_lookup", &2i64.to_le_bytes())
        .expect("undone key")
        .is_empty());
    assert_eq!(
        runtime
            .execute_query("SELECT * FROM mutation_records WHERE record_key = 1")
            .expect("restored value")
            .result
            .records
            .len(),
        1
    );
}

#[test]
fn savepoint_undo_does_not_hold_pending_journal_while_waiting_for_topology() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    let id = seed(&runtime, "table");
    create_index(&runtime, COLLECTION, "mutation_lookup", true).expect("index");
    runtime.execute_query("BEGIN").expect("begin");
    runtime
        .execute_query("SAVEPOINT before_update")
        .expect("savepoint");
    mutate(&runtime, "sql_update", id).expect("pending update");
    let topology_lock = runtime
        .index_store_ref()
        .collection_topology_lock(COLLECTION);
    let topology_guard = topology_lock.write();
    let (completed_early, journal_available) = std::thread::scope(|scope| {
        let (started_send, started_receive) = mpsc::channel();
        let (done_send, done_receive) = mpsc::channel();
        let runtime = &runtime;
        let rollback = scope.spawn(move || {
            started_send.send(()).expect("rollback started");
            let result = runtime.execute_query("ROLLBACK TO SAVEPOINT before_update");
            done_send.send(()).expect("rollback complete");
            result
        });
        started_receive
            .recv_timeout(TIMEOUT)
            .expect("rollback attempted");
        let completed_early = done_receive.recv_timeout(BLOCKING_PROBE).is_ok();
        let journal_available = runtime
            .inner
            .pending_versioned_updates
            .try_write()
            .is_some();
        // Release before asserting so a failing lock-order probe cannot hang the test.
        drop(topology_guard);
        rollback
            .join()
            .expect("rollback thread")
            .expect("savepoint rollback");
        (completed_early, journal_available)
    });
    assert!(
        !completed_early,
        "savepoint undo must coordinate with topology"
    );
    assert!(
        journal_available,
        "savepoint undo must not hold the pending journal while waiting for topology"
    );
    assert!(runtime
        .index_store_ref()
        .hash_lookup(COLLECTION, "mutation_lookup", &2i64.to_le_bytes())
        .expect("undone key")
        .is_empty());
    assert_eq!(
        runtime
            .execute_query("SELECT * FROM mutation_records WHERE record_key = 1")
            .expect("restored value")
            .result
            .records
            .len(),
        1
    );
    runtime
        .execute_query("COMMIT")
        .expect("transaction still usable after savepoint undo");
}
