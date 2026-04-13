use super::*;

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
                query_cache: std::sync::RwLock::new(
                    crate::storage::query::planner::cache::PlanCache::new(1000),
                ),
                result_cache: std::sync::RwLock::new((
                    HashMap::new(),
                    std::collections::VecDeque::new(),
                )),
                ec_registry: Arc::new(crate::ec::config::EcRegistry::new()),
                ec_worker: crate::ec::worker::EcWorker::new(),
                auth_store: std::sync::RwLock::new(None),
            }),
        };

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

        Ok(runtime)
    }

    pub fn db(&self) -> Arc<RedDB> {
        Arc::clone(&self.inner.db)
    }

    /// Inject an AuthStore into the runtime. Called by server boot
    /// after the vault has been bootstrapped, so that `Value::Secret`
    /// auto-encrypt/decrypt can reach the vault AES key.
    pub fn set_auth_store(&self, store: Arc<crate::auth::store::AuthStore>) {
        if let Ok(mut slot) = self.inner.auth_store.write() {
            *slot = Some(store);
        }
    }

    /// Returns the vault AES key (`red.secret.aes_key`) if an auth
    /// store is wired and a key has been generated. Used by the
    /// `Value::Secret` encrypt/decrypt pipeline.
    pub(crate) fn secret_aes_key(&self) -> Option<[u8; 32]> {
        let guard = self.inner.auth_store.read().ok()?;
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
    pub fn cdc_emit(
        &self,
        operation: crate::replication::cdc::ChangeOperation,
        collection: &str,
        entity_id: u64,
        entity_kind: &str,
    ) {
        self.inner
            .cdc
            .emit(operation, collection, entity_id, entity_kind);
        self.invalidate_result_cache();

        // Append to WAL replication buffer (if primary mode)
        if let Some(ref primary) = self.inner.db.replication {
            let record = format!(
                "{}:{}:{}:{}",
                operation.as_str(),
                collection,
                entity_id,
                entity_kind
            );
            primary.wal_buffer.append(record.into_bytes());
        }
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
        Ok(crate::replication::scheduler::BackupResult {
            snapshot_id: snapshot.snapshot_id,
            uploaded: false, // TODO: auto-upload when backend configured
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
        let pool = self
            .inner
            .pool
            .lock()
            .expect("stats: connection pool lock poisoned");
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
        if let Ok(cache) = self.inner.result_cache.read() {
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

        // ── Plan cache: skip parse if we already have a cached plan ──
        let expr = if let Ok(mut plan_cache) = self.inner.query_cache.write() {
            if let Some(cached) = plan_cache.get(query) {
                cached.plan.optimized.clone()
            } else {
                drop(plan_cache);
                let parsed =
                    parse_multi(query).map_err(|err| RedDBError::Query(err.to_string()))?;
                // Store in plan cache for next time
                if let Ok(mut pc) = self.inner.query_cache.write() {
                    let plan = crate::storage::query::planner::QueryPlan::new(
                        parsed.clone(),
                        parsed.clone(),
                        Default::default(),
                    );
                    pc.insert(
                        query.to_string(),
                        crate::storage::query::planner::CachedPlan::new(plan),
                    );
                }
                parsed
            }
        } else {
            parse_multi(query).map_err(|err| RedDBError::Query(err.to_string()))?
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

        // Cache SELECT results for 30s (skip caching pre-serialized results to avoid large clones)
        if let Ok(ref result) = query_result {
            if result.statement_type == "select" && result.result.pre_serialized_json.is_none() {
                if let Ok(mut cache) = self.inner.result_cache.write() {
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
        }

        query_result
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
    pub fn invalidate_result_cache(&self) {
        if let Ok(mut cache) = self.inner.result_cache.write() {
            cache.0.clear();
            cache.1.clear();
        }
    }
}
