//! RedDB - Main Entry Point
//!
//! Unified Database with best-in-class developer experience for Tables, Graphs, and Vectors.

use std::fs;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use std::{collections::BTreeMap, fmt::Debug};

use super::super::{
    EntityData, EntityId, StoreStats, UnifiedStore, UnifiedStoreConfig, UnifiedEntity,
};
use super::batch::BatchBuilder;
use super::builders::{EdgeBuilder, NodeBuilder, RowBuilder, VectorBuilder};
use super::helpers::cosine_similarity;
use super::preprocessors::{IndexConfig, Preprocessor};
use super::query::QueryBuilder;
use super::refs::{NodeRef, TableRef, VectorRef};
use super::types::{LinkedEntity, SimilarResult};
use crate::api::{Capability, CatalogSnapshot, CollectionStats, RedDBOptions, StorageMode};
use crate::catalog::{snapshot_store, CatalogModelSnapshot, CollectionDescriptor, CollectionModel};
use crate::health::{storage_file_health, HealthReport};
use crate::index::{IndexCatalog, IndexConfig as RuntimeIndexConfig, IndexKind};
use crate::physical::{
    ExportDescriptor, PhysicalAnalyticsJob, PhysicalGraphProjection, PhysicalIndexState,
    PhysicalMetadataFile,
};
use crate::storage::schema::Value;

/// RedDB - Unified Database with Best-in-Class DevX
///
/// Single entry point for Tables, Graphs, and Vectors with full
/// metadata support and cross-referencing.
pub struct RedDB {
    store: Arc<UnifiedStore>,
    /// Preprocessing hooks
    preprocessors: Vec<Box<dyn Preprocessor>>,
    /// Index configuration
    index_config: IndexConfig,
    /// Persistence path
    path: Option<PathBuf>,
    /// Construction/runtime options
    options: RedDBOptions,
    /// Whether the current persistence backend is page-based.
    paged_mode: bool,
}

impl RedDB {
    /// Create a new RedDB instance (in-memory)
    pub fn new() -> Self {
        Self {
            store: Arc::new(UnifiedStore::new()),
            preprocessors: Vec::new(),
            index_config: IndexConfig::default(),
            path: None,
            options: RedDBOptions::in_memory(),
            paged_mode: false,
        }
    }

    /// Open or create a RedDB instance with persistence
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Box<dyn std::error::Error>> {
        Self::open_with_options(&RedDBOptions::persistent(path.as_ref()))
    }

    /// Open using the crate-level runtime options.
    pub fn open_with_options(
        options: &RedDBOptions,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let (store, path, paged_mode) = match options.mode {
            StorageMode::InMemory => (
                UnifiedStore::with_config(UnifiedStoreConfig::default()),
                None,
                false,
            ),
            StorageMode::Persistent => {
                let path_buf = options.resolved_path("reddb.rdb");
                if path_buf.exists() {
                    if Self::is_binary_dump(&path_buf)? {
                        (UnifiedStore::load_from_file(&path_buf)?, Some(path_buf), false)
                    } else {
                        (UnifiedStore::open(&path_buf)?, Some(path_buf), true)
                    }
                } else {
                    if !options.create_if_missing {
                        return Err(format!(
                            "database path does not exist and create_if_missing is false: {}",
                            path_buf.display()
                        )
                        .into());
                    }
                    (UnifiedStore::open(&path_buf)?, Some(path_buf), true)
                }
            }
        };

        Ok(Self {
            store: Arc::new(store),
            preprocessors: Vec::new(),
            index_config: IndexConfig::default(),
            path,
            options: options.clone(),
            paged_mode,
        }
        .with_initialized_metadata())
    }

    /// Create with custom store
    pub fn with_store(store: Arc<UnifiedStore>) -> Self {
        Self {
            store,
            preprocessors: Vec::new(),
            index_config: IndexConfig::default(),
            path: None,
            options: RedDBOptions::in_memory(),
            paged_mode: false,
        }
    }

