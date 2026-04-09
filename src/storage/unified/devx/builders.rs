//! Fluent Builders for Entity Creation
//!
//! NodeBuilder, EdgeBuilder, VectorBuilder, RowBuilder, DocumentBuilder for fluent entity creation.

use std::collections::HashMap;
use std::sync::Arc;

use super::super::{
    CrossRef, EdgeData, EntityData, EntityId, EntityKind, Metadata, MetadataValue, NodeData,
    RefType, RowData, UnifiedEntity, UnifiedStore, VectorData,
};
use super::error::DevXError;
use super::refs::{NodeRef, TableRef, VectorRef};
use crate::json::{to_vec as json_to_vec, Value as JsonValue};
use crate::storage::schema::Value;

// ============================================================================
// Node Builder
// ============================================================================

/// Fluent builder for graph nodes
pub struct NodeBuilder {
    store: Arc<UnifiedStore>,
    collection: String,
    label: String,
    node_type: String,
    properties: HashMap<String, Value>,
    metadata: HashMap<String, MetadataValue>,
    embeddings: Vec<(String, Vec<f32>, String)>, // (name, vector, model)
    links: Vec<(EntityId, String, f32)>,         // (target, label, weight)
    cross_links: Vec<(EntityId, String, RefType)>, // (target, collection, ref_type)
}

impl NodeBuilder {
    pub(crate) fn new(
        store: Arc<UnifiedStore>,
        collection: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        let label_str = label.into();
        Self {
            store,
            collection: collection.into(),
            label: label_str.clone(),
            node_type: label_str,
            properties: HashMap::new(),
            metadata: HashMap::new(),
            embeddings: Vec::new(),
            links: Vec::new(),
            cross_links: Vec::new(),
        }
    }

    /// Set node type (defaults to label)
    pub fn node_type(mut self, node_type: impl Into<String>) -> Self {
        self.node_type = node_type.into();
        self
    }

    /// Add a property
    pub fn property(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.properties.insert(key.into(), value.into());
        self
    }

    /// Add multiple properties at once
    pub fn properties(
        mut self,
        props: impl IntoIterator<Item = (impl Into<String>, impl Into<Value>)>,
    ) -> Self {
        for (k, v) in props {
            self.properties.insert(k.into(), v.into());
        }
        self
    }

    /// Add metadata
    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<MetadataValue>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Add metadata linking to a table row
    pub fn link_to_table(mut self, key: impl Into<String>, table_ref: TableRef) -> Self {
        self.metadata.insert(key.into(), table_ref.to_metadata());
        self.cross_links.push((
            EntityId::new(table_ref.row_id),
            table_ref.table.clone(),
            RefType::NodeToRow,
        ));
        self
    }

    /// Add metadata linking to another node
    pub fn link_to_node(mut self, key: impl Into<String>, node_ref: NodeRef) -> Self {
        self.metadata.insert(key.into(), node_ref.to_metadata());
        self
    }

    /// Add an embedding vector
    pub fn embedding(mut self, name: impl Into<String>, vector: Vec<f32>) -> Self {
        self.embeddings
            .push((name.into(), vector, "default".to_string()));
        self
    }

    /// Add an embedding with model name
    pub fn embedding_with_model(
        mut self,
        name: impl Into<String>,
        vector: Vec<f32>,
        model: impl Into<String>,
    ) -> Self {
        self.embeddings.push((name.into(), vector, model.into()));
        self
    }

    /// Link to another node (creates edge)
    pub fn link_to(mut self, target: EntityId, edge_label: impl Into<String>) -> Self {
        self.links.push((target, edge_label.into(), 1.0));
        self
    }

    /// Link to another node with weight
    pub fn link_to_weighted(
        mut self,
        target: EntityId,
        edge_label: impl Into<String>,
        weight: f32,
    ) -> Self {
        self.links.push((target, edge_label.into(), weight));
        self
    }

