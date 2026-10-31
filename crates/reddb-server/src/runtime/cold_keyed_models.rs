//! Runtime regressions over real published generations, without enabling the
//! unfinished managed profile in the runtime factory.
use super::*;
use crate::storage::engine::BTree;
use crate::storage::RowData;
use reddb_engine::storage::collection_read::{CollectionCursor, PagedCollection};
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

fn install_indexed_cold(runtime: &RedDBRuntime, collection: &str, read_bytes_max: usize) {
    let store = runtime.db().store();
    let manager = store.get_collection(collection).expect("collection");
    let pager = Arc::clone(store.pager().expect("pager"));
    let keys = Arc::new(BTree::new(Arc::clone(&pager)));
    let data = Arc::new(BTree::new(Arc::clone(&pager)));
    let root = store
        .collection_root_page(collection)
        .expect("published root");
    // The ordinary writer remains enabled in this fixture. Copy the published
    // bytes into a private base so later ordinary writes cannot mutate a pinned
    // experimental generation in place.
    let published = PagedCollection::new(Arc::new(BTree::with_root(pager, root)));
    let mut cursor = published.scan().expect("published cursor");
    while let Some(record) = cursor.next_record(usize::MAX).expect("published record") {
        let (entity, _) = record
            .decode(store.format_version())
            .expect("native record");
        if let Some(key) = reddb_engine::storage::segment_key_lookup::SegmentKeyLookup::key(&entity)
        {
            let fingerprint = reddb_file::native_key_index::fingerprint(key);
            keys.insert(
                &reddb_file::native_key_index::encode_candidate(&fingerprint, entity.id.raw()),
                &[],
            )
            .expect("key candidate");
        }
        data.insert(&record.id.raw().to_be_bytes(), &record.bytes)
            .expect("private base");
    }
    let root = data.root_page_id();
    manager.install_backing(
        Arc::new(PagedCollection::with_key_index(data, keys)),
        root,
        store.format_version(),
        read_bytes_max,
        manager.count() as u64,
    );
}

fn insert_unrelated_overlay(runtime: &RedDBRuntime, collection: &str) {
    let mut entity = UnifiedEntity::table_row(EntityId::new(0), collection, 0, vec![]);
    entity.data = EntityData::Row(RowData::with_names(
        vec![
            Value::text("unrelated-overlay"),
            Value::text("x".repeat(32 * 1024)),
        ],
        vec!["key".into(), "value".into()],
    ));
    runtime
        .db()
        .store()
        .insert_auto(collection, entity)
        .expect("dirty unrelated row");
}

#[test]
fn cold_indexed_config_history_skips_unrelated_base_and_dirty_payloads() {
    let (_directory, runtime) = runtime();
    for sql in [
        format!(
            "PUT CONFIG settings unrelated = '{}'",
            "x".repeat(32 * 1024)
        ),
        "PUT CONFIG settings wanted = 'first'".into(),
        "ROTATE CONFIG settings wanted = 'second'".into(),
    ] {
        runtime.execute_query(&sql).expect("seed config");
    }
    let expected = observe(&runtime, "HISTORY CONFIG settings wanted");
    assert_eq!(expected.len(), 2);
    runtime.db().store().persist().expect("publish config");
    // Config's named columns/metadata require more than 4 KiB even for a
    // small value. This remains well below either unrelated 32 KiB payload.
    install_indexed_cold(&runtime, "settings", 8192);
    insert_unrelated_overlay(&runtime, "settings");
    assert_eq!(
        observe(&runtime, "HISTORY CONFIG settings wanted"),
        expected
    );
    runtime
        .execute_query("PUT CONFIG settings wanted = 'third'")
        .expect("append keyed version");
    assert_eq!(observe(&runtime, "HISTORY CONFIG settings wanted").len(), 3);
    runtime.invalidate_result_cache();
    assert!(runtime
        .execute_query("GET CONFIG settings unrelated")
        .is_err());
}

#[test]
fn cold_indexed_store_config_reads_and_replaces_only_the_target_key() {
    let (_directory, runtime) = runtime();
    let store = runtime.db().store();
    store
        .try_set_config_tree(
            "fixture",
            &crate::serde_json::json!({
                "unrelated": "x".repeat(32 * 1024), "wanted": "first"
            }),
        )
        .expect("seed store config");
    store.persist().expect("publish store config");
    install_indexed_cold(&runtime, "red_config", 4096);
    insert_unrelated_overlay(&runtime, "red_config");
    assert_eq!(
        store
            .try_get_config("fixture.wanted")
            .expect("small config"),
        Some(Value::text("first"))
    );
    assert_eq!(
        store
            .try_get_config("fixture.absent")
            .expect("absent config"),
        None
    );
    store
        .try_set_config_tree("fixture.wanted", &crate::serde_json::json!("second"))
        .expect("keyed replacement");
    assert_eq!(
        store.try_get_config("fixture.wanted").expect("new config"),
        Some(Value::text("second"))
    );
    assert!(store.try_get_config("fixture.unrelated").is_err());
}

#[test]
fn cold_indexed_vault_history_retains_metadata_and_tenant_scope() {
    let (_directory, runtime) = runtime();
    let store = runtime.db().store();
    runtime.set_auth_store(Arc::new(
        crate::auth::AuthStore::with_vault_certificate(
            crate::auth::AuthConfig::default(),
            Arc::clone(store.pager().expect("pager")),
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        )
        .expect("vault certificate"),
    ));
    // Values are compressed before encryption. Use deterministic noise so the
    // encrypted base payload itself exceeds the cursor's 4 KiB allowance.
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut state = 0x1234_5678_u32;
    let large: String = (0..32 * 1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            alphabet[(state & 63) as usize] as char
        })
        .collect();
    for sql in [
        "CREATE VAULT secrets WITH OWN MASTER KEY".to_string(),
        format!("VAULT PUT secrets.unrelated = '{large}'"),
        "VAULT PUT secrets.wanted = 'first' TAGS [scope:prod]".into(),
        "ROTATE VAULT secrets.wanted = 'second' TAGS [scope:prod]".into(),
        "WITHIN TENANT 'another' VAULT PUT secrets.wanted = 'tenant-only'".into(),
    ] {
        runtime.execute_query(&sql).expect("seed vault");
    }
    let queries = [
        "VAULT HISTORY secrets.wanted",
        "VAULT GET secrets.wanted",
        "VAULT REVEAL secrets.wanted",
        "WITHIN TENANT 'another' VAULT HISTORY secrets.wanted",
    ];
    let expected: Vec<_> = queries.iter().map(|sql| observe(&runtime, sql)).collect();
    assert_eq!(expected[0].len(), 2);
    assert_eq!(expected[3].len(), 1);
    store.persist().expect("publish encrypted vault");
    install_indexed_cold(&runtime, "secrets", 4096);
    insert_unrelated_overlay(&runtime, "secrets");
    let manager = store.get_collection("secrets").expect("secrets");
    let mut oversized = manager
        .try_paged_key_physical_cursor("unrelated")
        .expect("oversized base cursor")
        .expect("cold secrets");
    assert!(oversized
        .next()
        .expect("existing oversized secret")
        .is_err());
    runtime.invalidate_result_cache();
    assert!(
        runtime
            .execute_query("VAULT REVEAL secrets.unrelated")
            .is_err(),
        "before rotation"
    );
    for (query, expected) in queries.into_iter().zip(expected) {
        assert_eq!(observe(&runtime, query), expected, "{query}");
    }
    runtime
        .execute_query("ROTATE VAULT secrets.wanted = 'third' TAGS [scope:prod]")
        .expect("rotate indexed vault");
    assert_eq!(observe(&runtime, "VAULT HISTORY secrets.wanted").len(), 3);
    runtime.invalidate_result_cache();
    assert!(manager.has_backing(), "rotation must retain cold backing");
    assert!(
        runtime
            .execute_query("VAULT REVEAL secrets.unrelated")
            .is_err(),
        "after rotation"
    );
}

