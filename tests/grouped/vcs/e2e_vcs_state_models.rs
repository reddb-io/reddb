use reddb::application::{
    Author, CheckoutInput, CheckoutTarget, CreateBranchInput, CreateCommitInput, MergeInput,
    MergeOpts,
};
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

#[path = "../../support/prometheus.rs"]
#[allow(dead_code)]
mod prometheus;

fn author() -> Author {
    Author {
        name: "test".into(),
        email: "test@reddb.io".into(),
    }
}

fn commit(rt: &RedDBRuntime) -> String {
    rt.vcs_commit(CreateCommitInput {
        connection_id: 9101,
        message: "state".into(),
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
        connection_id: 9101,
        target: CheckoutTarget::Branch(branch.into()),
        force: false,
    })
    .expect("checkout");
}

fn value(rt: &RedDBRuntime, query: &str, field: &str) -> Value {
    let result = rt
        .execute_query(query)
        .unwrap_or_else(|error| panic!("{query}: {error}"));
    assert_eq!(result.result.records.len(), 1, "{query}: {result:?}");
    result.result.records[0].get(field).expect("field").clone()
}

#[test]
fn config_native_branches_merge_rollback_and_recovery() {
    for paged in [false, true] {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("config.rdb");
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
        set_current_connection_id(9101);
        let base;
        {
            let rt = RedDBRuntime::with_options(options()).expect("runtime");
            rt.execute_query("PUT CONFIG items theme = 'old'")
                .expect("legacy config");
            rt.execute_query("PUT CONFIG items theme = 'base'")
                .expect("legacy history");
            rt.vcs_set_versioned("items", true).expect("versioned");
            base = commit(&rt);
            rt.vcs_branch_create(CreateBranchInput {
                connection_id: 9101,
                name: "agent".into(),
                from: None,
            })
            .expect("fork");
            checkout(&rt, "agent");
            rt.execute_query("ROTATE CONFIG items theme = 'agent'")
                .expect("rotate");
            assert_eq!(
                value(&rt, "GET CONFIG items theme", "value"),
                Value::text("agent")
            );
            commit(&rt);
            assert_eq!(
                rt.execute_query("HISTORY CONFIG items theme")
                    .unwrap()
                    .result
                    .records
                    .len(),
                3
            );
            checkout(&rt, "main");
            assert_eq!(
                value(&rt, "GET CONFIG items theme", "value"),
                Value::text("base")
            );
            assert_eq!(
                rt.execute_query("HISTORY CONFIG items theme")
                    .unwrap()
                    .result
                    .records
                    .len(),
                2
            );
            rt.execute_query("BEGIN").expect("begin");
            rt.execute_query("PUT CONFIG items theme = 'discard'")
                .expect("pending write");
            assert_eq!(
                value(&rt, "GET CONFIG items theme", "value"),
                Value::text("discard")
            );
            rt.execute_query("ROLLBACK").expect("rollback");
            assert_eq!(
                value(&rt, "GET CONFIG items theme", "value"),
                Value::text("base")
            );
            rt.execute_query("PUT CONFIG items locale = 'pt-BR'")
                .expect("independent key");
            commit(&rt);
            let merged = rt
                .vcs_merge(MergeInput {
                    connection_id: 9101,
                    from: "agent".into(),
                    opts: MergeOpts::default(),
                    author: author(),
                })
                .expect("merge");
            assert!(merged.merge_commit.is_some());
            assert_eq!(
                value(&rt, "GET CONFIG items theme", "value"),
                Value::text("agent")
            );
            assert_eq!(
                value(&rt, "GET CONFIG items locale", "value"),
                Value::text("pt-BR")
            );
            rt.execute_query("DELETE CONFIG items theme")
                .expect("delete");
            assert_eq!(
                value(&rt, "GET CONFIG items theme", "tombstone"),
                Value::Boolean(true)
            );
            commit(&rt);
            checkout(&rt, "agent");
            assert_eq!(
                value(&rt, "GET CONFIG items theme", "value"),
                Value::text("agent")
            );
            rt.checkpoint().expect("checkpoint");
        }
        let rt = RedDBRuntime::with_options(options()).expect("reopen");
        checkout(&rt, "agent");
        assert_eq!(
            value(&rt, "GET CONFIG items theme", "value"),
            Value::text("agent")
        );
        checkout(&rt, "main");
        assert_eq!(
            value(&rt, "GET CONFIG items theme", "tombstone"),
            Value::Boolean(true)
        );
        assert_eq!(
            value(&rt, "GET CONFIG items locale", "value"),
            Value::text("pt-BR")
        );
        assert_eq!(value(&rt, &format!("SELECT value FROM items AS OF COMMIT '{base}' WHERE key = 'theme' AND version = 2"), "value"), Value::text("base"));
        clear_current_connection_id();
    }
}

