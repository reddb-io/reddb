use super::*;
use crate::api::DurabilityMode;

fn store_config_from_options(options: &RedDBOptions) -> UnifiedStoreConfig {
    let mut config = UnifiedStoreConfig::default()
        .with_durability_mode(options.durability_mode)
        .with_group_commit(options.group_commit);

    if let Ok(value) = std::env::var("REDDB_DURABILITY") {
        if let Some(mode) = DurabilityMode::from_str(&value) {
            config = config.with_durability_mode(mode);
        }
    }

    let mut group_commit = config.group_commit;
    if let Ok(value) = std::env::var("REDDB_GROUP_COMMIT_WINDOW_MS") {
        if let Ok(parsed) = value.parse::<u64>() {
            group_commit.window_ms = parsed;
        }
    }
    if let Ok(value) = std::env::var("REDDB_GROUP_COMMIT_MAX_STATEMENTS") {
        if let Ok(parsed) = value.parse::<usize>() {
            group_commit.max_statements = parsed.max(1);
        }
    }
    if let Ok(value) = std::env::var("REDDB_GROUP_COMMIT_MAX_WAL_BYTES") {
        if let Ok(parsed) = value.parse::<u64>() {
            group_commit.max_wal_bytes = parsed.max(1);
        }
    }
    config.with_group_commit(group_commit)
}

impl RedDB {
    fn remote_head_key(options: &RedDBOptions) -> String {
        options.default_backup_head_key()
    }

    fn resolve_remote_bootstrap_key(
        options: &RedDBOptions,
    ) -> Result<Option<String>, crate::storage::backend::BackendError> {
        let Some(backend) = &options.remote_backend else {
            return Ok(options.remote_key.clone());
        };
        let head_key = Self::remote_head_key(options);
        if let Some(head) = crate::storage::wal::load_backup_head(backend.as_ref(), &head_key)? {
            return Ok(Some(head.snapshot_key));
        }
        Ok(options.remote_key.clone())
    }

    fn bootstrap_replica_snapshot(
        primary_addr: &str,
        local_path: &Path,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let endpoint = if primary_addr.starts_with("http") {
            primary_addr.to_string()
        } else {
            format!("http://{primary_addr}")
        };

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let payload = runtime.block_on(async move {
            use crate::grpc::proto::red_db_client::RedDbClient;
            use crate::grpc::proto::Empty;
            let mut client = RedDbClient::connect(endpoint).await?;
            let response = client
                .replication_snapshot(tonic::Request::new(Empty {}))
                .await?;
            Ok::<String, Box<dyn std::error::Error>>(response.into_inner().payload)
        })?;

        let json = crate::json::from_str::<crate::json::Value>(&payload)?;
        let Some(snapshot_hex) = json
            .get("snapshot_hex")
            .and_then(crate::json::Value::as_str)
        else {
            return Ok(false);
        };

        let bytes = hex::decode(snapshot_hex)?;
        if let Some(parent) = local_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(local_path, bytes)?;
        Ok(true)
    }

    /// Construct an ephemeral RedDB instance backed by a unique tempfile.
    ///
    /// There is no longer a true in-memory execution mode — this simply opens
    /// a persistent database at a temp path so all code paths run through the
    /// same storage pipeline.
    pub fn new() -> Self {
        Self::open_with_options(&RedDBOptions::in_memory()).expect("failed to open ephemeral RedDB")
    }

    /// Open or create a RedDB instance with persistence
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Box<dyn std::error::Error>> {
        Self::open_with_options(&RedDBOptions::persistent(path.as_ref()))
    }