#[test]
fn cold_vault_compressed_plaintext_must_fit_the_read_allowance() {
    let (_directory, runtime) = runtime();
    let store = runtime.db().store();
    runtime.set_auth_store(Arc::new(
        crate::auth::AuthStore::with_vault_certificate(
            crate::auth::AuthConfig::default(),
            Arc::clone(store.pager().expect("pager")),
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        )
        .expect("vault certificate"),
    ));
    runtime
        .execute_query("CREATE VAULT packed WITH OWN MASTER KEY")
        .expect("vault");
    runtime
        .execute_query(&format!(
            "VAULT PUT packed.large = '{}'",
            "x".repeat(32 * 1024)
        ))
        .expect("compressed value");
    let expected = observe(&runtime, "VAULT REVEAL packed.large");
    assert_eq!(
        expected[0].get("value"),
        Some(&Value::text("x".repeat(32 * 1024)))
    );
    store.persist().expect("publish sealed value");
    install_indexed_cold(&runtime, "packed", 4096);
    let manager = store.get_collection("packed").expect("packed");
    let record = manager
        .try_paged_key_physical_cursor("large")
        .expect("key cursor")
        .expect("cold")
        .next()
        .expect("sealed record")
        .expect("encrypted row fits 4 KiB");
    assert!(
        matches!(record.0.data.as_row().and_then(|row| row.get_field("value")), Some(Value::Secret(bytes)) if bytes.len() < 512)
    );
    runtime.invalidate_result_cache();
    let error = match runtime.execute_query("VAULT REVEAL packed.large") {
        Err(error) => error,
        Ok(_) => {
            panic!("a tiny sealed record must not expand to 32 KiB within a 4 KiB read allowance")
        }
    };
    assert!(error.to_string().contains("read memory limit"), "{error}");
    install_indexed_cold(&runtime, "packed", 128 * 1024);
    assert_eq!(observe(&runtime, "VAULT REVEAL packed.large"), expected);
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
        "LIST CONFIG cold_settings PREFIX 'th'",
        "LIST CONFIG cold_settings PREFIX 'absent'",
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
        "LIST VAULT cold_secrets PREFIX 'api'",
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

#[test]
fn cold_vcs_settings_keep_listing_opt_out_and_idempotent_reenable() {
    let (_directory, runtime) = runtime();
    for collection in ["first_history", "second_history"] {
        runtime.vcs_set_versioned(collection, true).expect("opt in");
    }
    runtime
        .db()
        .store()
        .persist()
        .expect("publish VCS settings");
    let collection = crate::application::vcs_collections::SETTINGS;
    install_cold(&runtime, collection, 128 * 1024);
    assert_eq!(
        runtime
            .vcs_list_versioned()
            .expect("cold versioning inventory"),
        vec!["first_history", "second_history"]
    );
    runtime
        .vcs_set_versioned("first_history", false)
        .expect("explicit opt out");
    assert!(!runtime
        .vcs_is_versioned("first_history")
        .expect("cold opt out"));
    runtime
        .vcs_set_versioned("first_history", true)
        .expect("reenable");
    runtime
        .vcs_set_versioned("first_history", true)
        .expect("idempotent opt in");
    let store = runtime.db().store();
    assert_eq!(
        store.get_collection(collection).expect("settings").count(),
        2
    );
    store.persist().expect("publish changed settings");
    install_cold(&runtime, collection, 1);
    assert!(runtime.vcs_list_versioned().is_err());
    assert!(runtime.vcs_set_versioned("first_history", false).is_err());
    assert_eq!(
        store.get_collection(collection).expect("settings").count(),
        2
    );
}

#[test]
fn cold_counter_mutations_keep_ttl_and_tags() {
    let (_directory, runtime) = runtime();
    runtime
        .execute_query("KV PUT cold_counters.item = 1 EXPIRE 10m TAGS [scope:prod]")
        .expect("expiring counter");
    runtime.db().store().persist().expect("publish counter");
    install_cold(&runtime, "cold_counters", 128 * 1024);
    runtime
        .execute_query("KV INCR cold_counters.item BY 1")
        .expect("cold increment");
    let result = runtime
        .execute_query("KV GET cold_counters.item")
        .expect("incremented counter");
    let id = match result.result.records[0].get("rid") {
        Some(Value::UnsignedInteger(id)) => crate::storage::EntityId::new(*id),
        other => panic!("counter identity: {other:?}"),
    };
    let store = runtime.db().store();
    let manager = store.get_collection("cold_counters").expect("counter");
    let metadata = manager
        .try_get_metadata(id, 128 * 1024)
        .expect("metadata read")
        .expect("metadata");
    assert_eq!(
        metadata.get("_ttl_ms"),
        Some(&crate::storage::MetadataValue::Int(600_000))
    );
    assert_eq!(
        runtime
            .inner
            .kv_tag_index
            .tags_for_key("cold_counters", "item"),
        vec!["scope:prod"]
    );
}

#[test]
fn cold_signed_write_registry_preserves_allowed_signers() {
    let (_directory, runtime) = runtime();
    let store = runtime.db().store();
    runtime
        .execute_query("CREATE TABLE signed_events (id INTEGER)")
        .expect("table");
    super::signed_writes_kind::install(&store, "signed_events", &[[7; 32]], "fixture");
    assert_eq!(
        super::signed_writes_kind::registry(&store, "signed_events").allowed_len(),
        1
    );
    store.persist().expect("publish signer registry");
    install_cold(&runtime, "red_config", 128 * 1024);
    assert_eq!(
        super::signed_writes_kind::registry(&store, "signed_events").allowed_len(),
        1,
        "paging config must not turn an existing signer registry into an empty registry"
    );
}

#[test]
fn cold_unreadable_signature_policy_blocks_an_unsigned_insert() {
    let (_directory, runtime) = runtime();
    let store = runtime.db().store();
    runtime
        .execute_query("CREATE TABLE signed_events (id INTEGER)")
        .expect("table");
    super::signed_writes_kind::install(&store, "signed_events", &[[7; 32]], "fixture");
    assert!(runtime
        .execute_query("INSERT INTO signed_events (id) VALUES (1)")
        .is_err());
    store.persist().expect("publish signature policy");
    install_cold(&runtime, "red_config", 1);
    let error = runtime
        .execute_query("INSERT INTO signed_events (id) VALUES (2)")
        .expect_err("unreadable policy must not become an unsigned collection");
    assert!(error.to_string().contains("read memory limit"), "{error}");
    assert!(super::signed_writes_kind::try_is_signed(&store, "signed_events").is_err());
    assert!(super::signed_writes_kind::try_registry(&store, "signed_events").is_err());
    assert!(
        super::signed_writes_kind::is_signed(&store, "signed_events"),
        "the legacy boolean probe must also require verification when policy reads fail"
    );
    assert_eq!(
        store
            .get_collection("signed_events")
            .expect("events")
            .count(),
        0
    );
}

#[test]
fn cold_vault_listing_keeps_own_pending_version_over_newer_committed_versions() {
    use super::execution_context::{clear_current_connection_id, set_current_connection_id};
    struct Scope;
    impl Drop for Scope {
        fn drop(&mut self) {
            clear_current_connection_id();
        }
    }
    let _scope = Scope;
    let (_directory, runtime) = runtime();
    let store = runtime.db().store();
    runtime.set_auth_store(Arc::new(
        crate::auth::AuthStore::with_vault_certificate(
            crate::auth::AuthConfig::default(),
            Arc::clone(store.pager().expect("pager")),
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
        )
        .expect("vault certificate"),
    ));
    runtime
        .execute_query("CREATE VAULT shadow WITH OWN MASTER KEY")
        .expect("vault");
    runtime
        .execute_query("VAULT PUT shadow.token = 'initial'")
        .expect("initial version");
    store.persist().expect("publish initial version");
    install_indexed_cold(&runtime, "shadow", 128 * 1024);
    set_current_connection_id(6001);
    runtime
        .execute_query("BEGIN ISOLATION LEVEL READ COMMITTED")
        .expect("transaction");
    runtime
        .execute_query("ROTATE VAULT shadow.token = 'pending'")
        .expect("own pending version");
    set_current_connection_id(6002);
    runtime
        .execute_query("ROTATE VAULT shadow.token = 'committed'")
        .expect("concurrent version");
    runtime
        .execute_query("ROTATE VAULT shadow.token = 'committed-newer'")
        .expect("higher committed version");
    set_current_connection_id(6001);
    assert_eq!(
        observe(&runtime, "VAULT REVEAL shadow.token VERSION 3")[0].get("value"),
        Some(&Value::text("committed-newer"))
    );
    assert_eq!(
        observe(&runtime, "LIST VAULT shadow PREFIX 'tok'")[0].get("version"),
        Some(&Value::Integer(2))
    );
    assert_eq!(
        observe(&runtime, "VAULT REVEAL shadow.token")[0].get("value"),
        Some(&Value::text("pending"))
    );
    runtime
        .execute_query("ROLLBACK")
        .expect("discard pending version");
    assert_eq!(
        observe(&runtime, "LIST VAULT shadow")[0].get("version"),
        Some(&Value::Integer(3))
    );
}

#[test]
fn cold_integrity_tombstones_filter_persisted_rows() {
    let (_directory, runtime) = runtime();
    runtime
        .execute_query("CREATE TABLE cold_ingest (value INT)")
        .expect("table");
    runtime
        .execute_query("INSERT INTO cold_ingest (value) VALUES (1), (2), (3)")
        .expect("rows");
    let rows = runtime
        .execute_query("SELECT * FROM cold_ingest ORDER BY value")
        .expect("rows");
    let hidden = super::integrity_tombstone::record_rid(&rows.result.records[1]).expect("row id");
    let store = runtime.db().store();
    let ranges = vec![super::integrity_tombstone::TombstoneRange::new(
        "cold_ingest",
        hidden,
        hidden,
    )];
    store
        .try_set_config_tree(
            super::integrity_tombstone::TOMBSTONE_KEY,
            &crate::serde_json::Value::String(super::integrity_tombstone::serialize_ranges(
                &ranges,
            )),
        )
        .expect("tombstones");
    store.persist().expect("publish tombstones");
    install_indexed_cold(&runtime, "red_config", 128 * 1024);
    // The seed SELECT initialized the empty cache; simulate first load after boot.
    runtime
        .inner
        .integrity_tombstones_state
        .store(0, std::sync::atomic::Ordering::Relaxed);
    runtime.invalidate_result_cache();
    let visible = runtime
        .execute_query("SELECT * FROM cold_ingest ORDER BY value")
        .expect("filtered rows");
    assert_eq!(
        visible.result.records.len(),
        2,
        "persisted integrity tombstones must survive paging"
    );
    assert_eq!(
        visible.result.records[0].get("value"),
        Some(&Value::Integer(1))
    );
    assert_eq!(
        visible.result.records[1].get("value"),
        Some(&Value::Integer(3))
    );
}

#[test]
fn cold_integrity_read_failure_is_not_cached_as_an_empty_list() {
    let (_directory, runtime) = runtime();
    runtime
        .execute_query("CREATE TABLE cold_ingest (value INT)")
        .expect("table");
    runtime
        .execute_query("INSERT INTO cold_ingest (value) VALUES (1)")
        .expect("rows");
    let store = runtime.db().store();
    store
        .try_set_config_tree(
            super::integrity_tombstone::TOMBSTONE_KEY,
            &crate::serde_json::Value::String("[]".to_string()),
        )
        .expect("tombstones");
    store.persist().expect("publish tombstones");
    install_indexed_cold(&runtime, "red_config", 1);
    runtime
        .inner
        .integrity_tombstones_state
        .store(0, std::sync::atomic::Ordering::Relaxed);
    for _ in 0..2 {
        runtime.invalidate_result_cache();
        assert!(
            runtime.execute_query("SELECT * FROM cold_ingest").is_err(),
            "integrity state unavailable must fail the read"
        );
        assert_eq!(
            runtime
                .inner
                .integrity_tombstones_state
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "failed hydration must remain retryable"
        );
    }
    install_indexed_cold(&runtime, "red_config", 128 * 1024);
    runtime.invalidate_result_cache();
    assert_eq!(
        runtime
            .execute_query("SELECT * FROM cold_ingest")
            .expect("read after recovery")
            .result
            .records
            .len(),
        1
    );
}

#[test]
fn cold_integrity_malformed_persisted_list_refuses_reads() {
    for invalid in [
        crate::serde_json::Value::String("not JSON".into()),
        crate::serde_json::Value::String("[{\"table\":\"cold_ingest\",\"lo\":10}]".into()),
        crate::serde_json::Value::Bool(false),
    ] {
        let (_directory, runtime) = runtime();
        runtime
            .execute_query("CREATE TABLE cold_ingest (value INT)")
            .expect("table");
        let store = runtime.db().store();
        store
            .try_set_config_tree(super::integrity_tombstone::TOMBSTONE_KEY, &invalid)
            .expect("seed corrupt list");
        store.persist().expect("publish fixture");
        install_indexed_cold(&runtime, "red_config", 128 * 1024);
        runtime
            .inner
            .integrity_tombstones_state
            .store(0, std::sync::atomic::Ordering::Relaxed);
        assert!(
            runtime.execute_query("SELECT * FROM cold_ingest").is_err(),
            "invalid quarantine config must not silently expose rows"
        );
    }
}

#[test]
fn cold_integrity_persist_failure_is_reported_and_keeps_local_quarantine() {
    let (_directory, runtime) = runtime();
    runtime
        .execute_query("CREATE TABLE cold_ingest (value INT)")
        .expect("table");
    runtime
        .execute_query("INSERT INTO cold_ingest (value) VALUES (1)")
        .expect("rows");
    let rows = runtime
        .execute_query("SELECT * FROM cold_ingest")
        .expect("empty integrity cache loaded");
    let id = super::integrity_tombstone::record_rid(&rows.result.records[0]).expect("id");
    let store = runtime.db().store();
    store
        .try_set_config_tree(
            super::integrity_tombstone::TOMBSTONE_KEY,
            &crate::serde_json::Value::String("[]".to_string()),
        )
        .expect("seed config");
    store.persist().expect("publish config");
    install_indexed_cold(&runtime, "red_config", 1);
    assert!(
        runtime
            .record_integrity_tombstone("cold_ingest", id, id)
            .is_err(),
        "must report failed durable quarantine"
    );
    runtime.invalidate_result_cache();
    let visible = runtime
        .execute_query("SELECT * FROM cold_ingest")
        .expect("local quarantine remains applied");
    assert!(visible.result.records.is_empty());
    assert_eq!(
        runtime.integrity_tombstone_ranges().expect("local ranges"),
        vec![super::integrity_tombstone::TombstoneRange::new(
            "cold_ingest",
            id,
            id
        )]
    );
}

#[test]
fn cold_stream_config_keeps_persisted_limits_and_integrity_mode() {
    let (_directory, runtime) = runtime();
    let store = runtime.db().store();
    for (key, value) in [
        ("stream.snapshot.ttl_ms", crate::serde_json::json!(1234)),
        ("stream.chunk.max_rows", crate::serde_json::json!(7)),
        ("stream.max_global", crate::serde_json::json!(3)),
        ("stream.max_per_principal", crate::serde_json::json!(2)),
        (
            "stream.integrity.default_verify",
            crate::serde_json::json!("sha256"),
        ),
    ] {
        store
            .try_set_config_tree(key, &value)
            .expect("stream setting");
    }
    store.persist().expect("publish settings");
    install_indexed_cold(&runtime, "red_config", 128 * 1024);
    let config =
        crate::server::output_stream::StreamConfig::load(&runtime).expect("cold stream config");
    assert_eq!(
        config.default_verify,
        super::integrity_tombstone::VerifyMode::Sha256
    );
    assert_eq!(config.snapshot_ttl_ms, 1234);
    assert_eq!(config.chunk_max_rows, 7);
    assert_eq!(config.max_global_streams, 3);
    assert_eq!(config.max_per_principal_streams, 2);
    let lease = crate::server::output_stream::open_stream(
        config,
        0,
        false,
        &crate::server::output_stream::SystemClock,
    )
    .expect("open frozen lease");
    store
        .try_set_config_tree(
            "stream.chunk.max_rows",
            &crate::serde_json::Value::Integer(1),
        )
        .expect("next lease setting");
    assert_eq!(
        lease.config.chunk_max_rows, 7,
        "an open lease keeps its settings"
    );
    assert_eq!(
        crate::server::output_stream::StreamConfig::load(&runtime)
            .expect("next config")
            .chunk_max_rows,
        1
    );
}

#[test]
fn cold_stream_config_read_failure_remains_retryable() {
    let (_directory, runtime) = runtime();
    let store = runtime.db().store();
    store
        .try_set_config_tree(
            "stream.integrity.default_verify",
            &crate::serde_json::Value::String("sha256".into()),
        )
        .expect("verification setting");
    store.persist().expect("publish setting");
    install_indexed_cold(&runtime, "red_config", 1);
    assert!(crate::server::output_stream::StreamConfig::load(&runtime).is_err());
    install_indexed_cold(&runtime, "red_config", 128 * 1024);
    assert_eq!(
        crate::server::output_stream::StreamConfig::load(&runtime)
            .expect("recovered config")
            .default_verify,
        super::integrity_tombstone::VerifyMode::Sha256
    );
}

#[test]
fn cold_stream_config_rejects_invalid_limits_and_verification_modes() {
    for (key, value) in [
        ("stream.max_global", crate::serde_json::Value::Bool(true)),
        (
            "stream.chunk.max_rows",
            crate::serde_json::Value::Integer(-1),
        ),
        (
            "stream.snapshot.ttl_ms",
            crate::serde_json::Value::String("not-a-number".into()),
        ),
        (
            "stream.integrity.default_verify",
            crate::serde_json::Value::String("unknown".into()),
        ),
        (
            "stream.integrity.default_verify",
            crate::serde_json::Value::Bool(false),
        ),
    ] {
        let (_directory, runtime) = runtime();
        let store = runtime.db().store();
        store
            .try_set_config_tree(key, &value)
            .expect("invalid persisted setting");
        store.persist().expect("publish fixture");
        install_indexed_cold(&runtime, "red_config", 128 * 1024);
        let error = crate::server::output_stream::StreamConfig::load(&runtime)
            .expect_err("must not silently fall back");
        assert!(error.to_string().contains(key), "{error}");
    }
}

#[tokio::test]
async fn cold_stream_config_redwire_refuses_before_open_ack() {
    use crate::wire::redwire::output_stream::{parse_open_stream, run_output_stream, FrameTx};
    let (_directory, runtime) = runtime();
    let store = runtime.db().store();
    store
        .try_set_config_tree(
            "stream.integrity.default_verify",
            &crate::serde_json::Value::String("sha256".into()),
        )
        .expect("verification setting");
    store.persist().expect("publish setting");
    install_indexed_cold(&runtime, "red_config", 1);
    let (send, mut receive) = tokio::sync::mpsc::channel(8);
    let (_cancel, cancel) = tokio::sync::oneshot::channel();
    run_output_stream(
        Arc::new(runtime),
        37,
        11,
        parse_open_stream(br#"{"sql":"SELECT 1"}"#).expect("open request"),
        false,
        cancel,
        FrameTx::new(send),
        crate::application::OperationContext::read_only("fixture"),
        crate::auth::Role::Read,
    )
    .await;
    let bytes = receive.recv().await.expect("configuration error frame");
    let (frame, consumed) =
        reddb_wire::redwire::decode_frame(&bytes).expect("encoded RedWire response");
    assert_eq!(consumed, bytes.len());
    assert_eq!(frame.kind, reddb_wire::redwire::MessageKind::StreamError);
    assert_eq!(frame.correlation_id, 37);
    assert_eq!(frame.stream_id, 11);
    assert!(String::from_utf8_lossy(&frame.payload).contains("stream_config_unavailable"));
    assert!(
        receive.recv().await.is_none(),
        "unavailable config must not accept a lease or execute a query"
    );
}

#[test]
fn cold_chain_tip_does_not_disappear_with_persisted_rows() {
    let (_directory, runtime) = runtime();
    runtime
        .execute_query("CREATE COLLECTION cold_chain KIND blockchain")
        .expect("chain");
    runtime
        .execute_query("INSERT INTO cold_chain (actor) VALUES ('first')")
        .expect("block");
    let expected = runtime
        .chain_tip_for_collection("cold_chain")
        .expect("resident tip");
    runtime.db().store().persist().expect("publish chain");
    install_indexed_cold(&runtime, "cold_chain", 128 * 1024);
    runtime.inner.chain_tip_cache.lock().clear();
    assert_eq!(
        runtime.chain_tip_for_collection("cold_chain"),
        Some(expected),
        "cold physical blocks remain authoritative"
    );
}

#[test]
fn cold_chain_verification_cannot_verify_an_empty_resident_subset() {
    let (_directory, runtime) = runtime();
    runtime
        .execute_query("CREATE COLLECTION cold_chain KIND blockchain")
        .expect("chain");
    runtime
        .execute_query("INSERT INTO cold_chain (actor) VALUES ('first')")
        .expect("block");
    let expected = runtime
        .verify_chain_for_collection("cold_chain")
        .expect("resident verification");
    assert_eq!(expected.checked, 2);
    runtime.db().store().persist().expect("publish chain");
    install_indexed_cold(&runtime, "cold_chain", 128 * 1024);
    assert_eq!(
        runtime.verify_chain_for_collection("cold_chain"),
        Some(expected)
    );
}

#[test]
fn cold_chain_broken_integrity_flag_must_keep_blocking_appends() {
    let (_directory, runtime) = runtime();
    runtime
        .execute_query("CREATE COLLECTION cold_chain KIND blockchain")
        .expect("chain");
    let store = runtime.db().store();
    super::blockchain_kind::persist_integrity_flag(&store, "cold_chain", true);
    runtime.inner.chain_integrity_broken.lock().clear();
    store.persist().expect("publish broken flag");
    install_indexed_cold(&runtime, "red_config", 128 * 1024);
    let error = runtime
        .execute_query("INSERT INTO cold_chain (actor) VALUES ('must-remain-blocked')")
        .expect_err("persisted broken chain must remain locked");
    assert!(
        error.to_string().contains("ChainIntegrityBroken"),
        "{error}"
    );
}

fn install_chain_native_rows(
    runtime: &RedDBRuntime,
    mut transform: impl FnMut(&mut UnifiedEntity),
) {
    let store = runtime.db().store();
    let manager = store.get_collection("cold_chain").expect("chain");
    let data = Arc::new(BTree::new(Arc::clone(store.pager().expect("pager"))));
    manager
        .try_for_each_entity(usize::MAX, |entity| {
            let mut entity = entity.clone();
            transform(&mut entity);
            data.insert(
                &entity.id.raw().to_be_bytes(),
                &reddb_engine::storage::metadata_codec::serialize_entity_record(
                    &entity,
                    None,
                    store.format_version(),
                ),
            )
            .expect("private native chain");
            true
        })
        .expect("copy physical chain");
    let root = data.root_page_id();
    manager.install_backing(
        Arc::new(PagedCollection::new(data)),
        root,
        store.format_version(),
        128 * 1024,
        manager.count() as u64,
    );
    runtime.inner.chain_tip_cache.lock().clear();
}

#[test]
fn cold_chain_positional_rows_preserve_nullable_payload_hashes() {
    let (_directory, runtime) = runtime();
    runtime
        .execute_query("CREATE COLLECTION cold_chain KIND blockchain")
        .expect("chain");
    runtime
        .execute_query(
            "INSERT INTO cold_chain (actor, note) VALUES ('first', NULL), ('second', 'present')",
        )
        .expect("blocks");
    let expected = runtime
        .try_verify_chain_for_collection("cold_chain")
        .expect("read")
        .expect("verification");
    assert!(expected.ok);
    assert_eq!(expected.checked, 3);
    let tip = runtime
        .try_chain_tip_for_collection("cold_chain")
        .expect("read");
    install_chain_native_rows(&runtime, |entity| {
        let EntityData::Row(row) = &mut entity.data else {
            panic!("chain row");
        };
        let (names, values): (Vec<_>, Vec<_>) = row
            .iter_fields()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .unzip();
        *row = RowData {
            columns: values,
            named: None,
            schema: Some(Arc::new(names)),
        };
    });
    assert_eq!(
        runtime
            .try_chain_tip_for_collection("cold_chain")
            .expect("cold tip read"),
        tip
    );
    assert_eq!(
        runtime
            .try_verify_chain_for_collection("cold_chain")
            .expect("cold verification"),
        Some(expected)
    );
}

#[test]
fn cold_chain_failed_reads_leave_tip_and_integrity_caches_retryable() {
    let (_directory, runtime) = runtime();
    runtime
        .execute_query("CREATE COLLECTION cold_chain KIND blockchain")
        .expect("chain");
    let store = runtime.db().store();
    store.persist().expect("publish chain");
    install_indexed_cold(&runtime, "cold_chain", 1);
    runtime.inner.chain_tip_cache.lock().clear();
    assert!(runtime.try_chain_tip_for_collection("cold_chain").is_err());
    assert!(runtime
        .try_verify_chain_for_collection("cold_chain")
        .is_err());
    assert!(!runtime
        .inner
        .chain_tip_cache
        .lock()
        .contains_key("cold_chain"));
    install_indexed_cold(&runtime, "cold_chain", 128 * 1024);
    assert!(runtime
        .try_chain_tip_for_collection("cold_chain")
        .expect("recovered tip")
        .is_some());
    super::blockchain_kind::try_persist_integrity_flag(&store, "cold_chain", true)
        .expect("broken flag");
    runtime
        .inner
        .chain_integrity_broken
        .lock()
        .insert("cold_chain".into(), true);
    store.persist().expect("publish flag");
    install_indexed_cold(&runtime, "red_config", 1);
    assert!(runtime
        .try_clear_chain_integrity_flag("cold_chain")
        .is_err());
    assert_eq!(
        runtime
            .inner
            .chain_integrity_broken
            .lock()
            .get("cold_chain"),
        Some(&true)
    );
    install_indexed_cold(&runtime, "red_config", 128 * 1024);
    assert_eq!(
        super::blockchain_kind::try_is_integrity_broken_persisted(&store, "cold_chain")
            .expect("recover flag"),
        Some(true)
    );
}

#[test]
fn cold_chain_malformed_blocks_are_errors_instead_of_skipped_rows() {
    for field in ["hash", "prev_hash", "block_height", "timestamp"] {
        let (_directory, runtime) = runtime();
        runtime
            .execute_query("CREATE COLLECTION cold_chain KIND blockchain")
            .expect("chain");
        install_chain_native_rows(&runtime, |entity| {
            let EntityData::Row(row) = &mut entity.data else {
                panic!("chain row");
            };
            let mut fields: std::collections::HashMap<String, Value> = row
                .iter_fields()
                .map(|(key, value)| (key.to_string(), value.clone()))
                .collect();
            fields.remove(field);
            *row = RowData {
                columns: Vec::new(),
                named: Some(fields),
                schema: None,
            };
        });
        assert!(
            runtime
                .try_verify_chain_for_collection("cold_chain")
                .is_err(),
            "missing {field} must not verify as an empty chain"
        );
    }
}

#[test]
fn cold_chain_verification_obeys_work_budget_and_recovers_after_cancellation() {
    let (_directory, runtime) = runtime();
    runtime
        .execute_query("CREATE COLLECTION cold_chain KIND blockchain")
        .expect("chain");
    runtime
        .execute_query("INSERT INTO cold_chain (actor) VALUES ('first')")
        .expect("block");
    runtime.db().store().persist().expect("publish blocks");
    install_indexed_cold(&runtime, "cold_chain", 128 * 1024);
    let scope = super::function_budget::Scope::enter(1, 60_000).expect("bounded work");
    let error = runtime
        .try_verify_chain_for_collection("cold_chain")
        .expect_err("two blocks exceed one unit of work");
    assert!(error.to_string().contains("work_max"), "{error}");
    drop(scope);
    let outcome = runtime
        .try_verify_chain_for_collection("cold_chain")
        .expect("retry after cancellation")
        .expect("chain outcome");
    assert!(outcome.ok);
    assert_eq!(outcome.checked, 2);
}

fn seed_expression_config(runtime: &RedDBRuntime) {
    runtime
        .execute_query("CREATE TABLE expression_probe (id INTEGER)")
        .expect("probe table");
    runtime
        .execute_query("INSERT INTO expression_probe (id) VALUES (1)")
        .expect("probe row");
    runtime
        .execute_query("SET CONFIG fixture.flag = 'ready'")
        .expect("expression config");
}

#[test]
fn cold_config_expressions_retain_the_published_value() {
    let (_directory, runtime) = runtime();
    seed_expression_config(&runtime);
    let query = "SELECT CONFIG('fixture.flag', 'fallback') AS v FROM expression_probe";
    let expected = observe(&runtime, query);
    assert_eq!(expected.len(), 1);
    assert!(expected[0]
        .get("v")
        .is_some_and(|value| !value.is_null() && value != &Value::text("fallback")));
    runtime.db().store().persist().expect("publish config");
    install_indexed_cold(&runtime, "red_config", 4096);
    runtime.invalidate_result_cache();
    assert_eq!(
        observe(&runtime, query),
        expected,
        "CONFIG expressions must not replace cold values with defaults"
    );
}

#[test]
fn unreadable_config_expressions_return_errors_instead_of_defaults() {
    let (_directory, runtime) = runtime();
    seed_expression_config(&runtime);
    let query = "SELECT CONFIG('fixture.flag', 'fallback') AS v FROM expression_probe";
    runtime.execute_query(query).expect("resident value");
    runtime.db().store().persist().expect("publish config");
    install_indexed_cold(&runtime, "red_config", 1);
    runtime.invalidate_result_cache();
    assert!(
        runtime.db().store().try_get_config("fixture.flag").is_err(),
        "the published config is genuinely unreadable at this allowance"
    );
    assert!(
        runtime.execute_query(query).is_err(),
        "an operating read failure must not become a successful fallback value"
    );
}

#[test]
fn cold_config_expressions_preserve_aliases_nested_calls_and_predicates() {
    let (_directory, runtime) = runtime();
    seed_expression_config(&runtime);
    let queries = [
        "SELECT $config.fixture.flag AS v FROM expression_probe",
        "SELECT CONFIG(fixture.flag, 'fallback') AS v FROM expression_probe",
        "SELECT KV('red_config', 'fixture.flag', 'fallback') AS v FROM expression_probe",
        "SELECT LENGTH(CONFIG('fixture.flag')) AS v FROM expression_probe",
        "SELECT CASE WHEN CONFIG('fixture.flag') = 'ready' THEN 7 ELSE 8 END AS v FROM expression_probe",
        "SELECT id FROM expression_probe WHERE CONFIG('fixture.flag') = 'ready'",
    ];
    let expected: Vec<_> = queries
        .iter()
        .map(|query| observe(&runtime, query))
        .collect();
    assert!(expected.iter().all(|rows| rows.len() == 1));
    runtime
        .db()
        .store()
        .persist()
        .expect("publish expression config");
    install_indexed_cold(&runtime, "red_config", 4096);
    for (query, expected) in queries.into_iter().zip(expected) {
        assert_eq!(observe(&runtime, query), expected, "{query}");
    }
}

#[test]
fn unreadable_config_expressions_propagate_nested_and_predicate_errors_and_retry() {
    let (_directory, runtime) = runtime();
    seed_expression_config(&runtime);
    let queries = [
        "SELECT KV('red_config', 'fixture.flag', 'fallback') AS v FROM expression_probe",
        "SELECT LENGTH(CONFIG('fixture.flag')) AS v FROM expression_probe",
        "SELECT CONFIG('fixture.flag') IS NULL AS v FROM expression_probe",
        "SELECT CASE WHEN CONFIG('fixture.flag') = 'ready' THEN 7 ELSE 8 END AS v FROM expression_probe",
        "SELECT id FROM expression_probe WHERE CONFIG('fixture.flag') = 'ready'",
    ];
    let expected: Vec<_> = queries
        .iter()
        .map(|query| observe(&runtime, query))
        .collect();
    runtime
        .db()
        .store()
        .persist()
        .expect("publish expression config");
    install_indexed_cold(&runtime, "red_config", 1);
    for query in queries {
        runtime.invalidate_result_cache();
        assert!(
            runtime.execute_query(query).is_err(),
            "read failure became successful NULL/default/empty result: {query}"
        );
    }
    install_indexed_cold(&runtime, "red_config", 4096);
    for (query, expected) in queries.into_iter().zip(expected) {
        assert_eq!(
            observe(&runtime, query),
            expected,
            "read errors must not poison later retries: {query}"
        );
    }
}

#[test]
fn denied_config_expressions_do_not_load_an_unreadable_snapshot() {
    let (_directory, runtime) = runtime();
    seed_expression_config(&runtime);
    runtime
        .db()
        .store()
        .persist()
        .expect("publish expression config");
    install_indexed_cold(&runtime, "red_config", 1);
    for query in [
        "SELECT CONFIG('red.config.private') AS v FROM expression_probe",
        "SELECT KV('red_config', 'red.config.private') AS v FROM expression_probe",
    ] {
        let result = observe(&runtime, query);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].get("v"), Some(&Value::Null));
    }
}