    /// Flush changes to disk (if persistence is enabled)
    pub fn flush(&self) -> Result<(), Box<dyn std::error::Error>> {
        if let Some(path) = &self.path {
            if self.paged_mode {
                self.store.persist()?;
            } else {
                self.store.save_to_file(path)?;
            }
            self.persist_metadata()?;
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

    /// Provide a compact catalog snapshot for management/runtime layers.
    pub fn catalog_snapshot(&self) -> CatalogSnapshot {
        let mut stats_by_collection = std::collections::BTreeMap::new();
        for name in self.store.list_collections() {
            if let Some(manager) = self.store.get_collection(&name) {
                let manager_stats = manager.stats();
                let cross_refs = manager
                    .query_all(|_| true)
                    .iter()
                    .map(|entity| entity.cross_refs.len())
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
        snapshot_store("reddb", self.store.as_ref(), Some(&catalog))
    }

    /// Health report for the current database handle.
    pub fn health(&self) -> HealthReport {
        let mut report = match self.path() {
            Some(path) => storage_file_health(path),
            None => HealthReport::healthy().with_diagnostic("mode", "in-memory"),
        };
        report = report.with_diagnostic("collections", self.collections().len().to_string());
        report = report.with_diagnostic("entities", self.stats().total_entities.to_string());
        report = report.with_diagnostic(
            "retention.snapshots",
            self.options.snapshot_retention.to_string(),
        );
        report = report.with_diagnostic(
            "retention.exports",
            self.options.export_retention.to_string(),
        );
        if let Some(path) = self.path() {
            let metadata_path = PhysicalMetadataFile::metadata_path_for(path);
            report = report.with_diagnostic("metadata.path", metadata_path.display().to_string());
            report = report.with_diagnostic("metadata.exists", metadata_path.exists().to_string());
            if let Ok(metadata) = PhysicalMetadataFile::load_from_path(&metadata_path) {
                report = report.with_diagnostic(
                    "metadata.sequence",
                    metadata.superblock.sequence.to_string(),
                );
                report = report.with_diagnostic(
                    "metadata.snapshots",
                    metadata.snapshots.len().to_string(),
                );
                report = report.with_diagnostic(
                    "metadata.indexes",
                    metadata.indexes.len().to_string(),
                );
                report = report.with_diagnostic(
                    "metadata.exports",
                    metadata.exports.len().to_string(),
                );
                report = report.with_diagnostic(
                    "metadata.collection_roots",
                    metadata.superblock.collection_roots.len().to_string(),
                );
                report = report.with_diagnostic(
                    "metadata.manifest_events",
                    metadata.manifest_events.len().to_string(),
                );
                report = report.with_diagnostic(
                    "metadata.graph_projections",
                    metadata.graph_projections.len().to_string(),
                );
                report = report.with_diagnostic(
                    "metadata.analytics_jobs",
                    metadata.analytics_jobs.len().to_string(),
                );
            } else if self.options.mode == StorageMode::Persistent {
                report.issue("metadata", "physical metadata sidecar is missing or unreadable");
            }
        }
        report.with_diagnostic("paged_mode", self.paged_mode.to_string())
    }

    /// Run background maintenance for all active collections.
    pub fn run_maintenance(&self) -> Result<(), Box<dyn std::error::Error>> {
        self.store.run_maintenance()?;
        self.persist_metadata()?;
        Ok(())
    }

    /// Path to the physical metadata sidecar, if persistent.
    pub fn metadata_path(&self) -> Option<PathBuf> {
        self.path
            .as_ref()
            .map(|path| PhysicalMetadataFile::metadata_path_for(path))
    }

    /// Load the last persisted physical metadata sidecar, if present.
    pub fn physical_metadata(&self) -> Option<PhysicalMetadataFile> {
        self.path()
            .and_then(|path| PhysicalMetadataFile::load_for_data_path(path).ok())
    }

    /// Physical index registry derived for the current database state.
    pub fn physical_indexes(&self) -> Vec<PhysicalIndexState> {
        self.physical_metadata()
            .map(|metadata| metadata.indexes)
            .filter(|indexes| !indexes.is_empty())
            .unwrap_or_else(|| self.physical_index_state())
    }

    /// List registered named exports from the physical metadata sidecar.
    pub fn exports(&self) -> Vec<ExportDescriptor> {
        self.physical_metadata()
            .map(|metadata| metadata.exports)
            .unwrap_or_default()
    }

    /// List persisted named graph projections from the physical metadata sidecar.
    pub fn graph_projections(&self) -> Vec<PhysicalGraphProjection> {
        self.physical_metadata()
            .map(|metadata| metadata.graph_projections)
            .unwrap_or_default()
    }

    /// List persisted analytics job metadata from the physical metadata sidecar.
    pub fn analytics_jobs(&self) -> Vec<PhysicalAnalyticsJob> {
        self.physical_metadata()
            .map(|metadata| metadata.analytics_jobs)
            .unwrap_or_default()
    }

    /// Upsert a named graph projection in the persisted physical metadata.
    pub fn save_graph_projection(
        &self,
        name: impl Into<String>,
        node_labels: Vec<String>,
        node_types: Vec<String>,
        edge_labels: Vec<String>,
        source: impl Into<String>,
    ) -> Result<PhysicalGraphProjection, Box<dyn std::error::Error>> {
        let name = name.into();
        let source = source.into();
        self.update_physical_metadata(|metadata| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            let projection = if let Some(existing) = metadata
                .graph_projections
                .iter_mut()
                .find(|projection| projection.name == name)
            {
                existing.updated_at_unix_ms = now;
                existing.source = source.clone();
                existing.node_labels = node_labels.clone();
                existing.node_types = node_types.clone();
                existing.edge_labels = edge_labels.clone();
                existing.last_materialized_sequence = Some(metadata.superblock.sequence);
                existing.clone()
            } else {
                let projection = PhysicalGraphProjection {
                    name: name.clone(),
                    created_at_unix_ms: now,
                    updated_at_unix_ms: now,
                    source: source.clone(),
                    node_labels: node_labels.clone(),
                    node_types: node_types.clone(),
                    edge_labels: edge_labels.clone(),
                    last_materialized_sequence: Some(metadata.superblock.sequence),
                };
                metadata.graph_projections.push(projection.clone());
                projection
            };

            metadata
                .graph_projections
                .sort_by(|left, right| left.name.cmp(&right.name));
            projection
        })
    }

    /// Record or update analytics job metadata in the persisted physical metadata.
    pub fn record_analytics_job(
        &self,
        kind: impl Into<String>,
        projection: Option<String>,
        metadata_entries: BTreeMap<String, String>,
    ) -> Result<PhysicalAnalyticsJob, Box<dyn std::error::Error>> {
        let kind = kind.into();
        let job_id = match &projection {
            Some(projection) => format!("{kind}::{projection}"),
            None => format!("{kind}::global"),
        };

        self.update_physical_metadata(|metadata| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();

            let job = if let Some(existing) = metadata
                .analytics_jobs
                .iter_mut()
                .find(|job| job.id == job_id)
            {
                existing.state = "completed".to_string();
                existing.projection = projection.clone();
                existing.updated_at_unix_ms = now;
                existing.last_run_sequence = Some(metadata.superblock.sequence);
                existing.metadata = metadata_entries.clone();
                existing.clone()
            } else {
                let job = PhysicalAnalyticsJob {
                    id: job_id.clone(),
                    kind: kind.clone(),
                    state: "completed".to_string(),
                    projection: projection.clone(),
                    created_at_unix_ms: now,
                    updated_at_unix_ms: now,
                    last_run_sequence: Some(metadata.superblock.sequence),
                    metadata: metadata_entries.clone(),
                };
                metadata.analytics_jobs.push(job.clone());
                job
            };

            metadata
                .analytics_jobs
                .sort_by(|left, right| left.id.cmp(&right.id));
            job
        })
    }

    /// Create a named export by copying the current database file and metadata sidecar.
    pub fn create_named_export(
        &self,
        name: impl Into<String>,
    ) -> Result<ExportDescriptor, Box<dyn std::error::Error>> {
        let name = name.into();
        if self.options.mode != StorageMode::Persistent {
            return Err("exports require persistent mode".into());
        }
        let Some(path) = self.path() else {
            return Err("database path is not available".into());
        };

        self.flush()?;

        let mut metadata = PhysicalMetadataFile::load_for_data_path(path)?;
        let export_data_path = PhysicalMetadataFile::export_data_path_for(path, &name);
        let export_metadata_path = PhysicalMetadataFile::metadata_path_for(&export_data_path);

        fs::copy(path, &export_data_path)?;

        let descriptor = ExportDescriptor {
            name: name.clone(),
            created_at_unix_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis(),
            snapshot_id: metadata.snapshots.last().map(|snapshot| snapshot.snapshot_id),
            superblock_sequence: metadata.superblock.sequence,
            data_path: export_data_path.display().to_string(),
            metadata_path: export_metadata_path.display().to_string(),
            collection_count: metadata.catalog.total_collections,
            total_entities: metadata.catalog.total_entities,
        };

        metadata.exports.retain(|export| export.name != descriptor.name);
        metadata.exports.push(descriptor.clone());
        self.prune_export_registry(&mut metadata.exports);
        metadata.save_for_data_path(path)?;
        metadata.save_to_path(&export_metadata_path)?;

        Ok(descriptor)
    }

    /// Enable or disable a physical index entry in the persisted registry.
    pub fn set_index_enabled(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<Option<PhysicalIndexState>, Box<dyn std::error::Error>> {
        self.update_physical_metadata(|metadata| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            if let Some(index) = metadata.indexes.iter_mut().find(|index| index.name == name) {
                index.enabled = enabled;
                index.last_refresh_ms = Some(now);
                return Some(index.clone());
            }
            None
        })
    }

    /// Mark a physical index as warmed up/refreshed in the persisted registry.
    pub fn warmup_index(
        &self,
        name: &str,
    ) -> Result<Option<PhysicalIndexState>, Box<dyn std::error::Error>> {
        self.update_physical_metadata(|metadata| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            if let Some(index) = metadata.indexes.iter_mut().find(|index| index.name == name) {
                index.last_refresh_ms = Some(now);
                return Some(index.clone());
            }
            None
        })
    }

    /// Rebuild physical index metadata from the current catalog, optionally restricted to one collection.
    pub fn rebuild_index_registry(
        &self,
        collection: Option<&str>,
    ) -> Result<Vec<PhysicalIndexState>, Box<dyn std::error::Error>> {
        let fresh = self.physical_index_state();
        self.update_physical_metadata(|metadata| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();

            let mut affected = Vec::new();
            for fresh_index in fresh {
                let matches_collection = collection.map_or(true, |collection_name| {
                    fresh_index.collection.as_deref() == Some(collection_name)
                });
                if !matches_collection {
                    continue;
                }

                let enabled = metadata
                    .indexes
                    .iter()
                    .find(|index| index.name == fresh_index.name)
                    .map(|index| index.enabled)
                    .unwrap_or(true);

                let mut rebuilt = fresh_index.clone();
                rebuilt.enabled = enabled;
                rebuilt.last_refresh_ms = Some(now);

                if let Some(existing) = metadata
                    .indexes
                    .iter_mut()
                    .find(|index| index.name == rebuilt.name)
                {
                    *existing = rebuilt.clone();
                } else {
                    metadata.indexes.push(rebuilt.clone());
                }

                affected.push(rebuilt);
            }

            affected
        })
    }

