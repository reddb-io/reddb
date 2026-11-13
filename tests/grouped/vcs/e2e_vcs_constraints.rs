use reddb::application::{
    Author, CheckoutInput, CheckoutTarget, CreateBranchInput, CreateCommitInput, MergeInput,
    MergeOpts, ResetInput, ResetMode, StatusInput,
};
use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
use reddb::RedDBRuntime;

const CONNECTION: u64 = 9301;

#[test]
fn pending_unique_keys_reserve_only_their_writer_branch() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    set_current_connection_id(CONNECTION);
    runtime
        .execute_query("CREATE TABLE items (code INT UNIQUE)")
        .expect("table");
    runtime.vcs_set_versioned("items", true).expect("versioned");
    fork(&runtime);
    checkout(&runtime, "main");
    runtime.execute_query("BEGIN").expect("begin");
    runtime
        .execute_query("INSERT INTO items (code) VALUES (1)")
        .expect("pending row");
    set_current_connection_id(CONNECTION + 1);
    assert!(runtime
        .execute_query("INSERT INTO items (code) VALUES (1)")
        .is_err());
    runtime
        .vcs_checkout(CheckoutInput {
            connection_id: CONNECTION + 1,
            target: CheckoutTarget::Branch("agent".into()),
            force: false,
        })
        .expect("independent branch");
    runtime
        .execute_query("INSERT INTO items (code) VALUES (1)")
        .expect("sibling does not reserve this key");
    set_current_connection_id(CONNECTION);
    runtime.execute_query("ROLLBACK").expect("rollback");
    runtime
        .execute_query("INSERT INTO items (code) VALUES (1)")
        .expect("aborted writer releases key");
    clear_current_connection_id();
}

fn author() -> Author {
    Author {
        name: "test".into(),
        email: "test@reddb.io".into(),
    }
}

fn commit(runtime: &RedDBRuntime) -> String {
    runtime
        .vcs_commit(CreateCommitInput {
            connection_id: CONNECTION,
            message: "test".into(),
            author: author(),
            committer: None,
            amend: false,
            allow_empty: true,
        })
        .expect("commit")
        .hash
}

fn checkout(runtime: &RedDBRuntime, branch: &str) {
    runtime
        .vcs_checkout(CheckoutInput {
            connection_id: CONNECTION,
            target: CheckoutTarget::Branch(branch.into()),
            force: false,
        })
        .expect("checkout");
}

fn fork(runtime: &RedDBRuntime) {
    commit(runtime);
    runtime
        .vcs_branch_create(CreateBranchInput {
            connection_id: CONNECTION,
            name: "agent".into(),
            from: None,
        })
        .expect("fork");
    checkout(runtime, "agent");
}

fn merge(runtime: &RedDBRuntime) -> reddb::RedDBResult<reddb::application::MergeOutcome> {
    runtime.vcs_merge(MergeInput {
        connection_id: CONNECTION,
        from: "agent".into(),
        opts: MergeOpts::default(),
        author: author(),
    })
}