#[test]
fn unreadable_vault_expressions_propagate_errors_and_remain_retryable() {
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
    auth.ensure_vault_secret_key();
    runtime.set_auth_store(auth);
    for query in [
        "CREATE VAULT expression_secrets",
        "VAULT PUT expression_secrets.token = 'synthetic-value'",
        "CREATE TABLE expression_probe (id INTEGER)",
        "INSERT INTO expression_probe (id) VALUES (1)",
    ] {
        runtime.execute_query(query).expect(query);
    }
    let queries = [
        "SELECT $secrets.expression_secrets.token AS v FROM expression_probe",
        "SELECT LENGTH($secrets.expression_secrets.token) AS v FROM expression_probe",
        "SELECT id FROM expression_probe WHERE $secrets.expression_secrets.token = 'synthetic-value'",
    ];
    store.persist().expect("publish vault");
    install_indexed_cold(&runtime, "expression_secrets", 1);
    for query in queries {
        runtime.invalidate_result_cache();
        assert!(
            runtime.execute_query(query).is_err(),
            "an unreadable authorized secret must not become NULL: {query}"
        );
    }
    install_indexed_cold(&runtime, "expression_secrets", 8192);
    for query in queries {
        let rows = observe(&runtime, query);
        assert_eq!(rows.len(), 1, "failed reads must remain retryable: {query}");
        if let Some(value) = rows[0].get("v") {
            assert!(matches!(value, Value::Secret(_)), "{query}: {value:?}");
            assert_eq!(value.display_string(), "***");
        } else {
            assert_eq!(rows[0].get("id"), Some(&Value::Integer(1)));
        }
    }
}

