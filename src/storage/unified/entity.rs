//! Unified Entity Model
//!
//! Provides a single entity type that can represent table rows, graph nodes,
//! graph edges, or vectors with seamless interoperability.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use crate::storage::schema::Value;

/// Unique identifier for any entity in the unified storage
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EntityId(pub u64);

impl EntityId {
    /// Create a new entity ID
    pub fn new(id: u64) -> Self {
        Self(id)
    }

    /// Get the raw ID value
    pub fn raw(&self) -> u64 {
        self.0
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "e{}", self.0)
    }
}

impl From<u64> for EntityId {
    fn from(id: u64) -> Self {
        Self(id)
    }
}

/// The kind of entity (what storage type it belongs to)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EntityKind {
    /// A row in a structured table
    TableRow { table: Arc<str>, row_id: u64 },
    /// A node in the graph
    GraphNode { label: String, node_type: String },
    /// An edge in the graph
    GraphEdge {
        label: String,
        from_node: String,
        to_node: String,
        weight: u32, // Fixed-point weight (x1000)
    },
    /// A vector in a collection
    Vector { collection: String },
    /// A time-series data point
    TimeSeriesPoint { series: String, metric: String },
    /// A queue message
    QueueMessage { queue: String, position: u64 },
}

impl EntityKind {
    /// Get the storage type as a string
    pub fn storage_type(&self) -> &'static str {
        match self {
            Self::TableRow { .. } => "table",
            Self::GraphNode { .. } => "graph_node",
            Self::GraphEdge { .. } => "graph_edge",
            Self::Vector { .. } => "vector",
            Self::TimeSeriesPoint { .. } => "timeseries",
            Self::QueueMessage { .. } => "queue",
        }
    }

    /// Get the collection/table name
    pub fn collection(&self) -> &str {
        match self {
            Self::TableRow { table, .. } => table,
            Self::GraphNode { label, .. } => label,
            Self::GraphEdge { label, .. } => label,
            Self::Vector { collection } => collection,
            Self::TimeSeriesPoint { series, .. } => series,
            Self::QueueMessage { queue, .. } => queue,
        }
    }
}

/// The actual data content of an entity
#[derive(Debug, Clone)]
pub enum EntityData {
    /// Table row data
    Row(RowData),
    /// Graph node data
    Node(NodeData),
    /// Graph edge data
    Edge(EdgeData),
    /// Vector data
    Vector(VectorData),
    /// Time-series data point
    TimeSeries(TimeSeriesData),
    /// Queue message data
    QueueMessage(QueueMessageData),
}

impl EntityData {
    /// Check if this is row data
    pub fn is_row(&self) -> bool {
        matches!(self, Self::Row(_))
    }

    /// Check if this is node data
    pub fn is_node(&self) -> bool {
        matches!(self, Self::Node(_))
    }

    /// Check if this is edge data
    pub fn is_edge(&self) -> bool {
        matches!(self, Self::Edge(_))
    }

    /// Check if this is vector data
    pub fn is_vector(&self) -> bool {
        matches!(self, Self::Vector(_))
    }

    /// Get as row data
    pub fn as_row(&self) -> Option<&RowData> {
        match self {
            Self::Row(r) => Some(r),
            _ => None,
        }
    }

    /// Get as node data
    pub fn as_node(&self) -> Option<&NodeData> {
        match self {
            Self::Node(n) => Some(n),
            _ => None,
        }
    }

    /// Get as edge data
    pub fn as_edge(&self) -> Option<&EdgeData> {
        match self {
            Self::Edge(e) => Some(e),
            _ => None,
        }
    }

    /// Get as vector data
    pub fn as_vector(&self) -> Option<&VectorData> {
        match self {
            Self::Vector(v) => Some(v),
            _ => None,
        }
    }
}

/// Data for a table row
#[derive(Debug, Clone)]
pub struct RowData {
    /// Column values in schema order
    pub columns: Vec<Value>,
    /// Named column access (optional, for convenience)
    pub named: Option<HashMap<String, Value>>,
    /// Shared column schema: column names in order (maps index → name).
    /// When set, `columns` holds the values and `named` is None.
    /// This saves ~60% memory vs per-row HashMap.
    pub schema: Option<std::sync::Arc<Vec<String>>>,
}

