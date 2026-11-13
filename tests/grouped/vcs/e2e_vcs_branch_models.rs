use reddb::application::{
    Author, CheckoutInput, CheckoutTarget, CreateBranchInput, CreateCommitInput, DiffInput,
};
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

#[test]
fn kv_rmw_and_tag_invalidation_preserve_branch_history() {
    use reddb::catalog::CollectionModel;
    use reddb::runtime::impl_kv::KvAtomicOps;
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    set_current_connection_id(9002);
    runtime.execute_query("CREATE KV items").expect("kv");
    runtime.vcs_set_versioned("items", true).expect("versioned");
    let operations = KvAtomicOps::new(&runtime);
    operations
        .set_with_tags(
            "items",
            "count",
            Value::Integer(1),
            None,
            &["scope:counter".into()],
            false,
        )
        .expect("base");
    let base = runtime
        .vcs_commit(CreateCommitInput {
            connection_id: 9002,
            message: "base".into(),
            author: Author {
                name: "test".into(),
                email: "test@reddb.io".into(),
            },
            committer: None,
            amend: false,
            allow_empty: true,
        })
        .expect("commit");
    runtime
        .vcs_branch_create(CreateBranchInput {
            connection_id: 9002,
            name: "source".into(),
            from: None,
        })
        .expect("branch");
    let checkout = |branch: &str| {
        runtime
            .vcs_checkout(CheckoutInput {
                connection_id: 9002,
                target: CheckoutTarget::Branch(branch.into()),
                force: false,
            })
            .expect("checkout")
    };
    checkout("source");
    assert_eq!(
        operations
            .incr(CollectionModel::Kv, "items", "count", 2, None)
            .expect("increment"),
        3
    );
    assert!(
        operations
            .cas(
                CollectionModel::Kv,
                "items",
                "count",
                Some(&Value::Integer(3)),
                Value::Integer(5),
                None
            )
            .expect("cas")
            .0
    );
    assert_eq!(
        operations
            .get(CollectionModel::Kv, "items", "count")
            .expect("source"),
        Some(Value::Integer(5))
    );
    assert_eq!(
        operations.tags_for_key("items", "count"),
        vec!["scope:counter"]
    );
    checkout("main");
    assert_eq!(
        operations
            .get(CollectionModel::Kv, "items", "count")
            .expect("base branch"),
        Some(Value::Integer(1))
    );
    checkout("source");
    assert_eq!(
        operations
            .invalidate_tags("items", &["scope:counter".into()])
            .expect("invalidate"),
        1
    );
    assert_eq!(
        operations
            .get(CollectionModel::Kv, "items", "count")
            .expect("deleted source"),
        None
    );
    checkout("main");
    assert_eq!(
        operations
            .get(CollectionModel::Kv, "items", "count")
            .expect("unaffected main"),
        Some(Value::Integer(1))
    );
    let history = runtime
        .execute_query(&format!(
            "SELECT value FROM items AS OF COMMIT '{}' WHERE key = 'count'",
            base.hash
        ))
        .expect("history");
    assert_eq!(history.result.records.len(), 1);
    assert_eq!(
        history.result.records[0].get("value"),
        Some(&Value::Integer(1))
    );
    clear_current_connection_id();
}

