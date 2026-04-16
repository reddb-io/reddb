use super::*;
use crate::application::entity::metadata_to_json;
use crate::replication::cdc::ChangeRecord;
use crate::replication::logical::{ApplyMode, LogicalChangeApplier};

fn runtime_pool_lock(runtime: &RedDBRuntime) -> std::sync::MutexGuard<'_, PoolState> {
    runtime
        .inner
        .pool
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl RedDBRuntime {
    pub fn in_memory() -> RedDBResult<Self> {
        Self::with_options(RedDBOptions::in_memory())
    }

    #[inline(never)]
    pub fn with_options(options: RedDBOptions) -> RedDBResult<Self> {
        Self::with_pool(options, ConnectionPoolConfig::default())
    }

    pub fn with_pool(
        options: RedDBOptions,
        pool_config: ConnectionPoolConfig,
    ) -> RedDBResult<Self> {
        let db = Arc::new(
            RedDB::open_with_options(&options)
                .map_err(|err| RedDBError::Internal(err.to_string()))?,
        );

        let runtime = Self {
            inner: Arc::new(RuntimeInner {
                db,
                layout: PhysicalLayout::from_options(&options),
                indices: IndexCatalog::register_default_vector_graph(
                    options.has_capability(crate::api::Capability::Table),
                    options.has_capability(crate::api::Capability::Graph),
                ),
                pool_config,
                pool: Mutex::new(PoolState::default()),
                started_at_unix_ms: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis(),
                probabilistic: super::probabilistic_store::ProbabilisticStore::new(),
                index_store: super::index_store::IndexStore::new(),
                cdc: crate::replication::cdc::CdcBuffer::new(100_000),
                backup_scheduler: crate::replication::scheduler::BackupScheduler::new(3600),
                query_cache: parking_lot::RwLock::new(
                    crate::storage::query::planner::cache::PlanCache::new(1000),
                ),
                result_cache: parking_lot::RwLock::new((
                    HashMap::new(),
                    std::collections::VecDeque::new(),
                )),
                planner_dirty_tables: parking_lot::RwLock::new(HashSet::new()),
                ec_registry: Arc::new(crate::ec::config::EcRegistry::new()),
                ec_worker: crate::ec::worker::EcWorker::new(),
                auth_store: parking_lot::RwLock::new(None),
                commit_lock: Mutex::new(()),
            }),
        };

        let restored_cdc_lsn = runtime
            .inner
            .db
            .replication
            .as_ref()
            .map(|repl| {
                repl.logical_wal_spool
                    .as_ref()
                    .map(|spool| spool.current_lsn())
                    .unwrap_or(0)
            })
            .unwrap_or(0)
            .max(runtime.config_u64("red.config.timeline.last_archived_lsn", 0));
        runtime.inner.cdc.set_current_lsn(restored_cdc_lsn);
        if let Some(repl) = &runtime.inner.db.replication {
            repl.wal_buffer.set_current_lsn(restored_cdc_lsn);
        }

        // Save system info to red_config on boot
        {
            let sys = SystemInfo::collect();
            runtime.inner.db.store().set_config_tree(
                "red.system",
                &crate::serde_json::json!({
                    "pid": sys.pid,
                    "cpu_cores": sys.cpu_cores,
                    "total_memory_bytes": sys.total_memory_bytes,
                    "available_memory_bytes": sys.available_memory_bytes,
                    "os": sys.os,
                    "arch": sys.arch,
                    "hostname": sys.hostname,
                    "started_at": SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64
                }),
            );

            // Seed defaults on first boot (only if red_config is empty or missing defaults)
            let store = runtime.inner.db.store();
            if store
                .get_collection("red_config")
                .map(|m| m.query_all(|_| true).len())
                .unwrap_or(0)
                <= 10
            {
                store.set_config_tree("red.ai", &crate::json!({
                    "default": crate::json!({
                        "provider": "openai",
                        "model": crate::ai::DEFAULT_OPENAI_PROMPT_MODEL
                    }),
                    "max_embedding_inputs": 256,
                    "max_prompt_batch": 256,
                    "timeout": crate::json!({ "connect_secs": 10, "read_secs": 90, "write_secs": 30 })
                }));
                store.set_config_tree(
                    "red.server",
                    &crate::json!({
                        "max_scan_limit": 1000,
                        "max_body_size": 1048576,
                        "read_timeout_ms": 5000,
                        "write_timeout_ms": 5000
                    }),
                );
                store.set_config_tree(
                    "red.storage",
                    &crate::json!({
                        "page_size": 4096,
                        "page_cache_capacity": 100000,
                        "auto_checkpoint_pages": 1000,
                        "snapshot_retention": 16,
                        "verify_checksums": true,
                        "segment": crate::json!({
                            "max_entities": 100000,
                            "max_bytes": 268435456_u64,
                            "compression_level": 6
                        }),
                        "hnsw": crate::json!({ "m": 16, "ef_construction": 100, "ef_search": 50 }),
                        "ivf": crate::json!({ "n_lists": 100, "n_probes": 10 }),
                        "bm25": crate::json!({ "k1": 1.2, "b": 0.75 })
                    }),
                );
                store.set_config_tree(
                    "red.search",
                    &crate::json!({
                        "rag": crate::json!({
                            "max_chunks_per_source": 10,
                            "max_total_chunks": 25,
                            "similarity_threshold": 0.8,
                            "graph_depth": 2,
                            "min_relevance": 0.3
                        }),
                        "fusion": crate::json!({
                            "vector_weight": 0.5,
                            "graph_weight": 0.3,
                            "table_weight": 0.2,
                            "dedup_threshold": 0.85
                        })
                    }),
                );
                store.set_config_tree(
                    "red.auth",
                    &crate::json!({
                        "enabled": false,
                        "session_ttl_secs": 3600,
                        "require_auth": false
                    }),
                );
                store.set_config_tree(
                    "red.query",
                    &crate::json!({
                        "connection_pool": crate::json!({ "max_connections": 64, "max_idle": 16 }),
                        "max_recursion_depth": 1000
                    }),
                );
                store.set_config_tree(
                    "red.indexes",
                    &crate::json!({
                        "auto_select": true,
                        "bloom_filter": crate::json!({
                            "enabled": true,
                            "false_positive_rate": 0.01,
                            "prune_on_scan": true
                        }),
                        "hash": crate::json!({ "enabled": true }),
                        "bitmap": crate::json!({ "enabled": true, "max_cardinality": 1000 }),
                        "spatial": crate::json!({ "enabled": true })
                    }),
                );
                store.set_config_tree(
                    "red.memtable",
                    &crate::json!({
                        "enabled": true,
                        "max_bytes": 67108864_u64,
                        "flush_threshold": 0.75
                    }),
                );
                store.set_config_tree(
                    "red.probabilistic",
                    &crate::json!({
                        "hll_registers": 16384,
                        "sketch_default_width": 1000,
                        "sketch_default_depth": 5,
                        "filter_default_capacity": 100000
                    }),
                );
                store.set_config_tree(
                    "red.timeseries",
                    &crate::json!({
                        "default_chunk_size": 1024,
                        "compression": crate::json!({
                            "timestamps": "delta_of_delta",
                            "values": "gorilla_xor"
                        }),
                        "default_retention_days": 0
                    }),
                );
                store.set_config_tree(
                    "red.queue",
                    &crate::json!({
                        "default_max_size": 0,
                        "default_max_attempts": 3,
                        "visibility_timeout_ms": 30000,
                        "consumer_idle_timeout_ms": 60000
                    }),
                );
                store.set_config_tree(
                    "red.backup",
                    &crate::json!({
                        "enabled": false,
                        "interval_secs": 3600,
                        "retention_count": 24,
                        "upload": false,
                        "backend": "local"
                    }),
                );
                store.set_config_tree(
                    "red.wal",
                    &crate::json!({
                        "archive": crate::json!({
                            "enabled": false,
                            "retention_hours": 168,
                            "prefix": "wal/"
                        })
                    }),
                );
                store.set_config_tree(
                    "red.cdc",
                    &crate::json!({
                        "enabled": true,
                        "buffer_size": 100000
                    }),
                );
                store.set_config_tree(
                    "red.config.secret",
                    &crate::json!({
                        "auto_encrypt": true,
                        "auto_decrypt": true
                    }),
                );
            }
        }

        // Start background maintenance thread (context index refresh +
        // session purge). Held by a WEAK reference to `RuntimeInner`
        // so dropping the last `RedDBRuntime` handle actually releases
        // the underlying Arc<Pager> (and its file lock). Polling at
        // 200ms means shutdown latency is bounded; the real 60-second
        // work cadence is tracked independently via a `last_work`
        // timestamp.
        //
        // The previous version captured `rt = runtime.clone()` by
        // strong reference and ran an unterminated `loop`, which held
        // Arc<RuntimeInner> forever — reopening a persistent database
        // in the same process failed with "Database is locked" because
        // the pager could never drop. See the regression test
        // `finding_1_select_after_bulk_insert_persistent_reopen`.
        {
            let weak = Arc::downgrade(&runtime.inner);
            std::thread::Builder::new()
                .name("reddb-maintenance".into())
                .spawn(move || {
                    let tick = std::time::Duration::from_millis(200);
                    let work_interval = std::time::Duration::from_secs(60);
                    let mut last_work = std::time::Instant::now();
                    loop {
                        std::thread::sleep(tick);
                        let Some(inner) = weak.upgrade() else {
                            // All strong references dropped — the
                            // runtime is gone, exit cleanly.
                            break;
                        };
                        if last_work.elapsed() >= work_interval {
                            let _stats = inner.db.store().context_index().stats();
                            last_work = std::time::Instant::now();
                        }
                    }
                })
                .ok();
        }

        // Start backup scheduler if enabled via red_config
        {
            let store = runtime.inner.db.store();
            let mut backup_enabled = false;
            let mut backup_interval = 3600u64;

            if let Some(manager) = store.get_collection("red_config") {
                manager.for_each_entity(|entity| {
                    if let Some(row) = entity.data.as_row() {
                        let key = row.get_field("key").and_then(|v| match v {
                            crate::storage::schema::Value::Text(s) => Some(s.as_str()),
                            _ => None,
                        });
                        let val = row.get_field("value");
                        if key == Some("red.config.backup.enabled") {
                            backup_enabled = match val {
                                Some(crate::storage::schema::Value::Boolean(true)) => true,
                                Some(crate::storage::schema::Value::Text(s)) => s == "true",
                                _ => false,
                            };
                        } else if key == Some("red.config.backup.interval_secs") {
                            if let Some(crate::storage::schema::Value::Integer(n)) = val {
                                backup_interval = *n as u64;
                            }
                        }
                    }
                    true
                });
            }

            if backup_enabled {
                runtime.inner.backup_scheduler.set_interval(backup_interval);
                let rt = runtime.clone();
                runtime
                    .inner
                    .backup_scheduler
                    .start(move || rt.trigger_backup().map_err(|e| format!("{}", e)));
            }
        }

        // Load EC registry from red_config and start worker
        {
            runtime
                .inner
                .ec_registry
                .load_from_config_store(runtime.inner.db.store().as_ref());
            if !runtime.inner.ec_registry.async_configs().is_empty() {
                runtime.inner.ec_worker.start(
                    Arc::clone(&runtime.inner.ec_registry),
                    Arc::clone(&runtime.inner.db.store()),
                );
            }
        }

        if let crate::replication::ReplicationRole::Replica { primary_addr } =
            runtime.inner.db.options().replication.role.clone()
        {
            let rt = runtime.clone();
            std::thread::Builder::new()
                .name("reddb-replica".into())
                .spawn(move || rt.run_replica_loop(primary_addr))
                .ok();
        }

        Ok(runtime)
    }

    pub fn db(&self) -> Arc<RedDB> {
        Arc::clone(&self.inner.db)
    }

    /// Execute `f` holding the runtime-wide commit lock.
    ///
    /// Used by the stdio `tx.commit` path to serialize write-set replays
    /// so concurrent transactional commits do not interleave their
    /// buffered operations. Auto-committed writes (outside any `tx.begin`
    /// session) bypass this lock entirely and keep their current
    /// throughput.
    ///
    /// The lock is held for the full closure — any I/O or long-running
    /// work inside `f` blocks other commit attempts, so callers should
    /// keep the critical section tight (just the replay loop).
    pub fn with_commit_lock<T>(&self, f: impl FnOnce() -> T) -> T {
        let _guard = self
            .inner
            .commit_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        f()
    }

    /// Direct access to the runtime's secondary-index store.
    /// Used by bulk-insert entry points (gRPC binary bulk, HTTP bulk,
    /// wire bulk) that need to push new rows through the per-index
    /// maintenance hook after `store.bulk_insert` returns.
    pub fn index_store_ref(&self) -> &super::index_store::IndexStore {
        &self.inner.index_store
    }

    /// Inject an AuthStore into the runtime. Called by server boot
    /// after the vault has been bootstrapped, so that `Value::Secret`
    /// auto-encrypt/decrypt can reach the vault AES key.
    pub fn set_auth_store(&self, store: Arc<crate::auth::store::AuthStore>) {
        *self.inner.auth_store.write() = Some(store);
    }

    /// Returns the vault AES key (`red.secret.aes_key`) if an auth
    /// store is wired and a key has been generated. Used by the
    /// `Value::Secret` encrypt/decrypt pipeline.
    pub(crate) fn secret_aes_key(&self) -> Option<[u8; 32]> {
        let guard = self.inner.auth_store.read();
        guard.as_ref().and_then(|s| s.vault_secret_key())
    }

    /// Resolve a boolean flag from `red_config`. Defaults to `default`
    /// when the key is missing or not coercible. If the same key has
    /// been written multiple times (SET CONFIG appends new rows), the
    /// most recent entity wins.
    pub(crate) fn config_bool(&self, key: &str, default: bool) -> bool {
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection("red_config") else {
            return default;
        };
        let mut result = default;
        let mut latest_id: u64 = 0;
        manager.for_each_entity(|entity| {
            if let Some(row) = entity.data.as_row() {
                let entry_key = row.get_field("key").and_then(|v| match v {
                    crate::storage::schema::Value::Text(s) => Some(s.as_str()),
                    _ => None,
                });
                if entry_key == Some(key) {
                    let id = entity.id.raw();
                    if id >= latest_id {
                        latest_id = id;
                        result = match row.get_field("value") {
                            Some(crate::storage::schema::Value::Boolean(b)) => *b,
                            Some(crate::storage::schema::Value::Text(s)) => {
                                matches!(s.as_str(), "true" | "TRUE" | "True" | "1")
                            }
                            Some(crate::storage::schema::Value::Integer(n)) => *n != 0,
                            _ => default,
                        };
                    }
                }
            }
            true
        });
        result
    }

    pub(crate) fn config_u64(&self, key: &str, default: u64) -> u64 {
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection("red_config") else {
            return default;
        };
        let mut result = default;
        let mut latest_id: u64 = 0;
        manager.for_each_entity(|entity| {
            if let Some(row) = entity.data.as_row() {
                let entry_key = row.get_field("key").and_then(|v| match v {
                    crate::storage::schema::Value::Text(s) => Some(s.as_str()),
                    _ => None,
                });
                if entry_key == Some(key) {
                    let id = entity.id.raw();
                    if id >= latest_id {
                        latest_id = id;
                        result = match row.get_field("value") {
                            Some(crate::storage::schema::Value::Integer(n)) => *n as u64,
                            Some(crate::storage::schema::Value::UnsignedInteger(n)) => *n,
                            Some(crate::storage::schema::Value::Text(s)) => {
                                s.parse::<u64>().unwrap_or(default)
                            }
                            _ => default,
                        };
                    }
                }
            }
            true
        });
        result
    }

    pub(crate) fn config_string(&self, key: &str, default: &str) -> String {
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection("red_config") else {
            return default.to_string();
        };
        let mut result = default.to_string();
        let mut latest_id: u64 = 0;
        manager.for_each_entity(|entity| {
            if let Some(row) = entity.data.as_row() {
                let entry_key = row.get_field("key").and_then(|v| match v {
                    crate::storage::schema::Value::Text(s) => Some(s.as_str()),
                    _ => None,
                });
                if entry_key == Some(key) {
                    let id = entity.id.raw();
                    if id >= latest_id {
                        latest_id = id;
                        if let Some(crate::storage::schema::Value::Text(value)) =
                            row.get_field("value")
                        {
                            result = value.clone();
                        }
                    }
                }
            }
            true
        });
        result
    }

    fn latest_metadata_for(
        &self,
        collection: &str,
        entity_id: u64,
    ) -> Option<crate::serde_json::Value> {
        self.inner
            .db
            .store()
            .get_metadata(collection, EntityId::new(entity_id))
            .map(|metadata| metadata_to_json(&metadata))
    }

    fn persist_replica_lsn(&self, lsn: u64) {
        self.inner.db.store().set_config_tree(
            "red.replication",
            &crate::json!({
                "last_applied_lsn": lsn
            }),
        );
    }

    fn persist_replication_health(
        &self,
        state: &str,
        last_error: &str,
        primary_lsn: Option<u64>,
        oldest_available_lsn: Option<u64>,
    ) {
        self.inner.db.store().set_config_tree(
            "red.replication",
            &crate::json!({
                "state": state,
                "last_error": last_error,
                "last_seen_primary_lsn": primary_lsn.unwrap_or(0),
                "last_seen_oldest_lsn": oldest_available_lsn.unwrap_or(0),
                "updated_at_unix_ms": SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64
            }),
        );
    }

    /// Whether `SECRET('...')` literals should be encrypted with the
    /// vault AES key on INSERT. Default `true`.
    pub(crate) fn secret_auto_encrypt(&self) -> bool {
        self.config_bool("red.config.secret.auto_encrypt", true)
    }

    /// Whether `Value::Secret` columns should be decrypted back to
    /// plaintext on SELECT when the vault is unsealed. Default `true`.
    /// Turning this off keeps secrets masked as `***` even while the
    /// vault is open — useful for audit trails or read-only exports.
    pub(crate) fn secret_auto_decrypt(&self) -> bool {
        self.config_bool("red.config.secret.auto_decrypt", true)
    }

    /// Walk every record in `result` and swap `Value::Secret(bytes)`
    /// for the decrypted plaintext when the runtime has the vault
    /// AES key AND `red.config.secret.auto_decrypt = true`. If the
    /// key is missing, the vault is sealed, or auto_decrypt is off,
    /// secrets are left as `Value::Secret` which every formatter
    /// (Display, JSON) already masks as `***`.
    pub(crate) fn apply_secret_decryption(&self, result: &mut RuntimeQueryResult) {
        if !self.secret_auto_decrypt() {
            return;
        }
        let Some(key) = self.secret_aes_key() else {
            return;
        };
        for record in result.result.records.iter_mut() {
            for value in record.values.values_mut() {
                if let Value::Secret(ref bytes) = value {
                    if let Some(plain) =
                        super::impl_dml::decrypt_secret_payload(&key, bytes.as_slice())
                    {
                        if let Ok(text) = String::from_utf8(plain) {
                            *value = Value::Text(text);
                        }
                    }
                }
            }
        }
    }

    /// Emit a CDC change event and replicate to WAL buffer.
    /// Create a `MutationEngine` bound to this runtime.
    ///
    /// The engine is cheap to construct (no allocation) and should be
    /// dropped after `apply` returns. Use this from application-layer
    /// `create_row` / `create_rows_batch` instead of calling
    /// `bulk_insert` + `index_entity_insert` + `cdc_emit` separately.
    pub(crate) fn mutation_engine(&self) -> crate::runtime::mutation::MutationEngine<'_> {
        crate::runtime::mutation::MutationEngine::new(self)
    }

    /// Emit a CDC record without invalidating the result cache.
    ///
    /// Used by `MutationEngine::append_batch` which calls
    /// `invalidate_result_cache` once for the whole batch before this
    /// loop, avoiding N write-lock acquisitions.
    pub(crate) fn cdc_emit_no_cache_invalidate(
        &self,
        operation: crate::replication::cdc::ChangeOperation,
        collection: &str,
        entity_id: u64,
        entity_kind: &str,
    ) {
        let lsn = self
            .inner
            .cdc
            .emit(operation, collection, entity_id, entity_kind);

        // Append to logical WAL replication buffer (if primary mode)
        if let Some(ref primary) = self.inner.db.replication {
            let store = self.inner.db.store();
            let entity = if operation == crate::replication::cdc::ChangeOperation::Delete {
                None
            } else {
                store.get(collection, EntityId::new(entity_id))
            };
            let record = ChangeRecord {
                lsn,
                timestamp: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
                operation,
                collection: collection.to_string(),
                entity_id,
                entity_kind: entity_kind.to_string(),
                entity_bytes: entity
                    .as_ref()
                    .map(|e| UnifiedStore::serialize_entity(e, store.format_version())),
                metadata: self.latest_metadata_for(collection, entity_id),
            };
            let encoded = record.encode();
            primary.wal_buffer.append(record.lsn, encoded.clone());
            if let Some(spool) = &primary.logical_wal_spool {
                let _ = spool.append(record.lsn, &encoded);
            }
        }
    }

    pub fn cdc_emit(
        &self,
        operation: crate::replication::cdc::ChangeOperation,
        collection: &str,
        entity_id: u64,
        entity_kind: &str,
    ) {
        let lsn = self
            .inner
            .cdc
            .emit(operation, collection, entity_id, entity_kind);
        self.invalidate_result_cache();

        // Append to logical WAL replication buffer (if primary mode)
        if let Some(ref primary) = self.inner.db.replication {
            let store = self.inner.db.store();
            let entity = if operation == crate::replication::cdc::ChangeOperation::Delete {
                None
            } else {
                store.get(collection, EntityId::new(entity_id))
            };
            let record = ChangeRecord {
                lsn,
                timestamp: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
                operation,
                collection: collection.to_string(),
                entity_id,
                entity_kind: entity_kind.to_string(),
                entity_bytes: entity
                    .as_ref()
                    .map(|entity| UnifiedStore::serialize_entity(entity, store.format_version())),
                metadata: self.latest_metadata_for(collection, entity_id),
            };
            let encoded = record.encode();
            primary.wal_buffer.append(record.lsn, encoded.clone());
            if let Some(spool) = &primary.logical_wal_spool {
                let _ = spool.append(record.lsn, &encoded);
            }
        }
    }

    pub(crate) fn cdc_emit_prebuilt(
        &self,
        operation: crate::replication::cdc::ChangeOperation,
        collection: &str,
        entity: &UnifiedEntity,
        entity_kind: &str,
        metadata: Option<&crate::storage::Metadata>,
        invalidate_cache: bool,
    ) {
        if invalidate_cache {
            self.invalidate_result_cache();
        }

        let lsn = self
            .inner
            .cdc
            .emit(operation, collection, entity.id.raw(), entity_kind);

        if let Some(ref primary) = self.inner.db.replication {
            let store = self.inner.db.store();
            let record = ChangeRecord {
                lsn,
                timestamp: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64,
                operation,
                collection: collection.to_string(),
                entity_id: entity.id.raw(),
                entity_kind: entity_kind.to_string(),
                entity_bytes: Some(UnifiedStore::serialize_entity(
                    entity,
                    store.format_version(),
                )),
                metadata: metadata
                    .map(metadata_to_json)
                    .or_else(|| self.latest_metadata_for(collection, entity.id.raw())),
            };
            let encoded = record.encode();
            primary.wal_buffer.append(record.lsn, encoded.clone());
            if let Some(spool) = &primary.logical_wal_spool {
                let _ = spool.append(record.lsn, &encoded);
            }
        }
    }

    pub(crate) fn cdc_emit_prebuilt_batch<'a, I>(
        &self,
        operation: crate::replication::cdc::ChangeOperation,
        entity_kind: &str,
        items: I,
        invalidate_cache: bool,
    ) where
        I: IntoIterator<
            Item = (
                &'a str,
                &'a UnifiedEntity,
                Option<&'a crate::storage::Metadata>,
            ),
        >,
    {
        let items: Vec<(&str, &UnifiedEntity, Option<&crate::storage::Metadata>)> =
            items.into_iter().collect();
        if items.is_empty() {
            return;
        }

        if invalidate_cache {
            self.invalidate_result_cache();
        }

        for (collection, entity, metadata) in items {
            self.cdc_emit_prebuilt(operation, collection, entity, entity_kind, metadata, false);
        }
    }

    fn run_replica_loop(&self, primary_addr: String) {
        let endpoint = if primary_addr.starts_with("http") {
            primary_addr
        } else {
            format!("http://{primary_addr}")
        };
        let poll_ms = self.inner.db.options().replication.poll_interval_ms;
        let max_count = self.inner.db.options().replication.max_batch_size;
        let mut since_lsn = self.config_u64("red.replication.last_applied_lsn", 0);

        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => return,
        };

        runtime.block_on(async move {
            use crate::grpc::proto::red_db_client::RedDbClient;
            use crate::grpc::proto::JsonPayloadRequest;

            let mut client = loop {
                match RedDbClient::connect(endpoint.clone()).await {
                    Ok(client) => {
                        self.persist_replication_health("connecting", "", None, None);
                        break client;
                    }
                    Err(_) => {
                        self.persist_replication_health(
                            "connecting",
                            "waiting for primary connection",
                            None,
                            None,
                        );
                        std::thread::sleep(std::time::Duration::from_millis(poll_ms.max(250)))
                    }
                }
            };

            loop {
                let payload = crate::json!({
                    "since_lsn": since_lsn,
                    "max_count": max_count
                });
                let request = tonic::Request::new(JsonPayloadRequest {
                    payload_json: crate::json::to_string(&payload)
                        .unwrap_or_else(|_| "{}".to_string()),
                });

                if let Ok(response) = client.pull_wal_records(request).await {
                    if let Ok(value) =
                        crate::json::from_str::<crate::json::Value>(&response.into_inner().payload)
                    {
                        let current_lsn =
                            value.get("current_lsn").and_then(crate::json::Value::as_u64);
                        let oldest_available_lsn = value
                            .get("oldest_available_lsn")
                            .and_then(crate::json::Value::as_u64);
                        if since_lsn > 0
                            && oldest_available_lsn
                                .map(|oldest| oldest > since_lsn.saturating_add(1))
                                .unwrap_or(false)
                        {
                            self.persist_replication_health(
                                "stalled_gap",
                                "replica is behind the oldest logical WAL available on primary; re-bootstrap required",
                                current_lsn,
                                oldest_available_lsn,
                            );
                            std::thread::sleep(std::time::Duration::from_millis(poll_ms.max(250)));
                            continue;
                        }
                        if let Some(records) =
                            value.get("records").and_then(crate::json::Value::as_array)
                        {
                            for record in records {
                                let Some(data_hex) =
                                    record.get("data").and_then(crate::json::Value::as_str)
                                else {
                                    continue;
                                };
                                let Ok(data) = hex::decode(data_hex) else {
                                    self.persist_replication_health(
                                        "apply_error",
                                        "failed to decode WAL record hex payload",
                                        current_lsn,
                                        oldest_available_lsn,
                                    );
                                    continue;
                                };
                                let Ok(change) = ChangeRecord::decode(&data) else {
                                    self.persist_replication_health(
                                        "apply_error",
                                        "failed to decode logical WAL record",
                                        current_lsn,
                                        oldest_available_lsn,
                                    );
                                    continue;
                                };
                                if LogicalChangeApplier::apply_record(
                                    self.inner.db.as_ref(),
                                    &change,
                                    ApplyMode::Replica,
                                )
                                .is_err()
                                {
                                    self.persist_replication_health(
                                        "apply_error",
                                        "failed to apply logical WAL record on replica",
                                        current_lsn,
                                        oldest_available_lsn,
                                    );
                                    continue;
                                }
                                since_lsn = since_lsn.max(change.lsn);
                                self.persist_replica_lsn(since_lsn);
                            }
                        }
                        self.persist_replication_health(
                            "healthy",
                            "",
                            current_lsn,
                            oldest_available_lsn,
                        );
                    } else {
                        self.persist_replication_health(
                            "apply_error",
                            "failed to parse pull_wal_records response",
                            None,
                            None,
                        );
                    }
                } else {
                    self.persist_replication_health(
                        "connecting",
                        "primary pull_wal_records request failed",
                        None,
                        None,
                    );
                }

                std::thread::sleep(std::time::Duration::from_millis(poll_ms));
            }
        });
    }

    /// Poll CDC events since a given LSN.
    pub fn cdc_poll(
        &self,
        since_lsn: u64,
        max_count: usize,
    ) -> Vec<crate::replication::cdc::ChangeEvent> {
        self.inner.cdc.poll(since_lsn, max_count)
    }

    /// Get backup scheduler status.
    pub fn backup_status(&self) -> crate::replication::scheduler::BackupStatus {
        self.inner.backup_scheduler.status()
    }

    /// Trigger an immediate backup.
    pub fn trigger_backup(&self) -> RedDBResult<crate::replication::scheduler::BackupResult> {
        let started = std::time::Instant::now();
        let snapshot = self.create_snapshot()?;
        let mut uploaded = false;

        if let (Some(backend), Some(path)) = (&self.inner.db.remote_backend, self.inner.db.path()) {
            let default_snapshot_prefix = self.inner.db.options().default_snapshot_prefix();
            let default_wal_prefix = self.inner.db.options().default_wal_archive_prefix();
            let default_head_key = self.inner.db.options().default_backup_head_key();
            let snapshot_prefix = self.config_string(
                "red.config.backup.snapshot_prefix",
                &default_snapshot_prefix,
            );
            let wal_prefix =
                self.config_string("red.config.wal.archive.prefix", &default_wal_prefix);
            let head_key = self.config_string("red.config.backup.head_key", &default_head_key);
            let timeline_id = self.config_string("red.config.timeline.id", "main");
            let snapshot_key = crate::storage::wal::archive_snapshot(
                backend.as_ref(),
                path,
                snapshot.snapshot_id,
                &snapshot_prefix,
            )
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
            let current_lsn = self
                .inner
                .db
                .replication
                .as_ref()
                .map(|repl| {
                    repl.logical_wal_spool
                        .as_ref()
                        .map(|spool| spool.current_lsn())
                        .unwrap_or_else(|| repl.wal_buffer.current_lsn())
                })
                .unwrap_or_else(|| self.inner.cdc.current_lsn());
            let last_archived_lsn = self.config_u64("red.config.timeline.last_archived_lsn", 0);
            let manifest = crate::storage::wal::SnapshotManifest {
                timeline_id: timeline_id.clone(),
                snapshot_key: snapshot_key.clone(),
                snapshot_id: snapshot.snapshot_id,
                snapshot_time: snapshot.created_at_unix_ms as u64,
                base_lsn: current_lsn,
                schema_version: crate::api::REDDB_FORMAT_VERSION,
                format_version: crate::api::REDDB_FORMAT_VERSION,
            };
            crate::storage::wal::publish_snapshot_manifest(backend.as_ref(), &manifest)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;

            let archived_lsn = if let Some(primary) = &self.inner.db.replication {
                let oldest = primary
                    .logical_wal_spool
                    .as_ref()
                    .and_then(|spool| spool.oldest_lsn().ok().flatten())
                    .or_else(|| primary.wal_buffer.oldest_lsn())
                    .unwrap_or(last_archived_lsn);
                if last_archived_lsn > 0 && last_archived_lsn < oldest.saturating_sub(1) {
                    return Err(RedDBError::Internal(format!(
                        "logical WAL gap detected: last_archived_lsn={last_archived_lsn}, oldest_available_lsn={oldest}"
                    )));
                }
                let records = if let Some(spool) = &primary.logical_wal_spool {
                    spool
                        .read_since(last_archived_lsn, usize::MAX)
                        .map_err(|err| RedDBError::Internal(err.to_string()))?
                } else {
                    primary.wal_buffer.read_since(last_archived_lsn, usize::MAX)
                };
                if let Some(meta) = crate::storage::wal::archive_change_records(
                    backend.as_ref(),
                    &wal_prefix,
                    &records,
                )
                .map_err(|err| RedDBError::Internal(err.to_string()))?
                {
                    if let Some(spool) = &primary.logical_wal_spool {
                        let _ = spool.prune_through(meta.lsn_end);
                    }
                    meta.lsn_end
                } else {
                    last_archived_lsn
                }
            } else {
                last_archived_lsn
            };

            let head = crate::storage::wal::BackupHead {
                timeline_id,
                snapshot_key,
                snapshot_id: snapshot.snapshot_id,
                snapshot_time: snapshot.created_at_unix_ms as u64,
                current_lsn,
                last_archived_lsn: archived_lsn,
                wal_prefix,
            };
            crate::storage::wal::publish_backup_head(backend.as_ref(), &head_key, &head)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
            self.inner.db.store().set_config_tree(
                "red.config.timeline",
                &crate::json!({
                    "last_archived_lsn": archived_lsn,
                    "id": head.timeline_id
                }),
            );
            uploaded = true;
        }

        Ok(crate::replication::scheduler::BackupResult {
            snapshot_id: snapshot.snapshot_id,
            uploaded,
            duration_ms: started.elapsed().as_millis() as u64,
            timestamp: snapshot.created_at_unix_ms as u64,
        })
    }

    pub fn acquire(&self) -> RedDBResult<RuntimeConnection> {
        let mut pool = self
            .inner
            .pool
            .lock()
            .map_err(|e| RedDBError::Internal(format!("connection pool lock poisoned: {e}")))?;
        if pool.active >= self.inner.pool_config.max_connections {
            return Err(RedDBError::Internal(
                "connection pool exhausted".to_string(),
            ));
        }

        let id = if let Some(id) = pool.idle.pop() {
            id
        } else {
            let id = pool.next_id;
            pool.next_id += 1;
            id
        };
        pool.active += 1;
        pool.total_checkouts += 1;
        drop(pool);

        Ok(RuntimeConnection {
            id,
            inner: Arc::clone(&self.inner),
        })
    }

    pub fn checkpoint(&self) -> RedDBResult<()> {
        self.inner
            .db
            .flush()
            .map_err(|err| RedDBError::Engine(err.to_string()))
    }

    pub fn run_maintenance(&self) -> RedDBResult<()> {
        self.inner
            .db
            .run_maintenance()
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    pub fn scan_collection(
        &self,
        collection: &str,
        cursor: Option<ScanCursor>,
        limit: usize,
    ) -> RedDBResult<ScanPage> {
        let store = self.inner.db.store();
        let manager = store
            .get_collection(collection)
            .ok_or_else(|| RedDBError::NotFound(collection.to_string()))?;

        let mut entities = manager.query_all(|_| true);
        entities.sort_by_key(|entity| entity.id.raw());

        let offset = cursor.map(|cursor| cursor.offset).unwrap_or(0);
        let total = entities.len();
        let end = total.min(offset.saturating_add(limit.max(1)));
        let items = if offset >= total {
            Vec::new()
        } else {
            entities[offset..end].to_vec()
        };
        let next = (end < total).then_some(ScanCursor { offset: end });

        Ok(ScanPage {
            collection: collection.to_string(),
            items,
            next,
            total,
        })
    }

    pub fn catalog(&self) -> CatalogModelSnapshot {
        self.inner.db.catalog_model_snapshot()
    }

    pub fn catalog_consistency_report(&self) -> crate::catalog::CatalogConsistencyReport {
        self.inner.db.catalog_consistency_report()
    }

    pub fn catalog_attention_summary(&self) -> CatalogAttentionSummary {
        crate::catalog::attention_summary(&self.catalog())
    }

    pub fn collection_attention(&self) -> Vec<CollectionDescriptor> {
        crate::catalog::collection_attention(&self.catalog())
    }

    pub fn index_attention(&self) -> Vec<CatalogIndexStatus> {
        crate::catalog::index_attention(&self.catalog())
    }

    pub fn graph_projection_attention(&self) -> Vec<CatalogGraphProjectionStatus> {
        crate::catalog::graph_projection_attention(&self.catalog())
    }

    pub fn analytics_job_attention(&self) -> Vec<CatalogAnalyticsJobStatus> {
        crate::catalog::analytics_job_attention(&self.catalog())
    }

    pub fn stats(&self) -> RuntimeStats {
        let pool = runtime_pool_lock(self);
        RuntimeStats {
            active_connections: pool.active,
            idle_connections: pool.idle.len(),
            total_checkouts: pool.total_checkouts,
            paged_mode: self.inner.db.is_paged(),
            started_at_unix_ms: self.inner.started_at_unix_ms,
            store: self.inner.db.stats(),
            system: SystemInfo::collect(),
        }
    }

    #[inline(never)]
    pub fn execute_query(&self, query: &str) -> RedDBResult<RuntimeQueryResult> {
        // ── TURBO: bypass SQL parse for SELECT * FROM x WHERE _entity_id = N ──
        if let Some(result) = self.try_fast_entity_lookup(query) {
            return result;
        }

        // ── Result cache: return cached result if still fresh (30s TTL) ──
        {
            let cache = self.inner.result_cache.read();
            if let Some((result, cached_at)) = cache.0.get(query) {
                if cached_at.elapsed().as_secs() < 30 {
                    return Ok(result.clone());
                }
            }
        }

        let mode = detect_mode(query);
        if matches!(mode, QueryMode::Unknown) {
            return Err(RedDBError::Query("unable to detect query mode".to_string()));
        }

        // ── Plan cache: reuse only exact-query ASTs ──
        //
        // DML statements (INSERT/UPDATE/DELETE) almost always have unique literal
        // values, so caching them burns CPU on eviction bookkeeping (Vec::remove(0)
        // shifts the entire LRU list) with zero hit rate. Skip the cache entirely
        // for write operations — parse directly.
        //
        // Only SELECT/DDL statements benefit from plan caching.
        // Detect by peeking at the first keyword of the trimmed query.
        let first_word = query
            .trim()
            .split_ascii_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        let is_write_op =
            first_word == "INSERT" || first_word == "UPDATE" || first_word == "DELETE";

        let cache_key = if is_write_op {
            String::new() // unused
        } else {
            crate::storage::query::planner::cache_key::normalize_cache_key(query)
        };

        let expr = if is_write_op {
            // Bypass plan cache for write operations — no benefit, pure overhead
            parse_multi(query).map_err(|err| RedDBError::Query(err.to_string()))?
        } else {
            // ── Hot path: read lock only (no writer serialization on cache hits) ──
            //
            // peek() is a non-mutating probe: no LRU promotion, no touch().
            // This lets concurrent readers proceed without blocking each other.
            // On hit we bind literals if needed and return immediately.
            // Only on miss do we drop to a write lock to parse + insert.
            let hit = {
                let plan_cache = self.inner.query_cache.read();
                plan_cache.peek(&cache_key).map(|cached| {
                    let parameter_count = cached.parameter_count;
                    let optimized = cached.plan.optimized.clone();
                    let exact_query = cached.exact_query.clone();
                    (parameter_count, optimized, exact_query)
                })
            };

            if let Some((parameter_count, optimized, exact_query)) = hit {
                if parameter_count > 0 {
                    // Shape hit: substitute the current literal values into the shape.
                    let shape_binds =
                        crate::storage::query::planner::cache_key::extract_literal_bindings(query)
                            .unwrap_or_default();
                    if let Some(bound) =
                        crate::storage::query::planner::shape::bind_parameterized_query(
                            &optimized,
                            &shape_binds,
                            parameter_count,
                        )
                    {
                        bound
                    } else if exact_query.as_deref() == Some(query) {
                        // Bind failed but exact query matches — use as-is.
                        optimized
                    } else {
                        // Bind failed and literals differ: re-parse fresh.
                        parse_multi(query).map_err(|err| RedDBError::Query(err.to_string()))?
                    }
                } else {
                    // No parameters means either there truly are no literals,
                    // or this statement type does not participate in shape
                    // parameterization (for example graph/queue commands).
                    // Reusing a normalized-cache hit across a different exact
                    // query can therefore leak stale literals into execution.
                    if exact_query.as_deref() == Some(query) {
                        optimized
                    } else {
                        parse_multi(query).map_err(|err| RedDBError::Query(err.to_string()))?
                    }
                }
            } else {
                // Cache miss — parse, parameterize, store.
                let parsed =
                    parse_multi(query).map_err(|err| RedDBError::Query(err.to_string()))?;
                let (cached_expr, parameter_count) = if let Some(prepared) =
                    crate::storage::query::planner::shape::parameterize_query_expr(&parsed)
                {
                    (prepared.shape, prepared.parameter_count)
                } else {
                    (parsed.clone(), 0)
                };
                {
                    let mut pc = self.inner.query_cache.write();
                    let plan = crate::storage::query::planner::QueryPlan::new(
                        parsed.clone(),
                        cached_expr,
                        Default::default(),
                    );
                    pc.insert(
                        cache_key.clone(),
                        crate::storage::query::planner::CachedPlan::new(plan)
                            .with_shape_key(cache_key.clone())
                            .with_exact_query(query.to_string())
                            .with_parameter_count(parameter_count),
                    );
                }
                parsed
            }
        };
        let statement = query_expr_name(&expr);

        let query_result = match expr {
            QueryExpr::Graph(_) | QueryExpr::Path(_) => {
                let graph = materialize_graph(self.inner.db.store().as_ref())?;
                let node_properties =
                    materialize_graph_node_properties(self.inner.db.store().as_ref())?;
                let result =
                    crate::storage::query::unified::UnifiedExecutor::execute_on_with_node_properties(
                        &graph,
                        &expr,
                        node_properties,
                    )
                        .map_err(|err| RedDBError::Query(err.to_string()))?;

                Ok(RuntimeQueryResult {
                    query: query.to_string(),
                    mode,
                    statement,
                    engine: "materialized-graph",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            QueryExpr::Table(table) => Ok(RuntimeQueryResult {
                query: query.to_string(),
                mode,
                statement,
                engine: "runtime-table",
                result: execute_runtime_table_query(
                    &self.inner.db,
                    &table,
                    Some(&self.inner.index_store),
                )?,
                affected_rows: 0,
                statement_type: "select",
            }),
            QueryExpr::Join(join) => Ok(RuntimeQueryResult {
                query: query.to_string(),
                mode,
                statement,
                engine: "runtime-join",
                result: execute_runtime_join_query(&self.inner.db, &join)?,
                affected_rows: 0,
                statement_type: "select",
            }),
            QueryExpr::Vector(vector) => Ok(RuntimeQueryResult {
                query: query.to_string(),
                mode,
                statement,
                engine: "runtime-vector",
                result: execute_runtime_vector_query(&self.inner.db, &vector)?,
                affected_rows: 0,
                statement_type: "select",
            }),
            QueryExpr::Hybrid(hybrid) => Ok(RuntimeQueryResult {
                query: query.to_string(),
                mode,
                statement,
                engine: "runtime-hybrid",
                result: execute_runtime_hybrid_query(&self.inner.db, &hybrid)?,
                affected_rows: 0,
                statement_type: "select",
            }),
            // DML execution
            QueryExpr::Insert(ref insert) => self.execute_insert(query, insert),
            QueryExpr::Update(ref update) => self.execute_update(query, update),
            QueryExpr::Delete(ref delete) => self.execute_delete(query, delete),
            // DDL execution
            QueryExpr::CreateTable(ref create) => self.execute_create_table(query, create),
            QueryExpr::DropTable(ref drop_tbl) => self.execute_drop_table(query, drop_tbl),
            QueryExpr::AlterTable(ref alter) => self.execute_alter_table(query, alter),
            QueryExpr::ExplainAlter(ref explain) => self.execute_explain_alter(query, explain),
            // Graph analytics commands
            QueryExpr::GraphCommand(ref cmd) => self.execute_graph_command(query, cmd),
            // Search commands
            QueryExpr::SearchCommand(ref cmd) => self.execute_search_command(query, cmd),
            // ASK: RAG query with LLM synthesis
            QueryExpr::Ask(ref ask) => self.execute_ask(query, ask),
            QueryExpr::CreateIndex(ref create_idx) => self.execute_create_index(query, create_idx),
            QueryExpr::DropIndex(ref drop_idx) => self.execute_drop_index(query, drop_idx),
            QueryExpr::ProbabilisticCommand(ref cmd) => {
                self.execute_probabilistic_command(query, cmd)
            }
            // Time-series DDL
            QueryExpr::CreateTimeSeries(ref ts) => self.execute_create_timeseries(query, ts),
            QueryExpr::DropTimeSeries(ref ts) => self.execute_drop_timeseries(query, ts),
            // Queue DDL and commands
            QueryExpr::CreateQueue(ref q) => self.execute_create_queue(query, q),
            QueryExpr::DropQueue(ref q) => self.execute_drop_queue(query, q),
            QueryExpr::QueueCommand(ref cmd) => self.execute_queue_command(query, cmd),
            QueryExpr::CreateTree(ref tree) => self.execute_create_tree(query, tree),
            QueryExpr::DropTree(ref tree) => self.execute_drop_tree(query, tree),
            QueryExpr::TreeCommand(ref cmd) => self.execute_tree_command(query, cmd),
            // SET CONFIG key = value
            QueryExpr::SetConfig { ref key, ref value } => {
                let store = self.inner.db.store();
                let json_val = match value {
                    Value::Text(s) => crate::serde_json::Value::String(s.clone()),
                    Value::Integer(n) => crate::serde_json::Value::Number(*n as f64),
                    Value::Float(n) => crate::serde_json::Value::Number(*n),
                    Value::Boolean(b) => crate::serde_json::Value::Bool(*b),
                    _ => crate::serde_json::Value::String(value.to_string()),
                };
                store.set_config_tree(key, &json_val);
                // Config changes can flip runtime behavior mid-session
                // (auto_decrypt, auto_encrypt, etc.) — invalidate the
                // result cache so subsequent reads re-execute against
                // the new config.
                self.invalidate_result_cache();
                Ok(RuntimeQueryResult::ok_message(
                    query.to_string(),
                    &format!("config set: {key}"),
                    "set",
                ))
            }
            // SHOW CONFIG [prefix]
            QueryExpr::ShowConfig { ref prefix } => {
                let store = self.inner.db.store();
                let all_collections = store.list_collections();
                if !all_collections.contains(&"red_config".to_string()) {
                    let result = UnifiedResult::with_columns(vec!["key".into(), "value".into()]);
                    return Ok(RuntimeQueryResult {
                        query: query.to_string(),
                        mode,
                        statement: "show_config",
                        engine: "runtime-config",
                        result,
                        affected_rows: 0,
                        statement_type: "select",
                    });
                }
                let manager = store
                    .get_collection("red_config")
                    .ok_or_else(|| RedDBError::NotFound("red_config".to_string()))?;
                let entities = manager.query_all(|_| true);
                let mut result = UnifiedResult::with_columns(vec!["key".into(), "value".into()]);
                for entity in entities {
                    if let EntityData::Row(ref row) = entity.data {
                        if let Some(ref named) = row.named {
                            let key_val = named.get("key").cloned().unwrap_or(Value::Null);
                            let val = named.get("value").cloned().unwrap_or(Value::Null);
                            let key_str = match &key_val {
                                Value::Text(s) => s.as_str(),
                                _ => continue,
                            };
                            if let Some(ref pfx) = prefix {
                                if !key_str.starts_with(pfx.as_str()) {
                                    continue;
                                }
                            }
                            let mut record = UnifiedRecord::new();
                            record.set("key", key_val);
                            record.set("value", val);
                            result.push(record);
                        }
                    }
                }
                Ok(RuntimeQueryResult {
                    query: query.to_string(),
                    mode,
                    statement: "show_config",
                    engine: "runtime-config",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
        };

        // Decrypt Value::Secret columns in-place before caching, so
        // cached results match the post-decrypt shape and repeat
        // queries skip the per-row AES-GCM pass.
        let mut query_result = query_result;
        if let Ok(ref mut result) = query_result {
            if result.statement_type == "select" {
                self.apply_secret_decryption(result);
            }
        }

        // Cache SELECT results for 30s.
        // Skip: pre-serialized JSON (large clone), and result sets > 5 rows.
        // Large multi-row results (range scans, filtered scans) are rarely
        // repeated with the same literal values so the cache hit rate is near
        // zero while the clone cost (100 records × ~16 fields each) is high.
        // Aggregations (1 row) and point lookups (1 row) still benefit.
        if let Ok(ref result) = query_result {
            if result.statement_type == "select"
                && result.result.pre_serialized_json.is_none()
                && result.result.records.len() <= 5
            {
                let mut cache = self.inner.result_cache.write();
                let (ref mut map, ref mut order) = *cache;
                if !map.contains_key(query) {
                    order.push_back(query.to_string());
                }
                map.insert(
                    query.to_string(),
                    (result.clone(), std::time::Instant::now()),
                );
                while map.len() > 1000 {
                    if let Some(oldest) = order.pop_front() {
                        map.remove(&oldest);
                    } else {
                        break;
                    }
                }
            }
        }

        query_result
    }

    /// Execute a pre-parsed `QueryExpr` directly, bypassing SQL parsing and the
    /// plan cache. Used by the prepared-statement fast path so that `execute_prepared`
    /// calls pay zero parse + cache overhead.
    ///
    /// Applies secret decryption on SELECT results, identical to `execute_query`.
    pub(crate) fn execute_query_expr(&self, expr: QueryExpr) -> RedDBResult<RuntimeQueryResult> {
        let statement = query_expr_name(&expr);
        let mode = detect_mode(statement);
        let query_str = statement;

        let result = self.dispatch_expr(expr, query_str, mode)?;
        let mut r = result;
        if r.statement_type == "select" {
            self.apply_secret_decryption(&mut r);
        }
        Ok(r)
    }

    /// Internal dispatch: route a `QueryExpr` to the appropriate executor.
    /// Shared by `execute_query` (after parse/cache) and `execute_query_expr`
    /// (direct call from prepared-statement handler).
    fn dispatch_expr(
        &self,
        expr: QueryExpr,
        query_str: &str,
        mode: QueryMode,
    ) -> RedDBResult<RuntimeQueryResult> {
        let statement = query_expr_name(&expr);
        match expr {
            QueryExpr::Graph(_) | QueryExpr::Path(_) => {
                // Graph queries are not cacheable as prepared statements.
                return Err(RedDBError::Query(
                    "graph queries cannot be used as prepared statements".to_string(),
                ));
            }
            QueryExpr::Table(table) => Ok(RuntimeQueryResult {
                query: query_str.to_string(),
                mode,
                statement,
                engine: "runtime-table",
                result: execute_runtime_table_query(
                    &self.inner.db,
                    &table,
                    Some(&self.inner.index_store),
                )?,
                affected_rows: 0,
                statement_type: "select",
            }),
            QueryExpr::Join(join) => Ok(RuntimeQueryResult {
                query: query_str.to_string(),
                mode,
                statement,
                engine: "runtime-join",
                result: execute_runtime_join_query(&self.inner.db, &join)?,
                affected_rows: 0,
                statement_type: "select",
            }),
            QueryExpr::Vector(vector) => Ok(RuntimeQueryResult {
                query: query_str.to_string(),
                mode,
                statement,
                engine: "runtime-vector",
                result: execute_runtime_vector_query(&self.inner.db, &vector)?,
                affected_rows: 0,
                statement_type: "select",
            }),
            QueryExpr::Hybrid(hybrid) => Ok(RuntimeQueryResult {
                query: query_str.to_string(),
                mode,
                statement,
                engine: "runtime-hybrid",
                result: execute_runtime_hybrid_query(&self.inner.db, &hybrid)?,
                affected_rows: 0,
                statement_type: "select",
            }),
            _ => Err(RedDBError::Query(format!(
                "prepared-statement execution does not support {statement} statements"
            ))),
        }
    }

    /// Ultra-fast path: detect `SELECT * FROM table WHERE _entity_id = N` by string pattern
    /// and execute it without SQL parsing or planning. Returns None if pattern doesn't match.
    fn try_fast_entity_lookup(&self, query: &str) -> Option<RedDBResult<RuntimeQueryResult>> {
        // Pattern: "SELECT * FROM <table> WHERE _entity_id = <id>"
        // or "SELECT * FROM <table> WHERE _entity_id =<id>"
        let q = query.trim();
        if !q.starts_with("SELECT") && !q.starts_with("select") {
            return None;
        }

        // Find "WHERE _entity_id = " or "WHERE _entity_id ="
        let where_pos = q
            .find("WHERE _entity_id")
            .or_else(|| q.find("where _entity_id"))?;
        let after_field = &q[where_pos + 16..].trim_start(); // skip "WHERE _entity_id"
        let after_eq = after_field.strip_prefix('=')?.trim_start();

        // Parse the entity ID number
        let id_str = after_eq.trim();
        let entity_id: u64 = id_str.parse().ok()?;

        // Extract table name: between "FROM " and " WHERE"
        let from_pos = q.find("FROM ").or_else(|| q.find("from "))? + 5;
        let table = q[from_pos..where_pos].trim();
        if table.is_empty()
            || table.contains(' ') && !table.contains(" AS ") && !table.contains(" as ")
        {
            return None; // complex query, fall through
        }
        let table_name = table.split_whitespace().next()?;

        // Direct entity lookup
        let store = self.inner.db.store();
        let entity = store.get(
            table_name,
            crate::storage::unified::EntityId::new(entity_id),
        );

        let json = match entity {
            Some(ref e) => execute_runtime_serialize_single_entity(e),
            None => r#"{"columns":[],"record_count":0,"selection":{"scope":"any"},"records":[]}"#
                .to_string(),
        };

        let count = if entity.is_some() { 1u64 } else { 0 };

        Some(Ok(RuntimeQueryResult {
            query: query.to_string(),
            mode: crate::storage::query::modes::QueryMode::Sql,
            statement: "select",
            engine: "fast-entity-lookup",
            result: crate::storage::query::unified::UnifiedResult {
                columns: Vec::new(),
                records: Vec::new(),
                stats: crate::storage::query::unified::QueryStats {
                    rows_scanned: count,
                    ..Default::default()
                },
                pre_serialized_json: Some(json),
            },
            affected_rows: 0,
            statement_type: "select",
        }))
    }

    /// Invalidate the result cache (call after any write operation).
    /// Full clear — use for DDL (DROP TABLE, schema changes) or when table is unknown.
    pub fn invalidate_result_cache(&self) {
        let mut cache = self.inner.result_cache.write();
        cache.0.clear();
        cache.1.clear();
    }

    /// Invalidate only result cache entries whose query string references `table`.
    /// Cheaper than a full clear: concurrent reads on other tables keep their cached results.
    pub(crate) fn invalidate_result_cache_for_table(&self, table: &str) {
        let mut cache = self.inner.result_cache.write();
        let (ref mut map, ref mut order) = *cache;
        map.retain(|key, _| !key.contains(table));
        order.retain(|key| !key.contains(table));
    }

    pub(crate) fn invalidate_plan_cache(&self) {
        self.inner.query_cache.write().clear();
    }

    pub(crate) fn clear_table_planner_stats(&self, table: &str) {
        let store = self.inner.db.store();
        crate::storage::query::planner::stats_catalog::clear_table_stats(store.as_ref(), table);
        self.invalidate_plan_cache();
    }

    pub(crate) fn refresh_table_planner_stats(&self, table: &str) {
        let store = self.inner.db.store();
        if let Some(stats) =
            crate::storage::query::planner::stats_catalog::analyze_collection(store.as_ref(), table)
        {
            crate::storage::query::planner::stats_catalog::persist_table_stats(
                store.as_ref(),
                &stats,
            );
        } else {
            crate::storage::query::planner::stats_catalog::clear_table_stats(store.as_ref(), table);
        }
        self.invalidate_plan_cache();
    }

    pub(crate) fn note_table_write(&self, table: &str) {
        self.inner
            .planner_dirty_tables
            .write()
            .insert(table.to_string());
        self.invalidate_result_cache_for_table(table);
    }
}