#[test]
fn unreadable_config_update_expressions_do_not_publish_fallback_values() {
    for (query, expected) in [
        (
            "UPDATE expression_probe SET id = id + LENGTH(CONFIG('fixture.flag', 'fallback')) ORDER BY id LIMIT 2",
            [6, 7],
        ),
        (
            "UPDATE expression_probe SET id = CASE WHEN id = 1 THEN 11 ELSE LENGTH(CONFIG('fixture.flag', 'fallback')) END ORDER BY id LIMIT 2",
            [5, 11],
        ),
        (
            "UPDATE expression_probe SET id += LENGTH(CONFIG('fixture.flag', 'fallback')) ORDER BY id LIMIT 2",
            [6, 7],
        ),
    ] {
        let (_directory, runtime) = runtime();
        seed_expression_config(&runtime);
        runtime
            .execute_query("INSERT INTO expression_probe (id) VALUES (2)")
            .expect("second update target");
        runtime.db().store().persist().expect("publish config");
        install_indexed_cold(&runtime, "red_config", 1);
        runtime.invalidate_result_cache();
        assert!(runtime.db().store().try_get_config("fixture.flag").is_err());
        assert!(
            runtime.execute_query(query).is_err(),
            "an operating read failure must not publish a fallback UPDATE: {query}"
        );
        let ids = |runtime: &RedDBRuntime| {
            observe(runtime, "SELECT id FROM expression_probe ORDER BY id")
                .into_iter()
                .map(|row| row.get("id").cloned().expect("update target id"))
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&runtime), vec![Value::Integer(1), Value::Integer(2)]);
        install_indexed_cold(&runtime, "red_config", 4096);
        runtime.execute_query(query).expect("retry config UPDATE");
        assert_eq!(ids(&runtime), expected.map(Value::Integer));
    }
}

fn verify_config_expression_read_failure_and_retry(queries: &[&str]) {
    let (_directory, runtime) = runtime();
    seed_expression_config(&runtime);
    runtime
        .execute_query("INSERT INTO expression_probe (id) VALUES (2), (3), (4), (5)")
        .expect("ordered query inputs");
    let expected: Vec<_> = queries
        .iter()
        .map(|query| observe(&runtime, query))
        .collect();
    runtime.db().store().persist().expect("publish config");
    install_indexed_cold(&runtime, "red_config", 1);
    assert!(runtime.db().store().try_get_config("fixture.flag").is_err());
    for query in queries {
        runtime.invalidate_result_cache();
        assert!(
            runtime.execute_query(query).is_err(),
            "an operating read failure became a successful ordered result: {query}"
        );
    }
    install_indexed_cold(&runtime, "red_config", 4096);
    for (query, expected) in queries.iter().zip(expected) {
        runtime.invalidate_result_cache();
        assert_eq!(observe(&runtime, query), expected, "retry: {query}");
    }
}

#[test]
fn unreadable_config_ordering_expressions_propagate_errors_and_retry() {
    verify_config_expression_read_failure_and_retry(&[
        "SELECT id FROM expression_probe ORDER BY CONFIG('fixture.flag'), id",
        "SELECT id FROM expression_probe ORDER BY LENGTH(CONFIG('fixture.flag', 'fallback')) + id LIMIT 1",
        "SELECT id FROM expression_probe ORDER BY LENGTH(CONFIG('fixture.flag')) + id LIMIT 1 OFFSET 1",
        "SELECT id FROM expression_probe ORDER BY CASE WHEN id > 3 THEN CONFIG('fixture.flag') ELSE '0' END, id",
        "SELECT id, COUNT(*) AS n FROM expression_probe GROUP BY id ORDER BY CONFIG('fixture.flag'), id",
    ]);
}

#[test]
fn unreadable_config_window_expressions_propagate_errors_and_retry() {
    verify_config_expression_read_failure_and_retry(&[
        "SELECT id, ROW_NUMBER() OVER (ORDER BY CONFIG('fixture.flag'), id) AS n FROM expression_probe ORDER BY id",
        "SELECT id, ROW_NUMBER() OVER (PARTITION BY CONFIG('fixture.flag') ORDER BY id) AS n FROM expression_probe ORDER BY id",
        "SELECT id, LAG(id, 1, LENGTH(CONFIG('fixture.flag', 'fallback'))) OVER (ORDER BY id) AS previous FROM expression_probe ORDER BY id",
        "SELECT id, SUM(LENGTH(CONFIG('fixture.flag'))) OVER (ORDER BY id) AS total FROM expression_probe ORDER BY id",
    ]);
}

#[test]
fn unreadable_config_in_unvisited_ordering_and_window_branches_is_not_loaded() {
    let (_directory, runtime) = runtime();
    seed_expression_config(&runtime);
    runtime
        .execute_query("INSERT INTO expression_probe (id) VALUES (2), (3)")
        .expect("lazy expression inputs");
    let queries = [
        "SELECT id FROM expression_probe ORDER BY CASE WHEN id > 0 THEN id ELSE LENGTH(CONFIG('fixture.flag')) END",
        "SELECT id, ROW_NUMBER() OVER (ORDER BY CASE WHEN id > 0 THEN id ELSE LENGTH(CONFIG('fixture.flag')) END) AS n FROM expression_probe ORDER BY id",
        "SELECT id, SUM(CASE WHEN id > 0 THEN id ELSE LENGTH(CONFIG('fixture.flag')) END) OVER (ORDER BY id) AS total FROM expression_probe ORDER BY id",
    ];
    let expected: Vec<_> = queries
        .iter()
        .map(|query| observe(&runtime, query))
        .collect();
    runtime.db().store().persist().expect("publish lazy config");
    install_indexed_cold(&runtime, "red_config", 1);
    assert!(runtime.db().store().try_get_config("fixture.flag").is_err());
    for (query, expected) in queries.iter().zip(expected) {
        runtime.invalidate_result_cache();
        assert_eq!(observe(&runtime, query), expected, "lazy branch: {query}");
    }
}