    /// Apply snapshot/export retention policy to physical metadata and export files.
    pub fn enforce_retention_policy(&self) -> Result<(), Box<dyn std::error::Error>> {
        if self.options.mode != StorageMode::Persistent || self.options.read_only {
            return Ok(());
        }
        let Some(path) = self.path() else {
            return Ok(());
        };

        let mut metadata = match PhysicalMetadataFile::load_for_data_path(path) {
            Ok(metadata) => metadata,
            Err(_) => return Ok(()),
        };

        self.prune_export_registry(&mut metadata.exports);
        metadata.save_for_data_path(path)?;
        Ok(())
    }

    // ========================================================================
    // Builder Methods - Create Entities
    // ========================================================================

    /// Start building a graph node
    ///
    /// # Example
    /// ```ignore
    /// let host = db.node("hosts", "Host")
    ///     .property("ip", "192.168.1.1")
    ///     .save()?;
    /// ```
    pub fn node(&self, collection: impl Into<String>, label: impl Into<String>) -> NodeBuilder {
        NodeBuilder::new(self.store.clone(), collection, label)
    }

    /// Start building a graph edge
    ///
    /// # Example
    /// ```ignore
    /// let edge = db.edge("connections", "CONNECTS_TO")
    ///     .from(host_a)
    ///     .to(host_b)
    ///     .weight(0.95)
    ///     .property("protocol", "TCP")
    ///     .save()?;
    /// ```
    pub fn edge(&self, collection: impl Into<String>, label: impl Into<String>) -> EdgeBuilder {
        EdgeBuilder::new(self.store.clone(), collection, label)
    }