#[test]
fn probabilistic_branches_use_native_state_and_recover_resolved_merges() {
    let cases = [
        (
            "CREATE HLL items",
            "HLL ADD items 'base'",
            "HLL ADD items 'agent'",
            "HLL ADD items 'main' 'extra'",
            "HLL ADD items 'discard'",
            "HLL COUNT items",
            "count",
            "CARDINALITY",
            "cardinality",
            Value::UnsignedInteger(1),
            Value::UnsignedInteger(2),
            Value::UnsignedInteger(3),
        ),
        (
            "CREATE SKETCH items",
            "SKETCH ADD items 'agent' 2",
            "SKETCH ADD items 'agent' 3",
            "SKETCH ADD items 'agent' 7",
            "SKETCH ADD items 'agent' 11",
            "SKETCH COUNT items 'agent'",
            "estimate",
            "FREQ('agent')",
            "freq",
            Value::UnsignedInteger(2),
            Value::UnsignedInteger(5),
            Value::UnsignedInteger(9),
        ),
        (
            "CREATE FILTER items",
            "FILTER ADD items 'base'",
            "FILTER ADD items 'agent'",
            "FILTER ADD items 'main'",
            "FILTER DELETE items 'agent'",
            "FILTER CHECK items 'agent'",
            "exists",
            "CONTAINS('agent')",
            "contains",
            Value::Boolean(false),
            Value::Boolean(true),
            Value::Boolean(false),
        ),
    ];
    for (
        create,
        seed,
        source_write,
        target_write,
        rolled_back,
        read,
        field,
        projection,
        projected_field,
        base_value,
        source_value,
        target_value,
    ) in cases
    {
        for paged in [false, true] {
            let directory = tempfile::tempdir().expect("directory");
            let path = directory.path().join("state.rdb");
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
            set_current_connection_id(9101);
            let (base, source, target, merged);
            {
                let rt = RedDBRuntime::with_options(options()).expect("runtime");
                rt.execute_query(create).expect("create");
                rt.execute_query(seed).expect("pre-opt-in state");
                rt.vcs_set_versioned("items", true).expect("versioned");
                assert_eq!(value(&rt, read, field), base_value);
                base = commit(&rt);
                rt.vcs_branch_create(CreateBranchInput {
                    connection_id: 9101,
                    name: "agent".into(),
                    from: None,
                })
                .expect("fork");
                checkout(&rt, "agent");
                rt.execute_query(source_write).expect("source write");
                assert_eq!(value(&rt, read, field), source_value);
                rt.execute_query("BEGIN").expect("begin");
                rt.execute_query(rolled_back).expect("pending mutation");
                rt.execute_query("ROLLBACK").expect("rollback");
                assert_eq!(value(&rt, read, field), source_value);
                source = commit(&rt);
                checkout(&rt, "main");
                assert_eq!(value(&rt, read, field), base_value);
                rt.execute_query(target_write).expect("target write");
                assert_eq!(value(&rt, read, field), target_value);
                target = commit(&rt);
                let outcome = rt
                    .vcs_merge(MergeInput {
                        connection_id: 9101,
                        from: "agent".into(),
                        opts: MergeOpts::default(),
                        author: author(),
                    })
                    .expect("merge conflict");
                assert_eq!(outcome.conflicts.len(), 1);
                let conflicts = rt
                    .vcs_conflicts_list(outcome.merge_state_id.as_deref().expect("pending merge"))
                    .expect("conflicts");
                assert_eq!(conflicts.len(), 1);
                rt.vcs_conflict_resolve(
                    &conflicts[0].id,
                    reddb::json::Value::String("theirs".into()),
                )
                .expect("resolve");
                assert_eq!(value(&rt, read, field), target_value);
                merged = commit(&rt);
                assert_eq!(value(&rt, read, field), source_value);
                rt.checkpoint().expect("checkpoint");
            }
            let rt = RedDBRuntime::with_options(options()).expect("reopen");
            checkout(&rt, "main");
            assert_eq!(value(&rt, read, field), source_value);
            checkout(&rt, "agent");
            assert_eq!(value(&rt, read, field), source_value);
            for (hash, expected) in [
                (base, &base_value),
                (source, &source_value),
                (target, &target_value),
                (merged, &source_value),
            ] {
                assert_eq!(
                    &value(
                        &rt,
                        &format!("SELECT {projection} FROM items AS OF COMMIT '{hash}'"),
                        projected_field
                    ),
                    expected
                );
            }
            clear_current_connection_id();
        }
    }
}