    /// Open using the crate-level runtime options.
    pub fn open_with_options(options: &RedDBOptions) -> Result<Self, Box<dyn std::error::Error>> {
        if let ReplicationRole::Replica { primary_addr } = &options.replication.role {
            let local_path = options.resolved_path("data.rdb");
            if !local_path.exists() {
                let _ = Self::bootstrap_replica_snapshot(primary_addr, &local_path);
            }
        }

        // If remote backend configured, download before opening
        if let Some(backend) = &options.remote_backend {
            let local_path = options.resolved_path("data.rdb");
            if !local_path.exists() {
                let remote_key = Self::resolve_remote_bootstrap_key(options)
                    .map_err(|e| format!("remote bootstrap key resolution failed: {e}"))?;
                // Ensure parent directory exists for the download target
                if let Some(parent) = local_path.parent() {
                    if !parent.exists() {
                        std::fs::create_dir_all(parent)?;
                    }
                }
                if let Some(key) = remote_key {
                    // Download from remote to local cache
                    match backend.download(&key, &local_path) {
                        Ok(true) => { /* downloaded successfully */ }
                        Ok(false) => { /* doesn't exist remotely, will create fresh */ }
                        Err(e) => {
                            return Err(format!("remote backend download failed: {e}").into());
                        }
                    }
                }
            }
        }

        let path_buf = options.resolved_path("data.rdb");
        let store_config = store_config_from_options(options);
        let (store, path, paged_mode) = if path_buf.exists() {
            if Self::is_binary_dump(&path_buf)? {
                (
                    UnifiedStore::load_from_file(&path_buf)?,
                    Some(path_buf),
                    false,
                )
            } else {
                (
                    UnifiedStore::open_with_config(&path_buf, store_config.clone())?,
                    Some(path_buf),
                    true,
                )
            }
        } else {
            if !options.create_if_missing {
                return Err(format!(
                    "database path does not exist and create_if_missing is false: {}",
                    path_buf.display()
                )
                .into());
            }
            (
                UnifiedStore::open_with_config(&path_buf, store_config)?,
                Some(path_buf),
                true,
            )
        };

        let remote_key = options.remote_key.clone();

        // Initialize primary replication state if configured as primary.
        let replication = match &options.replication.role {
            ReplicationRole::Primary => Some(Arc::new(PrimaryReplication::new(path.as_deref()))),
            _ => None,
        };

        // Initialise quorum coordinator alongside primary replication.
        // Async mode (default) is the historical behaviour — wait-for-quorum
        // returns instantly so no write path changes are required. Sync /
        // Regions modes kick in only when the caller overrides the config.
        let quorum = replication.as_ref().map(|primary| {
            Arc::new(crate::replication::quorum::QuorumCoordinator::new(
                Arc::clone(primary),
                options.replication.quorum.clone(),
            ))
        });

        Self {
            store: Arc::new(store),
            preprocessors: Arc::new(RwLock::new(Vec::new())),
            index_config: IndexConfig::default(),
            path,
            options: options.clone(),
            paged_mode,
            vector_indexes: RwLock::new(HashMap::new()),
            collection_ttl_defaults_ms: RwLock::new(HashMap::new()),
            collection_contract_cache: RwLock::new(None),
            remote_backend: options.remote_backend.clone(),
            remote_key,
            replication,
            quorum,
            ec_registry: std::sync::Arc::new(crate::ec::config::EcRegistry::new()),
        }
        .with_initialized_metadata()
    }

    /// Flush changes to disk (if persistence is enabled).
    /// Consolidates all pending EC transactions before persisting.
    pub fn flush(&self) -> Result<(), Box<dyn std::error::Error>> {
        // Consolidate all EC fields before persisting
        let _ = self.ec_consolidate_all();

        if let Some(path) = &self.path {
            if self.paged_mode {
                self.store.persist()?;
            } else {
                self.store.save_to_file(path)?;
            }
            self.persist_metadata()?;

            // Upload to remote backend if configured
            if let (Some(backend), Some(key)) = (&self.remote_backend, &self.remote_key) {
                backend
                    .upload(path, key)
                    .map_err(|e| format!("remote backend upload failed: {e}"))?;
            }
        }
        Ok(())
    }

    /// List all collections in the store
    pub fn collections(&self) -> Vec<String> {
        self.store.list_collections()
    }

    /// Get path to the current persistent database, if any.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Get the options used to construct this database.
    pub fn options(&self) -> &RedDBOptions {
        &self.options
    }

    /// Whether this database is backed by the page-based storage backend.
    pub fn is_paged(&self) -> bool {
        self.paged_mode
    }

    /// Return aggregated store statistics.
    pub fn stats(&self) -> StoreStats {
        self.store.stats()
    }

    /// Project the expected native file header from the current persisted physical metadata.
    pub fn expected_native_header(&self) -> Option<PhysicalFileHeader> {
        self.physical_metadata()
            .map(|metadata| Self::native_header_from_metadata(&metadata))
    }