    /// Start building a vector entry
    ///
    /// # Example
    /// ```ignore
    /// let vec = db.vector("embeddings")
    ///     .dense(embedding)
    ///     .content("Original text content")
    ///     .metadata("source", "document.pdf")
    ///     .save()?;
    /// ```
    pub fn vector(&self, collection: impl Into<String>) -> VectorBuilder {
        VectorBuilder::new(self.store.clone(), collection)
    }

    /// Start building a table row
    ///
    /// # Example
    /// ```ignore
    /// let row = db.row("scans", vec![
    ///     ("timestamp", Value::Timestamp(now)),
    ///     ("target", Value::Text("192.168.1.0/24".into())),
    ///     ("findings", Value::Integer(42)),
    /// ]).save()?;
    /// ```
    pub fn row(&self, table: impl Into<String>, columns: Vec<(&str, Value)>) -> RowBuilder {
        RowBuilder::new(self.store.clone(), table, columns)
    }

    fn with_initialized_metadata(
        self,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        if self.options.mode == StorageMode::Persistent && !self.options.read_only {
            if let Some(path) = self.path() {
                let metadata_path = PhysicalMetadataFile::metadata_path_for(path);
                if !metadata_path.exists() {
                    self.persist_metadata()?;
                }
            }
        }
        Ok(self)
    }

