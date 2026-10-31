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