    /// Save the node and return its ID
    pub fn save(self) -> Result<EntityId, DevXError> {
        // Create the node entity
        let kind = EntityKind::GraphNode {
            label: self.label,
            node_type: self.node_type,
        };

        let data = EntityData::Node(NodeData::with_properties(self.properties));

        let id = self.store.next_entity_id();

        let mut entity = UnifiedEntity::new(id, kind, data);

        // Add embeddings
        for (name, vector, model) in self.embeddings {
            entity.add_embedding(super::super::EmbeddingSlot::new(name, vector, model));
        }
        for (target, target_collection, ref_type) in self.cross_links {
            entity.add_cross_ref(CrossRef::new(id, target, target_collection, ref_type));
        }

        // Insert the entity
        let id = self
            .store
            .insert_auto(&self.collection, entity)
            .map_err(|e| DevXError::Storage(format!("{:?}", e)))?;

        // Store metadata
        if !self.metadata.is_empty() {
            let _ = self.store.set_metadata(
                &self.collection,
                id,
                Metadata::with_fields(self.metadata.clone()),
            );
        }

        // Create edges for links
        for (target, edge_label, weight) in self.links {
            let edge_kind = EntityKind::GraphEdge {
                label: edge_label,
                from_node: id.0.to_string(),
                to_node: target.0.to_string(),
                weight: (weight * 1000.0) as u32,
            };

            let edge_data = EntityData::Edge(EdgeData::new(weight));
            let edge_id = self.store.next_entity_id();
            let mut edge_entity = UnifiedEntity::new(edge_id, edge_kind, edge_data);

            // Add cross-refs for fast traversal
            edge_entity.add_cross_ref(CrossRef::new(
                edge_id,
                target,
                self.collection.clone(),
                RefType::RelatedTo,
            ));

            let _ = self.store.insert_auto(&self.collection, edge_entity);

            // Add cross-ref from source node to edge
            let _ = self.store.add_cross_ref(
                &self.collection,
                id,
                &self.collection,
                edge_id,
                RefType::RelatedTo,
                1.0,
            );
        }

        Ok(id)
    }
}

// ============================================================================
// Edge Builder
// ============================================================================

/// Fluent builder for graph edges
pub struct EdgeBuilder {
    store: Arc<UnifiedStore>,
    collection: String,
    label: String,
    from_node: Option<EntityId>,
    to_node: Option<EntityId>,
    weight: f32,
    properties: HashMap<String, Value>,
    metadata: HashMap<String, MetadataValue>,
}

impl EdgeBuilder {
    pub(crate) fn new(
        store: Arc<UnifiedStore>,
        collection: impl Into<String>,
        label: impl Into<String>,
    ) -> Self {
        Self {
            store,
            collection: collection.into(),
            label: label.into(),
            from_node: None,
            to_node: None,
            weight: 1.0,
            properties: HashMap::new(),
            metadata: HashMap::new(),
        }
    }

    /// Set source node
    pub fn from(mut self, node_id: EntityId) -> Self {
        self.from_node = Some(node_id);
        self
    }

    /// Set target node
    pub fn to(mut self, node_id: EntityId) -> Self {
        self.to_node = Some(node_id);
        self
    }

    /// Set edge weight
    pub fn weight(mut self, weight: f32) -> Self {
        self.weight = weight;
        self
    }

    /// Add a property
    pub fn property(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.properties.insert(key.into(), value.into());
        self
    }

    /// Add metadata
    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<MetadataValue>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Link metadata to a table row
    pub fn link_to_table(mut self, key: impl Into<String>, table_ref: TableRef) -> Self {
        self.metadata.insert(key.into(), table_ref.to_metadata());
        self
    }