    fn persist_metadata(&self) -> Result<(), Box<dyn std::error::Error>> {
        if self.options.mode != StorageMode::Persistent || self.options.read_only {
            return Ok(());
        }
        let Some(path) = self.path() else {
            return Ok(());
        };

        let previous = PhysicalMetadataFile::load_for_data_path(path).ok();
        let collection_roots = self.physical_collection_roots();
        let indexes = self.physical_index_state();
        let metadata = PhysicalMetadataFile::from_state(
            self.options.clone(),
            self.catalog_snapshot(),
            collection_roots,
            indexes,
            previous.as_ref(),
        );
        metadata.save_for_data_path(path)?;
        Ok(())
    }

    fn update_physical_metadata<T, F>(
        &self,
        mutator: F,
    ) -> Result<T, Box<dyn std::error::Error>>
    where
        F: FnOnce(&mut PhysicalMetadataFile) -> T,
    {
        if self.options.mode != StorageMode::Persistent {
            return Err("physical metadata operations require persistent mode".into());
        }
        if self.options.read_only {
            return Err("physical metadata operations are not allowed in read-only mode".into());
        }
        let Some(path) = self.path() else {
            return Err("database path is not available".into());
        };

        let mut metadata = match PhysicalMetadataFile::load_for_data_path(path) {
            Ok(metadata) => metadata,
            Err(_) => {
                self.persist_metadata()?;
                PhysicalMetadataFile::load_for_data_path(path)?
            }
        };

        if metadata.indexes.is_empty() {
            metadata.indexes = self.physical_index_state();
        }
        metadata.superblock.collection_roots = self.physical_collection_roots();

        let result = mutator(&mut metadata);
        metadata.save_for_data_path(path)?;
        Ok(result)
    }

