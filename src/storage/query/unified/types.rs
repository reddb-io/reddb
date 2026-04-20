use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use crate::storage::engine::graph_store::{GraphEdgeType, GraphNodeType, GraphStore, StoredNode};
use crate::storage::engine::graph_table_index::GraphTableIndex;
use crate::storage::query::ast::{
    CompareOp, EdgeDirection, EdgePattern, FieldRef, Filter, GraphPattern, GraphQuery, JoinQuery,
    JoinType, NodePattern, NodeSelector, PathQuery, Projection, QueryExpr, TableQuery,
};
use crate::storage::schema::Value;

/// Execution error
#[derive(Debug, Clone)]
pub struct ExecutionError {
    pub message: String,
}

impl ExecutionError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Execution error: {}", self.message)
    }
}

impl std::error::Error for ExecutionError {}

/// Result of a unified query
#[derive(Debug, Clone, Default)]
pub struct UnifiedResult {
    /// Column names for table data
    pub columns: Vec<String>,
    /// Result records
    pub records: Vec<UnifiedRecord>,
    /// Query statistics
    pub stats: QueryStats,
    /// Pre-serialized JSON for fast-path queries (bypasses record-to-JSON conversion)
    pub pre_serialized_json: Option<String>,
}

impl UnifiedResult {
    /// Create an empty result
    pub fn empty() -> Self {
        Self::default()
    }

    /// Create a result with columns
    pub fn with_columns(columns: Vec<String>) -> Self {
        Self {
            columns,
            records: Vec::new(),
            stats: QueryStats::default(),
            pre_serialized_json: None,
        }
    }

    /// Add a record
    pub fn push(&mut self, record: UnifiedRecord) {
        self.records.push(record);
    }

    /// Number of records
    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// Is the result empty?
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }
}

/// A single result record containing mixed data.
///
/// Keys are `Arc<str>` so interned column names (system fields,
/// shared schema strings) can be reused across millions of records
/// without heap allocation — each `Arc::clone` is a single atomic
/// increment. `HashMap<Arc<str>, V>` lookups still work with `&str`
/// queries because `Arc<str>: Borrow<str>` and both hash identically.
#[derive(Debug, Clone, Default)]
pub struct UnifiedRecord {
    /// Column values (for table data). Scan hot paths populate the
    /// columnar side-channel instead — `values` stays empty in that
    /// case, and every accessor (`get`, `iter_fields`, `column_names`,
    /// `contains_key`) transparently merges both sources.
    pub values: HashMap<Arc<str>, Value>,
    /// Matched nodes (for graph data)
    pub nodes: HashMap<String, MatchedNode>,
    /// Matched edges (for graph data)
    pub edges: HashMap<String, MatchedEdge>,
    /// Paths (for path queries)
    pub paths: Vec<GraphPath>,
    /// Vector search results
    pub vector_results: Vec<VectorSearchResult>,
    /// Columnar fast-path for scan-built records. When `Some`, every
    /// HashMap per record (~2.3M drops on a 500-query × 4.5k-row scan)
    /// becomes one contiguous `Vec<Value>` drop plus a refcount bump
    /// on the shared schema. Mutation via `set*` migrates lazily to
    /// the HashMap so the invariant "once mutated, columnar drops
    /// out" is local to the writer.
    columnar: Option<ColumnarRow>,
}

/// Schema-shared value layout for scan rows.
///
/// `schema` is an `Arc<Vec<Arc<str>>>` shared across every record in
/// a single scan result (one alloc per query instead of per row).
/// `values` is the parallel array; position `i` in `values`
/// corresponds to `schema[i]`.
#[derive(Debug, Clone)]
pub struct ColumnarRow {
    pub schema: Arc<Vec<Arc<str>>>,
    pub values: Vec<Value>,
}