    /// Save the edge
    pub fn save(self) -> Result<EntityId, DevXError> {
        let from = self
            .from_node
            .ok_or_else(|| DevXError::Validation("Edge requires 'from' node".into()))?;
        let to = self
            .to_node
            .ok_or_else(|| DevXError::Validation("Edge requires 'to' node".into()))?;

        let kind = EntityKind::GraphEdge {
            label: self.label,
            from_node: from.0.to_string(),
            to_node: to.0.to_string(),
            weight: (self.weight * 1000.0) as u32,
        };

        let mut edge_data = EdgeData::new(self.weight);
        edge_data.properties = self.properties;

        let id = self.store.next_entity_id();

        let mut entity = UnifiedEntity::new(id, kind, EntityData::Edge(edge_data));

        // Add cross-refs for bidirectional traversal
        entity.add_cross_ref(CrossRef::new(
            id,
            from,
            self.collection.clone(),
            RefType::DerivesFrom,
        ));
        entity.add_cross_ref(CrossRef::new(
            id,
            to,
            self.collection.clone(),
            RefType::RelatedTo,
        ));

        let id = self
            .store
            .insert_auto(&self.collection, entity)
            .map_err(|e| DevXError::Storage(format!("{:?}", e)))?;

        // Store metadata
        if !self.metadata.is_empty() {
            let _ = self.store.set_metadata(
                &self.collection,
                id,
                Metadata::with_fields(self.metadata.clone()),
            );
        }

        // Update source and target nodes with cross-refs
        let _ = self.store.add_cross_ref(
            &self.collection,
            from,
            &self.collection,
            id,
            RefType::RelatedTo,
            1.0,
        );
        let _ = self.store.add_cross_ref(
            &self.collection,
            to,
            &self.collection,
            id,
            RefType::RelatedTo,
            1.0,
        );

        Ok(id)
    }
}

// ============================================================================
// Vector Builder
// ============================================================================

/// Fluent builder for vectors
pub struct VectorBuilder {
    store: Arc<UnifiedStore>,
    collection: String,
    dense: Option<Vec<f32>>,
    sparse: Option<Vec<(u32, f32)>>,
    content: Option<String>,
    metadata: HashMap<String, MetadataValue>,
    links: Vec<(EntityId, String, RefType)>,
}

impl VectorBuilder {
    pub(crate) fn new(store: Arc<UnifiedStore>, collection: impl Into<String>) -> Self {
        Self {
            store,
            collection: collection.into(),
            dense: None,
            sparse: None,
            content: None,
            metadata: HashMap::new(),
            links: Vec::new(),
        }
    }

    /// Set dense vector
    pub fn dense(mut self, vector: Vec<f32>) -> Self {
        self.dense = Some(vector);
        self
    }

    /// Set sparse vector
    pub fn sparse(mut self, indices_values: Vec<(u32, f32)>) -> Self {
        self.sparse = Some(indices_values);
        self
    }

    /// Set original content
    pub fn content(mut self, content: impl Into<String>) -> Self {
        self.content = Some(content.into());
        self
    }

    /// Add metadata
    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<MetadataValue>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Link to a table row
    pub fn link_to_table(mut self, table_ref: TableRef) -> Self {
        self.metadata
            .insert("_source_table".to_string(), table_ref.to_metadata());
        self.links.push((
            EntityId::new(table_ref.row_id),
            table_ref.table,
            RefType::VectorToRow,
        ));
        self
    }

    /// Link to a node
    pub fn link_to_node(mut self, node_ref: NodeRef) -> Self {
        self.links
            .push((node_ref.node_id, node_ref.collection, RefType::VectorToNode));
        self
    }

    /// Save the vector
    pub fn save(self) -> Result<EntityId, DevXError> {
        let dense = self
            .dense
            .ok_or_else(|| DevXError::Validation("Vector requires dense data".into()))?;

        // Capture dimension before moving dense
        let dense_len = dense.len();

        let kind = EntityKind::Vector {
            collection: self.collection.clone(),
        };

        let mut vec_data = VectorData::new(dense);
        vec_data.content = self.content;

        if let Some(sparse_data) = self.sparse {
            // Unzip indices and values from tuples
            let (indices, values): (Vec<u32>, Vec<f32>) = sparse_data.into_iter().unzip();
            // Dimension is dense length or max sparse index + 1
            let dimension = dense_len.max(
                indices
                    .iter()
                    .copied()
                    .max()
                    .map(|m| m as usize + 1)
                    .unwrap_or(0),
            );
            vec_data.sparse = Some(super::super::SparseVector::new(indices, values, dimension));
        }

        let id = self.store.next_entity_id();
        let mut entity = UnifiedEntity::new(id, kind, EntityData::Vector(vec_data));

        // Add cross-refs
        for (target, target_collection, ref_type) in self.links {
            entity.add_cross_ref(CrossRef::new(id, target, target_collection, ref_type));
        }

        let id = self
            .store
            .insert_auto(&self.collection, entity)
            .map_err(|e| DevXError::Storage(format!("{:?}", e)))?;

        // Store metadata
        if !self.metadata.is_empty() {
            let _ =
                self.store
                    .set_metadata(&self.collection, id, Metadata::with_fields(self.metadata));
        }

        Ok(id)
    }
}

