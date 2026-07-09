//! Runtime bootstrap / constructor / handle accessors.
//!
//! Extracted verbatim from `impl_core.rs` (impl_core slice 8/10, issue #1629).
//! Houses the in-memory / options constructors, the ~800-line `with_pool`
//! constructor (moved verbatim — restructuring it is future work), snapshot
//! rehydration and materialized-view backing lifecycle, system keyed-collection
//! bootstrap, and the db / index-store / schema-vocabulary / auth-store /
//! oauth / browser-token / registry handle accessors.
//!
//! - **Free helpers** — `view_records_to_entities`,
//!   `system_keyed_collection_contract`, `table_row_index_fields`.
use super::impl_config_secret::seed_storage_deploy_config;
use super::*;

/// Convert the rows produced by a materialized-view body into
/// `UnifiedEntity` table rows targeting the backing collection.
/// Issue #595 slice 9c — feeds `UnifiedStore::refresh_collection`.
///
/// Graph fragments and vector hits are ignored: a materialized view
/// is a relational result set (SELECT-shaped); slices 11+ may extend
/// this once we have a richer view body shape. Each row materialises
/// the union of its schema-bound columns + overflow.
pub(crate) fn view_records_to_entities(
    table: &str,
    records: &[crate::storage::query::unified::UnifiedRecord],
) -> Vec<crate::storage::UnifiedEntity> {
    use std::collections::HashMap;
    let table_arc: std::sync::Arc<str> = std::sync::Arc::from(table);
    let mut out = Vec::with_capacity(records.len());
    for record in records {
        let mut named: HashMap<String, crate::storage::schema::Value> = HashMap::new();
        for (name, value) in record.iter_fields() {
            named.insert(name.to_string(), value.clone());
        }
        let entity = crate::storage::UnifiedEntity::new(
            crate::storage::EntityId::new(0),
            crate::storage::EntityKind::TableRow {
                table: std::sync::Arc::clone(&table_arc),
                row_id: 0,
            },
            crate::storage::EntityData::Row(crate::storage::RowData {
                columns: Vec::new(),
                named: Some(named),
                schema: None,
            }),
        );
        out.push(entity);
    }
    out
}

fn system_keyed_collection_contract(
    name: &str,
    model: crate::catalog::CollectionModel,
) -> crate::physical::CollectionContract {
    let now = crate::utils::now_unix_millis() as u128;
    crate::physical::CollectionContract {
        name: name.to_string(),
        declared_model: model,
        schema_mode: crate::catalog::SchemaMode::Dynamic,
        origin: crate::physical::ContractOrigin::Implicit,
        version: 1,
        created_at_unix_ms: now,
        updated_at_unix_ms: now,
        default_ttl_ms: None,
        vector_dimension: None,
        vector_metric: None,
        context_index_fields: Vec::new(),
        declared_columns: Vec::new(),
        table_def: None,
        timestamps_enabled: false,
        context_index_enabled: false,
        metrics_raw_retention_ms: None,
        metrics_rollup_policies: Vec::new(),
        metrics_tenant_identity: None,
        metrics_namespace: None,
        append_only: false,
        subscriptions: Vec::new(),
        analytics_config: Vec::new(),
        session_key: None,
        session_gap_ms: None,
        retention_duration_ms: None,
        analytical_storage: None,

        ai_policy: None,
    }
}

pub(crate) fn table_row_index_fields(
    entity: &crate::storage::unified::entity::UnifiedEntity,
) -> Vec<(String, crate::storage::schema::Value)> {
    let crate::storage::EntityData::Row(row) = &entity.data else {
        return Vec::new();
    };
    if let Some(named) = &row.named {
        return named
            .iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
    }
    if let Some(schema) = &row.schema {
        return schema
            .iter()
            .zip(row.columns.iter())
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
    }
    Vec::new()
}

impl RedDBRuntime {
    pub fn in_memory() -> RedDBResult<Self> {
        Self::with_options(RedDBOptions::in_memory())
    }

