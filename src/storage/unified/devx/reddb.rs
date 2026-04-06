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
use crate::catalog::{
    consistency_report, snapshot_store_with_declarations, CatalogConsistencyReport,
    CatalogDeclarations, CatalogModelSnapshot,
    CollectionDescriptor, CollectionModel,
};
use crate::health::{storage_file_health, HealthReport};
use crate::index::{IndexCatalog, IndexConfig as RuntimeIndexConfig, IndexKind};
use crate::physical::{
    ExportDescriptor, PhysicalAnalyticsJob, PhysicalGraphProjection, PhysicalIndexState,
    PhysicalMetadataFile,
};
use crate::storage::engine::{HnswIndex, IvfConfig, IvfIndex, IvfStats, PhysicalFileHeader};
use crate::storage::schema::Value;
use crate::storage::unified::store::{
    NativeCatalogCollectionSummary, NativeCatalogSummary, NativeExportSummary,
    NativeManifestSummary, NativeMetadataStateSummary, NativePhysicalState,
    NativeRecoverySummary, NativeRegistryIndexSummary, NativeRegistryJobSummary,
    NativeRegistryProjectionSummary, NativeRegistrySummary, NativeSnapshotSummary,
    NativeVectorArtifactPageSummary, NativeVectorArtifactSummary,
};

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

#[derive(Debug, Clone)]
pub struct NativeHeaderMismatch {
    pub field: &'static str,
    pub native: String,
    pub expected: String,
}

#[derive(Debug, Clone)]
pub struct NativeHeaderInspection {
    pub native: PhysicalFileHeader,
    pub expected: PhysicalFileHeader,
    pub consistent: bool,
    pub mismatches: Vec<NativeHeaderMismatch>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeHeaderRepairPolicy {
    InSync,
    RepairNativeFromMetadata,
    NativeAheadOfMetadata,
}

#[derive(Debug, Clone)]
pub struct PhysicalAuthorityStatus {
    pub preference: String,
    pub sidecar_available: bool,
    pub native_state_available: bool,
    pub native_bootstrap_ready: bool,
    pub native_registry_complete: Option<bool>,
    pub native_recovery_complete: Option<bool>,
    pub native_catalog_complete: Option<bool>,
    pub sidecar_loaded_from: Option<String>,
    pub native_header_repair_policy: Option<String>,
    pub metadata_sequence: Option<u64>,
    pub native_sequence: Option<u64>,
    pub native_metadata_last_loaded_from: Option<String>,
    pub native_metadata_generated_at_unix_ms: Option<u128>,
}

#[derive(Debug, Clone)]
pub struct NativeVectorArtifactInspection {
    pub collection: String,
    pub artifact_kind: String,
    pub root_page: u32,
    pub page_count: u32,
    pub byte_len: u64,
    pub checksum: u64,
    pub node_count: u64,
    pub dimension: u32,
    pub max_layer: u32,
    pub total_connections: u64,
    pub avg_connections: f64,
    pub entry_point: Option<u64>,
    pub ivf_n_lists: Option<u32>,
    pub ivf_non_empty_lists: Option<u32>,
    pub ivf_trained: Option<bool>,
}

#[derive(Debug, Clone)]
pub struct NativeVectorArtifactBatchInspection {
    pub inspected_count: usize,
    pub valid_count: usize,
    pub artifacts: Vec<NativeVectorArtifactInspection>,
    pub failures: Vec<(String, String, String)>,
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

    /// Project the expected native file header from the current persisted physical metadata.
    pub fn expected_native_header(&self) -> Option<PhysicalFileHeader> {
        self.physical_metadata()
            .map(|metadata| Self::native_header_from_metadata(&metadata))
    }

    /// Compare the page-0 native header against the persisted physical metadata.
    pub fn inspect_native_header(&self) -> Option<NativeHeaderInspection> {
        let native = self.store.physical_file_header()?;
        let metadata = self.physical_metadata()?;
        Some(Self::inspect_native_header_against_metadata(native, &metadata))
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
        self.store.read_native_manifest_summary(header.manifest_page).ok()
    }

    /// Read native operational registry summary persisted in the paged file, when available.
    pub fn native_registry_summary(&self) -> Option<NativeRegistrySummary> {
        let header = self.store.physical_file_header()?;
        self.store.read_native_registry_summary(header.registry_page).ok()
    }

    /// Read native snapshot/export summary persisted in the paged file, when available.
    pub fn native_recovery_summary(&self) -> Option<NativeRecoverySummary> {
        let header = self.store.physical_file_header()?;
        self.store.read_native_recovery_summary(header.recovery_page).ok()
    }

    /// Read native catalog summary persisted in the paged file, when available.
    pub fn native_catalog_summary(&self) -> Option<NativeCatalogSummary> {
        let header = self.store.physical_file_header()?;
        self.store.read_native_catalog_summary(header.catalog_page).ok()
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
            match self.inspect_native_vector_artifact(
                &summary.collection,
                Some(&summary.artifact_kind),
            ) {
                Ok(artifact) => artifacts.push(artifact),
                Err(err) => failures.push((
                    summary.collection,
                    summary.artifact_kind,
                    err,
                )),
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
                NativeHeaderRepairPolicy::RepairNativeFromMetadata => {
                    "repair_native_from_metadata"
                }
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
            sidecar_loaded_from: sidecar.as_ref().map(|(_, source)| source.as_str().to_string()),
            native_header_repair_policy,
            metadata_sequence: sidecar.as_ref().map(|(metadata, _)| metadata.superblock.sequence),
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
            self.store.update_physical_file_header(inspection.expected)?;
            self.store.persist()?;
        }

        Ok(policy)
    }

    /// Republish the full native physical publication state from the current physical metadata view.
    pub fn repair_native_physical_state_from_metadata(
        &self,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        if self.options.mode != StorageMode::Persistent || !self.paged_mode || self.options.read_only
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
        let declarations = self.physical_metadata().map(|metadata| CatalogDeclarations {
            indexes: metadata.indexes,
            graph_projections: metadata.graph_projections,
            analytics_jobs: metadata.analytics_jobs,
        });
        snapshot_store_with_declarations(
            "reddb",
            self.store.as_ref(),
            Some(&catalog),
            declarations.as_ref(),
        )
    }

    pub fn catalog_consistency_report(&self) -> CatalogConsistencyReport {
        consistency_report(&self.catalog_model_snapshot())
    }