impl RowData {
    /// Create new row data from column values
    pub fn new(columns: Vec<Value>) -> Self {
        Self {
            columns,
            named: None,
            schema: None,
        }
    }

    /// Create row data with named columns
    pub fn with_names(columns: Vec<Value>, names: Vec<String>) -> Self {
        let named: HashMap<String, Value> =
            names.into_iter().zip(columns.iter().cloned()).collect();
        Self {
            columns,
            named: Some(named),
            schema: None,
        }
    }

    /// Get a named field value — checks named HashMap first, then schema+columns.
    pub fn get_field(&self, name: &str) -> Option<&Value> {
        // Fast path: named HashMap
        if let Some(ref named) = self.named {
            return named.get(name);
        }
        // Columnar path: use schema to find index
        if let Some(ref schema) = self.schema {
            if let Some(idx) = schema.iter().position(|s| s == name) {
                return self.columns.get(idx);
            }
        }
        None
    }

    /// Iterate over all (name, value) pairs — works for both named and columnar.
    pub fn iter_fields(&self) -> Box<dyn Iterator<Item = (&str, &Value)> + '_> {
        if let Some(ref named) = self.named {
            Box::new(named.iter().map(|(k, v)| (k.as_str(), v)))
        } else if let Some(ref schema) = self.schema {
            Box::new(
                schema
                    .iter()
                    .zip(self.columns.iter())
                    .map(|(k, v)| (k.as_str(), v)),
            )
        } else {
            Box::new(std::iter::empty())
        }
    }

    /// Get column by index
    pub fn get(&self, index: usize) -> Option<&Value> {
        self.columns.get(index)
    }

    /// Get column by name
    pub fn get_by_name(&self, name: &str) -> Option<&Value> {
        self.named.as_ref()?.get(name)
    }

    /// Number of columns
    pub fn len(&self) -> usize {
        self.columns.len()
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }
}

/// Data for a graph node
#[derive(Debug, Clone)]
pub struct NodeData {
    /// Node properties
    pub properties: HashMap<String, Value>,
}

impl NodeData {
    /// Create new node data
    pub fn new() -> Self {
        Self {
            properties: HashMap::new(),
        }
    }

    /// Create with properties
    pub fn with_properties(properties: HashMap<String, Value>) -> Self {
        Self { properties }
    }

    /// Set a property
    pub fn set(&mut self, key: impl Into<String>, value: Value) {
        self.properties.insert(key.into(), value);
    }

    /// Get a property
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.properties.get(key)
    }

    /// Check if property exists
    pub fn has(&self, key: &str) -> bool {
        self.properties.contains_key(key)
    }
}

impl Default for NodeData {
    fn default() -> Self {
        Self::new()
    }
}

/// Data for a graph edge
#[derive(Debug, Clone)]
pub struct EdgeData {
    /// Edge properties
    pub properties: HashMap<String, Value>,
    /// Edge weight (for weighted graphs)
    pub weight: f32,
}

impl EdgeData {
    /// Create new edge data
    pub fn new(weight: f32) -> Self {
        Self {
            properties: HashMap::new(),
            weight,
        }
    }

    /// Create with properties
    pub fn with_properties(weight: f32, properties: HashMap<String, Value>) -> Self {
        Self { properties, weight }
    }

    /// Set a property
    pub fn set(&mut self, key: impl Into<String>, value: Value) {
        self.properties.insert(key.into(), value);
    }

    /// Get a property
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.properties.get(key)
    }
}

impl Default for EdgeData {
    fn default() -> Self {
        Self::new(1.0)
    }
}

/// Data for a vector
#[derive(Debug, Clone)]
pub struct VectorData {
    /// Dense vector (primary embedding)
    pub dense: Vec<f32>,
    /// Optional sparse vector
    pub sparse: Option<SparseVector>,
    /// Original content (if applicable)
    pub content: Option<String>,
}

impl VectorData {
    /// Create new vector data from dense vector
    pub fn new(dense: Vec<f32>) -> Self {
        Self {
            dense,
            sparse: None,
            content: None,
        }
    }