/// Interned system-field column name. For a 4500-row `SELECT *` scan
/// these keys used to allocate 13.5k fresh `String`s per query; now
/// the pool of three stays resident and each record pays only an
/// atomic refcount bump.
pub fn sys_key_red_entity_id() -> Arc<str> {
    static KEY: std::sync::OnceLock<Arc<str>> = std::sync::OnceLock::new();
    Arc::clone(KEY.get_or_init(|| Arc::from("red_entity_id")))
}

pub fn sys_key_created_at() -> Arc<str> {
    static KEY: std::sync::OnceLock<Arc<str>> = std::sync::OnceLock::new();
    Arc::clone(KEY.get_or_init(|| Arc::from("created_at")))
}

pub fn sys_key_updated_at() -> Arc<str> {
    static KEY: std::sync::OnceLock<Arc<str>> = std::sync::OnceLock::new();
    Arc::clone(KEY.get_or_init(|| Arc::from("updated_at")))
}

pub fn sys_key_row_id() -> Arc<str> {
    static KEY: std::sync::OnceLock<Arc<str>> = std::sync::OnceLock::new();
    Arc::clone(KEY.get_or_init(|| Arc::from("row_id")))
}

pub fn sys_key_red_collection() -> Arc<str> {
    static KEY: std::sync::OnceLock<Arc<str>> = std::sync::OnceLock::new();
    Arc::clone(KEY.get_or_init(|| Arc::from("red_collection")))
}

pub fn sys_key_red_kind() -> Arc<str> {
    static KEY: std::sync::OnceLock<Arc<str>> = std::sync::OnceLock::new();
    Arc::clone(KEY.get_or_init(|| Arc::from("red_kind")))
}

pub fn sys_key_red_sequence_id() -> Arc<str> {
    static KEY: std::sync::OnceLock<Arc<str>> = std::sync::OnceLock::new();
    Arc::clone(KEY.get_or_init(|| Arc::from("red_sequence_id")))
}

pub fn sys_key_red_entity_type() -> Arc<str> {
    static KEY: std::sync::OnceLock<Arc<str>> = std::sync::OnceLock::new();
    Arc::clone(KEY.get_or_init(|| Arc::from("red_entity_type")))
}

pub fn sys_key_red_capabilities() -> Arc<str> {
    static KEY: std::sync::OnceLock<Arc<str>> = std::sync::OnceLock::new();
    Arc::clone(KEY.get_or_init(|| Arc::from("red_capabilities")))
}