#[test]
fn unreadable_config_dml_predicates_fail_before_publication_and_retry() {
    use reddb_rql::ast::{CompareOp, FieldRef, Filter};

    for index in [None, Some("HASH"), Some("BTREE")] {
        for (statement, predicate, expected) in [
            (
                "UPDATE expression_probe SET id = id + 10",
                "id = 2 AND CONFIG('fixture.flag') = 'ready'",
                vec![1, 3, 12],
            ),
            (
                "DELETE FROM expression_probe",
                "id = 2 AND CONFIG('fixture.flag') = 'ready'",
                vec![1, 3],
            ),
            (
                "UPDATE expression_probe SET id = id + 10",
                "id >= 1 AND (rid = {first} OR CONFIG('fixture.flag') = 'ready')",
                vec![11, 12, 13],
            ),
            (
                "DELETE FROM expression_probe",
                "id >= 1 AND (rid = {first} OR CONFIG('fixture.flag') = 'ready')",
                vec![],
            ),
        ] {
            let (_directory, runtime) = runtime();
            seed_expression_config(&runtime);
            runtime
                .execute_query("INSERT INTO expression_probe (id) VALUES (2), (3)")
                .expect("DML candidates");
            if let Some(kind) = index {
                runtime
                    .execute_query(&format!(
                        "CREATE INDEX expression_id ON expression_probe (id) USING {kind}"
                    ))
                    .expect("DML candidate index");
            }
            let eligible = Filter::Compare {
                field: FieldRef::TableColumn {
                    table: String::new(),
                    column: "id".into(),
                },
                op: CompareOp::Ge,
                value: Value::Integer(1),
            };
            let candidates = super::dml_target_scan::DmlTargetScan::new(
                &runtime,
                "expression_probe",
                Some(&eligible),
                None,
            )
            .find_target_ids()
            .expect("candidate traversal order");
            assert_eq!(candidates.len(), 3);
            // Unordered SQL does not promise insertion order. Choose the actual
            // first candidate so a later failed read follows an admitted match.
            let predicate = predicate.replace("{first}", &candidates[0].raw().to_string());
            runtime
                .db()
                .store()
                .persist()
                .expect("publish predicate config");
            install_indexed_cold(&runtime, "red_config", 1);
            assert!(runtime.db().store().try_get_config("fixture.flag").is_err());
            let query = format!("{statement} WHERE {predicate}");
            runtime.invalidate_result_cache();
            assert!(
                runtime.execute_query(&query).is_err(),
                "unreadable DML predicate succeeded ({index:?}): {query}"
            );
            let ids = |runtime: &RedDBRuntime| {
                observe(runtime, "SELECT id FROM expression_probe ORDER BY id")
                    .into_iter()
                    .map(|row| row.get("id").cloned().expect("candidate id"))
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                ids(&runtime),
                vec![Value::Integer(1), Value::Integer(2), Value::Integer(3)],
                "failed predicate published a mutation: {query}"
            );
            install_indexed_cold(&runtime, "red_config", 4096);
            runtime.execute_query(&query).expect("retry DML predicate");
            assert_eq!(
                ids(&runtime),
                expected.into_iter().map(Value::Integer).collect::<Vec<_>>(),
                "predicate retry: {query}"
            );
        }
    }
}

#[test]
fn dml_identity_candidates_recheck_the_entire_predicate() {
    for statement in [
        "UPDATE expression_probe SET id = 99",
        "DELETE FROM expression_probe",
    ] {
        let (_directory, runtime) = runtime();
        seed_expression_config(&runtime);
        let row = observe(&runtime, "SELECT rid FROM expression_probe");
        let rid = row[0]
            .get("rid")
            .expect("logical identity")
            .display_string();
        let query = format!("{statement} WHERE rid = {rid} AND id = 999");
        let result = runtime
            .execute_query(&query)
            .expect("nonmatching identity predicate");
        assert_eq!(
            result.affected_rows, 0,
            "identity lookup bypassed WHERE: {query}"
        );
        assert_eq!(
            observe(&runtime, "SELECT id FROM expression_probe")[0].get("id"),
            Some(&Value::Integer(1))
        );
        runtime
            .db()
            .store()
            .persist()
            .expect("publish identity config");
        install_indexed_cold(&runtime, "red_config", 1);
        let query = format!("{statement} WHERE rid = {rid} AND CONFIG('fixture.flag') = 'ready'");
        assert!(
            runtime.execute_query(&query).is_err(),
            "identity predicate hid a read failure: {query}"
        );
        assert_eq!(
            observe(&runtime, "SELECT id FROM expression_probe")[0].get("id"),
            Some(&Value::Integer(1))
        );
        install_indexed_cold(&runtime, "red_config", 4096);
        let retried = runtime
            .execute_query(&query)
            .expect("retry identity predicate");
        assert_eq!(retried.affected_rows, 1);
        let remaining = observe(&runtime, "SELECT id FROM expression_probe");
        if statement.starts_with("UPDATE") {
            assert_eq!(remaining[0].get("id"), Some(&Value::Integer(99)));
        } else {
            assert!(remaining.is_empty());
        }
    }
}

#[test]
fn unreadable_config_dml_predicates_preserve_lazy_branches_and_limit() {
    let (_directory, runtime) = runtime();
    seed_expression_config(&runtime);
    runtime
        .execute_query("INSERT INTO expression_probe (id) VALUES (2), (3)")
        .expect("lazy DML candidates");
    runtime
        .db()
        .store()
        .persist()
        .expect("publish lazy DML config");
    install_indexed_cold(&runtime, "red_config", 1);
    for query in [
        "UPDATE expression_probe SET id = 99 WHERE id < 0 AND CONFIG('fixture.flag') = 'ready'",
        "DELETE FROM expression_probe WHERE id < 0 AND CONFIG('fixture.flag') = 'ready'",
    ] {
        assert_eq!(
            runtime
                .execute_query(query)
                .expect("unvisited predicate")
                .affected_rows,
            0
        );
    }
    let candidates =
        super::dml_target_scan::DmlTargetScan::new(&runtime, "expression_probe", None, Some(1))
            .find_target_ids()
            .expect("first unordered candidate");
    let first = runtime
        .db()
        .store()
        .get("expression_probe", candidates[0])
        .expect("candidate row");
    let previous = first
        .data
        .as_row()
        .expect("table row")
        .get_field("id")
        .cloned()
        .expect("candidate value");
    let query = format!("UPDATE expression_probe SET id = 11 WHERE rid = {} OR CONFIG('fixture.flag') = 'ready' LIMIT 1", first.logical_id().raw());
    let updated = runtime
        .execute_query(&query)
        .expect("LIMIT stops before an unreadable later row");
    assert_eq!(updated.affected_rows, 1);
    let ids: Vec<_> = observe(&runtime, "SELECT id FROM expression_probe ORDER BY id")
        .into_iter()
        .map(|row| row.get("id").cloned().expect("remaining candidate id"))
        .collect();
    let mut expected: Vec<_> = (1..=3)
        .map(Value::Integer)
        .filter(|value| value != &previous)
        .collect();
    expected.push(Value::Integer(11));
    assert_eq!(ids, expected);
}

fn cold_dml_fixture(index: Option<&str>) -> (tempfile::TempDir, RedDBRuntime) {
    let (directory, runtime) = runtime();
    for sql in [
        "CREATE TABLE cold_dml_probe (id INTEGER, score INTEGER)",
        "INSERT INTO cold_dml_probe (id, score) VALUES (1, 10), (2, 20), (3, 30)",
    ] {
        runtime.execute_query(sql).expect(sql);
    }
    if let Some(kind) = index {
        runtime
            .execute_query(&format!(
                "CREATE INDEX cold_dml_id ON cold_dml_probe (id) USING {kind}"
            ))
            .expect("DML source index");
    }
    (directory, runtime)
}

#[test]
fn cold_dml_delete_event_snapshots_follow_actual_ids_and_keep_admission() {
    let (_directory, runtime) = cold_dml_fixture(None);
    let store = runtime.db().store();
    let manager = store
        .get_collection("cold_dml_probe")
        .expect("event source");
    let target = manager.query_all(|entity| {
        entity.data.as_row().and_then(|row| row.get_field("id")) == Some(&Value::Integer(2))
    })[0]
        .id;
    store.persist().expect("publish event source");
    install_indexed_cold(&runtime, "cold_dml_probe", 4096);
    let ids = [EntityId::new(u64::MAX - 1), target];
    let topology = runtime
        .index_store_ref()
        .collection_topology_lock("cold_dml_probe");
    let deleted = runtime
        .delete_entities_batch("cold_dml_probe", &ids, topology.read(), None, true)
        .expect("delete after a missing prefix");
    assert_eq!(deleted.affected, 1);
    assert_eq!(deleted.changes.len(), 1);
    assert_eq!(
        deleted.changes[0].0, target,
        "CDC identity must not be inferred from the input prefix"
    );
    assert_eq!(deleted.pre_images.len(), 1);
    let before = deleted
        .pre_images
        .get(&target.raw())
        .expect("actual deleted preimage");
    assert_eq!(
        before.get("id").and_then(crate::json::Value::as_i64),
        Some(2)
    );
    assert_eq!(
        before.get("score").and_then(crate::json::Value::as_i64),
        Some(20)
    );
    {
        let reservations = runtime.inner.memory_reservations.lock();
        assert!(
            reservations.reserved_bytes > reservations.completed_bytes,
            "event snapshots must retain admission after storage publication"
        );
    }
    drop(deleted);
    let reservations = runtime.inner.memory_reservations.lock();
    assert_eq!(reservations.reserved_bytes, reservations.completed_bytes);
}

#[test]
fn cold_dml_deletes_base_rows_and_preserves_transaction_rollback() {
    for index in [None, Some("HASH"), Some("BTREE")] {
        let (_directory, runtime) = cold_dml_fixture(index);
        runtime
            .db()
            .store()
            .persist()
            .expect("publish delete source");
        install_indexed_cold(&runtime, "cold_dml_probe", 4096);
        assert_eq!(
            runtime
                .execute_query("DELETE FROM cold_dml_probe WHERE id = 2")
                .expect("cold delete")
                .affected_rows,
            1,
            "cold delete missed the published row: {index:?}"
        );
        let remaining = || {
            observe(&runtime, "SELECT id FROM cold_dml_probe ORDER BY id")
                .into_iter()
                .map(|row| row.get("id").cloned().expect("id"))
                .collect::<Vec<_>>()
        };
        assert_eq!(remaining(), [Value::Integer(1), Value::Integer(3)]);
        runtime.execute_query("BEGIN").expect("begin delete");
        assert_eq!(
            runtime
                .execute_query("DELETE FROM cold_dml_probe WHERE id = 1")
                .expect("transactional cold delete")
                .affected_rows,
            1
        );
        assert_eq!(remaining(), [Value::Integer(3)]);
        runtime.execute_query("ROLLBACK").expect("rollback delete");
        assert_eq!(remaining(), [Value::Integer(1), Value::Integer(3)]);
        runtime
            .execute_query("BEGIN")
            .expect("begin committed delete");
        assert_eq!(
            runtime
                .execute_query("DELETE FROM cold_dml_probe WHERE id = 3")
                .expect("committed cold delete")
                .affected_rows,
            1
        );
        runtime.execute_query("COMMIT").expect("commit delete");
        assert_eq!(remaining(), [Value::Integer(1)]);
    }
}