    /// Create with sparse vector
    pub fn with_sparse(dense: Vec<f32>, sparse: SparseVector) -> Self {
        Self {
            dense,
            sparse: Some(sparse),
            content: None,
        }
    }

    /// Set content
    pub fn with_content(mut self, content: impl Into<String>) -> Self {
        self.content = Some(content.into());
        self
    }

    /// Get dimension
    pub fn dimension(&self) -> usize {
        self.dense.len()
    }

    /// Check if has sparse component
    pub fn is_hybrid(&self) -> bool {
        self.sparse.is_some()
    }
}

/// Time-series data point
#[derive(Debug, Clone)]
pub struct TimeSeriesData {
    /// Metric name (e.g., "cpu.idle")
    pub metric: String,
    /// Timestamp in nanoseconds since epoch
    pub timestamp_ns: u64,
    /// Metric value
    pub value: f64,
    /// Dimensional tags (e.g., {"host": "srv1"})
    pub tags: std::collections::HashMap<String, String>,
}

/// Queue message data
#[derive(Debug, Clone)]
pub struct QueueMessageData {
    /// Message payload
    pub payload: Value,
    /// Optional priority (higher = more urgent)
    pub priority: Option<i32>,
    /// Enqueue timestamp (nanoseconds)
    pub enqueued_at_ns: u64,
    /// Number of delivery attempts
    pub attempts: u32,
    /// Maximum delivery attempts before DLQ
    pub max_attempts: u32,
    /// Whether the message has been acknowledged
    pub acked: bool,
}

/// Sparse vector representation
#[derive(Debug, Clone)]
pub struct SparseVector {
    /// Indices of non-zero elements
    pub indices: Vec<u32>,
    /// Values at those indices
    pub values: Vec<f32>,
    /// Total dimension (may be larger than indices.len())
    pub dimension: usize,
}

impl SparseVector {
    /// Create new sparse vector
    pub fn new(indices: Vec<u32>, values: Vec<f32>, dimension: usize) -> Self {
        debug_assert_eq!(indices.len(), values.len());
        Self {
            indices,
            values,
            dimension,
        }
    }

    /// Number of non-zero elements
    pub fn nnz(&self) -> usize {
        self.indices.len()
    }

    /// Sparsity ratio
    pub fn sparsity(&self) -> f32 {
        if self.dimension == 0 {
            1.0
        } else {
            1.0 - (self.nnz() as f32 / self.dimension as f32)
        }
    }

    /// Get value at index (0 if not present)
    pub fn get(&self, index: u32) -> f32 {
        self.indices
            .iter()
            .position(|&i| i == index)
            .map(|pos| self.values[pos])
            .unwrap_or(0.0)
    }
}

/// A slot for embedding a specific aspect of an entity
#[derive(Debug, Clone)]
pub struct EmbeddingSlot {
    /// Slot name (e.g., "content", "summary", "title", "code")
    pub name: String,
    /// The embedding vector
    pub vector: Vec<f32>,
    /// Model used to generate embedding
    pub model: String,
    /// Vector dimension
    pub dimension: usize,
    /// Generation timestamp
    pub generated_at: u64,
}

impl EmbeddingSlot {
    /// Create a new embedding slot
    pub fn new(name: impl Into<String>, vector: Vec<f32>, model: impl Into<String>) -> Self {
        let dimension = vector.len();
        Self {
            name: name.into(),
            vector,
            model: model.into(),
            dimension,
            generated_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        }
    }
}

/// A unified entity that can represent any storage type
#[derive(Debug, Clone)]
pub struct UnifiedEntity {
    /// Unique entity identifier
    pub id: EntityId,
    /// What kind of entity this is
    pub kind: EntityKind,
    /// Creation timestamp
    pub created_at: u64,
    /// Last update timestamp
    pub updated_at: u64,
    /// The actual data content
    pub data: EntityData,
    /// Sequence ID for ordering/versioning
    pub sequence_id: u64,
    /// Optional auxiliary data (embeddings, cross-refs).
    /// None for most table rows — saves 40 bytes/entity.
    aux: Option<Box<EntityAux>>,
}