    /// Compare the page-0 native header against the persisted physical metadata.
    pub fn inspect_native_header(&self) -> Option<NativeHeaderInspection> {
        let native = self.store.physical_file_header()?;
        let metadata = self.physical_metadata()?;
        Some(Self::inspect_native_header_against_metadata(
            native, &metadata,
        ))
    }

    /// Read native collection roots persisted in the paged file, when available.
    pub fn native_collection_roots(&self) -> Option<BTreeMap<String, u64>> {
        let header = self.store.physical_file_header()?;
        self.store
            .read_native_collection_roots(header.collection_roots_page)
            .ok()
    }

    /// Read native manifest summary persisted in the paged file, when available.
    pub fn native_manifest_summary(&self) -> Option<NativeManifestSummary> {
        let header = self.store.physical_file_header()?;
        self.store
            .read_native_manifest_summary(header.manifest_page)
            .ok()
    }

    /// Read native operational registry summary persisted in the paged file, when available.
    pub fn native_registry_summary(&self) -> Option<NativeRegistrySummary> {
        let header = self.store.physical_file_header()?;
        self.store
            .read_native_registry_summary(header.registry_page)
            .ok()
    }

    /// Read native snapshot/export summary persisted in the paged file, when available.
    pub fn native_recovery_summary(&self) -> Option<NativeRecoverySummary> {
        let header = self.store.physical_file_header()?;
        self.store
            .read_native_recovery_summary(header.recovery_page)
            .ok()
    }

    /// Read native catalog summary persisted in the paged file, when available.
    pub fn native_catalog_summary(&self) -> Option<NativeCatalogSummary> {
        let header = self.store.physical_file_header()?;
        self.store
            .read_native_catalog_summary(header.catalog_page)
            .ok()
    }

    /// Read native metadata status persisted in the paged file, when available.
    pub fn native_metadata_state_summary(&self) -> Option<NativeMetadataStateSummary> {
        let header = self.store.physical_file_header()?;
        self.store
            .read_native_metadata_state_summary(header.metadata_state_page)
            .ok()
    }

    /// Read the consolidated native physical publication state from the paged file.
    pub fn native_physical_state(&self) -> Option<NativePhysicalState> {
        self.store.read_native_physical_state().ok()
    }

    /// Read native vector artifact pages persisted in the paged file, when available.
    pub fn native_vector_artifact_pages(&self) -> Option<Vec<NativeVectorArtifactPageSummary>> {
        let header = self.store.physical_file_header()?;
        self.store
            .read_native_vector_artifact_store(header.vector_artifact_page)
            .ok()
    }

