//! Logical catalog structures for the unified multi-structure model.

use std::collections::{BTreeMap, HashMap};
use std::time::SystemTime;

use crate::api::{CatalogSnapshot, CollectionStats};
use crate::index::{IndexCatalog, IndexCatalogSnapshot, IndexKind};
use crate::storage::{EntityKind, UnifiedEntity};
use crate::storage::unified::UnifiedStore;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionModel {
    Table,
    Document,
    Graph,
    Vector,
    Mixed,
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
    pub entities: usize,
    pub cross_refs: usize,
    pub segments: usize,
    pub indices: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct CatalogModelSnapshot {
    pub summary: CatalogSnapshot,
    pub collections: Vec<CollectionDescriptor>,
    pub indices: IndexCatalogSnapshot,
}

pub fn snapshot_store(
    name: &str,
    store: &UnifiedStore,
    index_catalog: Option<&IndexCatalog>,
) -> CatalogModelSnapshot {
    let mut grouped: HashMap<String, Vec<UnifiedEntity>> = HashMap::new();
    for (collection, entity) in store.query_all(|_| true) {
        grouped.entry(collection).or_default().push(entity);
    }

    let mut stats_by_collection = BTreeMap::new();
    let mut collections = Vec::new();

    for collection_name in store.list_collections() {
        let entities = grouped.remove(&collection_name).unwrap_or_default();
        let model = infer_model(&entities);
        let cross_refs = entities.iter().map(|entity| entity.cross_refs.len()).sum();
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

        collections.push(CollectionDescriptor {
            name: collection_name,
            model,
            schema_mode: infer_schema_mode(model),
            entities: entity_count,
            cross_refs,
            segments,
            indices: infer_indices(model, index_catalog),
        });
    }

    collections.sort_by(|left, right| left.name.cmp(&right.name));

    let summary = CatalogSnapshot {
        name: name.to_string(),
        total_entities: stats_by_collection.values().map(|stats| stats.entities).sum(),
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
    }
}

fn infer_model(entities: &[UnifiedEntity]) -> CollectionModel {
    let mut has_table = false;
    let mut has_graph = false;
    let mut has_vector = false;

    for entity in entities {
        match &entity.kind {
            EntityKind::TableRow { .. } => has_table = true,
            EntityKind::GraphNode { .. } | EntityKind::GraphEdge { .. } => has_graph = true,
            EntityKind::Vector { .. } => has_vector = true,
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
    }
}

fn infer_indices(model: CollectionModel, index_catalog: Option<&IndexCatalog>) -> Vec<String> {
    let available = index_catalog.map(IndexCatalog::snapshot).unwrap_or_default();
    let mut selected = Vec::new();

    for metric in available {
        let relevant = matches!(
            (model, metric.kind),
            (_, IndexKind::BTree)
                | (CollectionModel::Graph, IndexKind::GraphAdjacency)
                | (CollectionModel::Vector, IndexKind::VectorHnsw)
                | (CollectionModel::Vector, IndexKind::VectorInverted)
                | (CollectionModel::Document, IndexKind::FullText)
                | (CollectionModel::Mixed, _)
        );

        if relevant && metric.enabled {
            selected.push(metric.name);
        }
    }

    selected
}