/// Auxiliary entity data — only allocated when needed.
#[derive(Debug, Clone, Default)]
pub struct EntityAux {
    /// Embedding slots (for multi-vector support)
    pub embeddings: Vec<EmbeddingSlot>,
    /// Cross-references to other entities
    pub cross_refs: Vec<CrossRef>,
}

impl UnifiedEntity {
    /// Access embeddings (returns empty slice if no aux data).
    pub fn embeddings(&self) -> &[EmbeddingSlot] {
        self.aux
            .as_ref()
            .map(|a| a.embeddings.as_slice())
            .unwrap_or(&[])
    }

    /// Access cross-references (returns empty slice if no aux data).
    pub fn cross_refs(&self) -> &[CrossRef] {
        self.aux
            .as_ref()
            .map(|a| a.cross_refs.as_slice())
            .unwrap_or(&[])
    }

    /// Get mutable embeddings (allocates aux if needed).
    pub fn embeddings_mut(&mut self) -> &mut Vec<EmbeddingSlot> {
        &mut self.aux.get_or_insert_with(Default::default).embeddings
    }

    /// Get mutable cross-refs (allocates aux if needed).
    pub fn cross_refs_mut(&mut self) -> &mut Vec<CrossRef> {
        &mut self.aux.get_or_insert_with(Default::default).cross_refs
    }

    /// Check if entity has any auxiliary data.
    pub fn has_aux(&self) -> bool {
        self.aux.is_some()
    }
}

impl UnifiedEntity {
    /// Create a new unified entity
    pub fn new(id: EntityId, kind: EntityKind, data: EntityData) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Self {
            id,
            kind,
            created_at: now,
            updated_at: now,
            data,
            sequence_id: 0,
            aux: None,
        }
    }

    /// Create a table row entity
    pub fn table_row(
        id: EntityId,
        table: impl Into<Arc<str>>,
        row_id: u64,
        columns: Vec<Value>,
    ) -> Self {
        Self::new(
            id,
            EntityKind::TableRow {
                table: table.into(),
                row_id,
            },
            EntityData::Row(RowData::new(columns)),
        )
    }

    /// Create a graph node entity
    pub fn graph_node(
        id: EntityId,
        label: impl Into<String>,
        node_type: impl Into<String>,
        properties: HashMap<String, Value>,
    ) -> Self {
        Self::new(
            id,
            EntityKind::GraphNode {
                label: label.into(),
                node_type: node_type.into(),
            },
            EntityData::Node(NodeData::with_properties(properties)),
        )
    }

    /// Create a graph edge entity
    pub fn graph_edge(
        id: EntityId,
        label: impl Into<String>,
        from: impl Into<String>,
        to: impl Into<String>,
        weight: f32,
        properties: HashMap<String, Value>,
    ) -> Self {
        Self::new(
            id,
            EntityKind::GraphEdge {
                label: label.into(),
                from_node: from.into(),
                to_node: to.into(),
                weight: (weight * 1000.0) as u32,
            },
            EntityData::Edge(EdgeData::with_properties(weight, properties)),
        )
    }

    /// Create a vector entity
    pub fn vector(id: EntityId, collection: impl Into<String>, vector: Vec<f32>) -> Self {
        Self::new(
            id,
            EntityKind::Vector {
                collection: collection.into(),
            },
            EntityData::Vector(VectorData::new(vector)),
        )
    }

    /// Add an embedding to this entity
    pub fn add_embedding(&mut self, slot: EmbeddingSlot) {
        self.embeddings_mut().push(slot);
        self.touch();
    }

    /// Add a cross-reference
    pub fn add_cross_ref(&mut self, cross_ref: CrossRef) {
        self.cross_refs_mut().push(cross_ref);
        self.touch();
    }

    /// Get embedding by slot name
    pub fn get_embedding(&self, name: &str) -> Option<&EmbeddingSlot> {
        self.embeddings().iter().find(|e| e.name == name)
    }

    /// Update timestamp
    fn touch(&mut self) {
        self.updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
    }

    /// Check if entity is stale (not updated in given seconds)
    pub fn is_stale(&self, max_age_secs: u64) -> bool {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        now.saturating_sub(self.updated_at) > max_age_secs
    }
}

