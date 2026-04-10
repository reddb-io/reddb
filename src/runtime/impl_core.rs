use super::*;

impl RedDBRuntime {
    pub fn in_memory() -> RedDBResult<Self> {
        Self::with_options(RedDBOptions::in_memory())
    }

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
            }
        }

        Ok(runtime)
    }

    pub fn db(&self) -> Arc<RedDB> {
        Arc::clone(&self.inner.db)
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

    pub fn execute_query(&self, query: &str) -> RedDBResult<RuntimeQueryResult> {
        let mode = detect_mode(query);
        if matches!(mode, QueryMode::Unknown) {
            return Err(RedDBError::Query("unable to detect query mode".to_string()));
        }

        let expr = parse_multi(query).map_err(|err| RedDBError::Query(err.to_string()))?;
        let statement = query_expr_name(&expr);

        match expr {
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
                result: execute_runtime_table_query(&self.inner.db, &table)?,
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
            QueryExpr::ProbabilisticCommand(_) => Ok(RuntimeQueryResult::ok_message(
                query.to_string(),
                "probabilistic commands not yet implemented",
                "select",
            )),
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
        }
    }
}