#[test]
fn timeseries_and_mixed_branch_merges_preserve_history_after_recovery() {
    use reddb::application::{MergeInput, MergeOpts};
    for timeseries in [false, true] {
        for paged in [false, true] {
            let directory = tempfile::tempdir().expect("directory");
            let path = directory.path().join("branches.rdb");
            let options = || {
                let options = RedDBOptions::persistent(&path);
                if paged {
                    options
                        .with_storage_profile(reddb::StorageDeployPreset::Serverless.selection())
                        .expect("profile")
                } else {
                    options
                }
            };
            let author = || Author {
                name: "test".into(),
                email: "test@reddb.io".into(),
            };
            let commit = |rt: &RedDBRuntime| {
                rt.vcs_commit(CreateCommitInput {
                    connection_id: 9003,
                    message: "snapshot".into(),
                    author: author(),
                    committer: None,
                    amend: false,
                    allow_empty: true,
                })
                .expect("commit")
                .hash
            };
            let checkout = |rt: &RedDBRuntime, branch: &str| {
                rt.vcs_checkout(CheckoutInput {
                    connection_id: 9003,
                    target: CheckoutTarget::Branch(branch.into()),
                    force: false,
                })
                .expect("checkout")
            };
            let count = |rt: &RedDBRuntime| {
                rt.scan_collection("items", None, 20)
                    .expect("native scan")
                    .total
            };
            set_current_connection_id(9003);
            let (base, source, target, merged);
            {
                let rt = RedDBRuntime::with_options(options()).expect("runtime");
                rt.execute_query(if timeseries {
                    "CREATE TIMESERIES items"
                } else {
                    "CREATE TABLE items (name TEXT)"
                })
                .expect("create");
                if !timeseries {
                    let mut contract = rt.db().collection_contract("items").expect("contract");
                    contract.declared_model = reddb::catalog::CollectionModel::Mixed;
                    rt.db()
                        .save_collection_contract(contract)
                        .expect("mixed model");
                }
                rt.vcs_set_versioned("items", true).expect("versioned");
                rt.execute_query(if timeseries { "INSERT INTO items (metric,value,timestamp) VALUES ('requests',1.0,1704067200000000000)" } else { "INSERT INTO items (name) VALUES ('base')" }).expect("base insert");
                let base_id = rt.scan_collection("items", None, 20).unwrap().items[0].id;
                base = commit(&rt);
                rt.vcs_branch_create(CreateBranchInput {
                    connection_id: 9003,
                    name: "source".into(),
                    from: None,
                })
                .expect("fork");
                checkout(&rt, "source");
                rt.execute_query(if timeseries { "INSERT INTO items (metric,value,timestamp) VALUES ('requests',2.0,1704067200000000001)" } else { "INSERT INTO items NODE (label,node_type) VALUES ('agent','Person')" }).expect("source insert");
                let deletion =
                    rt.execute_query(&format!("DELETE FROM items WHERE rid = {}", base_id.raw()));
                if timeseries {
                    assert!(deletion.unwrap_err().to_string().contains("APPEND ONLY"));
                } else {
                    deletion.expect("branch delete");
                }
                source = commit(&rt);
                assert_eq!(count(&rt), if timeseries { 2 } else { 1 });
                checkout(&rt, "main");
                assert_eq!(count(&rt), 1);
                rt.execute_query(if timeseries { "INSERT INTO items (metric,value,timestamp) VALUES ('requests',3.0,1704067200000000002)" } else { "INSERT INTO items VECTOR (dense,content) VALUES ([1.0,0.0],'main')" }).expect("target insert");
                target = commit(&rt);
                assert_eq!(count(&rt), 2);
                merged = rt
                    .vcs_merge(MergeInput {
                        connection_id: 9003,
                        from: "source".into(),
                        opts: MergeOpts::default(),
                        author: author(),
                    })
                    .expect("merge actual data")
                    .merge_commit
                    .expect("merge commit")
                    .hash;
                assert_eq!(count(&rt), if timeseries { 3 } else { 2 });
                assert_eq!(
                    rt.get_collection_entity("items", base_id)
                        .expect("base visibility")
                        .is_some(),
                    timeseries
                );
                rt.checkpoint().expect("checkpoint");
            }
            let rt = RedDBRuntime::with_options(options()).expect("reopen");
            checkout(&rt, "source");
            assert_eq!(count(&rt), if timeseries { 2 } else { 1 });
            checkout(&rt, "main");
            assert_eq!(count(&rt), if timeseries { 3 } else { 2 });
            for (hash, expected) in [
                (base, 1),
                (source, if timeseries { 2 } else { 1 }),
                (target, 2),
                (merged, if timeseries { 3 } else { 2 }),
            ] {
                let result = rt
                    .execute_query(&format!("SELECT rid FROM items AS OF COMMIT '{hash}'"))
                    .expect("history");
                assert_eq!(result.result.records.len(), expected);
            }
            clear_current_connection_id();
        }
    }
}

fn check_branch_history(
    create: &str,
    insert: &str,
    update_main: &str,
    update_agent: &str,
    field: &str,
) {
    for paged in [false, true] {
        check_branch_history_profile(create, insert, update_main, update_agent, field, paged);
    }
}