    pub fn inspect_native_vector_artifact(
        &self,
        collection: &str,
        artifact_kind: Option<&str>,
    ) -> Result<NativeVectorArtifactInspection, String> {
        let header = self
            .store
            .physical_file_header()
            .ok_or_else(|| "native physical header is not available".to_string())?;
        if header.vector_artifact_page == 0 {
            return Err("native vector artifact store is not available".to_string());
        }
        let artifact_kind = artifact_kind.unwrap_or("hnsw");
        let (summary, bytes) = self
            .store
            .read_native_vector_artifact_blob(
                header.vector_artifact_page,
                collection,
                Some(artifact_kind),
            )
            .map_err(|err| err.to_string())?
            .ok_or_else(|| {
                format!(
                    "native vector artifact not found for collection '{collection}' and kind '{artifact_kind}'"
                )
            })?;
        match artifact_kind {
            "hnsw" => {
                let index = HnswIndex::from_bytes(&bytes)?;
                let stats = index.stats();
                Ok(NativeVectorArtifactInspection {
                    collection: summary.collection,
                    artifact_kind: summary.artifact_kind,
                    root_page: summary.root_page,
                    page_count: summary.page_count,
                    byte_len: summary.byte_len,
                    checksum: summary.checksum,
                    node_count: stats.node_count as u64,
                    dimension: stats.dimension as u32,
                    max_layer: stats.max_layer as u32,
                    total_connections: stats.total_connections as u64,
                    avg_connections: stats.avg_connections,
                    entry_point: stats.entry_point,
                    ivf_n_lists: None,
                    ivf_non_empty_lists: None,
                    ivf_trained: None,
                    graph_edge_count: None,
                    graph_node_count: None,
                    graph_label_count: None,
                    text_doc_count: None,
                    text_term_count: None,
                    text_posting_count: None,
                    document_doc_count: None,
                    document_path_count: None,
                    document_value_count: None,
                    document_unique_value_count: None,
                })
            }
            "ivf" => {
                let index = IvfIndex::from_bytes(&bytes)?;
                let stats: IvfStats = index.stats();
                Ok(NativeVectorArtifactInspection {
                    collection: summary.collection,
                    artifact_kind: summary.artifact_kind,
                    root_page: summary.root_page,
                    page_count: summary.page_count,
                    byte_len: summary.byte_len,
                    checksum: summary.checksum,
                    node_count: stats.total_vectors as u64,
                    dimension: stats.dimension as u32,
                    max_layer: 0,
                    total_connections: 0,
                    avg_connections: 0.0,
                    entry_point: None,
                    ivf_n_lists: Some(stats.n_lists as u32),
                    ivf_non_empty_lists: Some(stats.non_empty_lists as u32),
                    ivf_trained: Some(stats.trained),
                    graph_edge_count: None,
                    graph_node_count: None,
                    graph_label_count: None,
                    text_doc_count: None,
                    text_term_count: None,
                    text_posting_count: None,
                    document_doc_count: None,
                    document_path_count: None,
                    document_value_count: None,
                    document_unique_value_count: None,
                })
            }
            "graph.adjacency" => {
                let (edge_count, node_count, label_count) =
                    Self::inspect_native_graph_adjacency_artifact(&bytes)?;
                Ok(NativeVectorArtifactInspection {
                    collection: summary.collection,
                    artifact_kind: summary.artifact_kind,
                    root_page: summary.root_page,
                    page_count: summary.page_count,
                    byte_len: summary.byte_len,
                    checksum: summary.checksum,
                    node_count: edge_count,
                    dimension: 0,
                    max_layer: 0,
                    total_connections: edge_count,
                    avg_connections: if node_count == 0 {
                        0.0
                    } else {
                        edge_count as f64 / node_count as f64
                    },
                    entry_point: None,
                    ivf_n_lists: None,
                    ivf_non_empty_lists: None,
                    ivf_trained: None,
                    graph_edge_count: Some(edge_count),
                    graph_node_count: Some(node_count),
                    graph_label_count: Some(label_count),
                    text_doc_count: None,
                    text_term_count: None,
                    text_posting_count: None,
                    document_doc_count: None,
                    document_path_count: None,
                    document_value_count: None,
                    document_unique_value_count: None,
                })
            }
            "text.fulltext" => {
                let (doc_count, term_count, posting_count) =
                    Self::inspect_native_fulltext_artifact(&bytes)?;
                Ok(NativeVectorArtifactInspection {
                    collection: summary.collection,
                    artifact_kind: summary.artifact_kind,
                    root_page: summary.root_page,
                    page_count: summary.page_count,
                    byte_len: summary.byte_len,
                    checksum: summary.checksum,
                    node_count: posting_count,
                    dimension: 0,
                    max_layer: term_count as u32,
                    total_connections: posting_count,
                    avg_connections: if doc_count == 0 {
                        0.0
                    } else {
                        posting_count as f64 / doc_count as f64
                    },
                    entry_point: None,
                    ivf_n_lists: None,
                    ivf_non_empty_lists: None,
                    ivf_trained: None,
                    graph_edge_count: None,
                    graph_node_count: None,
                    graph_label_count: None,
                    text_doc_count: Some(doc_count),
                    text_term_count: Some(term_count),
                    text_posting_count: Some(posting_count),
                    document_doc_count: None,
                    document_path_count: None,
                    document_value_count: None,
                    document_unique_value_count: None,
                })
            }
            "document.pathvalue" => {
                let (doc_count, path_count, value_count, unique_value_count) =
                    Self::inspect_native_document_pathvalue_artifact(&bytes)?;
                Ok(NativeVectorArtifactInspection {
                    collection: summary.collection,
                    artifact_kind: summary.artifact_kind,
                    root_page: summary.root_page,
                    page_count: summary.page_count,
                    byte_len: summary.byte_len,
                    checksum: summary.checksum,
                    node_count: value_count,
                    dimension: 0,
                    max_layer: path_count as u32,
                    total_connections: value_count,
                    avg_connections: if doc_count == 0 {
                        0.0
                    } else {
                        value_count as f64 / doc_count as f64
                    },
                    entry_point: None,
                    ivf_n_lists: None,
                    ivf_non_empty_lists: None,
                    ivf_trained: None,
                    graph_edge_count: None,
                    graph_node_count: None,
                    graph_label_count: None,
                    text_doc_count: None,
                    text_term_count: None,
                    text_posting_count: None,
                    document_doc_count: Some(doc_count),
                    document_path_count: Some(path_count),
                    document_value_count: Some(value_count),
                    document_unique_value_count: Some(unique_value_count),
                })
            }
            other => Err(format!("unsupported native vector artifact kind '{other}'")),
        }
    }