    /// Health report for the current database handle.
    pub fn health(&self) -> HealthReport {
        let mut report = match self.path() {
            Some(path) => storage_file_health(path),
            None => HealthReport::healthy().with_diagnostic("mode", "in-memory"),
        };
        report = report.with_diagnostic("collections", self.collections().len().to_string());
        report = report.with_diagnostic("entities", self.stats().total_entities.to_string());
        let catalog_consistency = self.catalog_consistency_report();
        report = report.with_diagnostic(
            "catalog.declared_indexes",
            catalog_consistency.declared_index_count.to_string(),
        );
        report = report.with_diagnostic(
            "catalog.operational_indexes",
            catalog_consistency.operational_index_count.to_string(),
        );
        report = report.with_diagnostic(
            "catalog.declared_graph_projections",
            catalog_consistency.declared_graph_projection_count.to_string(),
        );
        report = report.with_diagnostic(
            "catalog.operational_graph_projections",
            catalog_consistency.operational_graph_projection_count.to_string(),
        );
        report = report.with_diagnostic(
            "catalog.declared_analytics_jobs",
            catalog_consistency.declared_analytics_job_count.to_string(),
        );
        report = report.with_diagnostic(
            "catalog.operational_analytics_jobs",
            catalog_consistency.operational_analytics_job_count.to_string(),
        );
        report = report.with_diagnostic(
            "catalog.missing_operational_indexes",
            catalog_consistency.missing_operational_indexes.len().to_string(),
        );
        report = report.with_diagnostic(
            "catalog.undeclared_operational_indexes",
            catalog_consistency.undeclared_operational_indexes.len().to_string(),
        );
        report = report.with_diagnostic(
            "catalog.missing_operational_graph_projections",
            catalog_consistency
                .missing_operational_graph_projections
                .len()
                .to_string(),
        );
        report = report.with_diagnostic(
            "catalog.undeclared_operational_graph_projections",
            catalog_consistency
                .undeclared_operational_graph_projections
                .len()
                .to_string(),
        );
        report = report.with_diagnostic(
            "catalog.missing_operational_analytics_jobs",
            catalog_consistency
                .missing_operational_analytics_jobs
                .len()
                .to_string(),
        );
        report = report.with_diagnostic(
            "catalog.undeclared_operational_analytics_jobs",
            catalog_consistency
                .undeclared_operational_analytics_jobs
                .len()
                .to_string(),
        );
        if !catalog_consistency.missing_operational_indexes.is_empty()
            || !catalog_consistency.undeclared_operational_indexes.is_empty()
            || !catalog_consistency
                .missing_operational_graph_projections
                .is_empty()
            || !catalog_consistency
                .undeclared_operational_graph_projections
                .is_empty()
            || !catalog_consistency
                .missing_operational_analytics_jobs
                .is_empty()
            || !catalog_consistency
                .undeclared_operational_analytics_jobs
                .is_empty()
        {
            report.issue(
                "catalog_consistency",
                "declared and operational catalog state are diverging",
            );
        }
        report = report.with_diagnostic(
            "retention.snapshots",
            self.options.snapshot_retention.to_string(),
        );
        report = report.with_diagnostic(
            "retention.exports",
            self.options.export_retention.to_string(),
        );
        if let Some(path) = self.path() {
            let metadata_for_native = self.physical_metadata();
            if let Some(native_state) = self.native_physical_state() {
                let native = native_state.header;
                report = report.with_diagnostic(
                    "native_header.sequence",
                    native.sequence.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.format_version",
                    native.format_version.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.manifest_root",
                    native.manifest_root.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.manifest_oldest_root",
                    native.manifest_oldest_root.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.free_set_root",
                    native.free_set_root.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.manifest_page",
                    native.manifest_page.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.manifest_checksum",
                    native.manifest_checksum.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.collection_roots_page",
                    native.collection_roots_page.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.collection_roots_checksum",
                    native.collection_roots_checksum.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.collection_root_count",
                    native.collection_root_count.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.snapshot_count",
                    native.snapshot_count.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.index_count",
                    native.index_count.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.catalog_collection_count",
                    native.catalog_collection_count.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.catalog_total_entities",
                    native.catalog_total_entities.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.export_count",
                    native.export_count.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.graph_projection_count",
                    native.graph_projection_count.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.analytics_job_count",
                    native.analytics_job_count.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.manifest_event_count",
                    native.manifest_event_count.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.registry_page",
                    native.registry_page.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.registry_checksum",
                    native.registry_checksum.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.recovery_page",
                    native.recovery_page.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.recovery_checksum",
                    native.recovery_checksum.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.catalog_page",
                    native.catalog_page.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.catalog_checksum",
                    native.catalog_checksum.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.metadata_state_page",
                    native.metadata_state_page.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.metadata_state_checksum",
                    native.metadata_state_checksum.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.vector_artifact_page",
                    native.vector_artifact_page.to_string(),
                );
                report = report.with_diagnostic(
                    "native_header.vector_artifact_checksum",
                    native.vector_artifact_checksum.to_string(),
                );
                report = report.with_diagnostic(
                    "native_collection_roots.entries",
                    native_state.collection_roots.len().to_string(),
                );
                if let Some(vector_artifact_pages) = native_state.vector_artifact_pages.as_ref() {
                    report = report.with_diagnostic(
                        "native_vector_artifacts.page_count",
                        vector_artifact_pages.len().to_string(),
                    );
                    match self.inspect_native_vector_artifacts() {
                        Ok(batch) => {
                            report = report.with_diagnostic(
                                "native_vector_artifacts.inspected_count",
                                batch.inspected_count.to_string(),
                            );
                            report = report.with_diagnostic(
                                "native_vector_artifacts.valid_count",
                                batch.valid_count.to_string(),
                            );
                            report = report.with_diagnostic(
                                "native_vector_artifacts.failure_count",
                                batch.failures.len().to_string(),
                            );
                            if !batch.failures.is_empty() {
                                report.issue(
                                    "native_vector_artifacts",
                                    "one or more native vector artifacts could not be deserialized",
                                );
                            }
                        }
                        Err(err) => report.issue("native_vector_artifacts", err),
                    }
                }
                if let Some(metadata) = metadata_for_native.as_ref() {
                    if native_state.collection_roots != metadata.superblock.collection_roots {
                        report.issue(
                            "native_collection_roots",
                            "native collection roots diverge from physical metadata",
                        );
                    }
                }
                if let Some(native_registry) = native_state.registry.as_ref() {
                    report = report.with_diagnostic(
                        "native_registry.collection_count",
                        native_registry.collection_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.index_count",
                        native_registry.index_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.graph_projection_count",
                        native_registry.graph_projection_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.analytics_job_count",
                        native_registry.analytics_job_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.vector_artifact_count",
                        native_registry.vector_artifact_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.collection_sample_count",
                        native_registry.collection_names.len().to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.collections_complete",
                        native_registry.collections_complete.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.omitted_collection_count",
                        native_registry.omitted_collection_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.index_sample_count",
                        native_registry.indexes.len().to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.indexes_complete",
                        native_registry.indexes_complete.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.omitted_index_count",
                        native_registry.omitted_index_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.graph_projection_sample_count",
                        native_registry.graph_projections.len().to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.graph_projections_complete",
                        native_registry.graph_projections_complete.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.omitted_graph_projection_count",
                        native_registry.omitted_graph_projection_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.analytics_job_sample_count",
                        native_registry.analytics_jobs.len().to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.analytics_jobs_complete",
                        native_registry.analytics_jobs_complete.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.vector_artifacts_complete",
                        native_registry.vector_artifacts_complete.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.omitted_analytics_job_count",
                        native_registry.omitted_analytics_job_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_registry.omitted_vector_artifact_count",
                        native_registry.omitted_vector_artifact_count.to_string(),
                    );

                    if let Some(metadata) = metadata_for_native.as_ref() {
                        let expected_registry = self.native_registry_summary_from_metadata(metadata);
                        if native_registry != expected_registry {
                            report.issue(
                                "native_registry",
                                "native registry summary diverges from physical metadata",
                            );
                        }
                    }
                }
                if let Some(native_catalog) = native_state.catalog.as_ref() {
                    report = report.with_diagnostic(
                        "native_catalog.collection_count",
                        native_catalog.collection_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_catalog.total_entities",
                        native_catalog.total_entities.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_catalog.collection_sample_count",
                        native_catalog.collections.len().to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_catalog.collections_complete",
                        native_catalog.collections_complete.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_catalog.omitted_collection_count",
                        native_catalog.omitted_collection_count.to_string(),
                    );
                    if let Some(metadata) = metadata_for_native.as_ref() {
                        let expected_catalog = Self::native_catalog_summary_from_metadata(metadata);
                        if native_catalog != expected_catalog {
                            report.issue(
                                "native_catalog",
                                "native catalog summary diverges from physical metadata",
                            );
                        }
                    }
                }
                if let Some(metadata_state) = native_state.metadata_state.as_ref() {
                    report = report.with_diagnostic(
                        "native_metadata_state.protocol_version",
                        metadata_state.protocol_version.clone(),
                    );
                    report = report.with_diagnostic(
                        "native_metadata_state.generated_at_unix_ms",
                        metadata_state.generated_at_unix_ms.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_metadata_state.last_loaded_from",
                        metadata_state
                            .last_loaded_from
                            .clone()
                            .unwrap_or_else(|| "null".to_string()),
                    );
                    report = report.with_diagnostic(
                        "native_metadata_state.last_healed_at_unix_ms",
                        metadata_state
                            .last_healed_at_unix_ms
                            .map(|value| value.to_string())
                            .unwrap_or_else(|| "null".to_string()),
                    );
                    if let Some(metadata) = metadata_for_native.as_ref() {
                        let expected_metadata_state =
                            Self::native_metadata_state_summary_from_metadata(metadata);
                        if metadata_state != &expected_metadata_state {
                            report.issue(
                                "native_metadata_state",
                                "native metadata state summary diverges from physical metadata",
                            );
                        }
                    }
                }
                report = report.with_diagnostic(
                    "native_bootstrap.ready",
                    Self::native_state_is_bootstrap_complete(&native_state).to_string(),
                );
                if !Self::native_state_is_bootstrap_complete(&native_state)
                    && metadata_for_native.is_none()
                {
                    report.issue(
                        "native_bootstrap",
                        "native physical publication is partial and cannot rebuild physical metadata without a sidecar",
                    );
                }
                if let Some(native_recovery) = native_state.recovery.as_ref() {
                    report = report.with_diagnostic(
                        "native_recovery.snapshot_count",
                        native_recovery.snapshot_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_recovery.export_count",
                        native_recovery.export_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_recovery.snapshot_sample_count",
                        native_recovery.snapshots.len().to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_recovery.snapshots_complete",
                        native_recovery.snapshots_complete.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_recovery.omitted_snapshot_count",
                        native_recovery.omitted_snapshot_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_recovery.export_sample_count",
                        native_recovery.exports.len().to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_recovery.exports_complete",
                        native_recovery.exports_complete.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_recovery.omitted_export_count",
                        native_recovery.omitted_export_count.to_string(),
                    );
                    if let Some(metadata) = metadata_for_native.as_ref() {
                        let expected_recovery = Self::native_recovery_summary_from_metadata(metadata);
                        if native_recovery != expected_recovery {
                            report.issue(
                                "native_recovery",
                                "native recovery summary diverges from physical metadata",
                            );
                        }
                    }
                }
                if let Some(native_manifest) = native_state.manifest.as_ref() {
                    report = report.with_diagnostic(
                        "native_manifest.sequence",
                        native_manifest.sequence.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_manifest.event_count",
                        native_manifest.event_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_manifest.events_complete",
                        native_manifest.events_complete.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_manifest.omitted_event_count",
                        native_manifest.omitted_event_count.to_string(),
                    );
                    report = report.with_diagnostic(
                        "native_manifest.sample_count",
                        native_manifest.recent_events.len().to_string(),
                    );
                    if let Some(metadata) = metadata_for_native.as_ref() {
                        if native_manifest.event_count != metadata.manifest_events.len() as u32 {
                            report.issue(
                                "native_manifest",
                                "native manifest summary diverges from physical metadata",
                            );
                        }
                    }
                }
            } else if self.store.physical_file_header().is_some() {
                report.issue(
                    "native_state",
                    "native physical state is not fully readable from the paged file",
                );
            }
            let metadata_path = PhysicalMetadataFile::metadata_path_for(path);
            let metadata_binary_path = PhysicalMetadataFile::metadata_binary_path_for(path);
            report = report.with_diagnostic("metadata.path", metadata_path.display().to_string());
            report = report.with_diagnostic(
                "metadata.binary_path",
                metadata_binary_path.display().to_string(),
            );
            report = report.with_diagnostic("metadata.exists", metadata_path.exists().to_string());
            report = report.with_diagnostic(
                "metadata.binary_exists",
                metadata_binary_path.exists().to_string(),
            );
            if let Some(preference) = self.physical_metadata_preference() {
                report = report.with_diagnostic("metadata.preference", preference);
            }
            if let Ok((metadata, source)) =
                PhysicalMetadataFile::load_for_data_path_with_source(path)
            {
                let journal_count = PhysicalMetadataFile::journal_paths_for_data_path(path)
                    .map(|paths| paths.len())
                    .unwrap_or(0);
                report = report.with_diagnostic("metadata.loaded_from", source.as_str());
                report = report.with_diagnostic(
                    "metadata.journal_entries",
                    journal_count.to_string(),
                );
                report = report.with_diagnostic(
                    "metadata.last_loaded_from",
                    metadata
                        .last_loaded_from
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string()),
                );
                report = report.with_diagnostic(
                    "metadata.last_healed_at_unix_ms",
                    metadata
                        .last_healed_at_unix_ms
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "null".to_string()),
                );
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
                if let Some(native) = self.store.physical_file_header() {
                    let inspection = Self::inspect_native_header_against_metadata(native, &metadata);
                    report = report.with_diagnostic(
                        "native_header.matches_metadata",
                        inspection.consistent.to_string(),
                    );
                    let policy = Self::repair_policy_for_inspection(&inspection);
                    report = report.with_diagnostic(
                        "native_header.repair_policy",
                        match policy {
                            NativeHeaderRepairPolicy::InSync => "in_sync",
                            NativeHeaderRepairPolicy::RepairNativeFromMetadata => {
                                "repair_native_from_metadata"
                            }
                            NativeHeaderRepairPolicy::NativeAheadOfMetadata => {
                                "native_ahead_of_metadata"
                            }
                        },
                    );
                    if !inspection.consistent {
                        match policy {
                            NativeHeaderRepairPolicy::RepairNativeFromMetadata => {
                                report.issue(
                                    "native_header",
                                    format!(
                                        "native header diverges from physical metadata on {} field(s); repairable from metadata",
                                        inspection.mismatches.len()
                                    ),
                                );
                            }
                            NativeHeaderRepairPolicy::NativeAheadOfMetadata => {
                                report.issue(
                                    "native_header",
                                    format!(
                                        "native header diverges from physical metadata on {} field(s); native header appears ahead of metadata",
                                        inspection.mismatches.len()
                                    ),
                                );
                            }
                            NativeHeaderRepairPolicy::InSync => {}
                        }
                        for mismatch in inspection.mismatches {
                            report = report.with_diagnostic(
                                format!("native_header.mismatch.{}", mismatch.field),
                                format!(
                                    "native={} expected={}",
                                    mismatch.native, mismatch.expected
                                ),
                            );
                        }
                    }
                }
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

    /// Load the current physical metadata view, bootstrapping from native state when needed.
    pub fn physical_metadata(&self) -> Option<PhysicalMetadataFile> {
        self.load_or_bootstrap_physical_metadata(!self.options.read_only)
            .ok()
    }

    /// Physical index registry derived for the current database state.
    pub fn physical_indexes(&self) -> Vec<PhysicalIndexState> {
        let indexes = self
            .physical_metadata()
            .map(|metadata| metadata.indexes)
            .filter(|indexes| !indexes.is_empty())
            .or_else(|| {
                self.native_physical_state()
                    .map(|state| self.physical_index_state_from_native_state(&state, None))
            })
            .unwrap_or_else(|| self.physical_index_state());
        self.reconcile_index_states_with_native_artifacts(indexes)
    }

    /// List registered named exports from the current physical metadata view.
    pub fn exports(&self) -> Vec<ExportDescriptor> {
        self.physical_metadata()
            .map(|metadata| metadata.exports)
            .or_else(|| self.native_physical_state().map(|state| self.exports_from_native_state(&state)))
            .unwrap_or_default()
    }

    /// List recorded snapshots from the current physical metadata view.
    pub fn snapshots(&self) -> Vec<crate::physical::SnapshotDescriptor> {
        self.physical_metadata()
            .map(|metadata| metadata.snapshots)
            .or_else(|| self.native_physical_state().map(|state| self.snapshots_from_native_state(&state)))
            .unwrap_or_default()
    }

    /// List persisted named graph projections from the current physical metadata view.
    pub fn graph_projections(&self) -> Vec<PhysicalGraphProjection> {
        self.physical_metadata()
            .map(|metadata| metadata.graph_projections)
            .or_else(|| {
                self.native_physical_state()
                    .map(|state| self.graph_projections_from_native_state(&state))
            })
            .unwrap_or_default()
    }

    /// List graph projections declared in the catalog view.
    pub fn declared_graph_projections(&self) -> Vec<PhysicalGraphProjection> {
        self.catalog_model_snapshot().declared_graph_projections
    }

    /// List graph projections currently observed in the operational view.
    pub fn operational_graph_projections(&self) -> Vec<PhysicalGraphProjection> {
        self.catalog_model_snapshot().operational_graph_projections
    }

    /// List persisted analytics job metadata from the current physical metadata view.
    pub fn analytics_jobs(&self) -> Vec<PhysicalAnalyticsJob> {
        self.physical_metadata()
            .map(|metadata| metadata.analytics_jobs)
            .or_else(|| {
                self.native_physical_state()
                    .map(|state| self.analytics_jobs_from_native_state(&state))
            })
            .unwrap_or_default()
    }

    /// List analytics jobs declared in the catalog view.
    pub fn declared_analytics_jobs(&self) -> Vec<PhysicalAnalyticsJob> {
        self.catalog_model_snapshot().declared_analytics_jobs
    }

    /// List analytics jobs currently observed in the operational view.
    pub fn operational_analytics_jobs(&self) -> Vec<PhysicalAnalyticsJob> {
        self.catalog_model_snapshot().operational_analytics_jobs
    }

    /// List indexes declared in the catalog view.
    pub fn declared_indexes(&self) -> Vec<PhysicalIndexState> {
        self.catalog_model_snapshot().declared_indexes
    }

    /// List indexes currently observed in the operational view.
    pub fn operational_indexes(&self) -> Vec<PhysicalIndexState> {
        self.catalog_model_snapshot().operational_indexes
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
        if let Some(projection_name) = projection.as_deref() {
            if !self.graph_projection_is_declared(projection_name) {
                return Err(format!(
                    "graph projection '{projection_name}' is not declared in physical metadata"
                )
                .into());
            }
        }

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

        let mut metadata = self.load_or_bootstrap_physical_metadata(true)?;
        let export_data_path = PhysicalMetadataFile::export_data_path_for(path, &name);
        let export_metadata_path = PhysicalMetadataFile::metadata_path_for(&export_data_path);
        let export_metadata_binary_path =
            PhysicalMetadataFile::metadata_binary_path_for(&export_data_path);

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
        metadata.save_to_binary_path(&export_metadata_binary_path)?;
        metadata.save_to_path(&export_metadata_path)?;

        Ok(descriptor)
    }

    /// Enable or disable a physical index entry in the persisted registry.
    pub fn set_index_enabled(
        &self,
        name: &str,
        enabled: bool,
    ) -> Result<Option<PhysicalIndexState>, Box<dyn std::error::Error>> {
        if !self.index_is_declared(name) {
            return Err(format!("index '{name}' is not declared in physical metadata").into());
        }
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
        if !self.index_is_declared(name) {
            return Err(format!("index '{name}' is not declared in physical metadata").into());
        }
        let warmed_artifact = self
            .physical_indexes()
            .into_iter()
            .find(|index| index.name == name)
            .map(|mut index| {
                self.warmup_native_vector_artifact_for_index(&index)?;
                self.apply_runtime_native_artifact_to_index_state(&mut index)?;
                Ok::<_, String>(index)
            })
            .transpose()
            .map_err(|err| -> Box<dyn std::error::Error> { err.into() })?;

        self.update_physical_metadata(|metadata| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();
            if let Some(index) = metadata.indexes.iter_mut().find(|index| index.name == name) {
                if let Some(warmed) = warmed_artifact.as_ref() {
                    index.entries = warmed.entries;
                    index.estimated_memory_bytes = warmed.estimated_memory_bytes;
                    index.backend = warmed.backend.clone();
                }
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
        let fresh = self.reconcile_index_states_with_native_artifacts(self.physical_index_state());
        self.update_physical_metadata(|metadata| {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis();

            let mut affected = Vec::new();
            let declared = metadata.indexes.clone();
            for declared_index in declared {
                let matches_collection = collection.map_or(true, |collection_name| {
                    declared_index.collection.as_deref() == Some(collection_name)
                });
                if !matches_collection {
                    continue;
                }

                let mut rebuilt = fresh
                    .iter()
                    .find(|index| index.name == declared_index.name)
                    .cloned()
                    .unwrap_or_else(|| {
                        let mut index = declared_index.clone();
                        index.build_state = "declared-unbuilt".to_string();
                        index
                    });
                rebuilt.enabled = declared_index.enabled;
                rebuilt.artifact_kind = rebuilt
                    .artifact_kind
                    .or_else(|| declared_index.artifact_kind.clone());
                rebuilt.artifact_root_page =
                    rebuilt.artifact_root_page.or(declared_index.artifact_root_page);
                rebuilt.artifact_checksum =
                    rebuilt.artifact_checksum.or(declared_index.artifact_checksum);
                if rebuilt.build_state == "catalog-derived" {
                    rebuilt.build_state = declared_index.build_state.clone();
                }
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

        let Ok(mut metadata) = self.load_or_bootstrap_physical_metadata(true) else {
            return Ok(());
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
            if self.load_or_bootstrap_physical_metadata(true).is_err() {
                self.persist_metadata()?;
            }
            let _ = self.repair_native_header_from_metadata();
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

        let previous = self.load_or_bootstrap_physical_metadata(false).ok();
        let collection_roots = self.physical_collection_roots();
        let indexes = self
            .native_physical_state()
            .map(|state| self.physical_index_state_from_native_state(&state, previous.as_ref()))
            .unwrap_or_else(|| self.physical_index_state());
        let metadata = PhysicalMetadataFile::from_state(
            self.options.clone(),
            self.catalog_snapshot(),
            collection_roots,
            indexes,
            previous.as_ref(),
        );
        metadata.save_for_data_path(path)?;
        self.persist_native_physical_header(&metadata)?;
        Ok(())
    }

    fn bootstrap_metadata_from_native_state(
        &self,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        if self.options.mode != StorageMode::Persistent || self.options.read_only {
            return Ok(false);
        }
        let Some(path) = self.path() else {
            return Ok(false);
        };
        let Some(native_state) = self.native_physical_state() else {
            return Ok(false);
        };
        if !Self::native_state_is_bootstrap_complete(&native_state) {
            return Ok(false);
        }

        let previous = PhysicalMetadataFile::load_for_data_path(path).ok();
        let metadata = self.metadata_from_native_state(&native_state, previous.as_ref());
        metadata.save_for_data_path(path)?;
        self.persist_native_physical_header(&metadata)?;
        Ok(true)
    }

    /// Rebuild the external physical metadata view from the native state published in the
    /// paged database file.
    pub fn rebuild_physical_metadata_from_native_state(
        &self,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        self.bootstrap_metadata_from_native_state()
    }

    fn native_state_is_bootstrap_complete(native_state: &NativePhysicalState) -> bool {
        let registry_complete = native_state.registry.as_ref().map(|registry| {
            registry.collections_complete
                && registry.indexes_complete
                && registry.graph_projections_complete
                && registry.analytics_jobs_complete
                && registry.vector_artifacts_complete
        });
        let recovery_complete = native_state
            .recovery
            .as_ref()
            .map(|recovery| recovery.snapshots_complete && recovery.exports_complete);
        let catalog_complete = native_state
            .catalog
            .as_ref()
            .map(|catalog| catalog.collections_complete);

        registry_complete == Some(true)
            && recovery_complete == Some(true)
            && catalog_complete == Some(true)
    }

    fn load_or_bootstrap_physical_metadata(
        &self,
        persist_bootstrapped: bool,
    ) -> Result<PhysicalMetadataFile, Box<dyn std::error::Error>> {
        if self.options.mode != StorageMode::Persistent {
            return Err("physical metadata requires persistent mode".into());
        }
        let Some(path) = self.path() else {
            return Err("database path is not available".into());
        };
        let native_state = self.native_physical_state();

        match PhysicalMetadataFile::load_for_data_path(path) {
            Ok(metadata) => {
                if let Some(native_state) = native_state.as_ref() {
                    let inspection =
                        Self::inspect_native_header_against_metadata(native_state.header, &metadata);
                    if Self::repair_policy_for_inspection(&inspection)
                        == NativeHeaderRepairPolicy::NativeAheadOfMetadata
                    {
                        let bootstrapped =
                            self.metadata_from_native_state(native_state, Some(&metadata));
                        if persist_bootstrapped && !self.options.read_only {
                            bootstrapped.save_for_data_path(path)?;
                            self.persist_native_physical_header(&bootstrapped)?;
                        }
                        return Ok(bootstrapped);
                    }
                }
                Ok(metadata)
            }
            Err(err) => {
                let Some(native_state) = native_state else {
                    return Err(err.into());
                };
                if !Self::native_state_is_bootstrap_complete(&native_state) {
                    return Err(err.into());
                }
                let metadata = self.metadata_from_native_state(&native_state, None);
                if persist_bootstrapped && !self.options.read_only {
                    metadata.save_for_data_path(path)?;
                    self.persist_native_physical_header(&metadata)?;
                }
                Ok(metadata)
            }
        }
    }

    fn physical_metadata_preference(&self) -> Option<&'static str> {
        let path = self.path()?;
        let native_state = self.native_physical_state();
        let metadata = PhysicalMetadataFile::load_for_data_path(path).ok();

        match (metadata, native_state) {
            (Some(metadata), Some(native_state)) => {
                let inspection =
                    Self::inspect_native_header_against_metadata(native_state.header, &metadata);
                match Self::repair_policy_for_inspection(&inspection) {
                    NativeHeaderRepairPolicy::InSync => Some("sidecar_current"),
                    NativeHeaderRepairPolicy::RepairNativeFromMetadata => Some("sidecar_current"),
                    NativeHeaderRepairPolicy::NativeAheadOfMetadata => Some("native_ahead"),
                }
            }
            (Some(_), None) => Some("sidecar_only"),
            (None, Some(_)) => Some("sidecar_missing_native_available"),
            (None, None) => Some("sidecar_missing_no_native"),
        }
    }

    fn metadata_from_native_state(
        &self,
        native_state: &NativePhysicalState,
        previous: Option<&PhysicalMetadataFile>,
    ) -> PhysicalMetadataFile {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let catalog = self.catalog_snapshot();
        let catalog_name = catalog.name.clone();
        let catalog_total_entities = catalog.total_entities;
        let catalog_total_collections = catalog.total_collections;
        let indexes = self.physical_index_state();

        let mut manifest = crate::api::SchemaManifest::now(
            self.options.clone(),
            catalog.total_collections,
        );
        manifest.updated_at_unix_ms = now;

        let manifest_events = native_state
            .manifest
            .as_ref()
            .map(|summary| {
                summary
                    .recent_events
                    .iter()
                    .map(|event| crate::physical::ManifestEvent {
                        collection: event.collection.clone(),
                        object_key: event.object_key.clone(),
                        kind: match event.kind.as_str() {
                            "insert" => crate::physical::ManifestEventKind::Insert,
                            "update" => crate::physical::ManifestEventKind::Update,
                            "remove" => crate::physical::ManifestEventKind::Remove,
                            _ => crate::physical::ManifestEventKind::Checkpoint,
                        },
                        block: crate::physical::BlockReference {
                            index: event.block_index,
                            checksum: event.block_checksum,
                        },
                        snapshot_min: event.snapshot_min,
                        snapshot_max: event.snapshot_max,
                    })
                    .collect()
            })
            .unwrap_or_default();

        let graph_projections = native_state
            .registry
            .as_ref()
            .and_then(|registry| {
                registry.graph_projections_complete.then(|| {
                    registry
                        .graph_projections
                        .iter()
                        .map(|projection| crate::physical::PhysicalGraphProjection {
                            name: projection.name.clone(),
                            created_at_unix_ms: projection.created_at_unix_ms,
                            updated_at_unix_ms: projection.updated_at_unix_ms,
                            source: projection.source.clone(),
                            node_labels: projection.node_labels.clone(),
                            node_types: projection.node_types.clone(),
                            edge_labels: projection.edge_labels.clone(),
                            last_materialized_sequence: projection.last_materialized_sequence,
                        })
                        .collect()
                })
            })
            .or_else(|| previous.map(|metadata| metadata.graph_projections.clone()))
            .unwrap_or_default();

        let analytics_jobs = native_state
            .registry
            .as_ref()
            .and_then(|registry| {
                registry.analytics_jobs_complete.then(|| {
                    registry
                        .analytics_jobs
                        .iter()
                        .map(|job| crate::physical::PhysicalAnalyticsJob {
                            id: job.id.clone(),
                            kind: job.kind.clone(),
                            state: job.state.clone(),
                            projection: job.projection.clone(),
                            created_at_unix_ms: job.created_at_unix_ms,
                            updated_at_unix_ms: job.updated_at_unix_ms,
                            last_run_sequence: job.last_run_sequence,
                            metadata: job.metadata.clone(),
                        })
                        .collect()
                })
            })
            .or_else(|| previous.map(|metadata| metadata.analytics_jobs.clone()))
            .unwrap_or_default();

        let exports = native_state
            .recovery
            .as_ref()
            .and_then(|recovery| {
                recovery.exports_complete.then(|| {
                    recovery
                        .exports
                        .iter()
                        .map(|export| crate::physical::ExportDescriptor {
                            name: export.name.clone(),
                            created_at_unix_ms: export.created_at_unix_ms,
                            snapshot_id: export.snapshot_id,
                            superblock_sequence: export.superblock_sequence,
                            data_path: self
                                .path()
                                .map(|path| {
                                    crate::physical::PhysicalMetadataFile::export_data_path_for(
                                        path,
                                        &export.name,
                                    )
                                    .display()
                                    .to_string()
                                })
                                .unwrap_or_default(),
                            metadata_path: self
                                .path()
                                .map(|path| {
                                    let export_data_path =
                                        crate::physical::PhysicalMetadataFile::export_data_path_for(
                                            path,
                                            &export.name,
                                        );
                                    crate::physical::PhysicalMetadataFile::metadata_path_for(
                                        &export_data_path,
                                    )
                                    .display()
                                    .to_string()
                                })
                                .unwrap_or_default(),
                            collection_count: export.collection_count as usize,
                            total_entities: export.total_entities as usize,
                        })
                        .collect()
                })
            })
            .or_else(|| previous.map(|metadata| metadata.exports.clone()))
            .unwrap_or_default();

        let snapshots = native_state
            .recovery
            .as_ref()
            .and_then(|recovery| {
                recovery.snapshots_complete.then(|| {
                    recovery
                        .snapshots
                        .iter()
                        .map(|snapshot| crate::physical::SnapshotDescriptor {
                            snapshot_id: snapshot.snapshot_id,
                            created_at_unix_ms: snapshot.created_at_unix_ms,
                            superblock_sequence: snapshot.superblock_sequence,
                            collection_count: snapshot.collection_count as usize,
                            total_entities: snapshot.total_entities as usize,
                        })
                        .collect()
                })
            })
            .or_else(|| previous.map(|metadata| metadata.snapshots.clone()))
            .unwrap_or_else(|| {
                vec![crate::physical::SnapshotDescriptor {
                    snapshot_id: native_state.header.sequence,
                    created_at_unix_ms: now,
                    superblock_sequence: native_state.header.sequence,
                    collection_count: catalog_total_collections,
                    total_entities: catalog_total_entities,
                }]
            });

        let catalog_stats = native_state
            .catalog
            .as_ref()
            .and_then(|native_catalog| {
                native_catalog.collections_complete.then(|| {
                    native_catalog
                        .collections
                        .iter()
                        .map(|collection| {
                            (
                                collection.name.clone(),
                                crate::api::CollectionStats {
                                    entities: collection.entities as usize,
                                    cross_refs: collection.cross_refs as usize,
                                    segments: collection.segments as usize,
                                },
                            )
                        })
                        .collect::<BTreeMap<_, _>>()
                })
            })
            .or_else(|| previous.map(|metadata| metadata.catalog.stats_by_collection.clone()))
            .unwrap_or_else(|| catalog.stats_by_collection.clone());

        PhysicalMetadataFile {
            protocol_version: crate::physical::PHYSICAL_METADATA_PROTOCOL_VERSION.to_string(),
            generated_at_unix_ms: now,
            last_loaded_from: Some("native_bootstrap".to_string()),
            last_healed_at_unix_ms: Some(now),
            manifest,
            catalog: crate::api::CatalogSnapshot {
                name: catalog_name,
                total_entities: native_state
                    .catalog
                    .as_ref()
                    .map(|summary| summary.total_entities as usize)
                    .unwrap_or(catalog_total_entities),
                total_collections: native_state
                    .catalog
                    .as_ref()
                    .map(|summary| summary.collection_count as usize)
                    .unwrap_or(catalog_total_collections),
                stats_by_collection: catalog_stats,
                updated_at: SystemTime::now(),
            },
            manifest_events,
            indexes,
            graph_projections,
            analytics_jobs,
            exports,
            superblock: crate::physical::SuperblockHeader {
                format_version: native_state.header.format_version,
                sequence: native_state.header.sequence,
                copies: crate::physical::DEFAULT_SUPERBLOCK_COPIES,
                manifest: crate::physical::ManifestPointers {
                    oldest: crate::physical::BlockReference {
                        index: native_state.header.manifest_oldest_root,
                        checksum: 0,
                    },
                    newest: crate::physical::BlockReference {
                        index: native_state.header.manifest_root,
                        checksum: 0,
                    },
                },
                free_set: crate::physical::BlockReference {
                    index: native_state.header.free_set_root,
                    checksum: 0,
                },
                collection_roots: native_state.collection_roots.clone(),
            },
            snapshots,
        }
    }

    fn reconcile_index_states_with_native_artifacts(
        &self,
        mut indexes: Vec<PhysicalIndexState>,
    ) -> Vec<PhysicalIndexState> {
        let native_artifacts = self
            .native_physical_state()
            .and_then(|state| state.registry)
            .map(|registry| registry.vector_artifacts)
            .unwrap_or_default();
        for index in &mut indexes {
            let Some(collection) = index.collection.as_deref() else {
                continue;
            };
            let Some(artifact_kind) = Self::native_artifact_kind_for_index(index.kind) else {
                continue;
            };
            let Some(artifact) = native_artifacts.iter().find(|artifact| {
                artifact.collection == collection && artifact.artifact_kind == artifact_kind
            }) else {
                index.build_state = "metadata-only".to_string();
                continue;
            };
            index.entries = artifact.vector_count as usize;
            index.estimated_memory_bytes = artifact.serialized_bytes;
            index.backend = format!("{}+native-artifact", index_backend_name(index.kind));
            index.artifact_kind = Some(artifact.artifact_kind.clone());
            index.artifact_checksum = Some(artifact.checksum);
            index.build_state = "artifact-published".to_string();
            if let Some(pages) = self.native_vector_artifact_pages() {
                index.artifact_root_page = pages
                    .into_iter()
                    .find(|page| {
                        page.collection == artifact.collection
                            && page.artifact_kind == artifact.artifact_kind
                    })
                    .map(|page| page.root_page);
            }
        }
        indexes
    }

    fn warmup_native_vector_artifact_for_index(
        &self,
        index: &PhysicalIndexState,
    ) -> Result<(), String> {
        let Some(collection) = index.collection.as_deref() else {
            return Ok(());
        };
        let Some(artifact_kind) = Self::native_artifact_kind_for_index(index.kind) else {
            return Ok(());
        };
        self.warmup_native_vector_artifact(collection, Some(artifact_kind))?;
        Ok(())
    }

    fn apply_runtime_native_artifact_to_index_state(
        &self,
        index: &mut PhysicalIndexState,
    ) -> Result<(), String> {
        let Some(collection) = index.collection.as_deref() else {
            return Ok(());
        };
        let Some(artifact_kind) = Self::native_artifact_kind_for_index(index.kind) else {
            return Ok(());
        };
        let artifact = self.inspect_native_vector_artifact(collection, Some(artifact_kind))?;
        index.entries = artifact.node_count as usize;
        index.estimated_memory_bytes = artifact.byte_len;
        index.backend = format!("{}+native-artifact", index_backend_name(index.kind));
        index.artifact_kind = Some(artifact.artifact_kind.clone());
        index.artifact_checksum = Some(artifact.checksum);
        index.build_state = "ready".to_string();
        index.artifact_root_page = self
            .native_vector_artifact_pages()
            .and_then(|pages| {
                pages.into_iter().find(|page| {
                    page.collection == artifact.collection
                        && page.artifact_kind == artifact.artifact_kind
                })
            })
            .map(|page| page.root_page);
        Ok(())
    }

    fn physical_index_state_from_native_state(
        &self,
        native_state: &NativePhysicalState,
        previous: Option<&PhysicalMetadataFile>,
    ) -> Vec<PhysicalIndexState> {
        let mut fresh = self.physical_index_state();
        let Some(registry) = native_state.registry.as_ref() else {
            if let Some(previous) = previous {
                for index in &previous.indexes {
                    if !fresh.iter().any(|candidate| candidate.name == index.name) {
                        fresh.push(index.clone());
                    }
                }
            }
            return fresh;
        };

        for index in &mut fresh {
            if let Some(native) = registry.indexes.iter().find(|candidate| candidate.name == index.name)
            {
                index.enabled = native.enabled;
                index.last_refresh_ms = native.last_refresh_ms;
                index.backend = native.backend.clone();
                index.entries = native.entries as usize;
                index.estimated_memory_bytes = native.estimated_memory_bytes;
                if index.artifact_kind.is_none() {
                    index.artifact_kind = Self::native_artifact_kind_for_index(index.kind)
                        .map(|value| value.to_string());
                }
                if index.build_state == "catalog-derived" {
                    index.build_state = "registry-loaded".to_string();
                }
            }
        }

        for native in &registry.indexes {
            if fresh.iter().any(|index| index.name == native.name) {
                continue;
            }
            let Some(kind) = Self::index_kind_from_str(&native.kind) else {
                continue;
            };
            fresh.push(PhysicalIndexState {
                name: native.name.clone(),
                kind,
                collection: native.collection.clone(),
                enabled: native.enabled,
                entries: native.entries as usize,
                estimated_memory_bytes: native.estimated_memory_bytes,
                last_refresh_ms: native.last_refresh_ms,
                backend: native.backend.clone(),
                artifact_kind: Self::native_artifact_kind_for_index(kind)
                    .map(|value| value.to_string()),
                artifact_root_page: None,
                artifact_checksum: None,
                build_state: "registry-loaded".to_string(),
            });
        }

        if !registry.indexes_complete {
            if let Some(previous) = previous {
                for index in &previous.indexes {
                    if !fresh.iter().any(|candidate| candidate.name == index.name) {
                        fresh.push(index.clone());
                    }
                }
            }
        }

        fresh
    }

    fn graph_projections_from_native_state(
        &self,
        native_state: &NativePhysicalState,
    ) -> Vec<PhysicalGraphProjection> {
        native_state
            .registry
            .as_ref()
            .map(|registry| {
                registry
                    .graph_projections
                    .iter()
                    .map(|projection| PhysicalGraphProjection {
                        name: projection.name.clone(),
                        created_at_unix_ms: projection.created_at_unix_ms,
                        updated_at_unix_ms: projection.updated_at_unix_ms,
                        source: projection.source.clone(),
                        node_labels: projection.node_labels.clone(),
                        node_types: projection.node_types.clone(),
                        edge_labels: projection.edge_labels.clone(),
                        last_materialized_sequence: projection.last_materialized_sequence,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn analytics_jobs_from_native_state(
        &self,
        native_state: &NativePhysicalState,
    ) -> Vec<PhysicalAnalyticsJob> {
        native_state
            .registry
            .as_ref()
            .map(|registry| {
                registry
                    .analytics_jobs
                    .iter()
                    .map(|job| PhysicalAnalyticsJob {
                        id: job.id.clone(),
                        kind: job.kind.clone(),
                        state: job.state.clone(),
                        projection: job.projection.clone(),
                        created_at_unix_ms: job.created_at_unix_ms,
                        updated_at_unix_ms: job.updated_at_unix_ms,
                        last_run_sequence: job.last_run_sequence,
                        metadata: job.metadata.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn exports_from_native_state(
        &self,
        native_state: &NativePhysicalState,
    ) -> Vec<ExportDescriptor> {
        native_state
            .recovery
            .as_ref()
            .map(|recovery| {
                recovery
                    .exports
                    .iter()
                    .map(|export| ExportDescriptor {
                        name: export.name.clone(),
                        created_at_unix_ms: export.created_at_unix_ms,
                        snapshot_id: export.snapshot_id,
                        superblock_sequence: export.superblock_sequence,
                        data_path: self
                            .path()
                            .map(|path| {
                                crate::physical::PhysicalMetadataFile::export_data_path_for(
                                    path,
                                    &export.name,
                                )
                                .display()
                                .to_string()
                            })
                            .unwrap_or_default(),
                        metadata_path: self
                            .path()
                            .map(|path| {
                                let export_data_path =
                                    crate::physical::PhysicalMetadataFile::export_data_path_for(
                                        path,
                                        &export.name,
                                    );
                                crate::physical::PhysicalMetadataFile::metadata_path_for(
                                    &export_data_path,
                                )
                                .display()
                                .to_string()
                            })
                            .unwrap_or_default(),
                        collection_count: export.collection_count as usize,
                        total_entities: export.total_entities as usize,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn snapshots_from_native_state(
        &self,
        native_state: &NativePhysicalState,
    ) -> Vec<crate::physical::SnapshotDescriptor> {
        native_state
            .recovery
            .as_ref()
            .map(|recovery| {
                recovery
                    .snapshots
                    .iter()
                    .map(|snapshot| crate::physical::SnapshotDescriptor {
                        snapshot_id: snapshot.snapshot_id,
                        created_at_unix_ms: snapshot.created_at_unix_ms,
                        superblock_sequence: snapshot.superblock_sequence,
                        collection_count: snapshot.collection_count as usize,
                        total_entities: snapshot.total_entities as usize,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn index_kind_from_str(value: &str) -> Option<crate::index::IndexKind> {
        match value {
            "btree" => Some(crate::index::IndexKind::BTree),
            "vector.hnsw" => Some(crate::index::IndexKind::VectorHnsw),
            "vector.inverted" => Some(crate::index::IndexKind::VectorInverted),
            "graph.adjacency" => Some(crate::index::IndexKind::GraphAdjacency),
            "text.fulltext" => Some(crate::index::IndexKind::FullText),
            "search.hybrid" => Some(crate::index::IndexKind::HybridSearch),
            _ => None,
        }
    }

    fn native_artifact_kind_for_index(kind: IndexKind) -> Option<&'static str> {
        match kind {
            IndexKind::VectorHnsw => Some("hnsw"),
            IndexKind::VectorInverted => Some("ivf"),
            _ => None,
        }
    }

    fn index_is_declared(&self, name: &str) -> bool {
        self.physical_metadata()
            .map(|metadata| metadata.indexes.iter().any(|index| index.name == name))
            .unwrap_or(false)
    }

    fn graph_projection_is_declared(&self, name: &str) -> bool {
        self.physical_metadata()
            .map(|metadata| {
                metadata
                    .graph_projections
                    .iter()
                    .any(|projection| projection.name == name)
            })
            .unwrap_or(false)
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

        let mut metadata = self.load_or_bootstrap_physical_metadata(true)?;

        if metadata.indexes.is_empty() {
            metadata.indexes = self.physical_index_state();
        }
        metadata.superblock.collection_roots = self.physical_collection_roots();

        let result = mutator(&mut metadata);
        metadata.save_for_data_path(path)?;
        self.persist_native_physical_header(&metadata)?;
        Ok(result)
    }

    fn persist_native_physical_header(
        &self,
        metadata: &PhysicalMetadataFile,
    ) -> Result<(), Box<dyn std::error::Error>> {
        if !self.paged_mode {
            return Ok(());
        }

        let existing_page = self
            .store
            .physical_file_header()
            .map(|header| header.collection_roots_page)
            .filter(|page| *page != 0);
        let existing_registry_page = self
            .store
            .physical_file_header()
            .map(|header| header.registry_page)
            .filter(|page| *page != 0);
        let existing_recovery_page = self
            .store
            .physical_file_header()
            .map(|header| header.recovery_page)
            .filter(|page| *page != 0);
        let existing_catalog_page = self
            .store
            .physical_file_header()
            .map(|header| header.catalog_page)
            .filter(|page| *page != 0);
        let existing_metadata_state_page = self
            .store
            .physical_file_header()
            .map(|header| header.metadata_state_page)
            .filter(|page| *page != 0);
        let existing_vector_artifact_page = self
            .store
            .physical_file_header()
            .map(|header| header.vector_artifact_page)
            .filter(|page| *page != 0);
        let existing_manifest_page = self
            .store
            .physical_file_header()
            .map(|header| header.manifest_page)
            .filter(|page| *page != 0);
        let (manifest_page, manifest_checksum) = self.store.write_native_manifest_summary(
            metadata.superblock.sequence,
            &metadata.manifest_events,
            existing_manifest_page,
        )?;
        let (collection_roots_page, collection_roots_checksum) = self
            .store
            .write_native_collection_roots(
                &metadata.superblock.collection_roots,
                existing_page,
            )?;
        let registry_summary = self.native_registry_summary_from_metadata(metadata);
        let (registry_page, registry_checksum) = self
            .store
            .write_native_registry_summary(&registry_summary, existing_registry_page)?;
        let recovery_summary = Self::native_recovery_summary_from_metadata(metadata);
        let (recovery_page, recovery_checksum) = self
            .store
            .write_native_recovery_summary(&recovery_summary, existing_recovery_page)?;
        let catalog_summary = Self::native_catalog_summary_from_metadata(metadata);
        let (catalog_page, catalog_checksum) = self
            .store
            .write_native_catalog_summary(&catalog_summary, existing_catalog_page)?;
        let metadata_state_summary = Self::native_metadata_state_summary_from_metadata(metadata);
        let (metadata_state_page, metadata_state_checksum) = self
            .store
            .write_native_metadata_state_summary(
                &metadata_state_summary,
                existing_metadata_state_page,
            )?;
        let vector_artifact_records = self.native_vector_artifact_records();
        let vector_artifact_payloads = vector_artifact_records
            .iter()
            .map(|(summary, bytes)| {
                (
                    summary.collection.clone(),
                    summary.artifact_kind.clone(),
                    bytes.clone(),
                )
            })
            .collect::<Vec<_>>();
        let (vector_artifact_page, vector_artifact_checksum, _vector_artifact_pages) = self
            .store
            .write_native_vector_artifact_store(
                &vector_artifact_payloads,
                existing_vector_artifact_page,
            )?;
        let mut header = Self::native_header_from_metadata(metadata);
        header.manifest_page = manifest_page;
        header.manifest_checksum = manifest_checksum;
        header.collection_roots_page = collection_roots_page;
        header.collection_roots_checksum = collection_roots_checksum;
        header.registry_page = registry_page;
        header.registry_checksum = registry_checksum;
        header.recovery_page = recovery_page;
        header.recovery_checksum = recovery_checksum;
        header.catalog_page = catalog_page;
        header.catalog_checksum = catalog_checksum;
        header.metadata_state_page = metadata_state_page;
        header.metadata_state_checksum = metadata_state_checksum;
        header.vector_artifact_page = vector_artifact_page;
        header.vector_artifact_checksum = vector_artifact_checksum;
        self.store.update_physical_file_header(header)?;
        self.store.persist()?;
        Ok(())
    }

    fn native_header_from_metadata(metadata: &PhysicalMetadataFile) -> PhysicalFileHeader {
        PhysicalFileHeader {
            format_version: metadata.superblock.format_version,
            sequence: metadata.superblock.sequence,
            manifest_oldest_root: metadata.superblock.manifest.oldest.index,
            manifest_root: metadata.superblock.manifest.newest.index,
            free_set_root: metadata.superblock.free_set.index,
            manifest_page: 0,
            manifest_checksum: 0,
            collection_roots_page: 0,
            collection_roots_checksum: 0,
            collection_root_count: metadata.superblock.collection_roots.len() as u32,
            snapshot_count: metadata.snapshots.len() as u32,
            index_count: metadata.indexes.len() as u32,
            catalog_collection_count: metadata.catalog.total_collections as u32,
            catalog_total_entities: metadata.catalog.total_entities as u64,
            export_count: metadata.exports.len() as u32,
            graph_projection_count: metadata.graph_projections.len() as u32,
            analytics_job_count: metadata.analytics_jobs.len() as u32,
            manifest_event_count: metadata.manifest_events.len() as u32,
            registry_page: 0,
            registry_checksum: 0,
            recovery_page: 0,
            recovery_checksum: 0,
            catalog_page: 0,
            catalog_checksum: 0,
            metadata_state_page: 0,
            metadata_state_checksum: 0,
            vector_artifact_page: 0,
            vector_artifact_checksum: 0,
        }
    }

    fn native_registry_summary_from_metadata(
        &self,
        metadata: &PhysicalMetadataFile,
    ) -> NativeRegistrySummary {
        const SAMPLE_LIMIT: usize = 16;

        let collection_names = metadata
            .catalog
            .stats_by_collection
            .keys()
            .take(SAMPLE_LIMIT)
            .cloned()
            .collect();
        let indexes = metadata
            .indexes
            .iter()
            .take(SAMPLE_LIMIT)
            .map(|index| NativeRegistryIndexSummary {
                name: index.name.clone(),
                kind: index.kind.as_str().to_string(),
                collection: index.collection.clone(),
                enabled: index.enabled,
                entries: index.entries as u64,
                estimated_memory_bytes: index.estimated_memory_bytes,
                last_refresh_ms: index.last_refresh_ms,
                backend: index.backend.clone(),
            })
            .collect();
        let graph_projections = metadata
            .graph_projections
            .iter()
            .take(SAMPLE_LIMIT)
            .map(|projection| NativeRegistryProjectionSummary {
                name: projection.name.clone(),
                source: projection.source.clone(),
                created_at_unix_ms: projection.created_at_unix_ms,
                updated_at_unix_ms: projection.updated_at_unix_ms,
                node_labels: projection.node_labels.clone(),
                node_types: projection.node_types.clone(),
                edge_labels: projection.edge_labels.clone(),
                last_materialized_sequence: projection.last_materialized_sequence,
            })
            .collect();
        let analytics_jobs = metadata
            .analytics_jobs
            .iter()
            .take(SAMPLE_LIMIT)
            .map(|job| NativeRegistryJobSummary {
                id: job.id.clone(),
                kind: job.kind.clone(),
                projection: job.projection.clone(),
                state: job.state.clone(),
                created_at_unix_ms: job.created_at_unix_ms,
                updated_at_unix_ms: job.updated_at_unix_ms,
                last_run_sequence: job.last_run_sequence,
                metadata: job.metadata.clone(),
            })
            .collect();
        let vector_artifacts = self
            .native_vector_artifact_records()
            .into_iter()
            .map(|(summary, _)| summary)
            .take(SAMPLE_LIMIT)
            .collect::<Vec<_>>();
        let vector_artifact_count = self.native_vector_artifact_collection_count() as u32;

        NativeRegistrySummary {
            collection_count: metadata.catalog.total_collections as u32,
            index_count: metadata.indexes.len() as u32,
            graph_projection_count: metadata.graph_projections.len() as u32,
            analytics_job_count: metadata.analytics_jobs.len() as u32,
            vector_artifact_count,
            collections_complete: metadata.catalog.stats_by_collection.len() <= SAMPLE_LIMIT,
            indexes_complete: metadata.indexes.len() <= SAMPLE_LIMIT,
            graph_projections_complete: metadata.graph_projections.len() <= SAMPLE_LIMIT,
            analytics_jobs_complete: metadata.analytics_jobs.len() <= SAMPLE_LIMIT,
            vector_artifacts_complete: vector_artifact_count as usize <= SAMPLE_LIMIT,
            omitted_collection_count: metadata
                .catalog
                .stats_by_collection
                .len()
                .saturating_sub(collection_names.len()) as u32,
            omitted_index_count: metadata.indexes.len().saturating_sub(indexes.len()) as u32,
            omitted_graph_projection_count: metadata
                .graph_projections
                .len()
                .saturating_sub(graph_projections.len()) as u32,
            omitted_analytics_job_count: metadata
                .analytics_jobs
                .len()
                .saturating_sub(analytics_jobs.len()) as u32,
            omitted_vector_artifact_count: vector_artifact_count
                .saturating_sub(vector_artifacts.len() as u32),
            collection_names,
            indexes,
            graph_projections,
            analytics_jobs,
            vector_artifacts,
        }
    }

    fn native_vector_artifact_collection_count(&self) -> usize {
        self.native_vector_artifact_records().len()
    }

    fn native_vector_artifact_records(&self) -> Vec<(NativeVectorArtifactSummary, Vec<u8>)> {
        let mut artifacts = Vec::new();
        for collection in self.store.list_collections() {
            let Some(manager) = self.store.get_collection(&collection) else {
                continue;
            };
            let entities = manager.query_all(|_| true);
            let mut vectors = Vec::new();
            for entity in entities {
                if let EntityData::Vector(vector) = entity.data {
                    if !vector.dense.is_empty() {
                        vectors.push((entity.id, vector.dense));
                    }
                }
            }
            if vectors.is_empty() {
                continue;
            }

            let dimension = vectors[0].1.len();
            let mut hnsw = HnswIndex::with_dimension(dimension);
            for (id, vector) in vectors.into_iter().filter(|(_, vector)| vector.len() == dimension) {
                hnsw.insert_with_id(id, vector);
            }
            let stats = hnsw.stats();
            let bytes = hnsw.to_bytes();
            let summary = NativeVectorArtifactSummary {
                collection: collection.clone(),
                artifact_kind: "hnsw".to_string(),
                vector_count: stats.node_count as u64,
                dimension: stats.dimension as u32,
                max_layer: stats.max_layer as u32,
                serialized_bytes: bytes.len() as u64,
                checksum: crate::storage::engine::crc32(&bytes) as u64,
            };
            artifacts.push((summary, bytes));

            let n_lists = ((stats.node_count as f64).sqrt().ceil() as usize).max(1);
            let mut ivf = IvfIndex::new(IvfConfig::new(dimension, n_lists));
            let training = manager
                .query_all(|_| true)
                .into_iter()
                .filter_map(|entity| match entity.data {
                    EntityData::Vector(vector) if vector.dense.len() == dimension => Some(vector.dense),
                    _ => None,
                })
                .collect::<Vec<_>>();
            ivf.train(&training);
            let items = manager
                .query_all(|_| true)
                .into_iter()
                .filter_map(|entity| match entity.data {
                    EntityData::Vector(vector) if vector.dense.len() == dimension => {
                        Some((entity.id, vector.dense))
                    }
                    _ => None,
                })
                .collect::<Vec<_>>();
            ivf.add_batch_with_ids(items);
            let ivf_stats = ivf.stats();
            let ivf_bytes = ivf.to_bytes();
            let ivf_summary = NativeVectorArtifactSummary {
                collection,
                artifact_kind: "ivf".to_string(),
                vector_count: ivf_stats.total_vectors as u64,
                dimension: ivf_stats.dimension as u32,
                max_layer: ivf_stats.n_lists as u32,
                serialized_bytes: ivf_bytes.len() as u64,
                checksum: crate::storage::engine::crc32(&ivf_bytes) as u64,
            };
            artifacts.push((ivf_summary, ivf_bytes));
        }
        artifacts
    }

    fn native_recovery_summary_from_metadata(
        metadata: &PhysicalMetadataFile,
    ) -> NativeRecoverySummary {
        const SAMPLE_LIMIT: usize = 16;

        let snapshots = metadata
            .snapshots
            .iter()
            .rev()
            .take(SAMPLE_LIMIT)
                    .map(|snapshot| NativeSnapshotSummary {
                        snapshot_id: snapshot.snapshot_id,
                        created_at_unix_ms: snapshot.created_at_unix_ms,
                        superblock_sequence: snapshot.superblock_sequence,
                        collection_count: snapshot.collection_count as u32,
                        total_entities: snapshot.total_entities as u64,
            })
            .collect();
        let exports = metadata
            .exports
            .iter()
            .rev()
            .take(SAMPLE_LIMIT)
                    .map(|export| NativeExportSummary {
                        name: export.name.clone(),
                        created_at_unix_ms: export.created_at_unix_ms,
                        snapshot_id: export.snapshot_id,
                        superblock_sequence: export.superblock_sequence,
                        collection_count: export.collection_count as u32,
                total_entities: export.total_entities as u64,
            })
            .collect();

        NativeRecoverySummary {
            snapshot_count: metadata.snapshots.len() as u32,
            export_count: metadata.exports.len() as u32,
            snapshots_complete: metadata.snapshots.len() <= SAMPLE_LIMIT,
            exports_complete: metadata.exports.len() <= SAMPLE_LIMIT,
            omitted_snapshot_count: metadata.snapshots.len().saturating_sub(snapshots.len()) as u32,
            omitted_export_count: metadata.exports.len().saturating_sub(exports.len()) as u32,
            snapshots,
            exports,
        }
    }

    fn native_catalog_summary_from_metadata(
        metadata: &PhysicalMetadataFile,
    ) -> NativeCatalogSummary {
        const SAMPLE_LIMIT: usize = 32;

        let collections = metadata
            .catalog
            .stats_by_collection
            .iter()
            .take(SAMPLE_LIMIT)
            .map(|(name, stats)| NativeCatalogCollectionSummary {
                name: name.clone(),
                entities: stats.entities as u64,
                cross_refs: stats.cross_refs as u64,
                segments: stats.segments as u32,
            })
            .collect();

        NativeCatalogSummary {
            collection_count: metadata.catalog.total_collections as u32,
            total_entities: metadata.catalog.total_entities as u64,
            collections_complete: metadata.catalog.stats_by_collection.len() <= SAMPLE_LIMIT,
            omitted_collection_count: metadata
                .catalog
                .stats_by_collection
                .len()
                .saturating_sub(collections.len()) as u32,
            collections,
        }
    }

    fn native_metadata_state_summary_from_metadata(
        metadata: &PhysicalMetadataFile,
    ) -> NativeMetadataStateSummary {
        NativeMetadataStateSummary {
            protocol_version: metadata.protocol_version.clone(),
            generated_at_unix_ms: metadata.generated_at_unix_ms,
            last_loaded_from: metadata.last_loaded_from.clone(),
            last_healed_at_unix_ms: metadata.last_healed_at_unix_ms,
        }
    }

    fn inspect_native_header_against_metadata(
        native: PhysicalFileHeader,
        metadata: &PhysicalMetadataFile,
    ) -> NativeHeaderInspection {
        let expected = Self::native_header_from_metadata(metadata);
        let mut mismatches = Vec::new();

        if native.format_version != expected.format_version {
            mismatches.push(NativeHeaderMismatch {
                field: "format_version",
                native: native.format_version.to_string(),
                expected: expected.format_version.to_string(),
            });
        }
        if native.sequence != expected.sequence {
            mismatches.push(NativeHeaderMismatch {
                field: "sequence",
                native: native.sequence.to_string(),
                expected: expected.sequence.to_string(),
            });
        }
        if native.manifest_oldest_root != expected.manifest_oldest_root {
            mismatches.push(NativeHeaderMismatch {
                field: "manifest_oldest_root",
                native: native.manifest_oldest_root.to_string(),
                expected: expected.manifest_oldest_root.to_string(),
            });
        }
        if native.manifest_root != expected.manifest_root {
            mismatches.push(NativeHeaderMismatch {
                field: "manifest_root",
                native: native.manifest_root.to_string(),
                expected: expected.manifest_root.to_string(),
            });
        }
        if native.free_set_root != expected.free_set_root {
            mismatches.push(NativeHeaderMismatch {
                field: "free_set_root",
                native: native.free_set_root.to_string(),
                expected: expected.free_set_root.to_string(),
            });
        }
        if native.collection_root_count != expected.collection_root_count {
            mismatches.push(NativeHeaderMismatch {
                field: "collection_root_count",
                native: native.collection_root_count.to_string(),
                expected: expected.collection_root_count.to_string(),
            });
        }
        if native.snapshot_count != expected.snapshot_count {
            mismatches.push(NativeHeaderMismatch {
                field: "snapshot_count",
                native: native.snapshot_count.to_string(),
                expected: expected.snapshot_count.to_string(),
            });
        }
        if native.index_count != expected.index_count {
            mismatches.push(NativeHeaderMismatch {
                field: "index_count",
                native: native.index_count.to_string(),
                expected: expected.index_count.to_string(),
            });
        }
        if native.catalog_collection_count != expected.catalog_collection_count {
            mismatches.push(NativeHeaderMismatch {
                field: "catalog_collection_count",
                native: native.catalog_collection_count.to_string(),
                expected: expected.catalog_collection_count.to_string(),
            });
        }
        if native.catalog_total_entities != expected.catalog_total_entities {
            mismatches.push(NativeHeaderMismatch {
                field: "catalog_total_entities",
                native: native.catalog_total_entities.to_string(),
                expected: expected.catalog_total_entities.to_string(),
            });
        }
        if native.export_count != expected.export_count {
            mismatches.push(NativeHeaderMismatch {
                field: "export_count",
                native: native.export_count.to_string(),
                expected: expected.export_count.to_string(),
            });
        }
        if native.graph_projection_count != expected.graph_projection_count {
            mismatches.push(NativeHeaderMismatch {
                field: "graph_projection_count",
                native: native.graph_projection_count.to_string(),
                expected: expected.graph_projection_count.to_string(),
            });
        }
        if native.analytics_job_count != expected.analytics_job_count {
            mismatches.push(NativeHeaderMismatch {
                field: "analytics_job_count",
                native: native.analytics_job_count.to_string(),
                expected: expected.analytics_job_count.to_string(),
            });
        }
        if native.manifest_event_count != expected.manifest_event_count {
            mismatches.push(NativeHeaderMismatch {
                field: "manifest_event_count",
                native: native.manifest_event_count.to_string(),
                expected: expected.manifest_event_count.to_string(),
            });
        }

        NativeHeaderInspection {
            native,
            expected,
            consistent: mismatches.is_empty(),
            mismatches,
        }
    }

    fn repair_policy_for_inspection(
        inspection: &NativeHeaderInspection,
    ) -> NativeHeaderRepairPolicy {
        if inspection.consistent {
            return NativeHeaderRepairPolicy::InSync;
        }

        if inspection.expected.sequence >= inspection.native.sequence {
            NativeHeaderRepairPolicy::RepairNativeFromMetadata
        } else {
            NativeHeaderRepairPolicy::NativeAheadOfMetadata
        }
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
            let binary_path =
                PhysicalMetadataFile::metadata_binary_path_for(std::path::Path::new(&export.data_path));
            let _ = fs::remove_file(binary_path);
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
                    artifact_kind: None,
                    artifact_root_page: None,
                    artifact_checksum: None,
                    build_state: "catalog-derived".to_string(),
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