    pub fn flush(&self) -> RedDBResult<()> {
        self.inner
            .db
            .flush()
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    /// Handle to the intent-lock manager for tests + introspection.
    /// Production code acquires via `LockerGuard::new(rt.lock_manager())`
    /// rather than touching the manager directly.
    pub fn lock_manager(&self) -> std::sync::Arc<crate::runtime::lock_manager::LockManager> {
        self.inner.lock_manager.clone()
    }

    /// Process-local governance registry for managed policy/config guardrails.
    pub fn config_registry(&self) -> std::sync::Arc<crate::auth::registry::ConfigRegistry> {
        self.inner.config_registry.clone()
    }

    pub fn query_audit(&self) -> std::sync::Arc<crate::runtime::query_audit::QueryAuditStream> {
        self.inner.query_audit.clone()
    }

    pub fn control_events_require_persistence(&self) -> bool {
        self.inner.control_event_config.require_persistence()
    }

    pub fn control_event_config(&self) -> crate::runtime::control_events::ControlEventConfig {
        self.inner.control_event_config
    }

    pub fn control_event_ledger(
        &self,
    ) -> Arc<dyn crate::runtime::control_events::ControlEventLedger> {
        self.inner.control_event_ledger.read().clone()
    }

    #[doc(hidden)]
    pub fn replace_control_event_ledger_for_tests(
        &self,
        ledger: Arc<dyn crate::runtime::control_events::ControlEventLedger>,
    ) {
        *self.inner.control_event_ledger.write() = ledger;
    }

    #[inline(never)]
    pub fn with_options(options: RedDBOptions) -> RedDBResult<Self> {
        Self::with_pool(options, ConnectionPoolConfig::default())
    }

    /// The memory budget resolved at boot (ADR 0073 §1). Immutable for the
    /// process lifetime; echoed by the boot log and the `red.stats` budget
    /// section.
    pub fn memory_budget(&self) -> crate::storage::memory_budget::MemoryBudget {
        self.inner.memory_budget
    }

    pub fn with_pool(
        options: RedDBOptions,
        pool_config: ConnectionPoolConfig,
    ) -> RedDBResult<Self> {
        // PLAN.md Phase 9.1 — capture wall-clock before storage
        // open so the cold-start phase markers can be backfilled
        // once Lifecycle is constructed below. Storage open
        // encapsulates auto-restore + WAL replay; we treat the
        // whole window as one combined "restore" + "wal_replay"
        // phase split at the same boundary because the storage
        // layer doesn't yet emit a finer signal.
        let boot_open_start_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        // ADR 0073 §1 — resolve the one memory budget before anything is
        // opened, so a nonsensical operator value fails the boot before the
        // process touches disk. The log line fires once per process.
        let memory_budget = crate::storage::memory_budget::resolve_for_boot(
            options.storage_profile.deploy_profile,
            options.memory_budget_bytes,
        )
        .map_err(|err| RedDBError::InvalidConfig(err.to_string()))?;
        crate::storage::memory_budget::log_resolved_once(&memory_budget);
        let embedded_single_file = options.storage_profile.deploy_profile
            == crate::storage::DeployProfile::Embedded
            && options.storage_profile.packaging == crate::storage::StoragePackaging::SingleFile;
        let db = Arc::new(
            RedDB::open_with_options(&options)
                .map_err(|err| RedDBError::Internal(err.to_string()))?,
        );
        let result_blob_cache_config = if embedded_single_file {
            crate::storage::cache::BlobCacheConfig::default()
        } else {
            crate::storage::cache::BlobCacheConfig::default().with_l2_path(
                reddb_file::layout::result_cache_l2_path(
                    &options.resolved_path(reddb_file::default_database_path()),
                ),
            )
        };
        let result_blob_cache =
            crate::storage::cache::BlobCache::open_with_l2(result_blob_cache_config).map_err(
                |err| RedDBError::Internal(format!("open result Blob Cache L2 failed: {err:?}")),
            )?;
        let storage_ready_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);

        let runtime = Self {
            inner: Arc::new(RuntimeInner {
                db: db.clone(),
                layout: PhysicalLayout::from_options(&options),
                embedded_single_file,
                memory_budget,
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
                checkpoint_projection_stats: super::CheckpointProjectionStats::default(),
                checkpoint_columnar_emission_budget_chunks: options
                    .checkpoint_columnar_emission_budget_chunks,
                columnar_projection_size_floor_rows: options.columnar_projection_size_floor_rows,
                backup_scheduler: crate::replication::scheduler::BackupScheduler::new(3600),
                query_cache: parking_lot::RwLock::new(
                    crate::storage::query::planner::cache::PlanCache::new(1000),
                ),
                result_cache: parking_lot::RwLock::new((
                    HashMap::new(),
                    std::collections::VecDeque::new(),
                )),
                result_blob_cache,
                result_blob_entries: parking_lot::RwLock::new((
                    HashMap::new(),
                    std::collections::VecDeque::new(),
                )),
                ask_answer_cache_entries: parking_lot::RwLock::new((
                    HashSet::new(),
                    std::collections::VecDeque::new(),
                )),
                result_cache_shadow_divergences: std::sync::atomic::AtomicU64::new(0),
                result_cache_hits: std::sync::atomic::AtomicU64::new(0),
                result_cache_misses: std::sync::atomic::AtomicU64::new(0),
                result_cache_evictions: std::sync::atomic::AtomicU64::new(0),
                ask_daily_spend: parking_lot::RwLock::new(HashMap::new()),
                queue_message_locks: parking_lot::RwLock::new(HashMap::new()),
                rmw_locks: RmwLockTable::new(),
                planner_dirty_tables: parking_lot::RwLock::new(HashSet::new()),
                ec_registry: Arc::new(crate::ec::config::EcRegistry::new()),
                config_registry: Arc::new(crate::auth::registry::ConfigRegistry::new()),
                ec_worker: crate::ec::worker::EcWorker::new(),
                auth_store: parking_lot::RwLock::new(None),
                oauth_validator: parking_lot::RwLock::new(None),
                browser_token_authority: parking_lot::RwLock::new(None),
                views: parking_lot::RwLock::new(HashMap::new()),
                materialized_views: parking_lot::RwLock::new(
                    crate::storage::cache::result::MaterializedViewCache::new(),
                ),
                retention_sweeper: parking_lot::RwLock::new(
                    crate::runtime::retention_sweeper::RetentionSweeperState::new(),
                ),
                snapshot_manager: Arc::new(
                    crate::storage::transaction::snapshot::SnapshotManager::new(),
                ),
                tx_contexts: parking_lot::RwLock::new(HashMap::new()),
                tx_local_tenants: parking_lot::RwLock::new(HashMap::new()),
                env_config_overrides: crate::runtime::config_overlay::collect_env_overrides(),
                lock_manager: Arc::new({
                    // Sourced from the matrix: Tier B key
                    // `concurrency.locking.deadlock_timeout_ms`
                    // (default 5000). Env var wins at boot so
                    // operators can tune without touching red_config.
                    let env = crate::runtime::config_overlay::collect_env_overrides();
                    let timeout_ms = env
                        .get("concurrency.locking.deadlock_timeout_ms")
                        .and_then(|raw| raw.parse::<u64>().ok())
                        .unwrap_or_else(|| {
                            match crate::runtime::config_matrix::default_for(
                                "concurrency.locking.deadlock_timeout_ms",
                            ) {
                                Some(crate::serde_json::Value::Number(n)) => n as u64,
                                _ => 5000,
                            }
                        });
                    let cfg = crate::runtime::lock_manager::LockConfig {
                        default_timeout: std::time::Duration::from_millis(timeout_ms),
                        ..Default::default()
                    };
                    crate::runtime::lock_manager::LockManager::new(cfg)
                }),
                rls_policies: parking_lot::RwLock::new(HashMap::new()),
                rls_enabled_tables: parking_lot::RwLock::new(HashSet::new()),
                foreign_tables: Arc::new(crate::storage::fdw::ForeignTableRegistry::with_builtins()),
                pending_tombstones: parking_lot::RwLock::new(HashMap::new()),
                pending_versioned_updates: parking_lot::RwLock::new(HashMap::new()),
                pending_queue_dedup: parking_lot::RwLock::new(HashMap::new()),
                pending_kv_watch_events: parking_lot::RwLock::new(HashMap::new()),
                pending_store_wal_actions: parking_lot::RwLock::new(HashMap::new()),
                pending_claim_locks: parking_lot::RwLock::new(HashMap::new()),
                queue_wait_registry: std::sync::Arc::new(
                    crate::runtime::queue_wait_registry::QueueWaitRegistry::new(),
                ),
                pending_queue_wakes: parking_lot::RwLock::new(HashMap::new()),
                tenant_tables: parking_lot::RwLock::new(HashMap::new()),
                ddl_epoch: std::sync::atomic::AtomicU64::new(0),
                write_gate: Arc::new(crate::runtime::write_gate::WriteGate::from_options(
                    &options,
                )),
                lifecycle: crate::runtime::lifecycle::Lifecycle::new(),
                resource_limits: crate::runtime::resource_limits::ResourceLimits::from_env(),
                audit_log: {
                    // Default audit-log path for the in-memory case
                    // sits in the system temp dir; persistent runs
                    // place it next to the resolved data file.
                    //
                    // gh-471 iter 2: route through the resolved
                    // `LogDestination`. Performance/Max tiers emit a
                    // file-backed log destination under the file-owned
                    // support-directory logs tier;
                    // lower tiers / ephemeral runs report `Stderr`
                    // and we keep the legacy file-next-to-data sink.
                    // #1375 — single-file embedded mode keeps the data
                    // directory to exactly the `.rdb` artifact, so the audit
                    // log must NOT land as a sibling. Route it to a
                    // process-unique temp location even when a data path is
                    // set; only the non-embedded case uses the data dir.
                    let data_path = if embedded_single_file {
                        std::env::temp_dir()
                            .join("reddb-embedded-runtime")
                            .join(format!("audit-{}", std::process::id()))
                    } else {
                        options
                            .data_path
                            .clone()
                            .unwrap_or_else(|| std::env::temp_dir().join("reddb"))
                    };
                    let (audit_dest, _) = crate::api::tier_wiring::current_log_destinations();
                    if !matches!(audit_dest, crate::storage::layout::LogDestination::File(_))
                        && (embedded_single_file
                            || options
                                .metadata
                                .contains_key(crate::api::EPHEMERAL_RUNTIME_METADATA_KEY))
                    {
                        // The Stderr/Syslog lower-tier sink resolves to a
                        // `for_data_path` sibling that collides across concurrent
                        // temp-dir runtimes — nextest's process-per-test model
                        // truncates one shared file, flaking audit assertions.
                        // Pin a unique sibling for these short-lived ephemeral /
                        // single-file embedded runtimes. The file-owned support-
                        // dir tier (`File`) is already per-data unique, so leave
                        // it to `for_destination` (#1375: the embedded audit then
                        // still never lands a sibling next to the `.rdb`).
                        let audit_path = reddb_file::layout::sibling_path(
                            &data_path,
                            &reddb_file::layout::sidecar_file_name(&data_path, "audit.log"),
                        );
                        Arc::new(crate::runtime::audit_log::AuditLogger::with_path(
                            audit_path,
                        ))
                    } else {
                        Arc::new(crate::runtime::audit_log::AuditLogger::for_destination(
                            &audit_dest,
                            &data_path,
                        ))
                    }
                },
                control_event_ledger: parking_lot::RwLock::new(Arc::new(
                    crate::runtime::control_events::RuntimeLedger::new(db.store()),
                )),
                control_event_config: options.control_events,
                query_audit: Arc::new(crate::runtime::query_audit::QueryAuditStream::new(
                    db.store(),
                    options.query_audit.clone(),
                )),
                lease_lifecycle: std::sync::OnceLock::new(),
                replica_apply_metrics: std::sync::Arc::new(
                    crate::replication::logical::ReplicaApplyMetrics::default(),
                ),
                replica_link_metrics: std::sync::Arc::new(
                    crate::replication::reconnect::ReplicaLinkMetrics::default(),
                ),
                quota_bucket: crate::runtime::quota_bucket::QuotaBucket::from_env(),
                schema_vocabulary: parking_lot::RwLock::new(
                    crate::runtime::schema_vocabulary::SchemaVocabulary::new(),
                ),
                slow_query_logger: {
                    // Issue #205 — slow-query sink lives in the same
                    // directory the audit log uses, so backup/restore
                    // ships them together. Threshold + sample-pct
                    // default conservatively (1 s, 100% sampling) so
                    // emitted lines are rare and complete. Operators
                    // tune via env / config matrix in a follow-up.
                    //
                    // gh-471 iter 2: same routing as the audit log —
                    // `LogDestination::File(...)` for Performance/Max
                    // lands under the file-owned support-directory logs tier;
                    // lower tiers fall back to `red-slow.log` in the
                    // data directory.
                    // #1375 — see the audit-log note above: single-file mode
                    // never writes the slow-query log as a sibling of the
                    // `.rdb`. Route to a process-unique temp dir when embedded,
                    // regardless of the data path.
                    let fallback_dir = if embedded_single_file {
                        std::env::temp_dir()
                            .join("reddb-embedded-runtime")
                            .join(format!("slow-{}", std::process::id()))
                    } else {
                        options
                            .data_path
                            .as_ref()
                            .and_then(|p| p.parent().map(std::path::PathBuf::from))
                            .unwrap_or_else(|| std::env::temp_dir().join("reddb"))
                    };
                    let threshold_ms = std::env::var("RED_SLOW_QUERY_THRESHOLD_MS")
                        .ok()
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(1000);
                    let sample_pct = std::env::var("RED_SLOW_QUERY_SAMPLE_PCT")
                        .ok()
                        .and_then(|s| s.parse::<u8>().ok())
                        .unwrap_or(100);
                    let (_, slow_dest) = crate::api::tier_wiring::current_log_destinations();
                    crate::telemetry::slow_query_logger::SlowQueryLogger::for_destination(
                        &slow_dest,
                        &fallback_dir,
                        threshold_ms,
                        sample_pct,
                    )
                },
                slow_query_store: crate::telemetry::slow_query_store::SlowQueryStore::new(
                    crate::telemetry::slow_query_store::DEFAULT_CAP,
                ),
                kv_stats: crate::runtime::KvStatsCounters::default(),
                metrics_ingest_stats: crate::runtime::MetricsIngestCounters::default(),
                metrics_tenant_activity_stats:
                    crate::runtime::MetricsTenantActivityCounters::default(),
                claim_telemetry: Arc::new(
                    crate::runtime::claim_telemetry::ClaimTelemetryCounters::default(),
                ),
                queue_telemetry: Arc::new(
                    crate::runtime::queue_telemetry::QueueTelemetryCounters::default(),
                ),
                query_latency_telemetry: Arc::new(
                    crate::runtime::query_latency_telemetry::QueryLatencyTelemetry::default(),
                ),
                occupancy_sampler: Arc::new(
                    crate::runtime::occupancy_sampler::OccupancySampler::new(),
                ),
                node_load_telemetry: Arc::new(
                    crate::runtime::node_load_telemetry::NodeLoadTelemetry::default(),
                ),
                queue_presence: Arc::new(
                    crate::storage::queue::presence::ConsumerPresenceRegistry::new(),
                ),
                vector_introspection: Arc::new(
                    crate::storage::vector::introspection::VectorIntrospectionRegistry::new(),
                ),
                kv_tag_index: crate::runtime::KvTagIndex::default(),
                chain_tip_cache: parking_lot::Mutex::new(HashMap::new()),
                chain_integrity_broken: parking_lot::Mutex::new(HashMap::new()),
                integrity_tombstones: parking_lot::Mutex::new(Vec::new()),
                integrity_tombstones_state: std::sync::atomic::AtomicU8::new(0),
            }),
        };

        // Issue #205 — install the process-wide OperatorEvent sink so
        // emit sites buried in storage / replication / signal handlers
        // can record without threading an `&AuditLogger` through every
        // call stack. First registration wins; subsequent in-memory
        // runtimes (test harnesses) fall through to tracing+eprintln.
        crate::telemetry::operator_event::install_global_audit_sink(Arc::clone(
            &runtime.inner.audit_log,
        ));

        // Issue #1238 — wire the slow-query telemetry substrate (ADR 0060).
        // The logger dual-writes: file sink (existing) + ring store (new).
        runtime
            .inner
            .slow_query_logger
            .attach_store(Arc::clone(&runtime.inner.slow_query_store));

        // PLAN.md Phase 9.1 — backfill cold-start phase markers
        // from the wall-clock captured before storage open. The
        // entire `RedDB::open_with_options` call covers both
        // auto-restore (when configured) and WAL replay. We
        // record both phases against the same boundary today;
        // a follow-up will split them once the storage layer
        // surfaces a finer-grained event.
        runtime
            .inner
            .lifecycle
            .set_restore_started_at_ms(boot_open_start_ms);
        runtime
            .inner
            .lifecycle
            .set_restore_ready_at_ms(storage_ready_ms);
        runtime
            .inner
            .lifecycle
            .set_wal_replay_started_at_ms(boot_open_start_ms);
        runtime
            .inner
            .lifecycle
            .set_wal_replay_ready_at_ms(storage_ready_ms);

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
        runtime.rehydrate_snapshot_xid_floor();
        runtime
            .bootstrap_system_keyed_collections()
            .map_err(|err| RedDBError::Internal(format!("bootstrap system collections: {err}")))?;
        runtime.rehydrate_declared_column_schemas();
        runtime.rehydrate_runtime_index_registry()?;
        runtime
            .load_probabilistic_state()
            .map_err(|err| RedDBError::Internal(format!("load probabilistic state: {err}")))?;

        // Phase 2.5.4: replay `tenant_tables.{table}.column` markers so
        // tables declared via `TENANT BY (col)` survive restart. Each
        // entry re-registers the auto-policy and flips RLS on again.
        runtime.rehydrate_tenant_tables();
        // Issue #593 slice 9a — replay persisted materialized-view
        // descriptors so `CREATE MATERIALIZED VIEW v AS …` survives a
        // restart. Runs after the system-keyed collections bootstrap
        // and before the API opens.
        runtime.rehydrate_materialized_view_descriptors();
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
                            "prefix": reddb_file::backup_wal_prefix("")
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

            // Perf-parity config matrix: heal the Tier A (critical)
            // keys unconditionally on every boot. Idempotent — only
            // writes the default when the key is missing. Keeps
            // `SHOW CONFIG` showing every guarantee the operator has
            // (durability.mode, concurrency.locking.enabled, …) even
            // on long-running datadirs that predate the matrix.
            crate::runtime::config_matrix::heal_critical_keys(store.as_ref());
            seed_storage_deploy_config(store.as_ref(), options.storage_profile);

            // Phase 5 — Lehman-Yao runtime flag. Read the Tier A
            // `storage.btree.lehman_yao` value from the matrix (env
            // > file > red_config > default) and publish it to the
            // storage layer's atomic so the B-tree read / split
            // paths can branch without re-reading the config on
            // every hot-path call.
            let lehman_yao = runtime.config_bool("storage.btree.lehman_yao", true);
            crate::storage::engine::btree::lehman_yao::set_enabled(lehman_yao);
            if lehman_yao {
                tracing::info!(
                    "storage.btree.lehman_yao=true — lock-free concurrent descent enabled"
                );
            }

            // Config file overlay — mounted `/etc/reddb/config.json`
            // (override path via REDDB_CONFIG_FILE). Writes keys with
            // write-if-absent semantics so a later user `SET CONFIG`
            // always wins. Missing file = silent no-op.
            let overlay_path = crate::runtime::config_overlay::config_file_path();
            let _ =
                crate::runtime::config_overlay::apply_config_file(store.as_ref(), &overlay_path);
        }

