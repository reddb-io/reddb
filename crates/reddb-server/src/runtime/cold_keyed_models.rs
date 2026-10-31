//! Runtime regressions over real published generations, without enabling the
//! unfinished managed profile in the runtime factory.
use super::*;
use crate::storage::engine::BTree;
use reddb_engine::storage::collection_read::PagedCollection;
use std::collections::BTreeMap;
use std::sync::Arc;

fn runtime() -> (tempfile::TempDir, RedDBRuntime) {
    let directory = tempfile::tempdir().expect("cold model fixture");
    let options = RedDBOptions::persistent(directory.path().join("models.rdb"))
        .with_memory_budget(128 * 1024 * 1024)
        .with_storage_profile(
            crate::storage::profile::StorageDeployPreset::PrimaryReplicaDev.selection(),
        )
        .expect("profile");
    let runtime = RedDBRuntime::with_options(options).expect("runtime");
    (directory, runtime)
}

fn install_cold(runtime: &RedDBRuntime, collection: &str, read_bytes_max: usize) {
    let store = runtime.db().store();
    let manager = store.get_collection(collection).expect("collection");
    let root = store
        .collection_root_page(collection)
        .expect("published root");
    let tree = BTree::with_root(Arc::clone(store.pager().expect("pager")), root);
    manager.install_backing(
        Arc::new(PagedCollection::new(Arc::new(tree))),
        root,
        store.format_version(),
        read_bytes_max,
        manager.count() as u64,
    );
}

fn observe(runtime: &RedDBRuntime, query: &str) -> Vec<BTreeMap<String, Value>> {
    runtime.invalidate_result_cache();
    runtime
        .execute_query(query)
        .expect(query)
        .result
        .records
        .iter()
        .map(|row| {
            row.iter_fields()
                .map(|(key, value)| (key.to_string(), value.clone()))
                .collect()
        })
        .collect()
}

#[test]
fn cold_config_reads_preserve_history_tombstones_and_failures() {
    let (_directory, runtime) = runtime();
    for sql in [
        "PUT CONFIG cold_settings theme = 'dark'",
        "PUT CONFIG cold_settings theme = 'light'",
        "ROTATE CONFIG cold_settings theme = 'blue'",
        "PUT CONFIG cold_settings removed = 'old'",
        "DELETE CONFIG cold_settings removed",
    ] {
        runtime.execute_query(sql).expect(sql);
    }
    let queries = [
        "GET CONFIG cold_settings theme",
        "HISTORY CONFIG cold_settings theme",
        "LIST CONFIG cold_settings",
        "GET CONFIG cold_settings removed",
    ];
    let expected: Vec<_> = queries.iter().map(|sql| observe(&runtime, sql)).collect();
    assert_eq!(expected[1].len(), 3);
    runtime
        .db()
        .store()
        .persist()
        .expect("publish config history");
    install_cold(&runtime, "cold_settings", 128 * 1024);
    for (query, expected) in queries.into_iter().zip(expected) {
        assert_eq!(observe(&runtime, query), expected, "{query}");
    }
    install_cold(&runtime, "cold_settings", 1);
    for sql in queries {
        runtime.invalidate_result_cache();
        assert!(
            runtime.execute_query(sql).is_err(),
            "{sql}: failed reads must not return empty history"
        );
    }
    assert!(runtime
        .execute_query("PUT CONFIG cold_settings theme = 'lost-history'")
        .is_err());
    assert_eq!(
        runtime
            .db()
            .store()
            .get_collection("cold_settings")
            .expect("settings")
            .count(),
        5
    );
}

#[test]
fn cold_vault_reads_preserve_encrypted_history_and_export() {
    let (_directory, runtime) = runtime();
    let store = runtime.db().store();
    let auth = Arc::new(
        crate::auth::AuthStore::with_vault_certificate(
            crate::auth::AuthConfig::default(),
            Arc::clone(store.pager().expect("pager")),
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        )
        .expect("test vault"),
    );
    runtime.set_auth_store(auth);
    for sql in [
        "CREATE VAULT cold_secrets WITH OWN MASTER KEY",
        "VAULT PUT cold_secrets.api_key = 'synthetic-first' TAGS [scope:prod]",
        "ROTATE VAULT cold_secrets.api_key = 'synthetic-second' TAGS [scope:prod]",
        "VAULT PUT cold_secrets.removed = 'synthetic-removed'",
        "VAULT DELETE cold_secrets.removed",
    ] {
        runtime.execute_query(sql).expect(sql);
    }
    let queries = [
        "VAULT GET cold_secrets.api_key",
        "VAULT HISTORY cold_secrets.api_key",
        "LIST VAULT cold_secrets",
        "VAULT REVEAL cold_secrets.api_key",
    ];
    let expected: Vec<_> = queries.iter().map(|sql| observe(&runtime, sql)).collect();
    assert_eq!(expected[1].len(), 2);
    let encrypted = runtime
        .export_vault_collection_records("cold_secrets")
        .expect("resident export");
    assert_eq!(encrypted.len(), 4);
    store.persist().expect("publish vault history");
    install_cold(&runtime, "cold_secrets", 128 * 1024);
    for (query, expected) in queries.into_iter().zip(expected) {
        assert_eq!(observe(&runtime, query), expected, "{query}");
    }
    assert_eq!(
        runtime
            .export_vault_collection_records("cold_secrets")
            .expect("cold export"),
        encrypted
    );
    install_cold(&runtime, "cold_secrets", 1);
    for sql in queries {
        runtime.invalidate_result_cache();
        assert!(
            runtime.execute_query(sql).is_err(),
            "{sql}: failed reads must not return absent secrets"
        );
    }
    assert!(runtime
        .export_vault_collection_records("cold_secrets")
        .is_err());
    assert!(runtime
        .execute_query("VAULT PUT cold_secrets.api_key = 'lost-history'")
        .is_err());
    assert_eq!(
        store
            .get_collection("cold_secrets")
            .expect("secrets")
            .count(),
        4
    );
}

#[test]
fn cold_store_config_reads_and_idempotent_boot_reseed() {
    let (_directory, runtime) = runtime();
    let store = runtime.db().store();
    let marker = crate::serde_json::json!(true);
    assert_eq!(
        store.set_config_tree("system.bootstrap.completed", &marker),
        1
    );
    assert_eq!(
        store.get_config("system.bootstrap.completed"),
        Some(Value::Boolean(true))
    );
    store.persist().expect("publish bootstrap marker");
    install_cold(&runtime, "red_config", 128 * 1024);
    assert_eq!(
        store.get_config("system.bootstrap.completed"),
        Some(Value::Boolean(true))
    );
    let manager = store.get_collection("red_config").expect("config");
    let count = manager.count();
    assert_eq!(
        store.set_config_tree("system.bootstrap.completed", &marker),
        1
    );
    assert_eq!(
        manager.count(),
        count,
        "boot reseeding must not duplicate paged config"
    );
    install_cold(&runtime, "red_config", 1);
    assert!(store.try_get_config("system.bootstrap.completed").is_err());
    assert!(store
        .try_set_config_tree("system.bootstrap.completed", &marker)
        .is_err());
    assert_eq!(
        manager.count(),
        count,
        "failed config scans must not mutate boot state"
    );
}