#[test]
fn cold_dml_delete_late_point_failure_preserves_preimages_and_writer_horizon() {
    use reddb_engine::storage::collection_read::{CollectionRead, EncodedRecord};
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct FailingPoint {
        source: Arc<dyn CollectionRead + Send + Sync>,
        reads: AtomicUsize,
        fail_at: AtomicUsize,
    }
    impl CollectionRead for FailingPoint {
        fn get_record(
            &self,
            id: EntityId,
            limit: usize,
        ) -> Result<Option<EncodedRecord>, crate::storage::StoreError> {
            let read = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
            if read == self.fail_at.load(Ordering::SeqCst) {
                return Err(crate::storage::StoreError::Io(std::io::Error::other(
                    "injected delete point failure",
                )));
            }
            self.source.get_record(id, limit)
        }
        fn cursor(&self) -> Result<Box<dyn CollectionCursor + '_>, crate::storage::StoreError> {
            self.source.cursor()
        }
        fn owned_cursor(&self) -> Result<Box<dyn CollectionCursor>, crate::storage::StoreError> {
            self.source.owned_cursor()
        }
    }
    for failure_at in 1..=9 {
        let (_directory, runtime) = cold_dml_fixture(None);
        let expected = observe(&runtime, "SELECT id, score FROM cold_dml_probe ORDER BY id");
        let ids =
            super::dml_target_scan::DmlTargetScan::new(&runtime, "cold_dml_probe", None, None)
                .find_target_ids()
                .expect("delete discovery");
        let store = runtime.db().store();
        store.persist().expect("publish delete failure source");
        let pager = Arc::clone(store.pager().expect("pager"));
        let published = PagedCollection::new(Arc::new(BTree::with_root(
            Arc::clone(&pager),
            store.collection_root_page("cold_dml_probe").expect("root"),
        )));
        let data = Arc::new(BTree::new(pager));
        let mut cursor = published.scan().expect("source cursor");
        while let Some(record) = cursor.next_record(usize::MAX).expect("source record") {
            data.insert(&record.id.raw().to_be_bytes(), &record.bytes)
                .expect("private base");
        }
        let root = data.root_page_id();
        let source = Arc::new(FailingPoint {
            source: Arc::new(PagedCollection::new(data)),
            reads: AtomicUsize::new(0),
            fail_at: AtomicUsize::new(failure_at),
        });
        let manager = store
            .get_collection("cold_dml_probe")
            .expect("delete source");
        manager.install_backing(
            source.clone(),
            root,
            store.format_version(),
            4096,
            manager.count() as u64,
        );
        let topology = runtime
            .index_store_ref()
            .collection_topology_lock("cold_dml_probe");
        let error = runtime
            .delete_entities_batch("cold_dml_probe", &ids, topology.read(), None, false)
            .err()
            .expect("later delete read must fail");
        assert!(
            error.to_string().contains("injected delete point failure"),
            "{error}"
        );
        assert_eq!(runtime.snapshot_manager().oldest_active_xid(), None);
        assert_eq!(runtime.snapshot_manager().oldest_required_xid(), None);
        source.fail_at.store(0, Ordering::SeqCst);
        assert_eq!(
            observe(&runtime, "SELECT id, score FROM cold_dml_probe ORDER BY id"),
            expected
        );
        assert_eq!(
            runtime
                .delete_entities_batch("cold_dml_probe", &ids, topology.read(), None, false)
                .expect("retry delete")
                .affected,
            3
        );
        assert!(observe(&runtime, "SELECT id FROM cold_dml_probe").is_empty());
    }
}

#[test]
fn cold_dml_deferred_delete_abort_preserves_indexes_and_releases_admission() {
    for index in [None, Some("HASH"), Some("BTREE")] {
        let (directory, runtime) = cold_dml_fixture(index);
        runtime
            .db()
            .store()
            .persist()
            .expect("publish captured delete");
        install_indexed_cold(&runtime, "cold_dml_probe", 4096);
        let visible = || observe(&runtime, "SELECT id FROM cold_dml_probe WHERE id = 2");
        let expected = visible();
        let scope = super::delete_publication::DeletePublicationScope::begin();
        UnifiedStore::begin_deferred_store_wal_capture();
        assert_eq!(
            runtime
                .execute_query("DELETE FROM cold_dml_probe WHERE id = 2")
                .expect("prepare captured delete")
                .affected_rows,
            1
        );
        assert!(runtime.snapshot_manager().oldest_active_xid().is_some());
        {
            let reservations = runtime.inner.memory_reservations.lock();
            assert!(
                reservations.reserved_bytes > reservations.completed_bytes,
                "the rollback owner must stay charged beyond the DML callback"
            );
        }
        assert_eq!(
            visible(),
            expected,
            "unpublished delete must remain visible to a new reader"
        );
        let captured = UnifiedStore::take_deferred_store_wal_capture();
        assert!(!captured.is_empty());
        // Exercise the same scope destruction as an outer WAL append error.
        drop(captured);
        drop(scope);
        assert_eq!(runtime.snapshot_manager().oldest_active_xid(), None);
        assert_eq!(runtime.snapshot_manager().oldest_required_xid(), None);
        runtime.invalidate_result_cache();
        assert_eq!(
            visible(),
            expected,
            "aborted capture must preserve indexed reads"
        );
        {
            let reservations = runtime.inner.memory_reservations.lock();
            assert_eq!(reservations.reserved_bytes, reservations.completed_bytes);
        }
        for cycle in 0..100 {
            runtime
                .db()
                .store()
                .test_fail_deferred_wal_append
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let failed = runtime
                .with_deferred_store_wal_for_dml(true, || {
                    runtime.execute_query("DELETE FROM cold_dml_probe WHERE id = 2")
                })
                .err()
                .expect("captured WAL publication must fail");
            assert!(
                failed
                    .to_string()
                    .contains("injected deferred WAL append failure"),
                "cycle {cycle}: {failed}"
            );
            assert!(!UnifiedStore::deferred_store_wal_capture_active());
            assert_eq!(runtime.snapshot_manager().oldest_active_xid(), None);
            assert_eq!(runtime.snapshot_manager().oldest_required_xid(), None);
            runtime.invalidate_result_cache();
            assert_eq!(
                visible(),
                expected,
                "WAL failure changed indexed preimages at cycle {cycle}"
            );
            let reservations = runtime.inner.memory_reservations.lock();
            assert_eq!(reservations.reserved_bytes, reservations.completed_bytes);
        }
        let deleted = runtime
            .with_deferred_store_wal_for_dml(true, || {
                let result = runtime.execute_query("DELETE FROM cold_dml_probe WHERE id = 2")?;
                assert!(runtime.snapshot_manager().oldest_active_xid().is_some());
                assert_eq!(
                    visible(),
                    expected,
                    "retry stays invisible until its actual WAL append"
                );
                Ok(result)
            })
            .expect("durable captured retry");
        assert_eq!(deleted.affected_rows, 1);
        assert_eq!(runtime.snapshot_manager().oldest_active_xid(), None);
        assert!(visible().is_empty());
        runtime.checkpoint().expect("publish cold delete");
        drop(runtime);
        let options = RedDBOptions::persistent(directory.path().join("models.rdb"))
            .with_memory_budget(128 * 1024 * 1024)
            .with_storage_profile(
                crate::storage::profile::StorageDeployPreset::PrimaryReplicaDev.selection(),
            )
            .expect("reopen profile");
        let reopened = RedDBRuntime::with_options(options).expect("reopen deleted rows");
        assert!(observe(&reopened, "SELECT id FROM cold_dml_probe WHERE id = 2").is_empty());
        let remaining = observe(&reopened, "SELECT id FROM cold_dml_probe ORDER BY id");
        assert_eq!(remaining.len(), 2);
        assert_eq!(remaining[0].get("id"), Some(&Value::Integer(1)));
        assert_eq!(remaining[1].get("id"), Some(&Value::Integer(3)));
    }
}

#[test]
fn cold_dml_deferred_delete_unwind_clears_capture_and_allows_retry() {
    let (_directory, runtime) = cold_dml_fixture(Some("HASH"));
    runtime
        .db()
        .store()
        .persist()
        .expect("publish unwind source");
    install_indexed_cold(&runtime, "cold_dml_probe", 4096);
    let expected = observe(&runtime, "SELECT id FROM cold_dml_probe ORDER BY id");
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _: RedDBResult<()> = runtime.with_deferred_store_wal_for_dml(true, || {
            assert_eq!(
                runtime
                    .execute_query("DELETE FROM cold_dml_probe WHERE id = 2")?
                    .affected_rows,
                1
            );
            panic!("injected unwind before captured delete WAL publication");
        });
    }));
    assert!(failure.is_err());
    assert!(!UnifiedStore::deferred_store_wal_capture_active());
    assert_eq!(runtime.snapshot_manager().oldest_active_xid(), None);
    assert_eq!(runtime.snapshot_manager().oldest_required_xid(), None);
    runtime.invalidate_result_cache();
    assert_eq!(
        observe(&runtime, "SELECT id FROM cold_dml_probe ORDER BY id"),
        expected
    );
    let retried = runtime
        .with_deferred_store_wal_for_dml(true, || {
            runtime.execute_query("DELETE FROM cold_dml_probe WHERE id = 2")
        })
        .expect("retry after unwinding capture");
    assert_eq!(retried.affected_rows, 1);
    assert_eq!(
        observe(&runtime, "SELECT id FROM cold_dml_probe ORDER BY id").len(),
        2
    );
}

#[test]
fn cold_dml_candidate_reads_include_base_rows_without_hydration() {
    use reddb_rql::ast::{CompareOp, FieldRef, Filter};
    for index in [None, Some("HASH"), Some("BTREE")] {
        let (_directory, runtime) = cold_dml_fixture(index);
        let first = observe(&runtime, "SELECT rid FROM cold_dml_probe WHERE id = 1")[0]
            .get("rid")
            .cloned()
            .expect("logical identity");
        let filters = [
            None,
            Some(Filter::Compare {
                field: FieldRef::TableColumn {
                    table: String::new(),
                    column: "id".into(),
                },
                op: CompareOp::Eq,
                value: Value::Integer(2),
            }),
            Some(Filter::Compare {
                field: FieldRef::TableColumn {
                    table: String::new(),
                    column: "rid".into(),
                },
                op: CompareOp::Eq,
                value: first,
            }),
        ];
        let targets = |filter: Option<&Filter>| {
            super::dml_target_scan::DmlTargetScan::new(&runtime, "cold_dml_probe", filter, None)
                .find_target_ids()
                .expect("physical DML targets")
                .into_iter()
                .map(|id| id.raw())
                .collect::<std::collections::BTreeSet<_>>()
        };
        let expected: Vec<_> = filters
            .iter()
            .map(|filter| targets(filter.as_ref()))
            .collect();
        assert_eq!(expected[0].len(), 3);
        assert_eq!(expected[1].len(), 1);
        assert_eq!(expected[2].len(), 1);
        runtime.db().store().persist().expect("publish DML source");
        install_indexed_cold(&runtime, "cold_dml_probe", 4096);
        for (filter, expected) in filters.iter().zip(expected) {
            assert_eq!(
                targets(filter.as_ref()),
                expected,
                "cold target scan: {index:?}"
            );
        }
        let manager = runtime
            .db()
            .store()
            .get_collection("cold_dml_probe")
            .expect("source");
        let mut resident = 0;
        manager.for_each_entity(|_| {
            resident += 1;
            true
        });
        assert_eq!(resident, 0, "target reads must not hydrate the base");
    }
}

#[test]
fn unreadable_cold_dml_source_returns_an_error_and_preserves_rows() {
    for index in [None, Some("HASH"), Some("BTREE")] {
        for query in [
            "UPDATE cold_dml_probe SET score = score + 1",
            "UPDATE cold_dml_probe SET score = score + 1 WHERE id = 2",
            "DELETE FROM cold_dml_probe",
            "DELETE FROM cold_dml_probe WHERE id = 2",
        ] {
            let (_directory, runtime) = cold_dml_fixture(index);
            let expected = observe(&runtime, "SELECT id, score FROM cold_dml_probe ORDER BY id");
            runtime
                .db()
                .store()
                .persist()
                .expect("publish unreadable source");
            install_indexed_cold(&runtime, "cold_dml_probe", 1);
            runtime.invalidate_result_cache();
            assert!(
                runtime.execute_query(query).is_err(),
                "unreadable source returned success: {index:?} {query}"
            );
            install_indexed_cold(&runtime, "cold_dml_probe", 4096);
            assert_eq!(
                observe(&runtime, "SELECT id, score FROM cold_dml_probe ORDER BY id"),
                expected
            );
        }
    }
}

