//! Batch Operations for High-Performance Bulk Inserts
//!
//! BatchBuilder for efficient multi-entity insertion.

use std::collections::HashMap;
use std::sync::Arc;

use super::super::{
    CrossRef, EntityData, EntityId, EntityKind, GraphNodeKind, Metadata, MetadataValue, NodeData,
    RefType, RowData, UnifiedEntity, UnifiedStore, VectorData,
};
use super::error::DevXError;
use super::{run_preprocessors, SharedPreprocessors};
use crate::storage::schema::Value;
use crate::storage::unified::devx::refs::{NodeRef, VectorRef};

/// Batch operations for high-performance bulk inserts
pub struct BatchBuilder {
    store: Arc<UnifiedStore>,
    preprocessors: SharedPreprocessors,
    nodes: Vec<(String, UnifiedEntity, HashMap<String, MetadataValue>)>,
    edges: Vec<(String, UnifiedEntity, HashMap<String, MetadataValue>)>,
    vectors: Vec<(String, UnifiedEntity, HashMap<String, MetadataValue>)>,
    rows: Vec<(String, UnifiedEntity, HashMap<String, MetadataValue>)>,
}

impl BatchBuilder {
    pub(crate) fn new(store: Arc<UnifiedStore>, preprocessors: SharedPreprocessors) -> Self {
        Self {
            store,
            preprocessors,
            nodes: Vec::new(),
            edges: Vec::new(),
            vectors: Vec::new(),
            rows: Vec::new(),
        }
    }

    /// Add a node to the batch
    pub fn add_node(
        mut self,
        collection: impl Into<String>,
        label: impl Into<String>,
        properties: HashMap<String, Value>,
        metadata: HashMap<String, MetadataValue>,
    ) -> Self {
        let collection = collection.into();
        let label_str = label.into();

        let id = self.store.next_entity_id();

        let kind = EntityKind::GraphNode(Box::new(GraphNodeKind {
            label: label_str.clone(),
            node_type: label_str,
        }));

        let entity = UnifiedEntity::new(
            id,
            kind,
            EntityData::Node(NodeData::with_properties(properties)),
        );
        self.nodes.push((collection, entity, metadata));
        self
    }

    /// Add a vector to the batch
    pub fn add_vector(
        mut self,
        collection: impl Into<String>,
        dense: Vec<f32>,
        content: Option<String>,
        metadata: HashMap<String, MetadataValue>,
    ) -> Self {
        let collection = collection.into();

        let id = self.store.next_entity_id();

        let kind = EntityKind::Vector {
            collection: collection.clone(),
        };

        let mut vec_data = VectorData::new(dense);
        vec_data.content = content;

        let entity = UnifiedEntity::new(id, kind, EntityData::Vector(vec_data));
        self.vectors.push((collection, entity, metadata));
        self
    }

    /// Add a table row to the batch.
    pub fn add_row(
        mut self,
        collection: impl Into<String>,
        fields: Vec<(String, Value)>,
        metadata: HashMap<String, MetadataValue>,
        node_links: Vec<NodeRef>,
        vector_links: Vec<VectorRef>,
    ) -> Self {
        let collection = collection.into();
        let id = self.store.next_entity_id();
        let kind = EntityKind::TableRow {
            table: Arc::from(collection.as_str()),
            row_id: 0,
        };

        let mut row = RowData::new(fields.iter().map(|(_, value)| value.clone()).collect());
        row.named = Some(fields.into_iter().collect());
        let mut entity = UnifiedEntity::new(id, kind, EntityData::Row(row));
        for node_ref in node_links {
            entity.add_cross_ref(CrossRef::new(
                id,
                node_ref.node_id,
                node_ref.collection,
                RefType::RowToNode,
            ));
        }
        for vector_ref in vector_links {
            entity.add_cross_ref(CrossRef::new(
                id,
                vector_ref.vector_id,
                vector_ref.collection,
                RefType::RowToVector,
            ));
        }

        self.rows.push((collection, entity, metadata));
        self
    }

    /// Execute the batch
    pub fn execute(self) -> Result<BatchResult, DevXError> {
        let mut inserted_nodes = Vec::new();
        let mut inserted_edges = Vec::new();
        let mut inserted_vectors = Vec::new();
        let mut inserted_rows = Vec::new();

        // Insert nodes
        for (collection, mut entity, metadata) in self.nodes {
            let id = entity.id;
            run_preprocessors(&self.preprocessors, &mut entity)?;
            if self.store.insert_auto(&collection, entity).is_ok() {
                if !metadata.is_empty() {
                    let _ =
                        self.store
                            .set_metadata(&collection, id, Metadata::with_fields(metadata));
                }
                inserted_nodes.push(id);
            }
        }

        // Insert vectors
        for (collection, mut entity, metadata) in self.vectors {
            let id = entity.id;
            run_preprocessors(&self.preprocessors, &mut entity)?;
            if self.store.insert_auto(&collection, entity).is_ok() {
                if !metadata.is_empty() {
                    let _ =
                        self.store
                            .set_metadata(&collection, id, Metadata::with_fields(metadata));
                }
                inserted_vectors.push(id);
            }
        }

        // Insert edges
        for (collection, mut entity, metadata) in self.edges {
            let id = entity.id;
            run_preprocessors(&self.preprocessors, &mut entity)?;
            if self.store.insert_auto(&collection, entity).is_ok() {
                if !metadata.is_empty() {
                    let _ =
                        self.store
                            .set_metadata(&collection, id, Metadata::with_fields(metadata));
                }
                inserted_edges.push(id);
            }
        }

        // Insert rows. Keep them on the same physical bulk-insert path used by
        // other frontends, while still running preprocessors and post-insert
        // metadata/context/cross-ref maintenance exactly once per row.
        let mut rows = self.rows.into_iter().peekable();
        while let Some((collection, mut first_entity, first_metadata)) = rows.next() {
            let mut entities = Vec::new();
            let mut metadata_items = Vec::new();
            run_preprocessors(&self.preprocessors, &mut first_entity)?;
            entities.push(first_entity);
            metadata_items.push(first_metadata);

            while let Some((next_collection, _, _)) = rows.peek() {
                if next_collection != &collection {
                    break;
                }
                let (_, mut entity, metadata) = rows.next().expect("peeked row missing");
                run_preprocessors(&self.preprocessors, &mut entity)?;
                entities.push(entity);
                metadata_items.push(metadata);
            }

            let ids = self
                .store
                .bulk_insert(&collection, entities)
                .map_err(|err| DevXError::Storage(format!("{err:?}")))?;

            for (id, metadata) in ids.iter().zip(metadata_items.into_iter()) {
                if !metadata.is_empty() {
                    let _ =
                        self.store
                            .set_metadata(&collection, *id, Metadata::with_fields(metadata));
                }
                if let Some(entity) = self.store.get(&collection, *id) {
                    self.store
                        .context_index()
                        .index_entity(&collection, &entity);
                    let _ = self.store.index_cross_refs(&entity, &collection);
                }
                inserted_rows.push(*id);
            }
        }

        Ok(BatchResult {
            nodes: inserted_nodes,
            edges: inserted_edges,
            vectors: inserted_vectors,
            rows: inserted_rows,
        })
    }
}

/// Result of batch operations
#[derive(Debug)]
pub struct BatchResult {
    pub nodes: Vec<EntityId>,
    pub edges: Vec<EntityId>,
    pub vectors: Vec<EntityId>,
    pub rows: Vec<EntityId>,
}

impl BatchResult {
    pub fn total(&self) -> usize {
        self.nodes.len() + self.edges.len() + self.vectors.len() + self.rows.len()
    }
}