    pub fn warmup_native_vector_artifact(
        &self,
        collection: &str,
        artifact_kind: Option<&str>,
    ) -> Result<NativeVectorArtifactInspection, String> {
        self.inspect_native_vector_artifact(collection, artifact_kind)
    }

    pub fn inspect_native_vector_artifacts(
        &self,
    ) -> Result<NativeVectorArtifactBatchInspection, String> {
        let summaries = self
            .native_vector_artifact_pages()
            .ok_or_else(|| "native vector artifact store is not available".to_string())?;
        let mut artifacts = Vec::new();
        let mut failures = Vec::new();
        for summary in summaries {
            match self
                .inspect_native_vector_artifact(&summary.collection, Some(&summary.artifact_kind))
            {
                Ok(artifact) => artifacts.push(artifact),
                Err(err) => failures.push((summary.collection, summary.artifact_kind, err)),
            }
        }
        Ok(NativeVectorArtifactBatchInspection {
            inspected_count: artifacts.len() + failures.len(),
            valid_count: artifacts.len(),
            artifacts,
            failures,
        })
    }

    pub fn warmup_native_vector_artifacts(
        &self,
    ) -> Result<NativeVectorArtifactBatchInspection, String> {
        self.inspect_native_vector_artifacts()
    }

    /// Inspect which physical source is currently authoritative for operational recovery.
    pub fn physical_authority_status(&self) -> PhysicalAuthorityStatus {
        if self.options.mode != StorageMode::Persistent {
            return PhysicalAuthorityStatus {
                preference: "not_persistent".to_string(),
                sidecar_available: false,
                native_state_available: false,
                native_bootstrap_ready: false,
                native_registry_complete: None,
                native_recovery_complete: None,
                native_catalog_complete: None,
                sidecar_loaded_from: None,
                native_header_repair_policy: None,
                metadata_sequence: None,
                native_sequence: None,
                native_metadata_last_loaded_from: None,
                native_metadata_generated_at_unix_ms: None,
            };
        }

        let native_state = self.native_physical_state();
        let native_header_repair_policy = self.native_header_repair_policy().map(|policy| {
            match policy {
                NativeHeaderRepairPolicy::InSync => "in_sync",
                NativeHeaderRepairPolicy::RepairNativeFromMetadata => "repair_native_from_metadata",
                NativeHeaderRepairPolicy::NativeAheadOfMetadata => "native_ahead_of_metadata",
            }
            .to_string()
        });

        let Some(path) = self.path() else {
            return PhysicalAuthorityStatus {
                preference: "path_unavailable".to_string(),
                sidecar_available: false,
                native_state_available: native_state.is_some(),
                native_bootstrap_ready: native_state
                    .as_ref()
                    .map(Self::native_state_is_bootstrap_complete)
                    .unwrap_or(false),
                native_registry_complete: native_state
                    .as_ref()
                    .and_then(|state| state.registry.as_ref())
                    .map(|registry| {
                        registry.collections_complete
                            && registry.indexes_complete
                            && registry.graph_projections_complete
                            && registry.analytics_jobs_complete
                            && registry.vector_artifacts_complete
                    }),
                native_recovery_complete: native_state
                    .as_ref()
                    .and_then(|state| state.recovery.as_ref())
                    .map(|recovery| recovery.snapshots_complete && recovery.exports_complete),
                native_catalog_complete: native_state
                    .as_ref()
                    .and_then(|state| state.catalog.as_ref())
                    .map(|catalog| catalog.collections_complete),
                sidecar_loaded_from: None,
                native_header_repair_policy,
                metadata_sequence: None,
                native_sequence: native_state.as_ref().map(|state| state.header.sequence),
                native_metadata_last_loaded_from: native_state
                    .as_ref()
                    .and_then(|state| state.metadata_state.as_ref())
                    .and_then(|summary| summary.last_loaded_from.clone()),
                native_metadata_generated_at_unix_ms: native_state
                    .as_ref()
                    .and_then(|state| state.metadata_state.as_ref())
                    .map(|summary| summary.generated_at_unix_ms),
            };
        };

        let sidecar = PhysicalMetadataFile::load_for_data_path_with_source(path).ok();
        PhysicalAuthorityStatus {
            preference: self
                .physical_metadata_preference()
                .unwrap_or("unknown")
                .to_string(),
            sidecar_available: sidecar.is_some(),
            native_state_available: native_state.is_some(),
            native_bootstrap_ready: native_state
                .as_ref()
                .map(Self::native_state_is_bootstrap_complete)
                .unwrap_or(false),
            native_registry_complete: native_state
                .as_ref()
                .and_then(|state| state.registry.as_ref())
                .map(|registry| {
                    registry.collections_complete
                        && registry.indexes_complete
                        && registry.graph_projections_complete
                        && registry.analytics_jobs_complete
                        && registry.vector_artifacts_complete
                }),
            native_recovery_complete: native_state
                .as_ref()
                .and_then(|state| state.recovery.as_ref())
                .map(|recovery| recovery.snapshots_complete && recovery.exports_complete),
            native_catalog_complete: native_state
                .as_ref()
                .and_then(|state| state.catalog.as_ref())
                .map(|catalog| catalog.collections_complete),
            sidecar_loaded_from: sidecar
                .as_ref()
                .map(|(_, source)| source.as_str().to_string()),
            native_header_repair_policy,
            metadata_sequence: sidecar
                .as_ref()
                .map(|(metadata, _)| metadata.superblock.sequence),
            native_sequence: native_state.as_ref().map(|state| state.header.sequence),
            native_metadata_last_loaded_from: native_state
                .as_ref()
                .and_then(|state| state.metadata_state.as_ref())
                .and_then(|summary| summary.last_loaded_from.clone()),
            native_metadata_generated_at_unix_ms: native_state
                .as_ref()
                .and_then(|state| state.metadata_state.as_ref())
                .map(|summary| summary.generated_at_unix_ms),
        }
    }