    fn prune_export_registry(
        &self,
        exports: &mut Vec<ExportDescriptor>,
    ) {
        let retention = self.options.export_retention.max(1);
        if exports.len() <= retention {
            return;
        }

        exports.sort_by_key(|export| export.created_at_unix_ms);
        let removed: Vec<ExportDescriptor> = exports
            .drain(0..(exports.len() - retention))
            .collect();

        for export in removed {
            let _ = fs::remove_file(&export.data_path);
            let _ = fs::remove_file(&export.metadata_path);
        }
    }

    fn runtime_index_catalog(&self) -> IndexCatalog {
        let mut catalog = IndexCatalog::register_default_vector_graph(
            self.options.has_capability(Capability::Table),
            self.options.has_capability(Capability::Graph),
        );
        if self.options.has_capability(Capability::FullText) {
            catalog.register(RuntimeIndexConfig::new("text-fulltext", IndexKind::FullText));
        }
        catalog.register(RuntimeIndexConfig::new("search-hybrid", IndexKind::HybridSearch));
        catalog
    }

    fn physical_index_state(&self) -> Vec<PhysicalIndexState> {
        let snapshot = self.catalog_model_snapshot();
        let mut metrics_by_name = std::collections::BTreeMap::new();
        for metric in &snapshot.indices {
            metrics_by_name.insert(metric.name.clone(), metric.clone());
        }

        let mut states = Vec::new();
        for collection in snapshot.collections {
            for index_name in &collection.indices {
                let metric = metrics_by_name.get(index_name);
                let kind = metric
                    .map(|metric| metric.kind)
                    .unwrap_or_else(|| infer_collection_index_kind(collection.model, index_name));
                let entries = estimate_index_entries(&collection, kind);
                states.push(PhysicalIndexState {
                    name: format!("{}::{}", collection.name, index_name),
                    kind,
                    collection: Some(collection.name.clone()),
                    enabled: metric.map(|metric| metric.enabled).unwrap_or(true),
                    entries,
                    estimated_memory_bytes: estimate_index_memory(entries, kind),
                    last_refresh_ms: metric.and_then(|metric| metric.last_refresh_ms),
                    backend: index_backend_name(kind).to_string(),
                });
            }
        }

        states
    }