        // VCS ("Git for Data") — create the `red_*` metadata
        // collections on first boot. Idempotent: `get_or_create_collection`
        // is a no-op if the collection already exists.
        {
            let store = runtime.inner.db.store();
            for name in crate::application::vcs_collections::ALL {
                let _ = store.get_or_create_collection(*name);
            }
            // Seed VCS config namespace with sensible defaults on first
            // boot, matching the pattern used by red.ai / red.storage.
            store.set_config_tree(
                crate::application::vcs_collections::CONFIG_NAMESPACE,
                &crate::json!({
                    "default_branch": "main",
                    "author": crate::json!({
                        "name": "reddb",
                        "email": "reddb@localhost"
                    }),
                    "protected_branches": crate::json!(["main"]),
                    "closure": crate::json!({
                        "enabled": true,
                        "lazy": true
                    }),
                    "merge": crate::json!({
                        "default_strategy": "auto",
                        "fast_forward": true
                    })
                }),
            );
        }

        // Migrations — create the `red_migrations` / `red_migration_deps`
        // system collections on first boot. Idempotent.
        {
            let store = runtime.inner.db.store();
            for name in crate::application::migration_collections::ALL {
                let _ = store.get_or_create_collection(*name);
            }
        }

        // Topology graph (#803) — ensure the built-in `red.topology.cluster`
        // graph collection (declared WITH ANALYTICS) and its metadata sidecar
        // exist. Idempotent and survives restarts via the WAL-backed contract.
        let _ = crate::application::topology_collections::ensure(&runtime);