    /// Decide how to reconcile page-0 native state against persisted physical metadata.
    pub fn native_header_repair_policy(&self) -> Option<NativeHeaderRepairPolicy> {
        let inspection = self.inspect_native_header()?;
        Some(Self::repair_policy_for_inspection(&inspection))
    }

    /// Repair the native header from persisted physical metadata when it is safe to do so.
    pub fn repair_native_header_from_metadata(
        &self,
    ) -> Result<NativeHeaderRepairPolicy, Box<dyn std::error::Error>> {
        if !self.paged_mode || self.options.read_only {
            return Ok(NativeHeaderRepairPolicy::InSync);
        }

        let Some(inspection) = self.inspect_native_header() else {
            return Ok(NativeHeaderRepairPolicy::InSync);
        };
        let policy = Self::repair_policy_for_inspection(&inspection);

        if policy == NativeHeaderRepairPolicy::RepairNativeFromMetadata {
            self.store
                .update_physical_file_header(inspection.expected)?;
            self.store.persist()?;
        }

        Ok(policy)
    }

    /// Republish the full native physical publication state from the current physical metadata view.
    pub fn repair_native_physical_state_from_metadata(
        &self,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        if self.options.mode != StorageMode::Persistent
            || !self.paged_mode
            || self.options.read_only
        {
            return Ok(false);
        }

        let metadata = self.load_or_bootstrap_physical_metadata(true)?;
        self.persist_native_physical_header(&metadata)?;
        Ok(true)
    }