    fn physical_collection_roots(&self) -> BTreeMap<String, u64> {
        let mut roots = BTreeMap::new();

        for name in self.store.list_collections() {
            let Some(manager) = self.store.get_collection(&name) else {
                continue;
            };

            let stats = manager.stats();
            let mut root = fnv1a_seed();
            fnv1a_hash_value(&mut root, &name);
            fnv1a_hash_value(&mut root, &stats.total_entities);
            fnv1a_hash_value(&mut root, &stats.growing_count);
            fnv1a_hash_value(&mut root, &stats.sealed_count);
            fnv1a_hash_value(&mut root, &stats.archived_count);
            fnv1a_hash_value(&mut root, &stats.total_memory_bytes);
            fnv1a_hash_value(&mut root, &stats.seal_ops);
            fnv1a_hash_value(&mut root, &stats.compact_ops);

            let mut entities = manager.query_all(|_| true);
            entities.sort_by_key(|entity| entity.id.raw());

            for entity in entities {
                fnv1a_hash_value(&mut root, &entity.id.raw());
                fnv1a_hash_value(&mut root, &entity.kind);
                fnv1a_hash_value(&mut root, &entity.created_at);
                fnv1a_hash_value(&mut root, &entity.updated_at);
                fnv1a_hash_value(&mut root, &entity.data);
                fnv1a_hash_value(&mut root, &entity.sequence_id);
                fnv1a_hash_value(&mut root, &entity.embeddings.len());
                fnv1a_hash_value(&mut root, &entity.cross_refs.len());
            }

            roots.insert(name, root);
        }

        roots
    }

    // ========================================================================
    // Reference Helpers - For Metadata Linking
    // ========================================================================

    /// Create a reference to a table row
    pub fn table_ref(&self, table: impl Into<String>, row_id: u64) -> TableRef {
        TableRef::new(table, row_id)
    }

    /// Create a reference to a graph node
    pub fn node_ref(&self, collection: impl Into<String>, node_id: EntityId) -> NodeRef {
        NodeRef::new(collection, node_id)
    }

    /// Create a reference to a vector
    pub fn vector_ref(&self, collection: impl Into<String>, vector_id: EntityId) -> VectorRef {
        VectorRef::new(collection, vector_id)
    }

    // ========================================================================
    // Query API
    // ========================================================================

    /// Start building a query
    pub fn query(&self) -> QueryBuilder {
        QueryBuilder::new(self.store.clone())
    }

    /// Quick vector similarity search
    pub fn similar(&self, collection: &str, vector: &[f32], k: usize) -> Vec<SimilarResult> {
        let manager = match self.store.get_collection(collection) {
            Some(m) => m,
            None => return Vec::new(),
        };

        let entities = manager.query_all(|_| true);
        let mut results: Vec<SimilarResult> = entities
            .iter()
            .filter_map(|e| {
                // Check if entity has matching vector data or embeddings
                let score = match &e.data {
                    EntityData::Vector(v) => cosine_similarity(vector, &v.dense),
                    _ => {
                        // Check embeddings
                        e.embeddings
                            .iter()
                            .map(|emb| cosine_similarity(vector, &emb.vector))
                            .fold(0.0f32, f32::max)
                    }
                };
                if score > 0.0 {
                    Some(SimilarResult {
                        entity_id: e.id,
                        score,
                        entity: e.clone(),
                    })
                } else {
                    None
                }
            })
            .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(k);
        results
    }

    /// Get entity by ID from any collection
    pub fn get(&self, id: EntityId) -> Option<UnifiedEntity> {
        self.store.get_any(id).map(|(_, e)| e)
    }

    /// Get entity with its collection name
    pub fn get_with_collection(&self, id: EntityId) -> Option<(String, UnifiedEntity)> {
        self.store.get_any(id)
    }

    // ========================================================================
    // Batch Operations - Performance
    // ========================================================================

    /// Start a batch operation for bulk inserts
    pub fn batch(&self) -> BatchBuilder {
        BatchBuilder::new(self.store.clone())
    }

    // ========================================================================
    // Preprocessing
    // ========================================================================

    /// Add a preprocessor hook
    pub fn add_preprocessor(&mut self, preprocessor: Box<dyn Preprocessor>) {
        self.preprocessors.push(preprocessor);
    }

    /// Run preprocessors on an entity
    #[allow(dead_code)]
    fn preprocess(&self, entity: &mut UnifiedEntity) {
        for preprocessor in &self.preprocessors {
            preprocessor.process(entity);
        }
    }