// ============================================================================
// Row Builder
// ============================================================================

/// Fluent builder for table rows
pub struct RowBuilder {
    store: Arc<UnifiedStore>,
    table: String,
    columns: Vec<Value>,
    named: HashMap<String, Value>,
    metadata: HashMap<String, MetadataValue>,
    links: Vec<(EntityId, String, RefType)>,
}

impl RowBuilder {
    pub(crate) fn new(
        store: Arc<UnifiedStore>,
        table: impl Into<String>,
        columns: Vec<(&str, Value)>,
    ) -> Self {
        let mut named = HashMap::new();
        let mut col_values = Vec::new();

        for (name, value) in columns {
            named.insert(name.to_string(), value.clone());
            col_values.push(value);
        }

        Self {
            store,
            table: table.into(),
            columns: col_values,
            named,
            metadata: HashMap::new(),
            links: Vec::new(),
        }
    }

    /// Add metadata
    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<MetadataValue>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Link to a node
    pub fn link_to_node(mut self, node_ref: NodeRef) -> Self {
        self.links
            .push((node_ref.node_id, node_ref.collection, RefType::RowToNode));
        self
    }

    /// Link to a vector
    pub fn link_to_vector(mut self, vector_ref: VectorRef) -> Self {
        self.links.push((
            vector_ref.vector_id,
            vector_ref.collection,
            RefType::RowToVector,
        ));
        self
    }

    /// Save the row
    pub fn save(self) -> Result<EntityId, DevXError> {
        let id = self.store.next_entity_id();

        let kind = EntityKind::TableRow {
            table: self.table.clone(),
            row_id: id.0,
        };

        let mut row_data = RowData::new(self.columns);
        row_data.named = Some(self.named);

        let mut entity = UnifiedEntity::new(id, kind, EntityData::Row(row_data));

        // Add cross-refs
        for (target, target_collection, ref_type) in self.links {
            entity.add_cross_ref(CrossRef::new(id, target, target_collection, ref_type));
        }

        let id = self
            .store
            .insert_auto(&self.table, entity)
            .map_err(|e| DevXError::Storage(format!("{:?}", e)))?;

        // Store metadata
        if !self.metadata.is_empty() {
            let _ = self
                .store
                .set_metadata(&self.table, id, Metadata::with_fields(self.metadata));
        }

        Ok(id)
    }
}

// ============================================================================
// KV Builder
// ============================================================================

/// Fluent builder for key-value pairs
///
/// Stores KV pairs as table rows with named fields `key` (Text) and `value`.
pub struct KvBuilder {
    store: Arc<UnifiedStore>,
    collection: String,
    key: String,
    value: Value,
    metadata: HashMap<String, MetadataValue>,
}

impl KvBuilder {
    pub(crate) fn new(
        store: Arc<UnifiedStore>,
        collection: impl Into<String>,
        key: impl Into<String>,
        value: Value,
    ) -> Self {
        Self {
            store,
            collection: collection.into(),
            key: key.into(),
            value,
            metadata: HashMap::new(),
        }
    }

    /// Add metadata
    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<MetadataValue>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Save the key-value pair as a table row with named fields `key` and `value`
    pub fn save(self) -> Result<EntityId, DevXError> {
        let columns = vec![("key", Value::Text(self.key)), ("value", self.value)];
        let mut builder = RowBuilder::new(self.store, &self.collection, columns);
        for (k, v) in self.metadata {
            builder = builder.metadata(k, v);
        }
        builder.save()
    }
}