    /// Provide a compact catalog snapshot for management/runtime layers.
    pub fn catalog_snapshot(&self) -> CatalogSnapshot {
        let mut stats_by_collection = std::collections::BTreeMap::new();
        for name in self.store.list_collections() {
            if let Some(manager) = self.store.get_collection(&name) {
                let manager_stats = manager.stats();
                let cross_refs = manager
                    .query_all(|_| true)
                    .iter()
                    .map(|entity| entity.cross_refs().len())
                    .sum();
                stats_by_collection.insert(
                    name,
                    CollectionStats {
                        entities: manager_stats.total_entities,
                        cross_refs,
                        segments: manager_stats.growing_count
                            + manager_stats.sealed_count
                            + manager_stats.archived_count,
                    },
                );
            }
        }

        CatalogSnapshot {
            name: "reddb".to_string(),
            total_entities: stats_by_collection
                .values()
                .map(|stats| stats.entities)
                .sum(),
            total_collections: stats_by_collection.len(),
            stats_by_collection,
            updated_at: std::time::SystemTime::now(),
        }
    }

    /// Full logical catalog snapshot including inferred collection models and indices.
    pub fn catalog_model_snapshot(&self) -> CatalogModelSnapshot {
        let catalog = self.runtime_index_catalog();
        let contracts = self.collection_contracts();
        let declarations = self
            .physical_metadata()
            .map(|metadata| CatalogDeclarations {
                declared_indexes: metadata.indexes,
                declared_graph_projections: metadata.graph_projections,
                declared_analytics_jobs: metadata.analytics_jobs,
                operational_indexes: self.physical_indexes(),
                operational_graph_projections: self.graph_projections(),
                operational_analytics_jobs: self.analytics_jobs(),
            });
        snapshot_store_with_declarations(
            "reddb",
            self.store.as_ref(),
            Some(&catalog),
            declarations.as_ref(),
            Some(contracts.as_slice()),
        )
    }

    pub fn catalog_consistency_report(&self) -> CatalogConsistencyReport {
        consistency_report(&self.catalog_model_snapshot())
    }

    pub fn readiness_for_query(&self) -> bool {
        let report = self.health();
        self.readiness_flags_from_health(&report).0
    }

    pub fn readiness_for_query_serverless(&self) -> bool {
        let report = self.health();
        self.readiness_flags_from_health_serverless(&report).0
    }

    pub fn readiness_for_write(&self) -> bool {
        let report = self.health();
        self.readiness_flags_from_health(&report).1
    }

    pub fn readiness_for_write_serverless(&self) -> bool {
        let report = self.health();
        self.readiness_flags_from_health_serverless(&report).1
    }

    pub fn readiness_for_repair(&self) -> bool {
        let report = self.health();
        self.readiness_flags_from_health(&report).2
    }

    pub fn readiness_for_repair_serverless(&self) -> bool {
        let report = self.health();
        self.readiness_flags_from_health_serverless(&report).2
    }

    pub(crate) fn readiness_flags_from_health(&self, report: &HealthReport) -> (bool, bool, bool) {
        let query_allowed = if self.options.mode == StorageMode::Persistent {
            let authority = self.physical_authority_status();
            (authority.native_bootstrap_ready || authority.sidecar_loaded_from.is_some())
                && matches!(report.state, HealthState::Healthy)
        } else {
            matches!(report.state, HealthState::Healthy)
        };

        let write_allowed = !self.options.read_only && report.state != HealthState::Unhealthy;
        let repair_allowed = self.options.mode == StorageMode::Persistent
            && !self.options.read_only
            && report.state != HealthState::Unhealthy;

        (query_allowed, write_allowed, repair_allowed)
    }

    pub(crate) fn readiness_flags_from_health_serverless(
        &self,
        report: &HealthReport,
    ) -> (bool, bool, bool) {
        if self.options.mode != StorageMode::Persistent {
            let query_allowed = matches!(report.state, HealthState::Healthy);
            let write_allowed = !self.options.read_only && report.state != HealthState::Unhealthy;
            let repair_allowed = !self.options.read_only && report.state != HealthState::Unhealthy;

            return (query_allowed, write_allowed, repair_allowed);
        }

        let authority = self.physical_authority_status();
        let native_bootstrap_ready = authority.native_bootstrap_ready;
        let query_allowed = native_bootstrap_ready && matches!(report.state, HealthState::Healthy);
        let write_allowed = !self.options.read_only
            && native_bootstrap_ready
            && report.state != HealthState::Unhealthy;
        let repair_allowed = !self.options.read_only
            && native_bootstrap_ready
            && report.state != HealthState::Unhealthy;

        (query_allowed, write_allowed, repair_allowed)
    }
}