    // ========================================================================
    // Cross-Reference Navigation
    // ========================================================================

    /// Get all entities linked FROM the given entity
    pub fn linked_from(&self, id: EntityId) -> Vec<LinkedEntity> {
        self.store
            .get_refs_from(id)
            .into_iter()
            .filter_map(|(target_id, ref_type, collection)| {
                self.store
                    .get(&collection, target_id)
                    .map(|entity| LinkedEntity {
                        entity,
                        ref_type,
                        collection,
                    })
            })
            .collect()
    }

    /// Get all entities linked TO the given entity
    pub fn linked_to(&self, id: EntityId) -> Vec<LinkedEntity> {
        self.store
            .get_refs_to(id)
            .into_iter()
            .filter_map(|(source_id, ref_type, collection)| {
                self.store
                    .get(&collection, source_id)
                    .map(|entity| LinkedEntity {
                        entity,
                        ref_type,
                        collection,
                    })
            })
            .collect()
    }

    /// Get the underlying store (for advanced operations)
    pub fn store(&self) -> Arc<UnifiedStore> {
        self.store.clone()
    }

    fn is_binary_dump(path: &Path) -> Result<bool, std::io::Error> {
        let mut file = File::open(path)?;
        let mut magic = [0u8; 4];
        let read = file.read(&mut magic)?;
        Ok(read == 4 && &magic == b"RDST")
    }
}

impl Default for RedDB {
    fn default() -> Self {
        Self::new()
    }
}

fn infer_collection_index_kind(model: CollectionModel, index_name: &str) -> IndexKind {
    match index_name {
        "graph-adjacency" => IndexKind::GraphAdjacency,
        "vector-hnsw" => IndexKind::VectorHnsw,
        "vector-inverted" => IndexKind::VectorInverted,
        "text-fulltext" => IndexKind::FullText,
        "search-hybrid" => IndexKind::HybridSearch,
        _ => match model {
            CollectionModel::Graph => IndexKind::GraphAdjacency,
            CollectionModel::Vector => IndexKind::VectorHnsw,
            CollectionModel::Document => IndexKind::FullText,
            _ => IndexKind::BTree,
        },
    }
}

fn estimate_index_entries(collection: &CollectionDescriptor, kind: IndexKind) -> usize {
    match kind {
        IndexKind::BTree => collection.entities,
        IndexKind::GraphAdjacency => collection.cross_refs.max(collection.entities),
        IndexKind::VectorHnsw | IndexKind::VectorInverted => collection.entities,
        IndexKind::FullText => collection.entities.saturating_mul(4),
        IndexKind::HybridSearch => collection.entities,
    }
}

fn estimate_index_memory(entries: usize, kind: IndexKind) -> u64 {
    let per_entry = match kind {
        IndexKind::BTree => 64,
        IndexKind::GraphAdjacency => 96,
        IndexKind::VectorHnsw => 256,
        IndexKind::VectorInverted => 128,
        IndexKind::FullText => 80,
        IndexKind::HybridSearch => 144,
    };
    (entries as u64).saturating_mul(per_entry)
}

fn index_backend_name(kind: IndexKind) -> &'static str {
    match kind {
        IndexKind::BTree => "page-btree",
        IndexKind::GraphAdjacency => "adjacency-map",
        IndexKind::VectorHnsw => "vector-hnsw",
        IndexKind::VectorInverted => "vector-ivf",
        IndexKind::FullText => "inverted-text",
        IndexKind::HybridSearch => "hybrid-score",
    }
}

fn fnv1a_seed() -> u64 {
    0xcbf29ce484222325
}

fn fnv1a_hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= *byte as u64;
        *hash = hash.wrapping_mul(0x100000001b3);
    }
}

fn fnv1a_hash_value<T: Debug>(hash: &mut u64, value: &T) {
    let rendered = format!("{value:?}");
    fnv1a_hash_bytes(hash, rendered.as_bytes());
}