impl UnifiedRecord {
    /// Create an empty record
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a record with pre-allocated capacity for the values HashMap.
    /// Use this when you know approximately how many fields will be inserted
    /// to avoid repeated HashMap resizing.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            values: HashMap::with_capacity(capacity),
            nodes: HashMap::new(),
            edges: HashMap::new(),
            paths: Vec::new(),
            vector_results: Vec::new(),
            columnar: None,
        }
    }

    /// Build a record directly from a shared schema and a parallel
    /// value array. No HashMap allocation — scan hot paths use this
    /// to avoid the per-record bucket alloc + per-field hash cost.
    pub fn from_columnar(schema: Arc<Vec<Arc<str>>>, values: Vec<Value>) -> Self {
        Self {
            values: HashMap::new(),
            nodes: HashMap::new(),
            edges: HashMap::new(),
            paths: Vec::new(),
            vector_results: Vec::new(),
            columnar: Some(ColumnarRow { schema, values }),
        }
    }

    /// If the record is columnar, copy its fields into the `values`
    /// HashMap and drop the columnar representation. Called
    /// automatically by every mutating `set*` so writers see a
    /// single coherent store.
    fn flatten_columnar(&mut self) {
        if let Some(col) = self.columnar.take() {
            if self.values.is_empty() {
                self.values.reserve(col.schema.len());
            }
            for (k, v) in col.schema.iter().zip(col.values.into_iter()) {
                self.values.insert(Arc::clone(k), v);
            }
        }
    }

    /// Set a column value. Allocates an `Arc<str>` for the key; hot-path
    /// callers with a pre-interned key should prefer [`set_arc`].
    pub fn set(&mut self, column: &str, value: Value) {
        self.flatten_columnar();
        self.values.insert(Arc::from(column), value);
    }

    /// Set a column value from an already-owned String key — one less
    /// copy than `set(&column, v)` (promotes the String's buffer to
    /// `Arc<str>` without reallocating the bytes).
    #[inline]
    pub fn set_owned(&mut self, column: String, value: Value) {
        self.flatten_columnar();
        self.values
            .insert(Arc::from(column.into_boxed_str()), value);
    }

    /// Set a column value using a pre-interned key. Zero allocation —
    /// just an atomic refcount bump. Used by the scan lean path for
    /// system fields like `red_entity_id`.
    #[inline]
    pub fn set_arc(&mut self, column: Arc<str>, value: Value) {
        self.flatten_columnar();
        self.values.insert(column, value);
    }

    /// Get a column value. Checks columnar first (scan fast-path),
    /// then the HashMap so mutated records still resolve.
    ///
    /// When the columnar schema has duplicate names — e.g. the sys
    /// key `created_at` plus a user column also named `created_at` —
    /// the LAST occurrence wins. This mirrors the pre-refactor
    /// `HashMap::insert` behaviour where a later `set(name, value)`
    /// overwrote an earlier one, so scan output still shows the
    /// user-provided value in preference to the system timestamp.
    pub fn get(&self, column: &str) -> Option<&Value> {
        if let Some(col) = &self.columnar {
            if let Some(idx) = col.schema.iter().rposition(|k| &**k == column) {
                return col.values.get(idx);
            }
        }
        self.values.get(column)
    }

    /// Number of visible fields across both representations.
    pub fn field_count(&self) -> usize {
        let columnar_len = self.columnar.as_ref().map(|c| c.values.len()).unwrap_or(0);
        columnar_len + self.values.len()
    }

    /// Whether the record has a field with this column name.
    pub fn contains_column(&self, column: &str) -> bool {
        self.get(column).is_some()
    }

    /// Iterate `(name, value)` pairs across columnar + HashMap.
    /// Columnar rows come first in their schema order; HashMap rows
    /// follow in arbitrary order. Consumers that need deterministic
    /// ordering should sort by name.
    pub fn iter_fields(&self) -> Box<dyn Iterator<Item = (&Arc<str>, &Value)> + '_> {
        let col_iter = self
            .columnar
            .as_ref()
            .into_iter()
            .flat_map(|c| c.schema.iter().zip(c.values.iter()));
        let hash_iter = self.values.iter();
        Box::new(col_iter.chain(hash_iter))
    }

    /// Collect column names (both representations) in a Vec.
    pub fn column_names(&self) -> Vec<Arc<str>> {
        let mut out: Vec<Arc<str>> = Vec::with_capacity(self.field_count());
        if let Some(col) = &self.columnar {
            out.extend(col.schema.iter().cloned());
        }
        out.extend(self.values.keys().cloned());
        out
    }

    /// Set a matched node
    pub fn set_node(&mut self, alias: &str, node: MatchedNode) {
        self.nodes.insert(alias.to_string(), node);
    }

    /// Get a matched node
    pub fn get_node(&self, alias: &str) -> Option<&MatchedNode> {
        self.nodes.get(alias)
    }

    /// Set a matched edge
    pub fn set_edge(&mut self, alias: &str, edge: MatchedEdge) {
        self.edges.insert(alias.to_string(), edge);
    }
}

/// A matched node from graph query
#[derive(Debug, Clone)]
pub struct MatchedNode {
    pub id: String,
    pub label: String,
    pub node_type: GraphNodeType,
    pub properties: HashMap<String, Value>,
}