#[test]
fn metrics_remote_write_branches_and_queries_recover_history() {
    for paged in [false, true] {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("metrics.rdb");
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
        let select_branch = |rt: &RedDBRuntime, branch: &str| {
            checkout(rt, branch);
            rt.vcs_checkout(CheckoutInput {
                connection_id: 0,
                target: CheckoutTarget::Branch(branch.into()),
                force: false,
            })
            .expect("HTTP branch");
        };
        let insert = |rt: &RedDBRuntime, sample: f64, timestamp: i64| {
            let request = prometheus::WriteRequest {
                timeseries: vec![prometheus::TimeSeries {
                    labels: vec![prometheus::label("__name__", "requests_total")],
                    samples: vec![prometheus::sample(sample, timestamp)],
                }],
            };
            let (status, body) = prometheus::post_remote_write(rt.clone(), "items", &request);
            assert_eq!(status, 204, "remote write: {body}");
        };
        let query = |rt: &RedDBRuntime, expected: &str| {
            let (status, body) = prometheus::get(rt.clone(), "/api/v1/query?query=requests_total");
            assert_eq!(status, 200, "Prometheus query: {body}");
            let json: serde_json::Value = serde_json::from_str(&body).expect("response");
            assert_eq!(
                json["data"]["result"][0]["value"][1].as_str(),
                Some(expected)
            );
        };
        let (base, source, target, merged);
        set_current_connection_id(9101);
        {
            let rt = RedDBRuntime::with_options(options()).expect("runtime");
            rt.execute_query("CREATE METRICS items RETENTION 1 d DOWNSAMPLE 60s:raw:avg")
                .expect("metrics");
            insert(&rt, 1.0, 1_704_067_200_000);
            rt.vcs_set_versioned("items", true).expect("versioned");
            base = commit(&rt);
            rt.vcs_branch_create(CreateBranchInput {
                connection_id: 9101,
                name: "agent".into(),
                from: None,
            })
            .expect("fork");
            select_branch(&rt, "agent");
            insert(&rt, 2.0, 1_704_067_201_000);
            source = commit(&rt);
            query(&rt, "2");
            select_branch(&rt, "main");
            query(&rt, "1");
            insert(&rt, 3.0, 1_704_067_202_000);
            target = commit(&rt);
            query(&rt, "3");
            merged = rt
                .vcs_merge(MergeInput {
                    connection_id: 9101,
                    from: "agent".into(),
                    opts: MergeOpts::default(),
                    author: author(),
                })
                .expect("merge")
                .merge_commit
                .expect("merge commit")
                .hash;
            assert_eq!(
                rt.scan_collection("items", None, 10)
                    .expect("merged samples")
                    .total,
                3
            );
            rt.apply_retention_policy()
                .expect("preserve pinned raw samples");
            rt.checkpoint().expect("checkpoint");
        }
        let rt = RedDBRuntime::with_options(options()).expect("reopen");
        select_branch(&rt, "main");
        query(&rt, "3");
        select_branch(&rt, "agent");
        query(&rt, "2");
        for (hash, expected) in [
            (base, vec![1.0]),
            (source, vec![1.0, 2.0]),
            (target, vec![1.0, 3.0]),
            (merged, vec![1.0, 2.0, 3.0]),
        ] {
            let result = rt
                .execute_query(&format!(
                    "SELECT value FROM items AS OF COMMIT '{hash}' ORDER BY value"
                ))
                .expect("sample history");
            let values: Vec<_> = result
                .result
                .records
                .iter()
                .map(|row| row.get("value").unwrap().clone())
                .collect();
            assert_eq!(
                values,
                expected.into_iter().map(Value::Float).collect::<Vec<_>>()
            );
        }
        clear_current_connection_id();
    }
}

#[test]
fn independently_created_keys_conflict_instead_of_duplicating_state() {
    for (create, source_write, target_write, read) in [
        (
            "CREATE KV items",
            "KV PUT items.shared = 'agent'",
            "KV PUT items.shared = 'main'",
            "KV GET items.shared",
        ),
        (
            "CREATE CONFIG items",
            "PUT CONFIG items shared = 'agent'",
            "PUT CONFIG items shared = 'main'",
            "GET CONFIG items shared",
        ),
    ] {
        let rt = RedDBRuntime::in_memory().expect("runtime");
        set_current_connection_id(9101);
        rt.execute_query(create).expect("create");
        rt.vcs_set_versioned("items", true).expect("versioned");
        commit(&rt);
        rt.vcs_branch_create(CreateBranchInput {
            connection_id: 9101,
            name: "agent".into(),
            from: None,
        })
        .expect("fork");
        checkout(&rt, "agent");
        rt.execute_query(source_write).expect("source insert");
        commit(&rt);
        checkout(&rt, "main");
        rt.execute_query(target_write).expect("target insert");
        commit(&rt);
        let outcome = rt
            .vcs_merge(MergeInput {
                connection_id: 9101,
                from: "agent".into(),
                opts: MergeOpts::default(),
                author: author(),
            })
            .expect("merge");
        assert_eq!(outcome.conflicts.len(), 1);
        assert_eq!(value(&rt, read, "value"), Value::text("main"));
        rt.vcs_conflict_resolve(
            &outcome.conflicts[0].id,
            reddb::json::Value::String("theirs".into()),
        )
        .expect("resolve key");
        commit(&rt);
        assert_eq!(value(&rt, read, "value"), Value::text("agent"));
        assert_eq!(
            rt.scan_collection("items", None, 10)
                .expect("unique key")
                .total,
            1
        );
        checkout(&rt, "agent");
        assert_eq!(value(&rt, read, "value"), Value::text("agent"));
        clear_current_connection_id();
    }
}
