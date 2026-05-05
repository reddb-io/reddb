//! Logical catalog structures for the unified multi-structure model.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::time::SystemTime;

use crate::api::{CatalogSnapshot, CollectionStats};
use crate::index::{IndexCatalog, IndexCatalogSnapshot, IndexKind};
use crate::physical::{
    CollectionContract, PhysicalAnalyticsJob, PhysicalGraphProjection, PhysicalIndexState,
};
use crate::storage::unified::UnifiedStore;
use crate::storage::{EntityKind, UnifiedEntity};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionModel {
    Table,
    Document,
    Graph,
    Vector,
    Mixed,
    TimeSeries,
    Queue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaMode {
    Strict,
    SemiStructured,
    Dynamic,
}

#[derive(Debug, Clone)]
pub struct CollectionDescriptor {
    pub name: String,
    pub model: CollectionModel,
    pub schema_mode: SchemaMode,
    pub contract_present: bool,
    pub contract_origin: Option<crate::physical::ContractOrigin>,
    pub declared_model: Option<CollectionModel>,
    pub observed_model: CollectionModel,
    pub declared_schema_mode: Option<SchemaMode>,
    pub observed_schema_mode: SchemaMode,
    pub entities: usize,
    pub cross_refs: usize,
    pub segments: usize,
    pub indices: Vec<String>,
    pub declared_indices: Vec<String>,
    pub operational_indices: Vec<String>,
    pub indexes_in_sync: bool,
    pub missing_operational_indices: Vec<String>,
    pub undeclared_operational_indices: Vec<String>,
    pub queryable_index_count: usize,
    pub indexes_requiring_rebuild_count: usize,
    pub queryable_graph_projection_count: usize,
    pub graph_projections_requiring_rematerialization_count: usize,
    pub executable_analytics_job_count: usize,
    pub analytics_jobs_requiring_rerun_count: usize,
    pub resources_in_sync: bool,
    pub attention_required: bool,
    pub attention_score: usize,
    pub attention_reasons: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CatalogModelSnapshot {
    pub summary: CatalogSnapshot,
    pub collections: Vec<CollectionDescriptor>,
    pub indices: IndexCatalogSnapshot,
    pub declared_indexes: Vec<PhysicalIndexState>,
    pub declared_graph_projections: Vec<PhysicalGraphProjection>,
    pub declared_analytics_jobs: Vec<PhysicalAnalyticsJob>,
    pub operational_indexes: Vec<PhysicalIndexState>,
    pub operational_graph_projections: Vec<PhysicalGraphProjection>,
    pub operational_analytics_jobs: Vec<PhysicalAnalyticsJob>,
    pub index_statuses: Vec<CatalogIndexStatus>,
    pub graph_projection_statuses: Vec<CatalogGraphProjectionStatus>,
    pub analytics_job_statuses: Vec<CatalogAnalyticsJobStatus>,
    pub queryable_index_count: usize,
    pub indexes_requiring_rebuild_count: usize,
    pub queryable_graph_projection_count: usize,
    pub graph_projections_requiring_rematerialization_count: usize,
    pub executable_analytics_job_count: usize,
    pub analytics_jobs_requiring_rerun_count: usize,
}

#[derive(Debug, Clone)]
pub struct CatalogAttentionSummary {
    pub collections_requiring_attention: usize,
    pub indexes_requiring_attention: usize,
    pub graph_projections_requiring_attention: usize,
    pub analytics_jobs_requiring_attention: usize,
    pub top_collection: Option<CollectionDescriptor>,
    pub top_index: Option<CatalogIndexStatus>,
    pub top_graph_projection: Option<CatalogGraphProjectionStatus>,
    pub top_analytics_job: Option<CatalogAnalyticsJobStatus>,
}

#[derive(Debug, Clone, Default)]
pub struct CatalogDeclarations {
    pub declared_indexes: Vec<PhysicalIndexState>,
    pub declared_graph_projections: Vec<PhysicalGraphProjection>,
    pub declared_analytics_jobs: Vec<PhysicalAnalyticsJob>,
    pub operational_indexes: Vec<PhysicalIndexState>,
    pub operational_graph_projections: Vec<PhysicalGraphProjection>,
    pub operational_analytics_jobs: Vec<PhysicalAnalyticsJob>,
}

#[derive(Debug, Clone)]
pub struct CatalogGraphProjectionStatus {
    pub name: String,
    pub source: Option<String>,
    pub lifecycle_state: String,
    pub declared: bool,
    pub operational: bool,
    pub in_sync: bool,
    pub last_materialized_sequence: Option<u64>,
    pub queryable: bool,
    pub requires_rematerialization: bool,
    pub dependent_job_count: usize,
    pub active_dependent_job_count: usize,
    pub stale_dependent_job_count: usize,
    pub dependent_jobs_in_sync: bool,
    pub rerun_required: bool,
    pub attention_score: usize,
    pub attention_reasons: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CatalogIndexStatus {
    pub name: String,
    pub collection: Option<String>,
    pub kind: String,
    pub declared: bool,
    pub operational: bool,
    pub enabled: bool,
    pub build_state: Option<String>,
    pub artifact_state: crate::physical::ArtifactState,
    pub lifecycle_state: String,
    pub in_sync: bool,
    pub queryable: bool,
    pub requires_rebuild: bool,
    pub attention_score: usize,
    pub attention_reasons: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CatalogAnalyticsJobStatus {
    pub id: String,
    pub kind: String,
    pub projection: Option<String>,
    pub state: String,
    pub lifecycle_state: String,
    pub declared: bool,
    pub operational: bool,
    pub in_sync: bool,
    pub last_run_sequence: Option<u64>,
    pub projection_declared: Option<bool>,
    pub projection_operational: Option<bool>,
    pub projection_lifecycle_state: Option<String>,
    pub dependency_in_sync: Option<bool>,
    pub executable: bool,
    pub requires_rerun: bool,
    pub attention_score: usize,
    pub attention_reasons: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct CatalogConsistencyReport {
    pub declared_index_count: usize,
    pub operational_index_count: usize,
    pub declared_graph_projection_count: usize,
    pub operational_graph_projection_count: usize,
    pub declared_analytics_job_count: usize,
    pub operational_analytics_job_count: usize,
    pub missing_operational_indexes: Vec<String>,
    pub undeclared_operational_indexes: Vec<String>,
    pub missing_operational_graph_projections: Vec<String>,
    pub undeclared_operational_graph_projections: Vec<String>,
    pub missing_operational_analytics_jobs: Vec<String>,
    pub undeclared_operational_analytics_jobs: Vec<String>,
}

pub fn snapshot_store(
    name: &str,
    store: &UnifiedStore,
    index_catalog: Option<&IndexCatalog>,
) -> CatalogModelSnapshot {
    snapshot_store_with_declarations(name, store, index_catalog, None, None)
}

pub fn snapshot_store_with_declarations(
    name: &str,
    store: &UnifiedStore,
    index_catalog: Option<&IndexCatalog>,
    declarations: Option<&CatalogDeclarations>,
    contracts: Option<&[CollectionContract]>,
) -> CatalogModelSnapshot {
    let index_statuses = index_statuses(declarations);
    let graph_projection_statuses = graph_projection_statuses(declarations);
    let analytics_job_statuses = analytics_job_statuses(declarations);

    let mut grouped: HashMap<String, Vec<UnifiedEntity>> = HashMap::new();
    for (collection, entity) in store.query_all(|_| true) {
        grouped.entry(collection).or_default().push(entity);
    }

    let mut stats_by_collection = BTreeMap::new();
    let mut collections = Vec::new();

    for collection_name in store.list_collections() {
        let entities = grouped.remove(&collection_name).unwrap_or_default();
        let inferred_model = infer_model(&entities);
        let inferred_schema_mode = infer_schema_mode(inferred_model);
        let contract = collection_contract(&collection_name, contracts);
        let model = contract
            .map(|contract| contract.declared_model)
            .unwrap_or(inferred_model);
        let cross_refs = entities
            .iter()
            .map(|entity| entity.cross_refs().len())
            .sum();
        let entity_count = entities.len();
        let manager_stats = store
            .get_collection(&collection_name)
            .map(|manager| manager.stats());
        let segments = manager_stats
            .map(|stats| stats.growing_count + stats.sealed_count + stats.archived_count)
            .unwrap_or(0);

        stats_by_collection.insert(
            collection_name.clone(),
            CollectionStats {
                entities: entity_count,
                cross_refs,
                segments,
            },
        );

        let declared_indices = declared_indices(&collection_name, declarations);
        let operational_indices = operational_indices(&collection_name, index_catalog);
        let (indexes_in_sync, missing_operational_indices, undeclared_operational_indices) =
            collection_index_consistency(&declared_indices, &operational_indices);
        let (queryable_index_count, indexes_requiring_rebuild_count, indexes_locally_in_sync) =
            collection_index_readiness(&collection_name, &index_statuses);
        let (
            queryable_graph_projection_count,
            graph_projections_requiring_rematerialization_count,
            graph_projections_locally_in_sync,
        ) = collection_graph_projection_readiness(&collection_name, &graph_projection_statuses);
        let (
            executable_analytics_job_count,
            analytics_jobs_requiring_rerun_count,
            analytics_jobs_locally_in_sync,
        ) = collection_analytics_job_readiness(
            &collection_name,
            &graph_projection_statuses,
            &analytics_job_statuses,
        );
        let resources_in_sync = indexes_in_sync
            && indexes_locally_in_sync
            && graph_projections_locally_in_sync
            && analytics_jobs_locally_in_sync;
        let attention_required = !resources_in_sync
            || indexes_requiring_rebuild_count > 0
            || graph_projections_requiring_rematerialization_count > 0
            || analytics_jobs_requiring_rerun_count > 0;
        let mut attention_reasons = Vec::new();
        if !indexes_in_sync || !indexes_locally_in_sync {
            attention_reasons.push("index_drift".to_string());
        }
        if indexes_requiring_rebuild_count > 0 {
            attention_reasons.push("indexes_require_rebuild".to_string());
        }
        if !graph_projections_locally_in_sync {
            attention_reasons.push("graph_projection_drift".to_string());
        }
        if graph_projections_requiring_rematerialization_count > 0 {
            attention_reasons.push("graph_projections_require_rematerialization".to_string());
        }
        if !analytics_jobs_locally_in_sync {
            attention_reasons.push("analytics_job_drift".to_string());
        }
        if analytics_jobs_requiring_rerun_count > 0 {
            attention_reasons.push("analytics_jobs_require_rerun".to_string());
        }
        let attention_score = indexes_requiring_rebuild_count.saturating_mul(3)
            + graph_projections_requiring_rematerialization_count.saturating_mul(4)
            + analytics_jobs_requiring_rerun_count.saturating_mul(2)
            + usize::from(!resources_in_sync);

        collections.push(CollectionDescriptor {
            name: collection_name.clone(),
            model,
            schema_mode: contract
                .map(|contract| contract.schema_mode)
                .unwrap_or(inferred_schema_mode),
            contract_present: contract.is_some(),
            contract_origin: contract.map(|contract| contract.origin),
            declared_model: contract.map(|contract| contract.declared_model),
            observed_model: inferred_model,
            declared_schema_mode: contract.map(|contract| contract.schema_mode),
            observed_schema_mode: inferred_schema_mode,
            entities: entity_count,
            cross_refs,
            segments,
            indices: infer_indices(&collection_name, model, index_catalog, declarations),
            declared_indices,
            operational_indices,
            indexes_in_sync,
            missing_operational_indices,
            undeclared_operational_indices,
            queryable_index_count,
            indexes_requiring_rebuild_count,
            queryable_graph_projection_count,
            graph_projections_requiring_rematerialization_count,
            executable_analytics_job_count,
            analytics_jobs_requiring_rerun_count,
            resources_in_sync,
            attention_required,
            attention_score,
            attention_reasons,
        });
    }

    collections.sort_by(|left, right| left.name.cmp(&right.name));

    let summary = CatalogSnapshot {
        name: name.to_string(),
        total_entities: stats_by_collection
            .values()
            .map(|stats| stats.entities)
            .sum(),
        total_collections: stats_by_collection.len(),
        stats_by_collection,
        updated_at: SystemTime::now(),
    };

    CatalogModelSnapshot {
        summary,
        collections,
        indices: index_catalog
            .map(IndexCatalog::snapshot)
            .unwrap_or_default(),
        declared_indexes: declarations
            .map(|declarations| declarations.declared_indexes.clone())
            .unwrap_or_default(),
        declared_graph_projections: declarations
            .map(|declarations| declarations.declared_graph_projections.clone())
            .unwrap_or_default(),
        declared_analytics_jobs: declarations
            .map(|declarations| declarations.declared_analytics_jobs.clone())
            .unwrap_or_default(),
        operational_indexes: declarations
            .map(|declarations| declarations.operational_indexes.clone())
            .unwrap_or_default(),
        operational_graph_projections: declarations
            .map(|declarations| declarations.operational_graph_projections.clone())
            .unwrap_or_default(),
        operational_analytics_jobs: declarations
            .map(|declarations| declarations.operational_analytics_jobs.clone())
            .unwrap_or_default(),
        queryable_index_count: index_statuses
            .iter()
            .filter(|status| status.queryable)
            .count(),
        indexes_requiring_rebuild_count: index_statuses
            .iter()
            .filter(|status| status.requires_rebuild)
            .count(),
        queryable_graph_projection_count: graph_projection_statuses
            .iter()
            .filter(|status| status.queryable)
            .count(),
        graph_projections_requiring_rematerialization_count: graph_projection_statuses
            .iter()
            .filter(|status| status.requires_rematerialization)
            .count(),
        executable_analytics_job_count: analytics_job_statuses
            .iter()
            .filter(|status| status.executable)
            .count(),
        analytics_jobs_requiring_rerun_count: analytics_job_statuses
            .iter()
            .filter(|status| status.requires_rerun)
            .count(),
        index_statuses,
        graph_projection_statuses,
        analytics_job_statuses,
    }
}

fn collection_contract<'a>(
    collection_name: &str,
    contracts: Option<&'a [CollectionContract]>,
) -> Option<&'a CollectionContract> {
    contracts.and_then(|contracts| {
        contracts
            .iter()
            .find(|contract| contract.name == collection_name)
    })
}

fn infer_model(entities: &[UnifiedEntity]) -> CollectionModel {
    let mut has_table = false;
    let mut has_graph = false;
    let mut has_vector = false;

    for entity in entities {
        match &entity.kind {
            EntityKind::TableRow { .. } => has_table = true,
            EntityKind::GraphNode(_) | EntityKind::GraphEdge(_) => has_graph = true,
            EntityKind::Vector { .. } => has_vector = true,
            EntityKind::TimeSeriesPoint(_) | EntityKind::QueueMessage { .. } => {}
        }
    }

    match (has_table, has_graph, has_vector) {
        (true, false, false) => CollectionModel::Table,
        (false, true, false) => CollectionModel::Graph,
        (false, false, true) => CollectionModel::Vector,
        _ => CollectionModel::Mixed,
    }
}

fn infer_schema_mode(model: CollectionModel) -> SchemaMode {
    match model {
        CollectionModel::Table => SchemaMode::Strict,
        CollectionModel::Graph | CollectionModel::Vector => SchemaMode::SemiStructured,
        CollectionModel::Document | CollectionModel::Mixed => SchemaMode::Dynamic,
        CollectionModel::TimeSeries => SchemaMode::SemiStructured,
        CollectionModel::Queue => SchemaMode::Dynamic,
    }
}

fn infer_indices(
    collection_name: &str,
    model: CollectionModel,
    index_catalog: Option<&IndexCatalog>,
    declarations: Option<&CatalogDeclarations>,
) -> Vec<String> {
    let declared = declared_indices(collection_name, declarations);
    if !declared.is_empty() {
        return declared;
    }

    let available = index_catalog
        .map(IndexCatalog::snapshot)
        .unwrap_or_default();
    let mut selected = Vec::new();

    for metric in available {
        let relevant = matches!(
            (model, metric.kind),
            (_, IndexKind::BTree)
                | (CollectionModel::Graph, IndexKind::GraphAdjacency)
                | (CollectionModel::Vector, IndexKind::VectorHnsw)
                | (CollectionModel::Vector, IndexKind::VectorInverted)
                | (CollectionModel::Document, IndexKind::FullText)
                | (CollectionModel::Document, IndexKind::DocumentPathValue)
                | (CollectionModel::Mixed, _)
        );

        if relevant && metric.enabled {
            selected.push(metric.name);
        }
    }

    selected
}

fn declared_indices(
    collection_name: &str,
    declarations: Option<&CatalogDeclarations>,
) -> Vec<String> {
    let mut selected = declarations
        .map(|declarations| {
            declarations
                .declared_indexes
                .iter()
                .filter(|index| index.collection.as_deref() == Some(collection_name))
                .map(|index| index.name.clone())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    selected.sort();
    selected.dedup();
    selected
}

fn operational_indices(collection_name: &str, index_catalog: Option<&IndexCatalog>) -> Vec<String> {
    let mut selected = index_catalog
        .map(IndexCatalog::snapshot)
        .unwrap_or_default()
        .into_iter()
        .filter(|metric| metric.enabled)
        .filter_map(|metric| {
            if metric.name.starts_with(collection_name)
                || matches!(
                    metric.kind,
                    IndexKind::BTree
                        | IndexKind::GraphAdjacency
                        | IndexKind::VectorHnsw
                        | IndexKind::VectorInverted
                        | IndexKind::FullText
                        | IndexKind::DocumentPathValue
                        | IndexKind::HybridSearch
                )
            {
                Some(metric.name)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    selected.sort();
    selected.dedup();
    selected
}

fn collection_index_consistency(
    declared_indices: &[String],
    operational_indices: &[String],
) -> (bool, Vec<String>, Vec<String>) {
    let declared = declared_indices.iter().cloned().collect::<BTreeSet<_>>();
    let operational = operational_indices.iter().cloned().collect::<BTreeSet<_>>();
    let missing_operational_indices = declared
        .difference(&operational)
        .cloned()
        .collect::<Vec<_>>();
    let undeclared_operational_indices = operational
        .difference(&declared)
        .cloned()
        .collect::<Vec<_>>();
    (
        missing_operational_indices.is_empty() && undeclared_operational_indices.is_empty(),
        missing_operational_indices,
        undeclared_operational_indices,
    )
}

fn collection_index_readiness(
    collection_name: &str,
    statuses: &[CatalogIndexStatus],
) -> (usize, usize, bool) {
    let local = statuses
        .iter()
        .filter(|status| status.collection.as_deref() == Some(collection_name))
        .collect::<Vec<_>>();
    let queryable_count = local.iter().filter(|status| status.queryable).count();
    let requires_rebuild_count = local
        .iter()
        .filter(|status| status.requires_rebuild)
        .count();
    let locally_in_sync = local.iter().all(|status| status.in_sync);
    (queryable_count, requires_rebuild_count, locally_in_sync)
}

fn collection_graph_projection_readiness(
    collection_name: &str,
    statuses: &[CatalogGraphProjectionStatus],
) -> (usize, usize, bool) {
    let local = statuses
        .iter()
        .filter(|status| status.source.as_deref() == Some(collection_name))
        .collect::<Vec<_>>();
    let queryable_count = local.iter().filter(|status| status.queryable).count();
    let requires_rematerialization_count = local
        .iter()
        .filter(|status| status.requires_rematerialization)
        .count();
    let locally_in_sync = local
        .iter()
        .all(|status| status.in_sync && status.dependent_jobs_in_sync);
    (
        queryable_count,
        requires_rematerialization_count,
        locally_in_sync,
    )
}

fn collection_analytics_job_readiness(
    collection_name: &str,
    projection_statuses: &[CatalogGraphProjectionStatus],
    job_statuses: &[CatalogAnalyticsJobStatus],
) -> (usize, usize, bool) {
    let local = job_statuses
        .iter()
        .filter(|status| {
            let Some(projection_name) = status.projection.as_deref() else {
                return false;
            };
            projection_statuses
                .iter()
                .find(|projection| projection.name == projection_name)
                .and_then(|projection| projection.source.as_deref())
                == Some(collection_name)
        })
        .collect::<Vec<_>>();
    let executable_count = local.iter().filter(|status| status.executable).count();
    let requires_rerun_count = local.iter().filter(|status| status.requires_rerun).count();
    let locally_in_sync = local
        .iter()
        .all(|status| status.in_sync && status.dependency_in_sync.unwrap_or(true));
    (executable_count, requires_rerun_count, locally_in_sync)
}

fn graph_projection_statuses(
    declarations: Option<&CatalogDeclarations>,
) -> Vec<CatalogGraphProjectionStatus> {
    let declared = declarations
        .map(|declarations| declarations.declared_graph_projections.as_slice())
        .unwrap_or(&[]);
    let operational = declarations
        .map(|declarations| declarations.operational_graph_projections.as_slice())
        .unwrap_or(&[]);
    let declared_jobs = declarations
        .map(|declarations| declarations.declared_analytics_jobs.as_slice())
        .unwrap_or(&[]);
    let operational_jobs = declarations
        .map(|declarations| declarations.operational_analytics_jobs.as_slice())
        .unwrap_or(&[]);

    let mut names = declared
        .iter()
        .map(|projection| projection.name.clone())
        .chain(operational.iter().map(|projection| projection.name.clone()))
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();

    names
        .into_iter()
        .map(|name| {
            let declared_projection = declared.iter().find(|projection| projection.name == name);
            let operational_projection = operational
                .iter()
                .find(|projection| projection.name == name);
            let declared_present = declared_projection.is_some();
            let operational_present = operational_projection.is_some();
            let mut dependent_job_ids = BTreeSet::new();
            let mut dependent_job_count = 0usize;
            let mut active_dependent_job_count = 0usize;
            let mut stale_dependent_job_count = 0usize;
            for job in declared_jobs
                .iter()
                .chain(operational_jobs.iter())
                .filter(|job| job.projection.as_deref() == Some(name.as_str()))
            {
                if !dependent_job_ids.insert(job.id.clone()) {
                    continue;
                }
                dependent_job_count += 1;
                if matches!(job.state.as_str(), "queued" | "running" | "completed") {
                    active_dependent_job_count += 1;
                }
                if job.state == "stale" {
                    stale_dependent_job_count += 1;
                }
            }
            let lifecycle_state = match (
                declared_present,
                operational_present,
                declared_projection
                    .map(|projection| projection.state.as_str())
                    .or_else(|| operational_projection.map(|projection| projection.state.as_str()))
                    .unwrap_or_default(),
            ) {
                (true, _, "failed") => "failed",
                (true, true, "stale") => "stale",
                (true, _, "materializing") => "materializing",
                (true, true, "materialized") => "materialized",
                (true, true, _) => "materialized",
                (false, true, _) => "orphaned-operational",
                (true, false, _) => "declared",
                (false, false, _) => "unknown",
            }
            .to_string();
            let queryable = declared_present
                && operational_present
                && matches!(
                    declared_projection
                        .map(|projection| projection.state.as_str())
                        .or_else(
                            || operational_projection.map(|projection| projection.state.as_str())
                        )
                        .unwrap_or_default(),
                    "materialized"
                );
            let requires_rematerialization = matches!(
                declared_projection
                    .map(|projection| projection.state.as_str())
                    .or_else(|| operational_projection.map(|projection| projection.state.as_str()))
                    .unwrap_or_default(),
                "declared" | "materializing" | "failed" | "stale"
            );
            let mut attention_reasons = Vec::new();
            if !declared_present || !operational_present {
                attention_reasons.push("declaration_drift".to_string());
            }
            if requires_rematerialization {
                attention_reasons.push("requires_rematerialization".to_string());
            }
            if stale_dependent_job_count > 0 {
                attention_reasons.push("dependent_jobs_stale".to_string());
            }
            let attention_score = stale_dependent_job_count.saturating_mul(2)
                + usize::from(requires_rematerialization).saturating_mul(4)
                + usize::from(!declared_present || !operational_present)
                + usize::from(!queryable);
            CatalogGraphProjectionStatus {
                name,
                source: declared_projection
                    .map(|projection| projection.source.clone())
                    .or_else(|| operational_projection.map(|projection| projection.source.clone())),
                lifecycle_state,
                declared: declared_present,
                operational: operational_present,
                in_sync: declared_present == operational_present,
                last_materialized_sequence: declared_projection
                    .and_then(|projection| projection.last_materialized_sequence)
                    .or_else(|| {
                        operational_projection
                            .and_then(|projection| projection.last_materialized_sequence)
                    }),
                queryable,
                requires_rematerialization,
                dependent_job_count,
                active_dependent_job_count,
                stale_dependent_job_count,
                dependent_jobs_in_sync: stale_dependent_job_count == 0,
                rerun_required: stale_dependent_job_count > 0,
                attention_score,
                attention_reasons,
            }
        })
        .collect()
}

fn index_statuses(declarations: Option<&CatalogDeclarations>) -> Vec<CatalogIndexStatus> {
    let declared = declarations
        .map(|declarations| declarations.declared_indexes.as_slice())
        .unwrap_or(&[]);
    let operational = declarations
        .map(|declarations| declarations.operational_indexes.as_slice())
        .unwrap_or(&[]);

    let mut names = declared
        .iter()
        .map(|index| index.name.clone())
        .chain(operational.iter().map(|index| index.name.clone()))
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();

    names
        .into_iter()
        .map(|name| {
            let declared_index = declared.iter().find(|index| index.name == name);
            let operational_index = operational.iter().find(|index| index.name == name);
            let collection = declared_index
                .and_then(|index| index.collection.clone())
                .or_else(|| operational_index.and_then(|index| index.collection.clone()));
            let kind = declared_index
                .map(|index| index_kind_string(index.kind))
                .or_else(|| operational_index.map(|index| index_kind_string(index.kind)))
                .unwrap_or_default();
            let enabled = declared_index
                .map(|index| index.enabled)
                .or_else(|| operational_index.map(|index| index.enabled))
                .unwrap_or(false);
            let build_state = operational_index
                .map(|index| index.build_state.clone())
                .or_else(|| declared_index.map(|index| index.build_state.clone()));
            let declared_present = declared_index.is_some();
            let operational_present = operational_index.is_some();
            let lifecycle_state = index_lifecycle_state(
                declared_present,
                operational_present,
                enabled,
                build_state.as_deref(),
            );

            let mut attention_reasons = Vec::new();
            if declared_present != operational_present {
                attention_reasons.push("declaration_drift".to_string());
            }
            if !enabled && declared_present {
                attention_reasons.push("disabled".to_string());
            }
            if matches!(build_state.as_deref().unwrap_or_default(), "failed") {
                attention_reasons.push("failed".to_string());
            }
            if matches!(build_state.as_deref().unwrap_or_default(), "stale") {
                attention_reasons.push("stale".to_string());
            }
            if matches!(
                build_state.as_deref().unwrap_or_default(),
                "building" | "declared-unbuilt"
            ) {
                attention_reasons.push("requires_rebuild".to_string());
            }
            let queryable = declared_present
                && operational_present
                && enabled
                && matches!(build_state.as_deref().unwrap_or_default(), "ready");
            let requires_rebuild = matches!(
                build_state.as_deref().unwrap_or_default(),
                "declared-unbuilt" | "building" | "stale" | "failed"
            );
            let attention_score = usize::from(requires_rebuild).saturating_mul(3)
                + usize::from(!enabled && declared_present)
                + usize::from(declared_present != operational_present)
                + usize::from(!queryable);

            let artifact_state = crate::physical::ArtifactState::from_build_state(
                build_state.as_deref().unwrap_or("declared-unbuilt"),
                enabled,
            );
            CatalogIndexStatus {
                name,
                collection,
                kind,
                declared: declared_present,
                operational: operational_present,
                enabled,
                build_state,
                artifact_state,
                lifecycle_state,
                in_sync: declared_present == operational_present,
                queryable,
                requires_rebuild,
                attention_score,
                attention_reasons,
            }
        })
        .collect()
}

fn index_lifecycle_state(
    declared: bool,
    operational: bool,
    enabled: bool,
    build_state: Option<&str>,
) -> String {
    if !declared && operational {
        return "orphaned-operational".to_string();
    }
    if declared && !enabled {
        return "disabled".to_string();
    }
    if !declared {
        return "unknown".to_string();
    }
    if !operational {
        return "declared".to_string();
    }

    match build_state.unwrap_or_default() {
        "ready" => "ready".to_string(),
        "failed" => "failed".to_string(),
        "stale" => "stale".to_string(),
        "declared-unbuilt" => "declared".to_string(),
        "catalog-derived" | "metadata-only" | "artifact-published" | "registry-loaded" => {
            "building".to_string()
        }
        _ => "building".to_string(),
    }
}

fn index_kind_string(kind: IndexKind) -> String {
    match kind {
        IndexKind::BTree => "btree",
        IndexKind::Hash => "hash",
        IndexKind::Bitmap => "bitmap",
        IndexKind::Spatial => "spatial.rtree",
        IndexKind::VectorHnsw => "vector.hnsw",
        IndexKind::VectorInverted => "vector.inverted",
        IndexKind::GraphAdjacency => "graph.adjacency",
        IndexKind::FullText => "text.fulltext",
        IndexKind::DocumentPathValue => "document.pathvalue",
        IndexKind::HybridSearch => "search.hybrid",
    }
    .to_string()
}

fn analytics_job_statuses(
    declarations: Option<&CatalogDeclarations>,
) -> Vec<CatalogAnalyticsJobStatus> {
    let declared = declarations
        .map(|declarations| declarations.declared_analytics_jobs.as_slice())
        .unwrap_or(&[]);
    let operational = declarations
        .map(|declarations| declarations.operational_analytics_jobs.as_slice())
        .unwrap_or(&[]);
    let declared_projections = declarations
        .map(|declarations| declarations.declared_graph_projections.as_slice())
        .unwrap_or(&[]);
    let operational_projections = declarations
        .map(|declarations| declarations.operational_graph_projections.as_slice())
        .unwrap_or(&[]);

    let mut ids = declared
        .iter()
        .map(|job| job.id.clone())
        .chain(operational.iter().map(|job| job.id.clone()))
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();

    ids.into_iter()
        .map(|id| {
            let declared_job = declared.iter().find(|job| job.id == id);
            let operational_job = operational.iter().find(|job| job.id == id);
            let kind = declared_job
                .map(|job| job.kind.clone())
                .or_else(|| operational_job.map(|job| job.kind.clone()))
                .unwrap_or_default();
            let projection = declared_job
                .and_then(|job| job.projection.clone())
                .or_else(|| operational_job.and_then(|job| job.projection.clone()));
            let state = declared_job
                .map(|job| job.state.clone())
                .or_else(|| operational_job.map(|job| job.state.clone()))
                .unwrap_or_default();
            let declared_present = declared_job.is_some();
            let operational_present = operational_job.is_some();
            let last_run_sequence = declared_job
                .and_then(|job| job.last_run_sequence)
                .or_else(|| operational_job.and_then(|job| job.last_run_sequence));
            let projection_declared = projection.as_ref().map(|projection_name| {
                declared_projections
                    .iter()
                    .any(|projection| projection.name == *projection_name)
            });
            let projection_operational = projection.as_ref().map(|projection_name| {
                operational_projections
                    .iter()
                    .any(|projection| projection.name == *projection_name)
            });
            let projection_lifecycle = projection.as_ref().and_then(|projection_name| {
                declared_projections
                    .iter()
                    .find(|projection| projection.name == *projection_name)
                    .map(|projection| projection.state.as_str())
                    .or_else(|| {
                        operational_projections
                            .iter()
                            .find(|projection| projection.name == *projection_name)
                            .map(|projection| projection.state.as_str())
                    })
            });
            let dependency_in_sync = projection.as_ref().map(|_| {
                matches!(projection_lifecycle, Some("materialized"))
                    && projection_operational == Some(true)
            });
            let lifecycle_state = match (declared_present, operational_present, state.as_str()) {
                (true, false, _) => "declared",
                (true, true, _)
                    if matches!(
                        projection_lifecycle,
                        Some("stale" | "failed" | "materializing" | "declared")
                    ) =>
                {
                    "stale"
                }
                (true, true, "completed") => "completed",
                (true, true, "running") => "running",
                (true, true, "failed") => "failed",
                (true, true, "queued") => "queued",
                (true, true, "stale") => "stale",
                (true, true, _) => "materialized",
                (false, true, _) => "orphaned-operational",
                (false, false, _) => "unknown",
            }
            .to_string();
            let executable = declared_present
                && operational_present
                && !matches!(state.as_str(), "failed" | "stale")
                && dependency_in_sync.unwrap_or(true);
            let requires_rerun =
                matches!(state.as_str(), "stale" | "failed") || dependency_in_sync == Some(false);
            let mut attention_reasons = Vec::new();
            if declared_present != operational_present {
                attention_reasons.push("declaration_drift".to_string());
            }
            if matches!(state.as_str(), "failed") {
                attention_reasons.push("failed".to_string());
            }
            if matches!(state.as_str(), "stale") {
                attention_reasons.push("stale".to_string());
            }
            if dependency_in_sync == Some(false) {
                attention_reasons.push("dependency_drift".to_string());
            }
            if requires_rerun {
                attention_reasons.push("requires_rerun".to_string());
            }
            let attention_score = usize::from(requires_rerun).saturating_mul(3)
                + usize::from(dependency_in_sync == Some(false)).saturating_mul(2)
                + usize::from(declared_present != operational_present)
                + usize::from(!executable);
            CatalogAnalyticsJobStatus {
                id,
                kind,
                projection,
                state: state.clone(),
                lifecycle_state,
                declared: declared_present,
                operational: operational_present,
                in_sync: declared_present == operational_present,
                last_run_sequence,
                projection_declared,
                projection_operational,
                projection_lifecycle_state: projection_lifecycle.map(str::to_string),
                dependency_in_sync,
                executable,
                requires_rerun,
                attention_score,
                attention_reasons,
            }
        })
        .collect()
}

pub fn consistency_report(snapshot: &CatalogModelSnapshot) -> CatalogConsistencyReport {
    let declared_indexes = snapshot
        .declared_indexes
        .iter()
        .map(|index| index.name.clone())
        .collect::<BTreeSet<_>>();
    let operational_indexes = snapshot
        .operational_indexes
        .iter()
        .map(|index| index.name.clone())
        .collect::<BTreeSet<_>>();
    let declared_graph_projections = snapshot
        .declared_graph_projections
        .iter()
        .map(|projection| projection.name.clone())
        .collect::<BTreeSet<_>>();
    let operational_graph_projections = snapshot
        .operational_graph_projections
        .iter()
        .map(|projection| projection.name.clone())
        .collect::<BTreeSet<_>>();
    let declared_analytics_jobs = snapshot
        .declared_analytics_jobs
        .iter()
        .map(|job| job.id.clone())
        .collect::<BTreeSet<_>>();
    let operational_analytics_jobs = snapshot
        .operational_analytics_jobs
        .iter()
        .map(|job| job.id.clone())
        .collect::<BTreeSet<_>>();

    CatalogConsistencyReport {
        declared_index_count: declared_indexes.len(),
        operational_index_count: operational_indexes.len(),
        declared_graph_projection_count: declared_graph_projections.len(),
        operational_graph_projection_count: operational_graph_projections.len(),
        declared_analytics_job_count: declared_analytics_jobs.len(),
        operational_analytics_job_count: operational_analytics_jobs.len(),
        missing_operational_indexes: declared_indexes
            .difference(&operational_indexes)
            .cloned()
            .collect(),
        undeclared_operational_indexes: operational_indexes
            .difference(&declared_indexes)
            .cloned()
            .collect(),
        missing_operational_graph_projections: declared_graph_projections
            .difference(&operational_graph_projections)
            .cloned()
            .collect(),
        undeclared_operational_graph_projections: operational_graph_projections
            .difference(&declared_graph_projections)
            .cloned()
            .collect(),
        missing_operational_analytics_jobs: declared_analytics_jobs
            .difference(&operational_analytics_jobs)
            .cloned()
            .collect(),
        undeclared_operational_analytics_jobs: operational_analytics_jobs
            .difference(&declared_analytics_jobs)
            .cloned()
            .collect(),
    }
}

pub fn attention_summary(snapshot: &CatalogModelSnapshot) -> CatalogAttentionSummary {
    CatalogAttentionSummary {
        collections_requiring_attention: snapshot
            .collections
            .iter()
            .filter(|collection| collection.attention_required)
            .count(),
        indexes_requiring_attention: snapshot
            .index_statuses
            .iter()
            .filter(|status| status.attention_score > 0)
            .count(),
        graph_projections_requiring_attention: snapshot
            .graph_projection_statuses
            .iter()
            .filter(|status| status.attention_score > 0)
            .count(),
        analytics_jobs_requiring_attention: snapshot
            .analytics_job_statuses
            .iter()
            .filter(|status| status.attention_score > 0)
            .count(),
        top_collection: snapshot
            .collections
            .iter()
            .filter(|collection| collection.attention_score > 0)
            .max_by_key(|collection| collection.attention_score)
            .cloned(),
        top_index: snapshot
            .index_statuses
            .iter()
            .filter(|status| status.attention_score > 0)
            .max_by_key(|status| status.attention_score)
            .cloned(),
        top_graph_projection: snapshot
            .graph_projection_statuses
            .iter()
            .filter(|status| status.attention_score > 0)
            .max_by_key(|status| status.attention_score)
            .cloned(),
        top_analytics_job: snapshot
            .analytics_job_statuses
            .iter()
            .filter(|status| status.attention_score > 0)
            .max_by_key(|status| status.attention_score)
            .cloned(),
    }
}

pub fn collection_attention(snapshot: &CatalogModelSnapshot) -> Vec<CollectionDescriptor> {
    let mut collections = snapshot
        .collections
        .iter()
        .filter(|collection| collection.attention_required)
        .cloned()
        .collect::<Vec<_>>();
    collections.sort_by(|left, right| {
        right
            .attention_score
            .cmp(&left.attention_score)
            .then_with(|| left.name.cmp(&right.name))
    });
    collections
}

pub fn index_attention(snapshot: &CatalogModelSnapshot) -> Vec<CatalogIndexStatus> {
    let mut statuses = snapshot
        .index_statuses
        .iter()
        .filter(|status| status.attention_score > 0)
        .cloned()
        .collect::<Vec<_>>();
    statuses.sort_by(|left, right| {
        right
            .attention_score
            .cmp(&left.attention_score)
            .then_with(|| left.name.cmp(&right.name))
    });
    statuses
}

pub fn graph_projection_attention(
    snapshot: &CatalogModelSnapshot,
) -> Vec<CatalogGraphProjectionStatus> {
    let mut statuses = snapshot
        .graph_projection_statuses
        .iter()
        .filter(|status| status.attention_score > 0)
        .cloned()
        .collect::<Vec<_>>();
    statuses.sort_by(|left, right| {
        right
            .attention_score
            .cmp(&left.attention_score)
            .then_with(|| left.name.cmp(&right.name))
    });
    statuses
}

pub fn analytics_job_attention(snapshot: &CatalogModelSnapshot) -> Vec<CatalogAnalyticsJobStatus> {
    let mut statuses = snapshot
        .analytics_job_statuses
        .iter()
        .filter(|status| status.attention_score > 0)
        .cloned()
        .collect::<Vec<_>>();
    statuses.sort_by(|left, right| {
        right
            .attention_score
            .cmp(&left.attention_score)
            .then_with(|| left.id.cmp(&right.id))
    });
    statuses
}
