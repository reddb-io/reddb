use reddb::application::ExecuteQueryInput;
use reddb::catalog::{CollectionModel, SchemaMode};
use reddb::physical::{CollectionContract, ContractOrigin};
use reddb::{QueryUseCases, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    QueryUseCases::new(rt)
        .execute(ExecuteQueryInput {
            query: sql.to_string(),
        })
        .unwrap_or_else(|err| panic!("{sql}: {err}"));
}

fn exec_err(rt: &RedDBRuntime, sql: &str) -> String {
    match QueryUseCases::new(rt).execute(ExecuteQueryInput {
        query: sql.to_string(),
    }) {
        Ok(_) => panic!("expected error for {sql}"),
        Err(err) => err.to_string(),
    }
}

fn register_collection(rt: &RedDBRuntime, name: &str, model: CollectionModel) {
    rt.db()
        .store()
        .create_collection(name)
        .unwrap_or_else(|err| panic!("create {name}: {err}"));
    rt.db()
        .save_collection_contract(CollectionContract {
            name: name.to_string(),
            declared_model: model,
            schema_mode: SchemaMode::Dynamic,
            origin: ContractOrigin::Explicit,
            version: 1,
            created_at_unix_ms: 0,
            updated_at_unix_ms: 0,
            default_ttl_ms: None,
            context_index_fields: Vec::new(),
            declared_columns: Vec::new(),
            table_def: None,
            timestamps_enabled: false,
            context_index_enabled: false,
            append_only: false,
        })
        .unwrap_or_else(|err| panic!("contract {name}: {err}"));
}

#[test]
fn typed_drop_removes_non_table_models() {
    let rt = rt();
    for (name, model, sql) in [
        ("identity", CollectionModel::Graph, "DROP GRAPH identity"),
        ("notes", CollectionModel::Vector, "DROP VECTOR notes"),
        ("logs", CollectionModel::Document, "DROP DOCUMENT logs"),
        ("settings", CollectionModel::Kv, "DROP KV settings"),
    ] {
        register_collection(&rt, name, model);
        exec(&rt, sql);
        assert!(rt.db().store().get_collection(name).is_none(), "{name}");
        assert!(rt.db().collection_contract(name).is_none(), "{name}");
    }
}

#[test]
fn drop_collection_dispatches_polymorphically_and_if_exists_is_idempotent() {
    let rt = rt();
    exec(&rt, "CREATE TABLE users (id INT)");
    exec(&rt, "DROP COLLECTION users");
    assert!(rt.db().store().get_collection("users").is_none());

    exec(&rt, "DROP TABLE IF EXISTS missing_table");
    exec(&rt, "DROP COLLECTION IF EXISTS missing_collection");
}

#[test]
fn drop_model_mismatch_and_system_schema_are_rejected() {
    let rt = rt();
    exec(&rt, "CREATE QUEUE jobs");

    let err = exec_err(&rt, "DROP TABLE jobs");
    assert!(
        err.contains("model mismatch: expected table, got queue"),
        "unexpected error: {err}"
    );

    let err = exec_err(&rt, "DROP COLLECTION red.collections");
    assert!(
        err.contains("system schema is read-only"),
        "unexpected error: {err}"
    );
}
