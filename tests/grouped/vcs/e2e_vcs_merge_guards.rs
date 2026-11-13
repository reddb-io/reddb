//! Regression for unsupported data application. These cases assert rejection
//! and unchanged HEAD, not successful merge support for any model.

#[path = "../../support/prometheus.rs"]
#[allow(dead_code)]
mod prometheus;

use reddb::application::{
    Author, CheckoutInput, CheckoutTarget, CreateBranchInput, CreateCommitInput, MergeInput,
    MergeOpts, MergeStrategy, StatusInput,
};
use reddb::catalog::CollectionModel;
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::RedDBRuntime;

fn author() -> Author {
    Author {
        name: "test".into(),
        email: "test@reddb.io".into(),
    }
}

fn check_merge_guard(model: CollectionModel, create: &str, mutation: Option<&str>) {
    let directory = tempfile::tempdir().expect("directory");
    let runtime = if model == CollectionModel::Vault {
        let options = reddb::RedDBOptions::persistent(directory.path().join("vault.rdb"))
            .with_storage_profile(reddb::StorageDeployPreset::Serverless.selection())
            .expect("paged vault profile");
        let runtime = RedDBRuntime::with_options(options).expect("vault runtime");
        let pager = std::sync::Arc::clone(runtime.db().store().pager().expect("pager"));
        let auth = reddb::auth::AuthStore::with_vault_certificate(
            reddb::auth::AuthConfig::default(),
            pager,
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        )
        .expect("synthetic vault certificate");
        auth.ensure_vault_secret_key();
        runtime.set_auth_store(std::sync::Arc::new(auth));
        runtime
    } else {
        RedDBRuntime::in_memory().expect("runtime")
    };
    set_current_connection_id(8201);
    runtime.execute_query(create).expect("create model");
    if model == CollectionModel::Mixed {
        let mut contract = runtime.db().collection_contract("items").expect("contract");
        contract.declared_model = CollectionModel::Mixed;
        runtime
            .db()
            .save_collection_contract(contract)
            .expect("mixed contract");
    }
    assert_eq!(
        runtime
            .db()
            .collection_contract("items")
            .expect("model")
            .declared_model,
        model
    );
    runtime.vcs_set_versioned("items", true).expect("versioned");
    let commit = |message: &str| {
        runtime
            .vcs_commit(CreateCommitInput {
                connection_id: 8201,
                message: message.into(),
                author: author(),
                committer: None,
                amend: false,
                allow_empty: true,
            })
            .expect("commit")
    };
    let base = commit("base");
    runtime
        .vcs_branch_create(CreateBranchInput {
            name: "agent".into(),
            from: None,
            connection_id: 8201,
        })
        .expect("branch");
    let checkout = |branch: &str| {
        runtime
            .vcs_checkout(CheckoutInput {
                connection_id: 8201,
                target: CheckoutTarget::Branch(branch.into()),
                force: false,
            })
            .expect("checkout")
    };
    checkout("agent");
    if let Some(mutation) = mutation {
        runtime.execute_query(mutation).expect("model mutation");
    }
    if model == CollectionModel::Metrics {
        let request = prometheus::WriteRequest {
            timeseries: vec![prometheus::TimeSeries {
                labels: vec![prometheus::label("__name__", "requests_total")],
                samples: vec![prometheus::sample(1.0, 1_704_067_200_000)],
            }],
        };
        let (status, body) = prometheus::post_remote_write(runtime.clone(), "items", &request);
        assert_eq!(status, 204, "metrics sample: {body}");
        let samples = runtime
            .execute_query("SELECT value FROM items WHERE metric = 'requests_total'")
            .expect("stored metric sample");
        assert_eq!(samples.result.records.len(), 1);
    }
    let source = commit("source");
    checkout("main");
    let assert_head = || {
        let status = runtime
            .vcs_status(StatusInput {
                connection_id: 8201,
            })
            .expect("status");
        assert_eq!(status.head_commit.as_deref(), Some(base.hash.as_str()));
        assert!(status.merge_state_id.is_none());
        let main = runtime
            .vcs_list_refs(None)
            .expect("refs")
            .into_iter()
            .find(|reference| reference.name == "refs/heads/main")
            .expect("main");
        assert_eq!(main.target, base.hash);
    };
    for strategy in [MergeStrategy::Auto, MergeStrategy::NoFastForward] {
        let error = runtime
            .vcs_merge(MergeInput {
                connection_id: 8201,
                from: "agent".into(),
                opts: MergeOpts {
                    strategy,
                    ..MergeOpts::default()
                },
                author: author(),
            })
            .expect_err("unapplied data must not be published");
        assert!(
            error.to_string().contains("applying data"),
            "{model:?}: {error}"
        );
        assert_head();
    }
    let error = runtime
        .vcs_cherry_pick(8201, &source.hash, author())
        .expect_err("cherry-pick must apply data before publishing");
    assert!(error.to_string().contains("applying data"));
    assert_head();
    let error = runtime
        .vcs_revert(8201, &source.hash, author())
        .expect_err("revert must apply data before publishing");
    assert!(error.to_string().contains("applying data"));
    assert_head();
    clear_current_connection_id();
}

macro_rules! merge_guard {
    ($name:ident, $model:ident, $create:expr, $mutation:expr) => {
        #[test]
        fn $name() {
            check_merge_guard(CollectionModel::$model, $create, $mutation);
        }
    };
}

merge_guard!(
    table,
    Table,
    "CREATE TABLE items (name TEXT)",
    Some("INSERT INTO items (name) VALUES ('agent')")
);
merge_guard!(
    document,
    Document,
    "CREATE DOCUMENT items",
    Some("INSERT INTO items DOCUMENT VALUES ({name:'agent'})")
);
merge_guard!(
    graph,
    Graph,
    "CREATE GRAPH items",
    Some("INSERT INTO items NODE (label,node_type) VALUES ('agent','Person')")
);
merge_guard!(
    vector,
    Vector,
    "CREATE VECTOR items DIM 2 METRIC cosine",
    Some("INSERT INTO items VECTOR (dense,content) VALUES ([1.0,0.0],'agent')")
);
merge_guard!(
    kv,
    Kv,
    "CREATE KV items",
    Some("KV PUT items.'agent' = 'value'")
);
merge_guard!(hll, Hll, "CREATE HLL items", Some("HLL ADD items 'agent'"));
merge_guard!(
    sketch,
    Sketch,
    "CREATE SKETCH items",
    Some("SKETCH ADD items 'agent' 3")
);
merge_guard!(
    filter,
    Filter,
    "CREATE FILTER items",
    Some("FILTER ADD items 'agent'")
);
merge_guard!(
    config,
    Config,
    "CREATE CONFIG items",
    Some("PUT CONFIG items agent = 'value'")
);
merge_guard!(
    vault,
    Vault,
    "CREATE VAULT items",
    Some("VAULT PUT items.agent = 'synthetic-vcs-test-value'")
);
merge_guard!(
    mixed,
    Mixed,
    "CREATE TABLE items (name TEXT)",
    Some("INSERT INTO items NODE (label,node_type) VALUES ('agent','Person')")
);
merge_guard!(
    timeseries,
    TimeSeries,
    "CREATE TIMESERIES items",
    Some("INSERT INTO items (metric,value,timestamp) VALUES ('requests',1.0,1704067200000000000)")
);
merge_guard!(
    queue,
    Queue,
    "CREATE QUEUE items",
    Some("QUEUE PUSH items 'agent'")
);
merge_guard!(metrics, Metrics, "CREATE METRICS items", None);