#[test]
fn cold_dml_updates_read_base_preimages_and_return_exact_postimages() {
    for index in [None, Some("HASH"), Some("BTREE")] {
        let (_directory, runtime) = cold_dml_fixture(index);
        runtime
            .db()
            .store()
            .persist()
            .expect("publish update source");
        install_indexed_cold(&runtime, "cold_dml_probe", 4096);
        let result = runtime
            .execute_query(
                "UPDATE cold_dml_probe SET score = score + 2 WHERE id = 2 RETURNING id, score",
            )
            .expect("cold UPDATE");
        assert_eq!(
            result.affected_rows, 1,
            "cold preimage was missed: {index:?}"
        );
        assert_eq!(result.result.records[0].get("id"), Some(&Value::Integer(2)));
        assert_eq!(
            result.result.records[0].get("score"),
            Some(&Value::Integer(22))
        );
        let rows = observe(&runtime, "SELECT id, score FROM cold_dml_probe ORDER BY id");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].get("score"), Some(&Value::Integer(10)));
        assert_eq!(rows[1].get("score"), Some(&Value::Integer(22)));
        assert_eq!(rows[2].get("score"), Some(&Value::Integer(30)));
        let ordered = runtime.execute_query("UPDATE cold_dml_probe SET score = score + 3 ORDER BY id DESC LIMIT 1 RETURNING id, score").expect("ordered cold UPDATE");
        assert_eq!(ordered.affected_rows, 1);
        assert_eq!(
            ordered.result.records[0].get("id"),
            Some(&Value::Integer(3))
        );
        assert_eq!(
            ordered.result.records[0].get("score"),
            Some(&Value::Integer(33))
        );
        let repeated = runtime
            .execute_query(
                "UPDATE cold_dml_probe SET score = score + 1 WHERE id = 2 RETURNING score",
            )
            .expect("read the latest dirty version over a cold base");
        assert_eq!(repeated.affected_rows, 1);
        assert_eq!(
            repeated.result.records[0].get("score"),
            Some(&Value::Integer(23))
        );
    }
}

#[test]
fn cold_dml_updates_preserve_transaction_and_savepoint_visibility() {
    let (_directory, runtime) = cold_dml_fixture(None);
    runtime
        .db()
        .store()
        .persist()
        .expect("publish transactional source");
    install_indexed_cold(&runtime, "cold_dml_probe", 4096);
    let score = || {
        observe(&runtime, "SELECT score FROM cold_dml_probe WHERE id = 2")[0]
            .get("score")
            .cloned()
            .expect("visible score")
    };
    runtime.execute_query("BEGIN").expect("begin cold writer");
    assert_eq!(
        runtime
            .execute_query("UPDATE cold_dml_probe SET score = score + 1 WHERE id = 2")
            .expect("parent write")
            .affected_rows,
        1
    );
    assert_eq!(score(), Value::Integer(21));
    runtime
        .execute_query("SAVEPOINT cold_child")
        .expect("savepoint");
    assert_eq!(
        runtime
            .execute_query("UPDATE cold_dml_probe SET score = score + 2 WHERE id = 2")
            .expect("child write")
            .affected_rows,
        1
    );
    assert_eq!(score(), Value::Integer(23));
    runtime
        .execute_query("ROLLBACK TO SAVEPOINT cold_child")
        .expect("rollback child");
    assert_eq!(score(), Value::Integer(21));
    runtime.execute_query("ROLLBACK").expect("rollback parent");
    assert_eq!(score(), Value::Integer(20));
    runtime
        .execute_query("BEGIN")
        .expect("begin constant update");
    assert_eq!(
        runtime
            .execute_query("UPDATE cold_dml_probe SET score = 25 WHERE id = 2")
            .expect("constant cold preimage")
            .affected_rows,
        1
    );
    assert_eq!(score(), Value::Integer(25));
    runtime
        .execute_query("ROLLBACK")
        .expect("rollback constant update");
    assert_eq!(score(), Value::Integer(20));
    assert_eq!(
        runtime
            .execute_query("UPDATE cold_dml_probe SET score = score + 2 WHERE id = 2")
            .expect("retry outside transaction")
            .affected_rows,
        1
    );
    assert_eq!(score(), Value::Integer(22));
}

#[test]
fn cold_dml_late_io_failure_discards_prepared_updates_and_remains_retryable() {
    use reddb_engine::storage::collection_read::{CollectionRead, EncodedRecord};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FailingCursor {
        source: Arc<dyn CollectionRead + Send + Sync>,
        opened: AtomicUsize,
        fail_at: AtomicUsize,
    }
    impl CollectionRead for FailingCursor {
        fn get_record(
            &self,
            id: EntityId,
            limit: usize,
        ) -> Result<Option<EncodedRecord>, crate::storage::StoreError> {
            self.source.get_record(id, limit)
        }
        fn cursor(&self) -> Result<Box<dyn CollectionCursor + '_>, crate::storage::StoreError> {
            let opened = self.opened.fetch_add(1, Ordering::SeqCst) + 1;
            if opened == self.fail_at.load(Ordering::SeqCst) {
                return Err(crate::storage::StoreError::Io(std::io::Error::other(
                    "injected late DML cursor failure",
                )));
            }
            self.source.cursor()
        }
        fn owned_cursor(&self) -> Result<Box<dyn CollectionCursor>, crate::storage::StoreError> {
            self.source.owned_cursor()
        }
    }
    let (_directory, runtime) = cold_dml_fixture(None);
    let expected = observe(&runtime, "SELECT id, score FROM cold_dml_probe ORDER BY id");
    let store = runtime.db().store();
    store.persist().expect("publish I/O failure source");
    let pager = Arc::clone(store.pager().expect("pager"));
    let published = PagedCollection::new(Arc::new(BTree::with_root(
        Arc::clone(&pager),
        store
            .collection_root_page("cold_dml_probe")
            .expect("published root"),
    )));
    let data = Arc::new(BTree::new(pager));
    let mut cursor = published.scan().expect("source cursor");
    while let Some(record) = cursor.next_record(usize::MAX).expect("published row") {
        data.insert(&record.id.raw().to_be_bytes(), &record.bytes)
            .expect("immutable private base");
    }
    let root = data.root_page_id();
    let source = Arc::new(FailingCursor {
        source: Arc::new(PagedCollection::new(data)),
        opened: AtomicUsize::new(0),
        fail_at: AtomicUsize::new(3),
    });
    let manager = store
        .get_collection("cold_dml_probe")
        .expect("source collection");
    manager.install_backing(
        source.clone(),
        root,
        store.format_version(),
        4096,
        manager.count() as u64,
    );
    let query = "UPDATE cold_dml_probe SET score = score + 1";
    let snapshots = runtime.snapshot_manager();
    assert_eq!(
        snapshots.oldest_required_xid(),
        None,
        "fixture has no retained reader"
    );
    for cycle in 0..100 {
        runtime.invalidate_result_cache();
        let opened = source.opened.load(Ordering::SeqCst);
        source.fail_at.store(opened + 3, Ordering::SeqCst);
        let error = runtime
            .execute_query(query)
            .err()
            .expect("a later preimage read must fail");
        assert!(
            error
                .to_string()
                .contains("injected late DML cursor failure"),
            "cycle {cycle}: {error}"
        );
        assert_eq!(source.opened.load(Ordering::SeqCst), opened + 3);
        assert_eq!(
            snapshots.oldest_active_xid(),
            None,
            "failed planning retained a writer at cycle {cycle}"
        );
        assert_eq!(
            snapshots.oldest_required_xid(),
            None,
            "failed planning pinned history at cycle {cycle}"
        );
        let mut resident = 0;
        manager.for_each_entity(|_| {
            resident += 1;
            true
        });
        assert_eq!(
            resident, 0,
            "failed planning retained decoded rows at cycle {cycle}"
        );
    }
    source.fail_at.store(0, Ordering::SeqCst);
    assert_eq!(
        observe(&runtime, "SELECT id, score FROM cold_dml_probe ORDER BY id"),
        expected,
        "failed planning published an earlier prepared update"
    );
    assert_eq!(
        runtime
            .execute_query(query)
            .expect("retry after I/O recovery")
            .affected_rows,
        3
    );
    let rows = observe(&runtime, "SELECT score FROM cold_dml_probe ORDER BY id");
    for (row, expected) in rows.iter().zip([11, 21, 31]) {
        assert_eq!(row.get("score"), Some(&Value::Integer(expected)));
    }
}

#[test]
fn cold_dml_delete_row_lock_table_releases_distinct_completed_keys() {
    let table = super::RmwLockTable::new();
    for id in 0..10_000 {
        let key = format!("row:{id}");
        let first = table.lock_for("lock_retention", &key);
        let second = table.lock_for("lock_retention", &key);
        assert!(
            Arc::ptr_eq(&first, &second),
            "overlapping owners share a lock"
        );
        {
            let _guard = first.lock();
        }
        drop(first);
        let third = table.lock_for("lock_retention", &key);
        assert!(
            Arc::ptr_eq(&second, &third),
            "one owner cannot remove another owner's lock"
        );
        drop(second);
        drop(third);
    }
    let retained: usize = table.shards.iter().map(|shard| shard.lock().len()).sum();
    assert_eq!(
        retained, 0,
        "completed row identities must not accumulate forever"
    );
}

#[test]
fn cold_dml_delete_row_lock_cleanup_preserves_mutual_exclusion() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let table = super::RmwLockTable::new();
    let value = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                for _ in 0..500 {
                    let lock = table.lock_for("lock_race", "row:7");
                    let _guard = lock.lock();
                    let previous = value.load(Ordering::Relaxed);
                    std::thread::yield_now();
                    value.store(previous + 1, Ordering::Relaxed);
                }
            });
        }
    });
    assert_eq!(
        value.load(Ordering::Relaxed),
        2000,
        "live owners must never split the same logical lock"
    );
    assert_eq!(
        table
            .shards
            .iter()
            .map(|shard| shard.lock().len())
            .sum::<usize>(),
        0
    );
}

