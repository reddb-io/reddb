use super::*;

#[cfg(test)]
thread_local! {
    static ALLOW_LEGACY_CONFIG_KEYS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn canonical_key(legacy: &str) -> Option<&str> {
    legacy
        .strip_prefix("red.config.")
        .or_else(|| legacy.strip_prefix("red."))
        .filter(|key| !key.is_empty())
}

pub(super) fn validate_config_key(key: &str) -> Result<(), StoreError> {
    #[cfg(test)]
    if ALLOW_LEGACY_CONFIG_KEYS.with(std::cell::Cell::get) {
        return Ok(());
    }
    if key.starts_with("red.") {
        return Err(StoreError::Internal(format!(
            "config key `{key}` is invalid inside red.config: omit the redundant `red.` prefix"
        )));
    }
    Ok(())
}

pub(super) fn validate_config_entity(
    collection: &str,
    entity: &UnifiedEntity,
) -> Result<(), StoreError> {
    if UnifiedStore::canonical_collection(collection) != "red.config" {
        return Ok(());
    }
    if let Some(Value::Text(key)) = entity.data.as_row().and_then(|row| row.get_field("key")) {
        validate_config_key(key)?;
    }
    Ok(())
}

impl UnifiedStore {
    pub(crate) fn legacy_red_config_key_count(&self) -> Result<usize, StoreError> {
        let Some(manager) = self.get_collection("red.config") else {
            return Ok(0);
        };
        let mut keys = std::collections::HashSet::new();
        manager.try_for_each_entity(usize::MAX, |entity| {
            let Some(row) = entity.data.as_row() else {
                return true;
            };
            let Some(Value::Text(key)) = row.get_field("key") else {
                return true;
            };
            if key.starts_with("red.") {
                keys.insert(key.to_string());
            }
            true
        })?;
        Ok(keys.len())
    }

    pub(crate) fn require_normalized_red_config_keys(&self) -> Result<(), StoreError> {
        let legacy = self.legacy_red_config_key_count()?;
        if legacy > 0 {
            return Err(StoreError::Internal(format!(
                "red.config contains {legacy} legacy `red.*` key(s); open the database once in writable mode to migrate them"
            )));
        }
        Ok(())
    }

    /// Move legacy `red.*` histories to their canonical names before runtime config is read.
    ///
    /// This is a boot-only migration. Any canonical history wins over every legacy spelling.
    /// When legacy spellings collide, the spelling with the newest physical row wins and its
    /// complete history is cloned. Every insert and legacy-row deletion is published in one store
    /// batch so a failed WAL publication restores the pre-migration state.
    pub(crate) fn normalize_red_config_keys(&self) -> Result<usize, StoreError> {
        let Some(manager) = self.get_collection("red.config") else {
            return Ok(0);
        };

        struct LegacySource {
            source_key: String,
            canonical_key: String,
            newest_id: EntityId,
            rows: Vec<(UnifiedEntity, Option<Metadata>)>,
        }

        let mut canonical_keys = std::collections::HashSet::new();
        let mut legacy = BTreeMap::<String, LegacySource>::new();
        let mut deletes = Vec::new();
        let mut invalid_legacy_key = None;
        manager.try_for_each_entity(usize::MAX, |entity| {
            let Some(row) = entity.data.as_row() else {
                return true;
            };
            let Some(Value::Text(key)) = row.get_field("key") else {
                return true;
            };
            if let Some(canonical) = canonical_key(key) {
                deletes.push(("red.config".to_string(), entity.id));
                let source = legacy
                    .entry(key.to_string())
                    .or_insert_with(|| LegacySource {
                        source_key: key.to_string(),
                        canonical_key: canonical.to_string(),
                        newest_id: entity.id,
                        rows: Vec::new(),
                    });
                if entity.id.raw() > source.newest_id.raw() {
                    source.newest_id = entity.id;
                }
                source
                    .rows
                    .push((entity.clone(), manager.get_metadata(entity.id)));
            } else if key.starts_with("red.") {
                invalid_legacy_key = Some(key.to_string());
            } else {
                canonical_keys.insert(key.to_string());
            }
            true
        })?;

        if let Some(key) = invalid_legacy_key {
            return Err(StoreError::Internal(format!(
                "config key `{key}` has a redundant `red.` prefix but no canonical suffix"
            )));
        }

        if legacy.is_empty() {
            return Ok(0);
        }

        let migrated = legacy.len();
        let mut winners = BTreeMap::<String, LegacySource>::new();
        for source in legacy.into_values() {
            if canonical_keys.contains(&source.canonical_key) {
                continue;
            }
            let replace = winners.get(&source.canonical_key).is_none_or(|winner| {
                source.newest_id.raw() > winner.newest_id.raw()
                    || (source.newest_id == winner.newest_id
                        && source.source_key > winner.source_key)
            });
            if replace {
                winners.insert(source.canonical_key.clone(), source);
            }
        }

        let mut inserts = Vec::new();
        for (canonical, mut source) in winners {
            source.rows.sort_by_key(|(entity, _)| entity.id.raw());
            for (mut entity, metadata) in source.rows {
                let EntityData::Row(row) = &mut entity.data else {
                    unreachable!("legacy config source was classified from a row");
                };
                let mut replaced = false;
                if let Some(named) = &mut row.named {
                    if let Some(key) = named.get_mut("key") {
                        *key = Value::text(canonical.as_str());
                        replaced = true;
                    }
                }
                if let Some(schema) = &row.schema {
                    if let Some(index) = schema.iter().position(|field| field == "key") {
                        if let Some(key) = row.columns.get_mut(index) {
                            *key = Value::text(canonical.as_str());
                            replaced = true;
                        }
                    }
                }
                debug_assert!(replaced, "classified config row retains its key field");
                entity.id = EntityId::new(0);
                inserts.push(("red.config".to_string(), entity, metadata));
            }
        }

        self.publish_rows(RowPublication {
            replacements: Vec::new(),
            inserts,
            deletes,
        })?;
        Ok(migrated)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const REPORTED_DATABASE_KEYS: &[&str] = &[
        "cache.blob.l1_bytes_max",
        "cache.blob.l2_bytes_max",
        "cache.blob.max_namespaces",
        "concurrency.locking.enabled",
        "durability.mode",
        "red.ai.default.model",
        "red.ai.default.provider",
        "red.ai.max_embedding_inputs",
        "red.ai.max_prompt_batch",
        "red.ai.timeout.connect_secs",
        "red.ai.timeout.read_secs",
        "red.ai.timeout.write_secs",
        "red.auth.enabled",
        "red.auth.require_auth",
        "red.auth.session_ttl_secs",
        "red.backup.backend",
        "red.backup.enabled",
        "red.backup.interval_secs",
        "red.backup.retention_count",
        "red.backup.upload",
        "red.cdc.buffer_size",
        "red.cdc.enabled",
        "red.config.ai.embeddings.provider",
        "red.config.ai.inference.provider",
        "red.config.ai.providers.openai.models.embeddings",
        "red.config.ai.providers.openai.models.inference",
        "red.config.ai.providers.openai.tokens.default.secret_ref",
        "red.config.secret.auto_decrypt",
        "red.config.secret.auto_encrypt",
        "red.indexes.auto_select",
        "red.indexes.bitmap.enabled",
        "red.indexes.bitmap.max_cardinality",
        "red.indexes.bloom_filter.enabled",
        "red.indexes.bloom_filter.false_positive_rate",
        "red.indexes.bloom_filter.prune_on_scan",
        "red.indexes.hash.enabled",
        "red.indexes.spatial.enabled",
        "red.probabilistic.filter_default_capacity",
        "red.probabilistic.hll_registers",
        "red.probabilistic.sketch_default_depth",
        "red.probabilistic.sketch_default_width",
        "red.query.connection_pool.max_connections",
        "red.query.connection_pool.max_idle",
        "red.query.max_recursion_depth",
        "red.queue.consumer_idle_timeout_ms",
        "red.queue.default_max_attempts",
        "red.queue.default_max_size",
        "red.queue.visibility_timeout_ms",
        "red.search.fusion.dedup_threshold",
        "red.search.fusion.graph_weight",
        "red.search.fusion.table_weight",
        "red.search.fusion.vector_weight",
        "red.search.rag.graph_depth",
        "red.search.rag.max_chunks_per_source",
        "red.search.rag.max_total_chunks",
        "red.search.rag.min_relevance",
        "red.search.rag.similarity_threshold",
        "red.server.max_body_size",
        "red.server.max_scan_limit",
        "red.server.read_timeout_ms",
        "red.server.write_timeout_ms",
        "red.storage.auto_checkpoint_pages",
        "red.storage.bm25.b",
        "red.storage.bm25.k1",
        "red.storage.hnsw.ef_construction",
        "red.storage.hnsw.ef_search",
        "red.storage.hnsw.m",
        "red.storage.ivf.n_lists",
        "red.storage.ivf.n_probes",
        "red.storage.page_cache_capacity",
        "red.storage.page_size",
        "red.storage.segment.compression_level",
        "red.storage.segment.max_bytes",
        "red.storage.segment.max_entities",
        "red.storage.snapshot_retention",
        "red.storage.verify_checksums",
        "red.system.arch",
        "red.system.available_memory_bytes",
        "red.system.cpu_cores",
        "red.system.hostname",
        "red.system.os",
        "red.system.pid",
        "red.system.started_at",
        "red.system.total_memory_bytes",
        "red.timeseries.compression.timestamps",
        "red.timeseries.compression.values",
        "red.timeseries.default_chunk_size",
        "red.timeseries.default_retention_days",
        "red.vcs.author.email",
        "red.vcs.author.name",
        "red.vcs.closure.enabled",
        "red.vcs.closure.lazy",
        "red.vcs.default_branch",
        "red.vcs.merge.default_strategy",
        "red.vcs.merge.fast_forward",
        "red.vcs.protected_branches",
        "red.wal.archive.enabled",
        "red.wal.archive.prefix",
        "red.wal.archive.retention_hours",
        "runtime.result_cache.backend",
        "runtime.result_cache.enabled",
        "storage.bgwriter.delay_ms",
        "storage.btree.lehman_yao",
        "storage.deploy.managed_backup",
        "storage.deploy.packaging",
        "storage.deploy.preset",
        "storage.deploy.profile",
        "storage.deploy.replica_count",
        "storage.deploy.wal_retention",
        "storage.wal.max_interval_ms",
        "system.bootstrap.completed",
        "system.bootstrap.first_admin_id",
        "system.bootstrap.manifest.registry_entries",
        "system.bootstrap.preset",
    ];

    fn insert_raw_config(store: &UnifiedStore, key: &str, value: Value) -> EntityId {
        let id = store.next_entity_id();
        let mut entity = UnifiedEntity::new(
            id,
            EntityKind::TableRow {
                table: Arc::from("red.config"),
                row_id: store.get_or_create_collection("red.config").next_row_id(),
            },
            EntityData::Row(RowData {
                columns: Vec::new(),
                named: Some(
                    [
                        ("key".to_string(), Value::text(key)),
                        ("value".to_string(), value),
                    ]
                    .into_iter()
                    .collect(),
                ),
                schema: None,
            }),
        );
        entity.ensure_table_logical_id();
        store
            .get_collection("red.config")
            .expect("red.config")
            .insert(entity)
            .expect("inject pre-migration config row");
        id
    }

    fn config_entity(key: &str, value: Value) -> UnifiedEntity {
        UnifiedEntity::new(
            EntityId::new(0),
            EntityKind::TableRow {
                table: Arc::from("red.config"),
                row_id: 0,
            },
            EntityData::Row(RowData {
                columns: Vec::new(),
                named: Some(
                    [
                        ("key".to_string(), Value::text(key)),
                        ("value".to_string(), value),
                    ]
                    .into_iter()
                    .collect(),
                ),
                schema: None,
            }),
        )
    }

    fn insert_persisted_legacy_config(store: &UnifiedStore, key: &str, value: Value) {
        ALLOW_LEGACY_CONFIG_KEYS.with(|allow| {
            assert!(
                !allow.replace(true),
                "legacy config test bypass cannot nest"
            );
            let result = store.insert_auto("red.config", config_entity(key, value));
            allow.set(false);
            result.expect("insert legacy config through the durable write path");
        });
    }

    fn insert_config_version(
        store: &UnifiedStore,
        key: &str,
        version: i64,
        value: &str,
        tombstone: bool,
        tag: &str,
    ) -> EntityId {
        let manager = store.get_or_create_collection("red.config");
        let id = store.next_entity_id();
        let mut entity = UnifiedEntity::new(
            id,
            EntityKind::TableRow {
                table: Arc::from("red.config"),
                row_id: manager.next_row_id(),
            },
            EntityData::Row(RowData {
                columns: Vec::new(),
                named: Some(
                    [
                        ("key".to_string(), Value::text(key)),
                        ("value".to_string(), Value::text(value)),
                        ("version".to_string(), Value::Integer(version)),
                        ("tombstone".to_string(), Value::Boolean(tombstone)),
                        ("tags".to_string(), Value::Array(vec![Value::text(tag)])),
                    ]
                    .into_iter()
                    .collect(),
                ),
                schema: None,
            }),
        );
        entity.created_at = 100 + version as u64;
        entity.updated_at = 200 + version as u64;
        entity.sequence_id = 300 + version as u64;
        entity.xmin = 400 + version as u64;
        entity.xmax = if tombstone { 500 + version as u64 } else { 0 };
        entity.ensure_table_logical_id();
        manager
            .insert(entity)
            .expect("inject pre-migration config history row");
        store
            .set_metadata(
                "red.config",
                id,
                Metadata::with_fields(HashMap::from([(
                    "source".to_string(),
                    MetadataValue::String(format!("{key}:{version}")),
                )])),
            )
            .expect("attach config history metadata");
        id
    }

    fn stored_config_keys(store: &UnifiedStore) -> Vec<String> {
        let mut keys = Vec::new();
        store
            .get_collection("red.config")
            .expect("red.config")
            .try_for_each_entity(usize::MAX, |entity| {
                if let Some(key) = entity
                    .data
                    .as_row()
                    .and_then(|row| row.get_field("key"))
                    .and_then(Value::as_text)
                {
                    keys.push(key.to_owned());
                }
                true
            })
            .expect("scan every persisted config key");
        keys.sort();
        keys
    }

    #[test]
    fn normalizes_every_legacy_red_config_shape_without_overwriting_canonical_values() {
        let store = UnifiedStore::new();
        insert_raw_config(&store, "red.ai.default.provider", Value::text("openai"));
        insert_raw_config(
            &store,
            "red.config.ai.inference.provider",
            Value::text("groq"),
        );
        insert_raw_config(&store, "red.auth.require_auth", Value::Boolean(true));
        insert_raw_config(&store, "auth.require_auth", Value::Boolean(false));
        insert_raw_config(
            &store,
            "cache.blob.l1_bytes_max",
            Value::UnsignedInteger(64),
        );

        let migrated = store
            .normalize_red_config_keys()
            .expect("normalize legacy config keys");

        assert_eq!(migrated, 3);
        assert_eq!(
            store.get_config("ai.default.provider"),
            Some(Value::text("openai"))
        );
        assert_eq!(
            store.get_config("ai.inference.provider"),
            Some(Value::text("groq"))
        );
        assert_eq!(
            store.get_config("auth.require_auth"),
            Some(Value::Boolean(false)),
            "an existing canonical value must win a migration collision"
        );
        assert_eq!(
            store.get_config("cache.blob.l1_bytes_max"),
            Some(Value::UnsignedInteger(64))
        );
        assert!(stored_config_keys(&store)
            .iter()
            .all(|key| !key.starts_with("red.")));
        assert_eq!(store.normalize_red_config_keys().unwrap(), 0);
    }

    #[test]
    fn preserves_the_complete_newest_legacy_history_group_and_physical_metadata() {
        let store = UnifiedStore::new();
        insert_config_version(&store, "red.ai.default.model", 1, "old-a", false, "a1");
        insert_config_version(
            &store,
            "red.config.ai.default.model",
            7,
            "losing-b",
            false,
            "b7",
        );
        insert_config_version(&store, "red.ai.default.model", 2, "new-a", false, "a2");
        insert_config_version(&store, "red.ai.default.model", 3, "deleted-a", true, "a3");

        let source_rows: Vec<(UnifiedEntity, Option<Metadata>)> = store
            .get_collection("red.config")
            .expect("red.config")
            .query_all(|entity| {
                matches!(
                    entity.data.as_row().and_then(|row| row.get_field("key")),
                    Some(Value::Text(key)) if key.as_ref() == "red.ai.default.model"
                )
            })
            .into_iter()
            .map(|entity| {
                let metadata = store.get_metadata("red.config", entity.id);
                (entity, metadata)
            })
            .collect();
        assert_eq!(source_rows.len(), 3);

        assert_eq!(store.normalize_red_config_keys().unwrap(), 2);

        let migrated_rows: Vec<UnifiedEntity> = store
            .get_collection("red.config")
            .expect("red.config")
            .query_all(|entity| {
                matches!(
                    entity.data.as_row().and_then(|row| row.get_field("key")),
                    Some(Value::Text(key)) if key.as_ref() == "ai.default.model"
                )
            });
        assert_eq!(
            migrated_rows.len(),
            3,
            "every historical row from the newest legacy source group must survive"
        );
        assert_eq!(store.legacy_red_config_key_count().unwrap(), 0);

        for (source, source_metadata) in source_rows {
            let source_row = source.data.as_row().expect("source row");
            let source_version = source_row.get_field("version").expect("source version");
            let migrated = migrated_rows
                .iter()
                .find(|entity| {
                    entity
                        .data
                        .as_row()
                        .and_then(|row| row.get_field("version"))
                        == Some(source_version)
                })
                .expect("matching migrated version");
            let migrated_row = migrated.data.as_row().expect("migrated row");
            for field in ["value", "version", "tombstone", "tags"] {
                assert_eq!(migrated_row.get_field(field), source_row.get_field(field));
            }
            assert_ne!(migrated.id, source.id);
            assert_eq!(migrated.logical_id(), source.logical_id());
            assert_eq!(migrated.created_at, source.created_at);
            assert_eq!(migrated.updated_at, source.updated_at);
            assert_eq!(migrated.xmin, source.xmin);
            assert_eq!(migrated.xmax, source.xmax);
            assert_eq!(
                store
                    .get_metadata("red.config", migrated.id)
                    .map(|metadata| metadata.fields),
                source_metadata.map(|metadata| metadata.fields),
            );
        }
    }

    #[test]
    fn failed_publication_restores_the_complete_legacy_state() {
        let store = UnifiedStore::new();
        let first = insert_config_version(&store, "red.auth.require_auth", 1, "false", false, "v1");
        let second = insert_config_version(&store, "red.auth.require_auth", 2, "true", false, "v2");
        let before = stored_config_keys(&store);
        let first_metadata = store.get_metadata("red.config", first);
        let second_metadata = store.get_metadata("red.config", second);

        store
            .test_fail_deferred_wal_append
            .store(true, std::sync::atomic::Ordering::Release);
        assert!(store.normalize_red_config_keys().is_err());

        assert_eq!(stored_config_keys(&store), before);
        assert_eq!(store.legacy_red_config_key_count().unwrap(), 1);
        assert!(store
            .get_collection("red.config")
            .expect("red.config")
            .query_all(|entity| {
                matches!(
                    entity.data.as_row().and_then(|row| row.get_field("key")),
                    Some(Value::Text(key)) if key.as_ref() == "auth.require_auth"
                )
            })
            .is_empty());
        assert_eq!(
            store
                .get_metadata("red.config", first)
                .map(|metadata| metadata.fields),
            first_metadata.map(|metadata| metadata.fields)
        );
        assert_eq!(
            store
                .get_metadata("red.config", second)
                .map(|metadata| metadata.fields),
            second_metadata.map(|metadata| metadata.fields)
        );

        assert_eq!(store.normalize_red_config_keys().unwrap(), 1);
        assert_eq!(store.legacy_red_config_key_count().unwrap(), 0);
    }

    #[test]
    fn normalizes_the_complete_reported_database_inventory_without_loss() {
        let store = UnifiedStore::new();
        for key in REPORTED_DATABASE_KEYS {
            insert_raw_config(&store, key, Value::text(*key));
        }

        let expected_migrations = REPORTED_DATABASE_KEYS
            .iter()
            .filter(|key| canonical_key(key).is_some())
            .count();
        assert_eq!(
            store.normalize_red_config_keys().unwrap(),
            expected_migrations
        );

        let stored = stored_config_keys(&store);
        assert_eq!(stored.len(), REPORTED_DATABASE_KEYS.len());
        assert!(stored.iter().all(|key| !key.starts_with("red.")));
        for legacy in REPORTED_DATABASE_KEYS {
            let canonical = canonical_key(legacy).unwrap_or(legacy);
            assert_eq!(
                store.get_config(canonical),
                Some(Value::text(*legacy)),
                "value lost while migrating {legacy} to {canonical}"
            );
        }
    }

    #[test]
    fn rejects_new_redundant_red_prefixes() {
        let store = UnifiedStore::new();
        store
            .try_set_config_tree("red", &crate::json!(true))
            .expect("the exact user key `red` has no redundant dotted prefix");
        let error = store
            .try_set_config_tree("red.auth.require_auth", &crate::json!(true))
            .expect_err("legacy write must be rejected");
        assert!(error
            .to_string()
            .contains("omit the redundant `red.` prefix"));
        assert!(store.try_get_config("red.auth.require_auth").is_err());
        assert_eq!(store.get_config("red"), Some(Value::Boolean(true)));
    }

    #[test]
    fn config_tree_rejects_every_invalid_flattened_key_before_mutation() {
        let store = UnifiedStore::new();
        store.get_or_create_collection("red.config");
        let tree: crate::serde_json::Value =
            crate::serde_json::from_str(r#"{"allowed":true,"red":{"auth":{"enabled":true}}}"#)
                .expect("nested config fixture");
        let error = store
            .try_set_config_tree("", &tree)
            .expect_err("a nested legacy root must reject the complete config tree");

        assert!(error
            .to_string()
            .contains("omit the redundant `red.` prefix"));
        assert_eq!(
            store.get_config("allowed"),
            None,
            "all flattened keys must be validated before the first write"
        );
        assert!(stored_config_keys(&store).is_empty());
    }

    #[test]
    fn every_store_insert_surface_rejects_redundant_red_prefixes() {
        let store = UnifiedStore::new();
        store.get_or_create_collection("red.config");
        let legacy = || {
            UnifiedEntity::new(
                EntityId::new(0),
                EntityKind::TableRow {
                    table: Arc::from("red.config"),
                    row_id: 0,
                },
                EntityData::Row(RowData {
                    columns: Vec::new(),
                    named: Some(
                        [
                            ("key".to_string(), Value::text("red.auth.enabled")),
                            ("value".to_string(), Value::Boolean(true)),
                        ]
                        .into_iter()
                        .collect(),
                    ),
                    schema: None,
                }),
            )
        };

        for result in [
            store.insert_auto("red.config", legacy()).map(|_| ()),
            store.insert("red.config", legacy()).map(|_| ()),
            store.bulk_insert("red.config", vec![legacy()]).map(|_| ()),
        ] {
            let error = result.expect_err("legacy config insert must be rejected");
            assert!(error
                .to_string()
                .contains("omit the redundant `red.` prefix"));
        }
        assert!(stored_config_keys(&store).is_empty());
    }

    #[test]
    fn prepared_row_publication_rejects_every_invalid_config_row_before_mutation() {
        let store = UnifiedStore::new();
        store.get_or_create_collection("red.config");

        let error = store
            .prepare_and_publish_rows(|_, _| {
                Ok((
                    RowPublication {
                        replacements: Vec::new(),
                        inserts: vec![
                            (
                                "publication.must.not.exist".to_string(),
                                UnifiedEntity::table_row(
                                    EntityId::new(0),
                                    "publication.must.not.exist",
                                    0,
                                    vec![Value::Boolean(true)],
                                ),
                                None,
                            ),
                            (
                                "red.config".to_string(),
                                config_entity("red.auth.enabled", Value::Boolean(true)),
                                None,
                            ),
                        ],
                        deletes: Vec::new(),
                    },
                    (),
                ))
            })
            .expect_err("prepared publication must reject a legacy config key");

        assert!(error
            .to_string()
            .contains("omit the redundant `red.` prefix"));
        assert!(
            store.get_collection("publication.must.not.exist").is_none(),
            "validation must finish before an earlier row in the publication mutates the store"
        );
        assert!(stored_config_keys(&store).is_empty());
    }

    #[test]
    fn versioned_update_rejects_an_invalid_config_key_before_mutation() {
        let store = UnifiedStore::new();
        let original_id = store
            .insert_auto(
                "red.config",
                config_entity("auth.enabled", Value::Boolean(false)),
            )
            .expect("insert canonical config row");
        let original = store
            .get("red.config", original_id)
            .expect("canonical config row");
        let mut old_version = original.clone();
        old_version.set_xmax(77);
        let mut new_version = config_entity("red.auth.enabled", Value::Boolean(true));
        new_version.id = store.next_entity_id();
        new_version.set_logical_id(original.logical_id());
        new_version.set_xmin(77);

        let error = store
            .install_versioned_table_row_update("red.config", old_version, new_version, None)
            .expect_err("versioned update must reject a legacy config key");

        assert!(error
            .to_string()
            .contains("omit the redundant `red.` prefix"));
        assert_eq!(
            store
                .get("red.config", original_id)
                .expect("original row remains")
                .xmax,
            0,
            "validation must happen before the old version is retired"
        );
        assert_eq!(stored_config_keys(&store), vec!["auth.enabled".to_string()]);
    }

    #[test]
    fn refuses_an_empty_legacy_suffix_instead_of_booting_partially_migrated() {
        let store = UnifiedStore::new();
        insert_raw_config(&store, "red.", Value::Boolean(true));

        assert_eq!(store.legacy_red_config_key_count().unwrap(), 1);
        let error = store
            .normalize_red_config_keys()
            .expect_err("an empty canonical suffix cannot be migrated safely");
        assert!(error.to_string().contains("no canonical suffix"));
        assert_eq!(stored_config_keys(&store), vec!["red.".to_string()]);
    }

    #[test]
    fn normalization_survives_a_persistent_reopen() {
        let directory = tempfile::tempdir().expect("config migration fixture");
        let path = directory.path().join("legacy.rdb");
        {
            let store = UnifiedStore::open(&path).expect("open legacy store");
            insert_raw_config(
                &store,
                "red.config.ai.providers.openai.models.inference",
                Value::text("gpt-5"),
            );
            insert_raw_config(&store, "red.cdc.enabled", Value::Boolean(false));
            store.persist().expect("persist legacy keys");
            assert_eq!(store.legacy_red_config_key_count().unwrap(), 2);
            assert_eq!(store.normalize_red_config_keys().unwrap(), 2);
            store.persist().expect("persist canonical keys");
        }

        let reopened = UnifiedStore::open(&path).expect("reopen normalized store");
        assert_eq!(reopened.legacy_red_config_key_count().unwrap(), 0);
        assert_eq!(
            reopened.get_config("ai.providers.openai.models.inference"),
            Some(Value::text("gpt-5"))
        );
        assert_eq!(
            reopened.get_config("cdc.enabled"),
            Some(Value::Boolean(false))
        );
    }

    #[test]
    fn writable_store_open_normalizes_legacy_keys_before_returning() {
        let directory = tempfile::tempdir().expect("writable config migration fixture");
        let path = directory.path().join("legacy-writable-open.rdb");
        {
            let store = UnifiedStore::open(&path).expect("create legacy store");
            insert_raw_config(&store, "red.auth.require_auth", Value::Boolean(true));
            store.persist().expect("persist legacy writable fixture");
        }

        let reopened = UnifiedStore::open(&path).expect("writable open migrates config keys");
        assert_eq!(reopened.legacy_red_config_key_count().unwrap(), 0);
        assert_eq!(
            reopened.get_config("auth.require_auth"),
            Some(Value::Boolean(true))
        );
    }

    #[test]
    fn read_only_store_open_refuses_legacy_keys_without_mutating_the_database() {
        let directory = tempfile::tempdir().expect("read-only config migration fixture");
        let path = directory.path().join("legacy-read-only-open.rdb");
        {
            let store = UnifiedStore::open(&path).expect("create legacy store");
            insert_raw_config(&store, "red.auth.require_auth", Value::Boolean(true));
            store.persist().expect("persist legacy read-only fixture");
        }
        let before = std::fs::read(&path).expect("read legacy source before read-only open");

        let error = match UnifiedStore::open_read_only_with_config(
            &path,
            crate::storage::unified::UnifiedStoreConfig::default(),
        ) {
            Ok(_) => panic!("read-only open must refuse an unapplied config-key migration"),
            Err(error) => error,
        };

        assert!(error
            .to_string()
            .contains("open the database once in writable mode"));
        assert_eq!(
            std::fs::read(&path).expect("read legacy source after rejected read-only open"),
            before
        );
    }

    #[test]
    fn runtime_boot_normalizes_legacy_keys_before_config_consumers_run() {
        let directory = tempfile::tempdir().expect("runtime config migration fixture");
        let path = directory.path().join("legacy-runtime.rdb");
        {
            let store = UnifiedStore::open(&path).expect("open legacy store");
            insert_raw_config(&store, "red.auth.require_auth", Value::Boolean(false));
            insert_raw_config(
                &store,
                "red.config.ai.inference.provider",
                Value::text("openai"),
            );
            store.persist().expect("persist legacy runtime fixture");
        }

        {
            let runtime = crate::RedDBRuntime::with_options(crate::RedDBOptions::persistent(&path))
                .expect("runtime boots and migrates legacy config");
            let store = runtime.db().store();
            assert_eq!(store.legacy_red_config_key_count().unwrap(), 0);
            assert_eq!(
                store.get_config("auth.require_auth"),
                Some(Value::Boolean(false))
            );
            assert_eq!(
                store.get_config("ai.inference.provider"),
                Some(Value::text("openai"))
            );
            assert!(store.get_config("red.auth.require_auth").is_none());
        }

        let reopened = UnifiedStore::open(&path).expect("reopen runtime-migrated store");
        assert_eq!(reopened.legacy_red_config_key_count().unwrap(), 0);
        assert_eq!(
            reopened.get_config("auth.require_auth"),
            Some(Value::Boolean(false))
        );
    }

    #[test]
    fn bounded_runtime_boot_normalization_survives_a_second_cold_reopen() {
        let directory = tempfile::tempdir().expect("bounded config migration fixture");
        let path = directory.path().join("legacy-bounded-runtime.rdb");
        {
            let mut config = UnifiedStoreConfig::default();
            config.resident_memory_bytes_max = Some(64 * 1024 * 1024);
            let store =
                UnifiedStore::open_with_config(&path, config).expect("open legacy bounded store");
            for key in REPORTED_DATABASE_KEYS {
                insert_persisted_legacy_config(&store, key, Value::text(*key));
            }
            assert_eq!(
                stored_config_keys(&store).len(),
                REPORTED_DATABASE_KEYS.len()
            );
            store.persist().expect("persist legacy bounded fixture");
            assert_eq!(
                stored_config_keys(&store).len(),
                REPORTED_DATABASE_KEYS.len()
            );
        }

        let options = || crate::RedDBOptions::persistent(&path).with_bounded_disk_residency(true);
        let expected_provider = {
            let runtime = crate::RedDBRuntime::with_options(options())
                .expect("bounded runtime boots and migrates every legacy config key");
            let store = runtime.db().store();
            assert_eq!(store.legacy_red_config_key_count().unwrap(), 0);
            assert!(stored_config_keys(&store).len() >= REPORTED_DATABASE_KEYS.len());
            for legacy in REPORTED_DATABASE_KEYS {
                let canonical = canonical_key(legacy).unwrap_or(legacy);
                assert!(
                    store.get_config(canonical).is_some(),
                    "first bounded boot lost {legacy} as {canonical}"
                );
            }
            store
                .get_config("ai.inference.provider")
                .expect("migrated provider remains configured")
        };

        for reopen in 1..=2 {
            let runtime = crate::RedDBRuntime::with_options(options())
                .unwrap_or_else(|error| panic!("bounded cold reopen {reopen}: {error}"));
            let store = runtime.db().store();
            assert_eq!(store.legacy_red_config_key_count().unwrap(), 0);
            assert!(
                stored_config_keys(&store).len() >= REPORTED_DATABASE_KEYS.len(),
                "cold reopen {reopen} lost migrated config rows"
            );
            for legacy in REPORTED_DATABASE_KEYS {
                let canonical = canonical_key(legacy).unwrap_or(legacy);
                assert!(
                    store.get_config(canonical).is_some(),
                    "cold reopen {reopen} lost {legacy} as {canonical}"
                );
            }
            assert_eq!(
                store.get_config("ai.inference.provider"),
                Some(expected_provider.clone()),
                "cold reopen {reopen} changed the migrated provider"
            );
        }
    }

    #[test]
    fn read_only_runtime_refuses_legacy_keys_without_mutating_the_database() {
        let directory = tempfile::tempdir().expect("read-only config migration fixture");
        let path = directory.path().join("legacy-read-only.rdb");
        {
            let store = UnifiedStore::open(&path).expect("open legacy store");
            insert_raw_config(&store, "red.auth.require_auth", Value::Boolean(true));
            store.persist().expect("persist legacy read-only fixture");
        }
        let before = std::fs::read(&path).expect("read legacy runtime source before open");

        let error = match crate::RedDBRuntime::with_options(
            crate::RedDBOptions::persistent(&path).with_read_only(true),
        ) {
            Ok(_) => panic!("read-only runtime must not boot with an unapplied migration"),
            Err(error) => error,
        };
        assert!(error
            .to_string()
            .contains("open the database once in writable mode"));

        assert_eq!(
            std::fs::read(&path).expect("read legacy runtime source after rejected open"),
            before
        );
    }
}