#[test]
fn unique_keys_are_branch_local_and_rechecked_on_merge() {
    for standalone in [false, true] {
        let directory = tempfile::tempdir().expect("directory");
        let options = reddb::RedDBOptions::persistent(directory.path().join("unique.rdb"))
            .with_storage_profile(reddb::StorageDeployPreset::Serverless.selection())
            .expect("profile");
        let runtime = RedDBRuntime::with_options(options).expect("runtime");
        set_current_connection_id(CONNECTION);
        let ddl = if standalone {
            "CREATE TABLE items (code INT, value TEXT)"
        } else {
            "CREATE TABLE items (code INT UNIQUE, value TEXT)"
        };
        runtime.execute_query(ddl).expect("table");
        if standalone {
            runtime
                .execute_query("CREATE UNIQUE INDEX items_code ON items (code)")
                .expect("index");
        }
        runtime.vcs_set_versioned("items", true).expect("versioned");
        fork(&runtime);
        runtime
            .execute_query("INSERT INTO items (code,value) VALUES (1,'agent')")
            .expect("source insert");
        commit(&runtime);
        checkout(&runtime, "main");
        runtime
            .execute_query("INSERT INTO items (code,value) VALUES (1,'main')")
            .expect("same key is allowed on an independent branch");
        assert!(runtime
            .execute_query("INSERT INTO items (code,value) VALUES (1,'duplicate')")
            .is_err());
        let head = commit(&runtime);
        assert!(merge(&runtime)
            .expect_err("combined keys conflict")
            .to_string()
            .contains("unique"));
        assert_eq!(
            runtime
                .vcs_status(StatusInput {
                    connection_id: CONNECTION
                })
                .expect("status")
                .head_commit,
            Some(head)
        );
        assert_eq!(
            runtime
                .execute_query("SELECT * FROM items")
                .expect("unchanged")
                .result
                .records
                .len(),
            1
        );
        runtime
            .execute_query("UPDATE items SET code = 2")
            .expect("resolve key");
        commit(&runtime);
        assert!(merge(&runtime)
            .expect("merge distinct keys")
            .conflicts
            .is_empty());
        assert_eq!(
            runtime
                .execute_query("SELECT * FROM items")
                .expect("merged")
                .result
                .records
                .len(),
            2
        );
        clear_current_connection_id();
    }
}

#[test]
fn combined_fields_recheck_checks_and_recompute_generated_columns() {
    for generated in [false, true] {
        let runtime = RedDBRuntime::in_memory().expect("runtime");
        set_current_connection_id(CONNECTION);
        let ddl = if generated {
            "CREATE TABLE items (a INT, b INT, total INT GENERATED ALWAYS AS (a + b) STORED)"
        } else {
            "CREATE TABLE items (a INT, b INT CHECK (a < b))"
        };
        runtime.execute_query(ddl).expect("table");
        runtime.vcs_set_versioned("items", true).expect("versioned");
        runtime
            .execute_query("INSERT INTO items (a,b) VALUES (1,10)")
            .expect("base");
        fork(&runtime);
        runtime
            .execute_query("UPDATE items SET b = 5")
            .expect("source valid");
        commit(&runtime);
        checkout(&runtime, "main");
        runtime
            .execute_query("UPDATE items SET a = 7")
            .expect("target valid");
        commit(&runtime);
        if generated {
            assert!(merge(&runtime)
                .expect("derived field is recomputed")
                .conflicts
                .is_empty());
            let rows = runtime
                .execute_query("SELECT total FROM items")
                .expect("sum");
            assert_eq!(
                rows.result.records[0].get("total"),
                Some(&reddb_types::Value::Integer(12))
            );
        } else {
            assert!(merge(&runtime)
                .expect_err("merged 7 < 5 is invalid")
                .to_string()
                .contains("CHECK"));
        }
        clear_current_connection_id();
    }
}

#[test]
fn head_movement_cannot_restore_data_invalid_under_current_contract() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    set_current_connection_id(CONNECTION);
    runtime
        .execute_query("CREATE TABLE items (value INT)")
        .expect("table");
    runtime.vcs_set_versioned("items", true).expect("versioned");
    runtime
        .execute_query("INSERT INTO items (value) VALUES (NULL)")
        .expect("old row");
    let old = commit(&runtime);
    runtime
        .execute_query("UPDATE items SET value = 1")
        .expect("valid now");
    let head = commit(&runtime);
    let mut contract = runtime.db().collection_contract("items").expect("contract");
    contract.declared_columns[0].not_null = true;
    contract.table_def = None;
    runtime
        .db()
        .save_collection_contract(contract)
        .expect("new contract");
    assert!(runtime
        .vcs_reset(ResetInput {
            connection_id: CONNECTION,
            target: old,
            mode: ResetMode::Hard,
        })
        .is_err());
    assert_eq!(
        runtime
            .vcs_status(StatusInput {
                connection_id: CONNECTION
            })
            .expect("status")
            .head_commit,
        Some(head)
    );
    clear_current_connection_id();
}