#[test]
fn cold_dml_delete_serializes_same_row_until_captured_wal_commits() {
    use std::sync::mpsc;
    use std::time::Duration;
    const TIMEOUT: Duration = Duration::from_secs(10);
    for index in [None, Some("HASH"), Some("BTREE")] {
        for first_fails in [false, true] {
            let (_directory, runtime) = cold_dml_fixture(index);
            runtime
                .db()
                .store()
                .persist()
                .expect("publish concurrent delete");
            install_indexed_cold(&runtime, "cold_dml_probe", 4096);
            let runtime = Arc::new(runtime);
            let (first_ready_tx, first_ready_rx) = mpsc::channel();
            let (release_first_tx, release_first_rx) = mpsc::channel();
            let first_runtime = Arc::clone(&runtime);
            let first = std::thread::spawn(move || {
                first_runtime.with_deferred_store_wal_for_dml(true, || {
                    let deleted =
                        first_runtime.execute_query("DELETE FROM cold_dml_probe WHERE id = 2")?;
                    first_ready_tx
                        .send(deleted.affected_rows)
                        .expect("first ready");
                    release_first_rx
                        .recv_timeout(TIMEOUT)
                        .expect("release first WAL");
                    Ok(deleted)
                })
            });
            assert_eq!(
                first_ready_rx
                    .recv_timeout(TIMEOUT)
                    .expect("first prepared"),
                1
            );
            let (second_attempt_tx, second_attempt_rx) = mpsc::channel();
            let (second_ready_tx, second_ready_rx) = mpsc::channel();
            let (release_second_tx, release_second_rx) = mpsc::channel();
            let second_runtime = Arc::clone(&runtime);
            let second = std::thread::spawn(move || {
                second_runtime.with_deferred_store_wal_for_dml(true, || {
                    second_attempt_tx.send(()).expect("second attempt");
                    let deleted =
                        second_runtime.execute_query("DELETE FROM cold_dml_probe WHERE id = 2")?;
                    second_ready_tx
                        .send(deleted.affected_rows)
                        .expect("second ready");
                    release_second_rx
                        .recv_timeout(TIMEOUT)
                        .expect("release second WAL");
                    Ok(deleted)
                })
            });
            second_attempt_rx
                .recv_timeout(TIMEOUT)
                .expect("second writer started");
            let early = second_ready_rx
                .recv_timeout(Duration::from_millis(500))
                .ok();
            // Queue a DDL writer behind the second writer's topology reader.
            // A predecessor waiting behind DDL for its final index maintenance
            // would otherwise form a topology -> row -> topology wait cycle.
            let topology = runtime
                .index_store_ref()
                .collection_topology_lock("cold_dml_probe");
            let (ddl_started_tx, ddl_started_rx) = mpsc::channel();
            let (ddl_done_tx, ddl_done_rx) = mpsc::channel();
            let ddl = std::thread::spawn(move || {
                ddl_started_tx.send(()).expect("DDL attempt");
                let guard = topology.try_write_for(Duration::from_secs(2));
                let acquired = guard.is_some();
                drop(guard);
                ddl_done_tx.send(acquired).expect("DDL completion");
            });
            ddl_started_rx.recv_timeout(TIMEOUT).expect("DDL started");
            let early_ddl = ddl_done_rx.recv_timeout(Duration::from_millis(100)).ok();
            if first_fails {
                runtime
                    .db()
                    .store()
                    .test_fail_deferred_wal_append
                    .store(true, std::sync::atomic::Ordering::SeqCst);
            }
            release_first_tx
                .send(())
                .expect("allow first WAL publication");
            let first_result = first.join().expect("first worker");
            let second_prepared = early.unwrap_or_else(|| {
                second_ready_rx
                    .recv_timeout(TIMEOUT)
                    .expect("second proceeds after first completion")
            });
            let visible_after_first =
                observe(&runtime, "SELECT id FROM cold_dml_probe WHERE id = 2");
            release_second_tx.send(()).expect("allow second completion");
            let second_result = second
                .join()
                .expect("second worker")
                .expect("second completion");
            let ddl_acquired = early_ddl.unwrap_or_else(|| {
                ddl_done_rx
                    .recv_timeout(TIMEOUT)
                    .expect("queued DDL completes")
            });
            ddl.join().expect("DDL worker");
            assert!(
                early.is_none(),
                "second writer replaced a tombstone before the first WAL commit: {index:?}"
            );
            assert!(ddl_acquired, "queued DDL caused a topology/row wait cycle");
            let expected_second = if first_fails { 1 } else { 0 };
            if first_fails {
                assert!(first_result
                    .err()
                    .expect("first WAL failure")
                    .to_string()
                    .contains("injected deferred WAL append failure"));
                assert_eq!(
                    visible_after_first.len(),
                    1,
                    "failed predecessor must leave its preimage readable while retry is pending"
                );
            } else {
                assert_eq!(first_result.expect("first durable commit").affected_rows, 1);
                assert!(
                    visible_after_first.is_empty(),
                    "acknowledged deletion reappeared while another writer was pending"
                );
            }
            assert_eq!(
                second_prepared, expected_second,
                "waited delete must recheck the latest committed row"
            );
            assert_eq!(second_result.affected_rows, expected_second);
            assert!(observe(&runtime, "SELECT id FROM cold_dml_probe WHERE id = 2").is_empty());
            assert_eq!(runtime.snapshot_manager().oldest_active_xid(), None);
        }
    }
}

#[test]
fn cold_dml_delete_captured_wal_holds_checkpoint_publication_fence() {
    let (_directory, runtime) = cold_dml_fixture(None);
    runtime
        .db()
        .store()
        .persist()
        .expect("publish fence fixture");
    install_indexed_cold(&runtime, "cold_dml_probe", 4096);
    runtime
        .db()
        .store()
        .test_require_deferred_publication_fence
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let deleted = runtime
        .with_deferred_store_wal_for_dml(true, || {
            runtime.execute_query("DELETE FROM cold_dml_probe WHERE id = 2")
        })
        .expect("captured DELETE must hold checkpoint fence at its actual WAL append");
    assert_eq!(deleted.affected_rows, 1);
}

#[test]
fn cold_dml_delete_events_abort_with_the_source() {
    for paged in [false, true] {
        for bounded in [false, true] {
            let directory = tempfile::tempdir().expect("event fixture directory");
            let options = || {
                let options = RedDBOptions::persistent(directory.path().join("models.rdb"))
                    .with_memory_budget(128 * 1024 * 1024);
                if paged {
                    options
                        .with_storage_profile(
                            crate::storage::profile::StorageDeployPreset::PrimaryReplicaDev
                                .selection(),
                        )
                        .expect("event profile")
                } else {
                    options
                }
            };
            let runtime = RedDBRuntime::with_options(options()).expect("event runtime");
            if bounded {
                runtime
                    .execute_query("CREATE QUEUE delete_events_outbox MAX_SIZE 1")
                    .expect("bounded event outbox");
            }
            runtime.execute_query("CREATE TABLE delete_events_probe (id INT) WITH EVENTS (DELETE) TO delete_events_outbox").expect("event source");
            runtime
                .execute_query("INSERT INTO delete_events_probe (id) VALUES (1), (2), (3)")
                .expect("event source row");
            runtime.checkpoint().expect("publish event fixture");
            if paged {
                install_indexed_cold(&runtime, "delete_events_probe", 4096);
            }
            let original = observe(&runtime, "SELECT id FROM delete_events_probe");
            for cycle in 0..100 {
                runtime
                    .db()
                    .store()
                    .test_fail_deferred_wal_append
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                let error = runtime
                    .execute_query("DELETE FROM delete_events_probe")
                    .expect_err("real event WAL append must fail");
                assert!(error
                    .to_string()
                    .contains("injected deferred WAL append failure"));
                runtime.invalidate_result_cache();
                assert_eq!(
                    observe(&runtime, "SELECT id FROM delete_events_probe"),
                    original
                );
                assert_eq!(
                    observe(&runtime, "QUEUE LEN delete_events_outbox")[0].get("len"),
                    Some(&Value::UnsignedInteger(0)),
                    "failed DELETE must not emit a false queued event at cycle {cycle}"
                );
                if bounded {
                    assert_eq!(
                        observe(&runtime, "QUEUE LEN delete_events_outbox_outbox_dlq")[0]
                            .get("len"),
                        Some(&Value::UnsignedInteger(0)),
                        "aborted overflow event must stay invisible at cycle {cycle}"
                    );
                }
                if cycle % 25 == 24 {
                    runtime
                        .checkpoint()
                        .expect("checkpoint failed event attempts");
                }
            }
            assert_eq!(
                runtime
                    .execute_query("DELETE FROM delete_events_probe")
                    .expect("retry real DELETE")
                    .affected_rows,
                3
            );
            assert!(observe(&runtime, "SELECT id FROM delete_events_probe").is_empty());
            let expected_messages = if bounded { 1 } else { 3 };
            assert_eq!(
                observe(&runtime, "QUEUE LEN delete_events_outbox")[0].get("len"),
                Some(&Value::UnsignedInteger(expected_messages))
            );
            if bounded {
                assert_eq!(
                    observe(&runtime, "QUEUE LEN delete_events_outbox_outbox_dlq")[0].get("len"),
                    Some(&Value::UnsignedInteger(2))
                );
            }

            let queue = runtime
                .db()
                .store()
                .get_collection("delete_events_outbox")
                .expect("outbox");
            let positions: std::collections::HashSet<_> = queue
                .query_all(|entity| !runtime.snapshot_manager().is_aborted(entity.xmin))
                .into_iter()
                .filter_map(|entity| match entity.kind {
                    EntityKind::QueueMessage { position, .. } => Some(position),
                    _ => None,
                })
                .collect();
            assert_eq!(
                positions.len(),
                usize::try_from(expected_messages).expect("small event count"),
                "pending events must retain unique queue positions"
            );
            runtime
                .checkpoint()
                .expect("checkpoint committed DELETE and outbox");
            drop(runtime);
            let reopened =
                RedDBRuntime::with_options(options()).expect("reopen real event rollback fixture");
            assert!(observe(&reopened, "SELECT id FROM delete_events_probe").is_empty());
            assert_eq!(
                observe(&reopened, "QUEUE LEN delete_events_outbox")[0].get("len"),
                Some(&Value::UnsignedInteger(expected_messages))
            );
            if bounded {
                assert_eq!(
                    observe(&reopened, "QUEUE LEN delete_events_outbox_outbox_dlq")[0].get("len"),
                    Some(&Value::UnsignedInteger(2))
                );
            }
        }
    }
}

#[test]
fn cold_dml_delete_conflicting_explicit_update_preserves_serializability() {
    use crate::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
    for index in [None, Some("HASH"), Some("BTREE")] {
        for update_commits_first in [false, true] {
            let (_directory, runtime) = cold_dml_fixture(index);
            runtime
                .db()
                .store()
                .persist()
                .expect("publish explicit writer fixture");
            install_indexed_cold(&runtime, "cold_dml_probe", 4096);
            let concurrent = runtime.clone();
            let deletion = runtime.with_deferred_store_wal_for_dml(true, || {
                let deleted = runtime.execute_query("DELETE FROM cold_dml_probe WHERE id = 2")?;
                assert_eq!(deleted.affected_rows, 1);
                let updater = std::thread::spawn(move || {
                    set_current_connection_id(873);
                    concurrent.execute_query("BEGIN").expect("concurrent BEGIN");
                    assert_eq!(
                        concurrent
                            .execute_query("UPDATE cold_dml_probe SET score = 99 WHERE id = 2")
                            .expect("concurrent constant UPDATE")
                            .affected_rows,
                        1
                    );
                    if update_commits_first {
                        concurrent
                            .execute_query("COMMIT")
                            .expect("first committed UPDATE");
                    }
                    clear_current_connection_id();
                });
                updater.join().expect("explicit UPDATE worker");
                Ok(deleted)
            });
            if update_commits_first {
                let error = deletion
                    .expect_err("DELETE must reject an UPDATE committed before its WAL append");
                assert!(
                    error.to_string().contains("serialization conflict"),
                    "{error}"
                );
                assert_eq!(
                    observe(&runtime, "SELECT score FROM cold_dml_probe WHERE id = 2")[0]
                        .get("score"),
                    Some(&Value::Integer(99))
                );
            } else {
                // A serialization failure before WAL is also a valid outcome.
                // If DELETE succeeds, no pending writer may resurrect its row.
                let acknowledged = match deletion {
                    Ok(result) => {
                        assert_eq!(result.affected_rows, 1);
                        true
                    }
                    Err(error) => {
                        assert!(
                            error.to_string().contains("serialization conflict"),
                            "{error}"
                        );
                        false
                    }
                };
                let visible_after_ack =
                    observe(&runtime, "SELECT id FROM cold_dml_probe WHERE id = 2");
                set_current_connection_id(873);
                let update_commit = runtime.execute_query("COMMIT");
                clear_current_connection_id();
                if acknowledged {
                    assert!(update_commit
                        .expect_err("later UPDATE must conflict")
                        .to_string()
                        .contains("serialization conflict"));
                    assert!(visible_after_ack.is_empty(), "acknowledged DELETE reappeared while explicit UPDATE was pending: {index:?}");
                    assert!(
                        observe(&runtime, "SELECT id FROM cold_dml_probe WHERE id = 2").is_empty()
                    );
                } else {
                    update_commit.expect("UPDATE survives rejected DELETE");
                    assert_eq!(
                        observe(&runtime, "SELECT score FROM cold_dml_probe WHERE id = 2")[0]
                            .get("score"),
                        Some(&Value::Integer(99))
                    );
                }
            }
            assert_eq!(runtime.snapshot_manager().oldest_active_xid(), None);
        }
    }
}

#[test]
fn cold_dml_delete_callback_error_does_not_publish_a_partial_statement() {
    let (_directory, runtime) = cold_dml_fixture(None);
    runtime
        .db()
        .store()
        .persist()
        .expect("publish callback fixture");
    install_indexed_cold(&runtime, "cold_dml_probe", 4096);
    let original = observe(&runtime, "SELECT id FROM cold_dml_probe ORDER BY id");
    let result: crate::api::RedDBResult<()> = runtime.with_deferred_store_wal_for_dml(true, || {
        let deleted = runtime.execute_query("DELETE FROM cold_dml_probe WHERE id = 2")?;
        assert_eq!(deleted.affected_rows, 1);
        Err(RedDBError::Query(
            "injected DELETE event callback failure".into(),
        ))
    });
    assert!(result
        .expect_err("callback fails")
        .to_string()
        .contains("injected DELETE event callback failure"));
    assert_eq!(
        observe(&runtime, "SELECT id FROM cold_dml_probe ORDER BY id"),
        original,
        "a failed captured statement must restore its preimages"
    );
    assert_eq!(runtime.snapshot_manager().oldest_active_xid(), None);
}