// ============================================================================
// Document Builder
// ============================================================================

/// Fluent builder for documents stored as enriched table rows.
///
/// Documents are stored as `TableRow` entities with:
/// - A `body` named field containing the full JSON serialized as `Value::Json`
/// - Flattened top-level keys from the body as additional named fields for filtering
///
/// # Example
/// ```ignore
/// let doc = db.doc("articles")
///     .field("title", "First Post")
///     .field("views", 42)
///     .metadata("source", "web")
///     .save()?;
/// ```
pub struct DocumentBuilder {
    store: Arc<UnifiedStore>,
    collection: String,
    body: HashMap<String, JsonValue>,
    metadata: HashMap<String, MetadataValue>,
    links: Vec<(EntityId, String, RefType)>,
}

impl DocumentBuilder {
    pub(crate) fn new(store: Arc<UnifiedStore>, collection: impl Into<String>) -> Self {
        Self {
            store,
            collection: collection.into(),
            body: HashMap::new(),
            metadata: HashMap::new(),
            links: Vec::new(),
        }
    }

    /// Set a document field
    pub fn field(mut self, key: impl Into<String>, value: impl Into<JsonValue>) -> Self {
        self.body.insert(key.into(), value.into());
        self
    }

    /// Add metadata
    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<MetadataValue>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }

    /// Link to a graph node
    pub fn link_to_node(mut self, node_ref: NodeRef) -> Self {
        self.links
            .push((node_ref.node_id, node_ref.collection, RefType::RowToNode));
        self
    }

    /// Link to a vector
    pub fn link_to_vector(mut self, vector_ref: VectorRef) -> Self {
        self.links.push((
            vector_ref.vector_id,
            vector_ref.collection,
            RefType::RowToVector,
        ));
        self
    }

    /// Save the document and return its ID
    pub fn save(self) -> Result<EntityId, DevXError> {
        let id = self.store.next_entity_id();

        let kind = EntityKind::TableRow {
            table: self.collection.clone(),
            row_id: id.0,
        };

        // Build the full JSON body object
        let body_object = JsonValue::Object(
            self.body
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        );
        let body_bytes = json_to_vec(&body_object)
            .map_err(|e| DevXError::Storage(format!("failed to serialize document body: {e}")))?;

        // Build named fields: "body" (full JSON) + flattened top-level keys
        let mut named = HashMap::new();
        named.insert("body".to_string(), Value::Json(body_bytes));

        for (key, value) in &self.body {
            let storage_value = json_value_to_storage_value(value);
            named.insert(key.clone(), storage_value);
        }

        let mut row_data = RowData::new(Vec::new());
        row_data.named = Some(named);

        let mut entity = UnifiedEntity::new(id, kind, EntityData::Row(row_data));

        // Add cross-refs
        for (target, target_collection, ref_type) in self.links {
            entity.add_cross_ref(CrossRef::new(id, target, target_collection, ref_type));
        }

        let id = self
            .store
            .insert_auto(&self.collection, entity)
            .map_err(|e| DevXError::Storage(format!("{:?}", e)))?;

        // Store metadata
        if !self.metadata.is_empty() {
            let _ =
                self.store
                    .set_metadata(&self.collection, id, Metadata::with_fields(self.metadata));
        }

        Ok(id)
    }
}

/// Convert a JSON value to a storage value for flattened document fields.
fn json_value_to_storage_value(value: &JsonValue) -> Value {
    match value {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(b) => Value::Boolean(*b),
        JsonValue::Number(n) => {
            if n.fract().abs() < f64::EPSILON {
                Value::Integer(*n as i64)
            } else {
                Value::Float(*n)
            }
        }
        JsonValue::String(s) => Value::Text(s.clone()),
        JsonValue::Array(_) | JsonValue::Object(_) => match json_to_vec(value) {
            Ok(bytes) => Value::Json(bytes),
            Err(_) => Value::Null,
        },
    }
}
