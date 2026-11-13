use reddb::application::{
    Author, CheckoutInput, CheckoutTarget, CreateBranchInput, CreateCommitInput, MergeInput,
    MergeOpts,
};
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

fn author() -> Author {
    Author {
        name: "test".into(),
        email: "test@reddb.io".into(),
    }
}
fn commit(rt: &RedDBRuntime) -> String {
    rt.vcs_commit(CreateCommitInput {
        connection_id: 9201,
        message: "queue state".into(),
        author: author(),
        committer: None,
        amend: false,
        allow_empty: true,
    })
    .expect("commit")
    .hash
}
fn checkout(rt: &RedDBRuntime, branch: &str) {
    rt.vcs_checkout(CheckoutInput {
        connection_id: 9201,
        target: CheckoutTarget::Branch(branch.into()),
        force: false,
    })
    .expect("checkout");
}
fn exec(rt: &RedDBRuntime, query: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(query)
        .unwrap_or_else(|error| panic!("{query}: {error}"))
}
fn text(result: &reddb::runtime::RuntimeQueryResult, field: &str) -> String {
    assert_eq!(result.result.records.len(), 1, "{result:?}");
    match result.result.records[0]
        .get(field)
        .unwrap_or_else(|| panic!("missing {field}: {result:?}"))
    {
        Value::Text(value) => value.to_string(),
        value => panic!("unexpected {field}: {value:?}"),
    }
}
fn payloads(rt: &RedDBRuntime) -> Vec<String> {
    let mut values: Vec<_> = rt
        .scan_collection("items", None, 100)
        .expect("visible messages")
        .items
        .into_iter()
        .map(|entity| match entity.data {
            reddb::storage::EntityData::QueueMessage(message) => match message.payload {
                Value::Text(value) => value.to_string(),
                value => panic!("payload: {value:?}"),
            },
            data => panic!("expected queue message: {data:?}"),
        })
        .collect();
    values.sort();
    values
}

#[test]
fn queue_branches_keep_payload_history_and_independent_deliveries() {
    for paged in [false, true] {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("queue.rdb");
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
        set_current_connection_id(9201);
        let (base, source, source_delivery);
        {
            let rt = RedDBRuntime::with_options(options()).expect("runtime");
            exec(&rt, "CREATE QUEUE items LOCK_DEADLINE_MS 600000");
            exec(&rt, "QUEUE GROUP CREATE items workers");
            exec(&rt, "QUEUE PUSH items 'base'");
            let main_delivery = read_delivery(&rt, "main");
            rt.vcs_set_versioned("items", true).expect("versioned");
            base = commit(&rt);
            rt.vcs_branch_create(CreateBranchInput {
                connection_id: 9201,
                name: "agent".into(),
                from: None,
            })
            .expect("fork");
            checkout(&rt, "agent");
            assert!(exec(&rt, "QUEUE PENDING items GROUP workers")
                .result
                .records
                .is_empty());
            let delivery = read_delivery(&rt, "agent");
            assert_eq!(
                delivery, main_delivery,
                "the inherited payload keeps its identity"
            );
            exec(&rt, &format!("QUEUE ACK items GROUP workers '{delivery}'"));
            assert!(payloads(&rt).is_empty());
            exec(&rt, "QUEUE PUSH items 'agent'");
            source = commit(&rt);
            source_delivery = read_delivery(&rt, "agent");
            checkout(&rt, "main");
            assert_eq!(payloads(&rt), ["base"]);
            assert_eq!(
                exec(&rt, "QUEUE PENDING items GROUP workers")
                    .result
                    .records
                    .len(),
                1
            );
            exec(&rt, "BEGIN");
            exec(
                &rt,
                &format!("QUEUE ACK items GROUP workers '{main_delivery}'"),
            );
            assert!(payloads(&rt).is_empty());
            exec(&rt, "ROLLBACK");
            assert_eq!(payloads(&rt), ["base"]);
            assert_eq!(
                exec(&rt, "QUEUE PENDING items GROUP workers")
                    .result
                    .records
                    .len(),
                1
            );
            exec(&rt, "QUEUE PUSH items 'main'");
            commit(&rt);
            assert!(rt
                .vcs_merge(MergeInput {
                    connection_id: 9201,
                    from: "agent".into(),
                    opts: MergeOpts::default(),
                    author: author()
                })
                .expect("merge")
                .is_clean());
            assert_eq!(payloads(&rt), ["agent", "main"]);
            assert!(rt
                .execute_query(&format!(
                    "QUEUE ACK items GROUP workers '{source_delivery}'"
                ))
                .is_err());
            assert!(rt
                .execute_query(&format!("QUEUE ACK items GROUP workers '{main_delivery}'"))
                .is_err());
            assert!(exec(&rt, "QUEUE PENDING items GROUP workers")
                .result
                .records
                .is_empty());
            let diff = rt
                .vcs_diff(reddb::application::DiffInput {
                    from: base.clone(),
                    to: source.clone(),
                    collection: Some("items".into()),
                    summary_only: false,
                })
                .expect("history");
            assert_eq!((diff.added, diff.removed), (1, 1));
            rt.checkpoint().expect("checkpoint");
        }
        let rt = RedDBRuntime::with_options(options()).expect("reopen");
        checkout(&rt, "main");
        assert_eq!(payloads(&rt), ["agent", "main"]);
        checkout(&rt, "agent");
        assert_eq!(payloads(&rt), ["agent"]);
        assert_eq!(
            exec(&rt, "QUEUE PENDING items GROUP workers")
                .result
                .records
                .len(),
            1
        );
        exec(
            &rt,
            &format!("QUEUE ACK items GROUP workers '{source_delivery}'"),
        );
        assert!(payloads(&rt).is_empty());
        checkout(&rt, "main");
        assert_eq!(payloads(&rt), ["agent", "main"]);
        clear_current_connection_id();
    }
}

