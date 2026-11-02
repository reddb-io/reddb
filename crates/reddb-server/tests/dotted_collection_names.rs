use std::sync::Arc;

use reddb_server::auth::{AuthConfig, AuthStore};
use reddb_server::storage::StorageDeployPreset;
use reddb_server::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

#[test]
fn dotted_table_names_survive_crud_indexes_and_reopen() {
    for name in [
        "app.audit.events",
        "\"app\".\"audit\".\"events\"",
        "\"app.audit.events\"",
    ] {
        let directory = tempfile::tempdir().expect("fixture directory");
        let path = directory.path().join("names.rdb");
        let open = || {
            RedDBRuntime::with_options(
                RedDBOptions::persistent(&path).with_memory_budget(64 * 1024 * 1024),
            )
            .expect("open")
        };
        let runtime = open();
        for sql in [
            "CREATE TABLE app (id INT PRIMARY KEY)".to_string(),
            "INSERT INTO app (id) VALUES (7)".to_string(),
            format!("CREATE TABLE {name} (id INT PRIMARY KEY, payload TEXT)"),
            format!("INSERT INTO {name} (id,payload) VALUES (7,'first'),(8,'second')"),
            format!("CREATE INDEX payload_idx ON {name} (payload) USING BTREE"),
            format!("UPDATE {name} SET payload='updated' WHERE id=7"),
            format!("DELETE FROM {name} WHERE id=8"),
            format!("ALTER TABLE {name} ADD COLUMN note TEXT"),
            format!("UPDATE {name} SET note='retained' WHERE id=7"),
            format!("SHOW SCHEMA {name}"),
            format!("SHOW INDEXES ON {name}"),
        ] {
            runtime
                .execute_query(&sql)
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
        let joined = runtime
            .execute_query(&format!(
                "SELECT e.payload AS payload FROM {name} e JOIN app p ON e.id=p.id"
            ))
            .expect("join with dotted source");
        assert_eq!(joined.result.records.len(), 1);
        assert_eq!(
            joined.result.records[0].get("payload"),
            Some(&Value::text("updated"))
        );
        let reverse_join = runtime
            .execute_query(&format!(
                "SELECT e.payload AS payload FROM app p JOIN {name} e ON p.id=e.id"
            ))
            .expect("join with dotted right source");
        assert_eq!(reverse_join.result.records.len(), 1);
        assert_eq!(
            reverse_join.result.records[0].get("payload"),
            Some(&Value::text("updated"))
        );
        let indexes = runtime
            .execute_query(&format!("SHOW INDEXES ON {name}"))
            .expect("show indexes");
        assert!(indexes
            .result
            .records
            .iter()
            .any(
                |record| record.get("name") == Some(&Value::text("payload_idx"))
                    && record.get("table") == Some(&Value::text("app.audit.events"))
            ));
        runtime.checkpoint().expect("checkpoint");
        drop(runtime);
        let runtime = open();
        let rows = runtime
            .execute_query(&format!("SELECT id,payload,note FROM {name} WHERE id=7"))
            .expect("reopened rows");
        assert_eq!(rows.result.records.len(), 1);
        assert_eq!(
            rows.result.records[0].get("note"),
            Some(&Value::text("retained"))
        );
        for sql in [
            format!("DROP INDEX payload_idx ON {name}"),
            format!("DROP TABLE {name}"),
        ] {
            runtime
                .execute_query(&sql)
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
        }
        assert!(runtime
            .db()
            .store()
            .get_collection("app.audit.events")
            .is_none());
        assert!(runtime.db().store().get_collection("app").is_some());
    }
}

#[test]
fn dotted_names_are_shared_by_collection_models_and_reserve_red() {
    let directory = tempfile::tempdir().expect("fixture directory");
    let runtime = RedDBRuntime::with_options(
        RedDBOptions::persistent(directory.path().join("models.rdb"))
            .with_storage_profile(StorageDeployPreset::Serverless.selection())
            .expect("paged fixture"),
    )
    .expect("runtime");
    let auth = Arc::new(
        AuthStore::with_vault_certificate(
            AuthConfig::default(),
            runtime.db().store().pager().cloned().expect("pager"),
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        )
        .expect("vault"),
    );
    auth.ensure_vault_secret_key();
    runtime.set_auth_store(auth);
    for model in [
        "TABLE",
        "KV",
        "CONFIG",
        "VAULT",
        "DOCUMENT",
        "GRAPH",
        "VECTOR",
        "QUEUE",
        "TIMESERIES",
        "METRICS",
        "HLL",
        "SKETCH",
        "FILTER",
    ] {
        let suffix = match model {
            "TABLE" => " (id INT)",
            "VECTOR" => " DIM 3",
            _ => "",
        };
        for (index, name) in [
            "app.audit.items",
            "\"app\".\"audit\".\"items\"",
            "\"app.audit.items\"",
        ]
        .iter()
        .enumerate()
        {
            let sql = format!("CREATE {model} {name}{suffix}");
            runtime
                .execute_query(&sql)
                .unwrap_or_else(|error| panic!("{sql}: {error}"));
            assert!(runtime
                .db()
                .store()
                .get_collection("app.audit.items")
                .is_some());
            runtime
                .execute_query(&format!("DROP {model} {name}"))
                .unwrap_or_else(|error| panic!("drop {model}/{index}: {error}"));
        }
        for name in ["red.private", "\"red\".\"private\"", "\"red.private\""] {
            let error = runtime
                .execute_query(&format!("CREATE {model} {name}{suffix}"))
                .expect_err("reserved namespace");
            assert!(
                error.to_string().contains("system schema"),
                "{model}/{name}: {error}"
            );
            assert!(runtime.db().store().get_collection("red.private").is_none());
        }
    }
}

#[test]
fn dotted_keyed_and_queue_commands_round_trip_values() {
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    for sql in [
        "CREATE KV app.audit.cache",
        "KV PUT \"app.audit.cache\".theme = 'dark'",
        "CREATE CONFIG app.audit.settings",
        "PUT CONFIG app.audit.settings theme = 'dark'",
        "CREATE QUEUE app.audit.jobs",
        "QUEUE PUSH app.audit.jobs 'dark'",
        "CREATE DOCUMENT app.audit.docs",
        r#"INSERT INTO "app.audit.docs" DOCUMENT VALUES ({"theme":"dark"})"#,
    ] {
        runtime
            .execute_query(sql)
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
    }
    for sql in [
        "KV GET \"app.audit.cache\".theme",
        "KV LIST app.audit.cache",
        "GET CONFIG app.audit.settings theme",
        "LIST CONFIG \"app.audit.settings\"",
        "QUEUE POP \"app.audit.jobs\"",
        "SELECT theme FROM app.audit.docs",
    ] {
        let result = runtime
            .execute_query(sql)
            .unwrap_or_else(|error| panic!("{sql}: {error}"));
        assert_eq!(result.result.records.len(), 1, "{sql}");
        assert!(
            format!("{:?}", result.result.records).contains("dark"),
            "{sql}: {:?}",
            result.result.records
        );
    }
}