impl MatchedNode {
    /// Create from a stored node
    pub fn from_stored(node: &StoredNode) -> Self {
        Self {
            id: node.id.clone(),
            label: node.label.clone(),
            node_type: node.node_type,
            properties: HashMap::new(),
        }
    }
}

/// A matched edge from graph query
#[derive(Debug, Clone)]
pub struct MatchedEdge {
    pub from: String,
    pub to: String,
    pub edge_type: GraphEdgeType,
    pub weight: f32,
}

impl MatchedEdge {
    /// Create from edge tuple (type, target_id, weight) with source
    pub fn from_tuple(source: &str, edge_type: GraphEdgeType, target: &str, weight: f32) -> Self {
        Self {
            from: source.to_string(),
            to: target.to_string(),
            edge_type,
            weight,
        }
    }
}

/// A vector search result
#[derive(Debug, Clone)]
pub struct VectorSearchResult {
    /// Vector ID
    pub id: u64,
    /// Collection name
    pub collection: String,
    /// Distance to query vector
    pub distance: f32,
    /// The vector data (if requested)
    pub vector: Option<Vec<f32>>,
    /// Metadata (if requested)
    pub metadata: Option<HashMap<String, Value>>,
    /// Linked node ID (if cross-referenced)
    pub linked_node: Option<String>,
    /// Linked table row (table, row_id)
    pub linked_row: Option<(String, u64)>,
}

impl VectorSearchResult {
    /// Create a new vector search result
    pub fn new(id: u64, collection: impl Into<String>, distance: f32) -> Self {
        Self {
            id,
            collection: collection.into(),
            distance,
            vector: None,
            metadata: None,
            linked_node: None,
            linked_row: None,
        }
    }

    /// Include vector data
    pub fn with_vector(mut self, vector: Vec<f32>) -> Self {
        self.vector = Some(vector);
        self
    }

    /// Include metadata
    pub fn with_metadata(mut self, metadata: HashMap<String, Value>) -> Self {
        self.metadata = Some(metadata);
        self
    }

    /// Link to a graph node
    pub fn with_linked_node(mut self, node_id: impl Into<String>) -> Self {
        self.linked_node = Some(node_id.into());
        self
    }

    /// Link to a table row
    pub fn with_linked_row(mut self, table: impl Into<String>, row_id: u64) -> Self {
        self.linked_row = Some((table.into(), row_id));
        self
    }
}

impl Default for VectorSearchResult {
    fn default() -> Self {
        Self::new(0, String::new(), 0.0)
    }
}

/// A path through the graph
#[derive(Debug, Clone)]
pub struct GraphPath {
    /// Sequence of node IDs
    pub nodes: Vec<String>,
    /// Sequence of edges (node_ids.len() - 1)
    pub edges: Vec<MatchedEdge>,
    /// Total path weight
    pub total_weight: f32,
}

impl GraphPath {
    /// Create a new path starting from a node
    pub fn start(node_id: &str) -> Self {
        Self {
            nodes: vec![node_id.to_string()],
            edges: Vec::new(),
            total_weight: 0.0,
        }
    }

    /// Extend the path with an edge and node
    pub fn extend(&self, edge: MatchedEdge, node_id: &str) -> Self {
        let mut new_path = self.clone();
        new_path.total_weight += edge.weight;
        new_path.edges.push(edge);
        new_path.nodes.push(node_id.to_string());
        new_path
    }

    /// Path length (number of edges)
    pub fn len(&self) -> usize {
        self.edges.len()
    }

    /// Is the path empty (no edges)?
    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }
}

/// Query execution statistics
#[derive(Debug, Clone, Default)]
pub struct QueryStats {
    /// Number of nodes scanned
    pub nodes_scanned: u64,
    /// Number of edges scanned
    pub edges_scanned: u64,
    /// Number of rows scanned
    pub rows_scanned: u64,
    /// Execution time in microseconds
    pub exec_time_us: u64,
}