fn read_delivery(rt: &RedDBRuntime, consumer: &str) -> String {
    text(
        &exec(
            rt,
            &format!("QUEUE READ items GROUP workers CONSUMER {consumer} COUNT 1"),
        ),
        "message_id",
    )
}

#[test]
fn queue_dedup_move_retry_and_purge_are_branch_local() {
    for paged in [false, true] {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("queue.rdb");
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
        set_current_connection_id(9201);
        {
            let rt = RedDBRuntime::with_options(options()).expect("runtime");
            for collection in ["items", "out"] {
                exec(
                    &rt,
                    &format!("CREATE QUEUE {collection} DEDUP_WINDOW 600 s"),
                );
                rt.vcs_set_versioned(collection, true).expect("versioned");
            }
            exec(&rt, "QUEUE GROUP CREATE items workers");
            let original = text(
                &exec(&rt, "QUEUE PUSH items 'base' DEDUP 'key'"),
                "message_id",
            );
            commit(&rt);
            rt.vcs_branch_create(CreateBranchInput {
                connection_id: 9201,
                name: "agent".into(),
                from: None,
            })
            .expect("fork");
            checkout(&rt, "agent");
            let pushed = text(
                &exec(&rt, "QUEUE PUSH items 'agent' DEDUP 'key'"),
                "message_id",
            );
            assert_ne!(pushed, original);
            assert_eq!(
                text(
                    &exec(&rt, "QUEUE PUSH items 'duplicate' DEDUP 'key'"),
                    "message_id"
                ),
                pushed
            );
            assert_eq!(payloads(&rt), ["agent", "base"]);
            exec(&rt, "BEGIN");
            exec(&rt, "QUEUE MOVE FROM items TO out LIMIT 1");
            assert_eq!(payloads(&rt), ["agent"]);
            exec(&rt, "ROLLBACK");
            assert_eq!(payloads(&rt), ["agent", "base"]);
            assert_eq!(
                rt.scan_collection("out", None, 10)
                    .expect("rolled back destination")
                    .total,
                0
            );
            exec(&rt, "QUEUE MOVE FROM items TO out LIMIT 1");
            assert_eq!(payloads(&rt), ["agent"]);
            let delivery = read_delivery(&rt, "agent");
            exec(
                &rt,
                &format!("QUEUE NACK items GROUP workers '{delivery}' WITH DELAY 600 s"),
            );
            assert!(
                exec(&rt, "QUEUE READ items GROUP workers CONSUMER agent COUNT 1")
                    .result
                    .records
                    .is_empty()
            );
            commit(&rt);
            checkout(&rt, "main");
            assert_eq!(payloads(&rt), ["base"]);
            assert_eq!(
                rt.scan_collection("out", None, 10)
                    .expect("isolated move")
                    .total,
                0
            );
            assert_eq!(
                text(
                    &exec(&rt, "QUEUE PUSH items 'ignored' DEDUP 'key'"),
                    "message_id"
                ),
                original
            );
            assert_eq!(text(&exec(&rt, "QUEUE POP items"), "payload"), "base");
            commit(&rt);
            rt.checkpoint().expect("checkpoint");
        }
        let rt = RedDBRuntime::with_options(options()).expect("reopen");
        checkout(&rt, "agent");
        assert_eq!(payloads(&rt), ["agent"]);
        assert_eq!(
            rt.scan_collection("out", None, 10)
                .expect("moved message")
                .total,
            1
        );
        assert!(exec(
            &rt,
            "QUEUE READ items GROUP workers CONSUMER agent COUNT 1 WAIT 1 ms"
        )
        .result
        .records
        .is_empty());
        exec(&rt, "QUEUE PURGE items");
        assert!(payloads(&rt).is_empty());
        checkout(&rt, "main");
        assert!(payloads(&rt).is_empty());
        assert_eq!(
            rt.scan_collection("out", None, 10)
                .expect("main destination")
                .total,
            0
        );
        clear_current_connection_id();
    }
}

#[test]
fn queue_fanout_acknowledgements_are_branch_local() {
    let rt = RedDBRuntime::in_memory().expect("runtime");
    set_current_connection_id(9201);
    exec(&rt, "CREATE QUEUE items FANOUT");
    rt.vcs_set_versioned("items", true).expect("versioned");
    for group in ["a", "b"] {
        exec(&rt, &format!("QUEUE GROUP CREATE items {group}"));
    }
    exec(&rt, "QUEUE PUSH items 'event'");
    commit(&rt);
    rt.vcs_branch_create(CreateBranchInput {
        connection_id: 9201,
        name: "agent".into(),
        from: None,
    })
    .expect("fork");
    checkout(&rt, "agent");
    let delivery = text(
        &exec(&rt, "QUEUE READ items GROUP a CONSUMER agent COUNT 1"),
        "message_id",
    );
    exec(&rt, &format!("QUEUE ACK items GROUP a '{delivery}'"));
    assert!(exec(&rt, "QUEUE READ items GROUP a CONSUMER agent COUNT 1")
        .result
        .records
        .is_empty());
    assert_eq!(
        text(
            &exec(&rt, "QUEUE READ items GROUP b CONSUMER agent COUNT 1"),
            "payload"
        ),
        "event"
    );
    checkout(&rt, "main");
    for group in ["a", "b"] {
        assert_eq!(
            text(
                &exec(
                    &rt,
                    &format!("QUEUE READ items GROUP {group} CONSUMER main COUNT 1")
                ),
                "payload"
            ),
            "event"
        );
    }
    clear_current_connection_id();
}