fn check_branch_history_profile(
    create: &str,
    insert: &str,
    update_main: &str,
    update_agent: &str,
    field: &str,
    paged: bool,
) {
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
    let commit = |runtime: &RedDBRuntime, connection, message: &str| {
        runtime
            .vcs_commit(CreateCommitInput {
                connection_id: connection,
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
    let read = |runtime: &RedDBRuntime, expected: &str| {
        let result = runtime
            .execute_query(&format!("SELECT {field} FROM items"))
            .expect("read");
        assert_eq!(
            result.result.records.len(),
            1,
            "{create}, expected {expected}: {result:?}; physical={:?}",
            runtime
                .db()
                .store()
                .get_collection("items")
                .expect("items")
                .query_all(|_| true)
        );
        assert_eq!(
            result.result.records[0].get(field),
            Some(&Value::text(expected)),
            "{create}"
        );
        let page = runtime
            .scan_collection("items", None, 10)
            .expect("native scan");
        assert_eq!(page.total, 1, "{create}");
        assert_eq!(page.items.len(), 1, "{create}");
        if field == "value" {
            use reddb::application::RuntimeEntityPort;
            assert_eq!(
                runtime
                    .get_kv("items", "key")
                    .expect("native key")
                    .map(|(value, _)| value),
                Some(Value::text(expected))
            );
        }
        if field == "content" {
            let search = runtime
                .execute_query("VECTOR SEARCH items SIMILAR TO [1.0,0.0] MODE APPROXIMATE LIMIT 1")
                .expect("search branch");
            assert_eq!(search.result.records.len(), 1);
            assert_eq!(
                search.result.records[0].get("content"),
                Some(&Value::text(expected))
            );
        }
    };
    let (base, main, agent);
    {
        let runtime = RedDBRuntime::with_options(options()).expect("runtime");
        set_current_connection_id(8301);
        runtime.execute_query(create).expect("create");
        runtime.vcs_set_versioned("items", true).expect("versioned");
        runtime.execute_query(insert).expect("insert");
        base = commit(&runtime, 8301, "base");
        runtime
            .vcs_branch_create(CreateBranchInput {
                name: "agent".into(),
                from: None,
                connection_id: 8301,
            })
            .expect("branch");
        mutate(&runtime, update_main, "main");
        main = commit(&runtime, 8301, "main");
        read(&runtime, "main");
        checkout(&runtime, 8302, "agent");
        read(&runtime, "base");
        mutate(&runtime, update_agent, "agent");
        read(&runtime, "agent");
        agent = commit(&runtime, 8302, "agent");
        checkout(&runtime, 8301, "main");
        read(&runtime, "main");
        let diff = runtime
            .vcs_diff(DiffInput {
                from: base.clone(),
                to: agent.clone(),
                collection: Some("items".into()),
                summary_only: false,
            })
            .expect("diff");
        assert_eq!(
            (diff.added, diff.removed, diff.modified),
            (0, 0, 1),
            "{create}"
        );
        runtime.checkpoint().expect("checkpoint");
    }
    let runtime = RedDBRuntime::with_options(options()).expect("reopen");
    checkout(&runtime, 8301, "main");
    read(&runtime, "main");
    checkout(&runtime, 8302, "agent");
    read(&runtime, "agent");
    for (hash, value) in [(base, "base"), (main, "main"), (agent, "agent")] {
        let result = runtime
            .execute_query(&format!("SELECT {field} FROM items AS OF COMMIT '{hash}'"))
            .expect("history");
        assert_eq!(result.result.records.len(), 1, "{create}");
        assert_eq!(
            result.result.records[0].get(field),
            Some(&Value::text(value)),
            "{create}"
        );
    }
    clear_current_connection_id();
}

fn mutate(runtime: &RedDBRuntime, sql: &str, value: &str) {
    if sql == "patch-vector" {
        use reddb::application::{
            PatchEntityInput, PatchEntityOperation, PatchEntityOperationType, RuntimeEntityPort,
        };
        let id = runtime
            .scan_collection("items", None, 10)
            .expect("vector scan")
            .items[0]
            .logical_id();
        runtime
            .patch_entity(PatchEntityInput {
                collection: "items".into(),
                id,
                payload: reddb::json::Value::Null,
                operations: vec![PatchEntityOperation {
                    op: PatchEntityOperationType::Set,
                    path: vec!["fields".into(), "content".into()],
                    value: Some(reddb::json::Value::String(value.into())),
                }],
            })
            .expect("vector patch");
    } else {
        runtime.execute_query(sql).expect("update");
    }
}

#[test]
fn table_branch_history() {
    check_branch_history(
        "CREATE TABLE items (name TEXT)",
        "INSERT INTO items (name) VALUES ('base')",
        "UPDATE items SET name='main'",
        "UPDATE items SET name='agent'",
        "name",
    );
}

#[test]
fn document_branch_history() {
    check_branch_history(
        "CREATE DOCUMENT items",
        "INSERT INTO items DOCUMENT VALUES ({name:'base'})",
        "UPDATE items SET name='main'",
        "UPDATE items SET name='agent'",
        "name",
    );
}

#[test]
fn vector_branch_history() {
    check_branch_history(
        "CREATE VECTOR items DIM 2 METRIC cosine",
        "INSERT INTO items VECTOR (dense,content) VALUES ([1.0,0.0],'base')",
        "patch-vector",
        "patch-vector",
        "content",
    );
}

#[test]
fn kv_branch_history() {
    check_branch_history(
        "CREATE KV items",
        "KV PUT items.'key'='base'",
        "KV PUT items.'key'='main'",
        "KV PUT items.'key'='agent'",
        "value",
    );
}

#[test]
fn turbo_vector_branch_history() {
    check_branch_history(
        "CREATE COLLECTION items KIND vector.turbo DIM 2 METRIC cosine",
        "INSERT INTO items VECTOR (dense,content) VALUES ([1.0,0.0],'base')",
        "patch-vector",
        "patch-vector",
        "content",
    );
}

#[test]
fn vcs_typed_divergent_merge_delete_and_recovery() {
    use reddb::application::{MergeInput, MergeOpts};
    for paged in [false, true] {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("merge.rdb");
        let options = || {
            let options = RedDBOptions::persistent(&path);
            if paged {
                options
                    .with_storage_profile(reddb::StorageDeployPreset::Serverless.selection())
                    .expect("profile")
            } else {
                options
            }
        };
        let author = || Author {
            name: "test".into(),
            email: "test@reddb.io".into(),
        };
        let commit = |rt: &RedDBRuntime| {
            rt.vcs_commit(CreateCommitInput {
                connection_id: 9001,
                message: "snapshot".into(),
                author: author(),
                committer: None,
                amend: false,
                allow_empty: true,
            })
            .expect("commit")
            .hash
        };
        let checkout = |rt: &RedDBRuntime, branch: &str| {
            rt.vcs_checkout(CheckoutInput {
                connection_id: 9001,
                target: CheckoutTarget::Branch(branch.into()),
                force: false,
            })
            .expect("checkout")
        };
        let read =
            |rt: &RedDBRuntime, sql: &str| rt.execute_query(sql).expect("query").result.records;
        set_current_connection_id(9001);
        let (base, source, target, merged);
        {
            let rt = RedDBRuntime::with_options(options()).expect("runtime");
            rt.execute_query("CREATE TABLE items (id INT, a BIGINT, b BIGINT)")
                .expect("table");
            rt.execute_query("CREATE INDEX merge_a ON items (a) USING HASH")
                .expect("index");
            rt.vcs_set_versioned("items", true).expect("versioned");
            rt.execute_query("INSERT INTO items (id,a,b) VALUES (1,1,1),(3,3,3)")
                .expect("base rows");
            base = commit(&rt);
            rt.vcs_branch_create(CreateBranchInput {
                connection_id: 9001,
                name: "source".into(),
                from: None,
            })
            .expect("fork");
            checkout(&rt, "source");
            rt.execute_query("UPDATE items SET a = 9007199254740993 WHERE id = 1")
                .expect("source edit");
            rt.execute_query("INSERT INTO items (id,a,b) VALUES (2,2,2)")
                .expect("source insert");
            source = commit(&rt);
            checkout(&rt, "main");
            rt.execute_query("UPDATE items SET b = 42 WHERE id = 1")
                .expect("target edit");
            rt.execute_query("DELETE FROM items WHERE id = 3")
                .expect("target delete");
            target = commit(&rt);
            merged = rt
                .vcs_merge(MergeInput {
                    connection_id: 9001,
                    from: "source".into(),
                    opts: MergeOpts::default(),
                    author: author(),
                })
                .expect("merge")
                .merge_commit
                .expect("merged commit")
                .hash;
            let result = read(&rt, "SELECT a,b FROM items WHERE a = 9007199254740993");
            assert_eq!(result.len(), 1);
            assert_eq!(result[0].get("a"), Some(&Value::BigInt(9007199254740993)));
            assert_eq!(result[0].get("b"), Some(&Value::BigInt(42)));
            assert_eq!(read(&rt, "SELECT id FROM items").len(), 2);
            rt.checkpoint().expect("checkpoint");
        }
        let rt = RedDBRuntime::with_options(options()).expect("reopen");
        for (hash, a, b) in [
            (base, 1, 1),
            (source, 9007199254740993, 1),
            (target, 1, 42),
            (merged, 9007199254740993, 42),
        ] {
            let result = read(
                &rt,
                &format!("SELECT a,b FROM items AS OF COMMIT '{hash}' WHERE id = 1"),
            );
            assert_eq!(result.len(), 1);
            assert_eq!(result[0].get("a"), Some(&Value::BigInt(a)));
            assert_eq!(result[0].get("b"), Some(&Value::BigInt(b)));
        }
        checkout(&rt, "source");
        assert_eq!(read(&rt, "SELECT id FROM items").len(), 3);
        checkout(&rt, "main");
        assert_eq!(read(&rt, "SELECT id FROM items").len(), 2);
        clear_current_connection_id();
    }
}