/// A cross-reference between entities
#[derive(Debug, Clone)]
pub struct CrossRef {
    /// Source entity ID (the entity that holds this reference)
    pub source: EntityId,
    /// Target entity ID
    pub target: EntityId,
    /// Target collection name
    pub target_collection: String,
    /// Type of reference
    pub ref_type: RefType,
    /// Reference weight/strength (0.0-1.0)
    pub weight: f32,
    /// When this reference was created
    pub created_at: u64,
}

impl CrossRef {
    /// Create a new cross-reference
    pub fn new(
        source: EntityId,
        target: EntityId,
        target_collection: impl Into<String>,
        ref_type: RefType,
    ) -> Self {
        Self {
            source,
            target,
            target_collection: target_collection.into(),
            ref_type,
            weight: 1.0,
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        }
    }

    /// Create with weight
    pub fn with_weight(
        source: EntityId,
        target: EntityId,
        target_collection: impl Into<String>,
        ref_type: RefType,
        weight: f32,
    ) -> Self {
        let mut cr = Self::new(source, target, target_collection, ref_type);
        cr.weight = weight;
        cr
    }
}

/// Types of cross-references between entities
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RefType {
    // Table ↔ Graph
    RowToNode, // Table row represents a graph node
    RowToEdge, // Table row represents a graph edge
    NodeToRow, // Node links back to source row

    // Table ↔ Vector
    RowToVector, // Table row has embeddings
    VectorToRow, // Vector search → source row

    // Graph ↔ Vector
    NodeToVector, // Node has embeddings
    EdgeToVector, // Edge has embeddings
    VectorToNode, // Vector search → source node

    // Semantic links (discovered)
    SimilarTo,   // Discovered by vector similarity
    RelatedTo,   // Domain-specific relationship
    DerivesFrom, // Data lineage
    Mentions,    // Text mentions another entity
    Contains,    // Structural containment
    DependsOn,   // Dependency relationship
}

impl RefType {
    /// Get the inverse reference type (for bidirectional tracking)
    pub fn inverse(&self) -> Option<Self> {
        match self {
            Self::RowToNode => Some(Self::NodeToRow),
            Self::NodeToRow => Some(Self::RowToNode),
            Self::RowToVector => Some(Self::VectorToRow),
            Self::VectorToRow => Some(Self::RowToVector),
            Self::NodeToVector => Some(Self::VectorToNode),
            Self::VectorToNode => Some(Self::NodeToVector),
            Self::SimilarTo => Some(Self::SimilarTo), // Symmetric
            Self::RelatedTo => Some(Self::RelatedTo), // Symmetric
            _ => None,                                // One-directional references
        }
    }

    /// Check if this is a symmetric reference type
    pub fn is_symmetric(&self) -> bool {
        matches!(self, Self::SimilarTo | Self::RelatedTo)
    }

    /// Convert RefType to byte for binary serialization
    pub fn to_byte(&self) -> u8 {
        match self {
            Self::RowToNode => 0,
            Self::RowToEdge => 1,
            Self::NodeToRow => 2,
            Self::RowToVector => 3,
            Self::VectorToRow => 4,
            Self::NodeToVector => 5,
            Self::EdgeToVector => 6,
            Self::VectorToNode => 7,
            Self::SimilarTo => 8,
            Self::RelatedTo => 9,
            Self::DerivesFrom => 10,
            Self::Mentions => 11,
            Self::Contains => 12,
            Self::DependsOn => 13,
        }
    }

    /// Create RefType from byte (binary deserialization)
    pub fn from_byte(byte: u8) -> Self {
        match byte {
            0 => Self::RowToNode,
            1 => Self::RowToEdge,
            2 => Self::NodeToRow,
            3 => Self::RowToVector,
            4 => Self::VectorToRow,
            5 => Self::NodeToVector,
            6 => Self::EdgeToVector,
            7 => Self::VectorToNode,
            8 => Self::SimilarTo,
            9 => Self::RelatedTo,
            10 => Self::DerivesFrom,
            11 => Self::Mentions,
            12 => Self::Contains,
            13 => Self::DependsOn,
            _ => Self::RelatedTo, // Default fallback
        }
    }
}