        // #1369 — reserve a fixed internal-id floor so the first user-inserted
        // entity always receives a stable, documented `rid` (FIRST_USER_ENTITY_ID),
        // independent of how many internal collection-descriptor / config-default
        // entities the boot sequence seeded above. `register_entity_id` only ever
        // raises the allocator, so a database that already holds user data
        // (counter past the floor) is untouched; a freshly-seeded database jumps
        // straight to the floor.
        runtime
            .inner
            .db
            .store()
            .register_entity_id(crate::storage::EntityId::new(
                crate::storage::FIRST_USER_ENTITY_ID - 1,
            ));

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
                            crate::storage::schema::Value::Text(s) => Some(s.as_ref()),
                            _ => None,
                        });
                        let val = row.get_field("value");
                        if key == Some("red.config.backup.enabled") {
                            backup_enabled = match val {
                                Some(crate::storage::schema::Value::Boolean(true)) => true,
                                Some(crate::storage::schema::Value::Text(s)) => &**s == "true",
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

        // PLAN.md Phase 1 — Lifecycle Contract. Mark Ready once every
        // boot stage above has completed (WAL replay, restore-from-
        // remote, replica-loop spawn). Health probes flip from 503 to
        // 200 here; shutdown begins from this state.
        runtime.inner.lifecycle.mark_ready();

        // Issue #583 slice 10 — ContinuousMaterializedView scheduler.
        // Low-priority background ticker that drains the cache's
        // `claim_due_at` set every ~50ms. Holds only a Weak<RuntimeInner>
        // so the thread exits cleanly when the runtime drops (≤50ms
        // latency between drop and exit). Materialized views without
        // a `REFRESH EVERY` clause stay on the manual-refresh path
        // and are skipped by `claim_due_at`, so the loop is a no-op
        // when no scheduled views exist.
        {
            let weak_inner = Arc::downgrade(&runtime.inner);
            std::thread::Builder::new()
                .name("reddb-mv-scheduler".into())
                // Rust's default for spawned threads is 2 MiB, which is too
                // small for the query-execution path that refresh_due_materialized_views
                // runs (StatementExecutionFrame + guards per view). Match the
                // 8 MiB that the OS gives the main thread.
                .stack_size(8 * 1024 * 1024)
                .spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    let Some(inner) = weak_inner.upgrade() else {
                        break;
                    };
                    let rt = RedDBRuntime { inner };
                    rt.refresh_due_materialized_views();
                })
                .ok();
        }

        // Issue #584 slice 12 — DeclarativeRetention background sweeper.
        // Low-priority ticker that physically reclaims rows whose
        // timestamp has fallen beyond the retention window. Holds a
        // `Weak<RuntimeInner>` so the thread exits within one tick of
        // the runtime drop (graceful shutdown leaves storage consistent
        // because each tick goes through the standard DELETE path —
        // there is no half-finished mutation state to clean up). The
        // tick interval is intentionally longer than the MV scheduler
        // (500ms) because retention is order-of-seconds at minimum.
        if !runtime.write_gate().is_read_only() {
            let weak_inner = Arc::downgrade(&runtime.inner);
            std::thread::Builder::new()
                .name("reddb-retention-sweeper".into())
                .spawn(move || loop {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                    let Some(inner) = weak_inner.upgrade() else {
                        break;
                    };
                    let rt = RedDBRuntime { inner };
                    rt.sweep_retention_tick(
                        crate::runtime::retention_sweeper::DEFAULT_SWEEPER_BATCH,
                    );
                })
                .ok();
        }

        Ok(runtime)
    }

    fn rehydrate_snapshot_xid_floor(&self) {
        let store = self.inner.db.store();
        for collection in store.list_collections() {
            let Some(manager) = store.get_collection(&collection) else {
                continue;
            };
            for entity in manager.query_all(|_| true) {
                self.inner
                    .snapshot_manager
                    .observe_committed_xid(entity.xmin);
                self.inner
                    .snapshot_manager
                    .observe_committed_xid(entity.xmax);
            }
        }
    }

    /// Provision an empty Table-shaped collection that backs a
    /// `CREATE MATERIALIZED VIEW v` (issue #594 slice 9b of #575).
    /// `SELECT FROM v` reads this collection directly; the rewriter is
    /// configured to skip materialized views so the body is no longer
    /// substituted. REFRESH still writes to the cache slot — wiring it
    /// into this backing collection is the job of slice 9c.
    ///
    /// Idempotent: re-running for the same name leaves the existing
    /// collection in place (mirrors `CREATE TABLE IF NOT EXISTS`
    /// semantics). This keeps `CREATE OR REPLACE MATERIALIZED VIEW v`
    /// cheap — the body change does not invalidate already-buffered
    /// rows. Until 9c lands the backing is always empty anyway.
    pub(crate) fn ensure_materialized_view_backing(&self, name: &str) -> RedDBResult<()> {
        let store = self.inner.db.store();
        let mut changed = false;
        if store.get_collection(name).is_none() {
            store.get_or_create_collection(name);
            changed = true;
        }
        if self.inner.db.collection_contract(name).is_none() {
            self.inner
                .db
                .save_collection_contract(system_keyed_collection_contract(
                    name,
                    crate::catalog::CollectionModel::Table,
                ))
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
            changed = true;
        }
        if changed {
            self.inner
                .db
                .persist_metadata()
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
        }
        Ok(())
    }

    /// Inverse of [`ensure_materialized_view_backing`] — drops the
    /// backing collection on `DROP MATERIALIZED VIEW v`. No-op when
    /// the collection was never created (e.g. a `DROP MATERIALIZED
    /// VIEW IF EXISTS v` against an unknown name).
    pub(crate) fn drop_materialized_view_backing(&self, name: &str) -> RedDBResult<()> {
        let store = self.inner.db.store();
        if store.get_collection(name).is_none() {
            return Ok(());
        }
        store
            .drop_collection(name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        // The contract may have been dropped already (DROP TABLE path)
        // — ignore "not found" errors by checking presence first.
        if self.inner.db.collection_contract(name).is_some() {
            self.inner
                .db
                .remove_collection_contract(name)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
        }
        self.invalidate_result_cache();
        self.inner
            .db
            .persist_metadata()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        Ok(())
    }

    fn bootstrap_system_keyed_collections(&self) -> RedDBResult<()> {
        let mut changed = false;
        for (name, model) in [
            ("red.config", crate::catalog::CollectionModel::Config),
            ("red.vault", crate::catalog::CollectionModel::Vault),
            // Issue #593 — materialized-view catalog. One row per
            // `CREATE MATERIALIZED VIEW`; rehydrated at boot before
            // the API opens.
            (
                crate::runtime::continuous_materialized_view::CATALOG_COLLECTION,
                crate::catalog::CollectionModel::Config,
            ),
        ] {
            if self.inner.db.store().get_collection(name).is_none() {
                self.inner.db.store().get_or_create_collection(name);
                changed = true;
            }
            if self.inner.db.collection_contract(name).is_none() {
                self.inner
                    .db
                    .save_collection_contract(system_keyed_collection_contract(name, model))
                    .map_err(|err| RedDBError::Internal(err.to_string()))?;
                changed = true;
            }
        }
        if changed {
            self.inner
                .db
                .persist_metadata()
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
        }
        Ok(())
    }

    pub fn db(&self) -> Arc<RedDB> {
        Arc::clone(&self.inner.db)
    }

    /// Direct access to the runtime's secondary-index store.
    /// Used by bulk-insert entry points (gRPC binary bulk, HTTP bulk,
    /// wire bulk) that need to push new rows through the per-index
    /// maintenance hook after `store.bulk_insert` returns.
    pub fn index_store_ref(&self) -> &super::index_store::IndexStore {
        &self.inner.index_store
    }

    /// Apply a DDL event to the schema-vocabulary reverse index
    /// (issue #120). Called by DDL execution paths after the catalog
    /// mutation has succeeded so the index never holds entries for
    /// half-applied DDL.
    pub(crate) fn schema_vocabulary_apply(
        &self,
        event: crate::runtime::schema_vocabulary::DdlEvent,
    ) {
        self.inner.schema_vocabulary.write().on_ddl(event);
    }

    /// Lookup `token` in the schema-vocabulary reverse index. Returns
    /// an owned `Vec<VocabHit>` because the underlying read lock
    /// cannot be borrowed across the call boundary; the slice from
    /// `SchemaVocabulary::lookup` is cloned per hit.
    pub fn schema_vocabulary_lookup(
        &self,
        token: &str,
    ) -> Vec<crate::runtime::schema_vocabulary::VocabHit> {
        self.inner.schema_vocabulary.read().lookup(token).to_vec()
    }

    /// Inject an AuthStore into the runtime. Called by server boot
    /// after the vault has been bootstrapped, so that `Value::Secret`
    /// auto-encrypt/decrypt can reach the vault AES key.
    pub fn set_auth_store(&self, store: Arc<crate::auth::store::AuthStore>) {
        *self.inner.auth_store.write() = Some(store);
    }

    /// Snapshot the current AuthStore (if any). Used by the wire listener
    /// to validate bearer tokens issued via HTTP `/auth/login`.
    pub fn auth_store(&self) -> Option<Arc<crate::auth::store::AuthStore>> {
        self.inner.auth_store.read().clone()
    }

    /// Inject an `OAuthValidator` into the runtime. When set, HTTP and
    /// wire transports try OAuth JWT validation before falling back to
    /// the local AuthStore lookup. Pass `None` to disable.
    pub fn set_oauth_validator(&self, validator: Option<Arc<crate::auth::oauth::OAuthValidator>>) {
        *self.inner.oauth_validator.write() = validator;
    }

    /// Returns a clone of the configured `OAuthValidator` Arc, if any.
    /// Hot path: called per HTTP request when an Authorization header
    /// is present, so we hand back a cheap Arc clone.
    pub fn oauth_validator(&self) -> Option<Arc<crate::auth::oauth::OAuthValidator>> {
        self.inner.oauth_validator.read().clone()
    }

    /// Inject the browser-token authority (issue #936). When set, the
    /// RedWire WS handshake accepts the short-lived access JWT it mints
    /// (alongside, and tried before, the federated OAuth validator), and
    /// the `/auth/browser/*` HTTP endpoints can issue/rotate the pair.
    /// `None` leaves the browser credential flow inert.
    pub fn set_browser_token_authority(
        &self,
        authority: Option<Arc<crate::auth::browser_token::BrowserTokenAuthority>>,
    ) {
        *self.inner.browser_token_authority.write() = authority;
    }

    /// Snapshot the browser-token authority, if wired. Read on the WS
    /// handshake path and by the `/auth/browser/*` handlers; a cheap Arc
    /// clone keeps the lock hold short.
    pub fn browser_token_authority(
        &self,
    ) -> Option<Arc<crate::auth::browser_token::BrowserTokenAuthority>> {
        self.inner.browser_token_authority.read().clone()
    }
}
