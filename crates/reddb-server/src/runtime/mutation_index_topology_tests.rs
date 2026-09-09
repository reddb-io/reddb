use super::index_store::MutationTestPhase;
use crate::application::entity::{
    CreateDocumentInput, CreateNodeInput, DeleteEntityInput, PatchEntityInput,
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
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    let id = seed(&runtime, "table");
    runtime.execute_query("BEGIN").expect("begin");
    mutate(&runtime, "sql_update", id).expect("pending update");
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
    let completed_early = std::thread::scope(|scope| {
        let ddl = scope.spawn(|| create_index(&runtime, COLLECTION, "mutation_lookup", true));
        snapshot_receive
            .recv_timeout(TIMEOUT)
            .expect("builder paused");
        let (started_send, started_receive) = mpsc::channel();
        let (done_send, done_receive) = mpsc::channel();
        let runtime = &runtime;
        let rollback = scope.spawn(move || {
            started_send.send(()).expect("rollback started");
            let result = mutate(runtime, "rollback", id);
            done_send.send(()).expect("rollback complete");
            result
        });
        started_receive
            .recv_timeout(TIMEOUT)
            .expect("rollback attempted");
        let completed_early = done_receive.recv_timeout(BLOCKING_PROBE).is_ok();
        release_send.send(()).expect("release backfill");
        ddl.join().expect("DDL thread").expect("index");
        rollback.join().expect("rollback thread").expect("rollback");
        completed_early
    });
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