/// Convert Vec<Value> to RowData
impl From<Vec<Value>> for RowData {
    fn from(columns: Vec<Value>) -> Self {
        RowData::new(columns)
    }
}

/// Convert HashMap to NodeData
impl From<HashMap<String, Value>> for NodeData {
    fn from(properties: HashMap<String, Value>) -> Self {
        NodeData::with_properties(properties)
    }
}

/// Convert dense vector to VectorData
impl From<Vec<f32>> for VectorData {
    fn from(dense: Vec<f32>) -> Self {
        VectorData::new(dense)
    }
}

/// Convert tuple (dense, sparse) to VectorData
impl From<(Vec<f32>, SparseVector)> for VectorData {
    fn from((dense, sparse): (Vec<f32>, SparseVector)) -> Self {
        VectorData::with_sparse(dense, sparse)
    }
}

// Helper trait for uniform entity creation
impl UnifiedEntity {
    /// Create a graph node entity from properties map
    pub fn from_properties(
        id: EntityId,
        label: impl Into<String>,
        node_type: impl Into<String>,
        properties: impl IntoIterator<Item = (impl Into<String>, Value)>,
    ) -> Self {
        let props: HashMap<String, Value> =
            properties.into_iter().map(|(k, v)| (k.into(), v)).collect();
        Self::graph_node(id, label, node_type, props)
    }

    /// Convert entity to row data if applicable
    pub fn into_row(self) -> Option<RowData> {
        match self.data {
            EntityData::Row(r) => Some(r),
            _ => None,
        }
    }

    /// Convert entity to node data if applicable
    pub fn into_node(self) -> Option<NodeData> {
        match self.data {
            EntityData::Node(n) => Some(n),
            _ => None,
        }
    }

    /// Convert entity to edge data if applicable
    pub fn into_edge(self) -> Option<EdgeData> {
        match self.data {
            EntityData::Edge(e) => Some(e),
            _ => None,
        }
    }

    /// Convert entity to vector data if applicable
    pub fn into_vector(self) -> Option<VectorData> {
        match self.data {
            EntityData::Vector(v) => Some(v),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_entity_creation() {
        let id = EntityId::new(1);
        let entity = UnifiedEntity::table_row(
            id,
            "users",
            100,
            vec![Value::Text("alice".to_string()), Value::Integer(25)],
        );

        assert!(entity.data.is_row());
        assert_eq!(entity.kind.storage_type(), "table");
        assert_eq!(entity.kind.collection(), "users");
    }

    #[test]
    fn test_cross_refs() {
        let id1 = EntityId::new(1);
        let id2 = EntityId::new(2);

        let cross_ref = CrossRef::new(id1, id2, "nodes", RefType::RowToNode);
        assert_eq!(cross_ref.source, id1);
        assert_eq!(cross_ref.target, id2);
        assert_eq!(cross_ref.ref_type.inverse(), Some(RefType::NodeToRow));
    }

    #[test]
    fn test_sparse_vector() {
        let sparse = SparseVector::new(vec![0, 5, 10], vec![1.0, 2.0, 3.0], 100);

        assert_eq!(sparse.nnz(), 3);
        assert_eq!(sparse.get(5), 2.0);
        assert_eq!(sparse.get(3), 0.0);
        assert!(sparse.sparsity() > 0.9);
    }

    #[test]
    fn test_embedding_slots() {
        let mut entity = UnifiedEntity::table_row(
            EntityId::new(1),
            "documents",
            1,
            vec![Value::Text("Hello world".to_string())],
        );

        entity.add_embedding(EmbeddingSlot::new(
            "content",
            vec![0.1, 0.2, 0.3],
            "text-embedding-3-small",
        ));

        assert_eq!(entity.embeddings().len(), 1);
        assert!(entity.get_embedding("content").is_some());
        assert!(entity.get_embedding("summary").is_none());
    }
}
