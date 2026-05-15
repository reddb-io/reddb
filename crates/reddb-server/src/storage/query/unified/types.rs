use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use crate::storage::engine::graph_store::{GraphStore, StoredNode};
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
/// Per Roadmap #1 (issue #156), table-column data uses a
/// schema-shared layout: `schema` is an `Arc<Vec<Arc<str>>>` shared
/// across every record in one result (one alloc per query instead of
/// per row), and `values` is the parallel array — position `i` in
/// `values` corresponds to `schema[i]`. `overflow` materialises only
/// for ragged rows (schemaless inserts, post-creation `set` calls
/// for keys not in the schema, projections that mix in alias-prefixed
/// columns, etc.).
///
/// Keys throughout use `Arc<str>` so interned column names (system
/// fields, shared schema strings) reuse storage across millions of
/// records without per-row heap allocation — each `Arc::clone` is a
/// single atomic increment. `HashMap<Arc<str>, V>` lookups still
/// work with `&str` queries because `Arc<str>: Borrow<str>` and both
/// hash identically.
///
/// The `nodes`, `edges`, `paths`, and `vector_results` fields carry
/// graph-query results and are orthogonal to the table layout.
#[derive(Debug, Clone)]
pub struct UnifiedRecord {
    /// Shared column-name schema. Empty (process-shared) `Arc` for
    /// schemaless records.
    schema: Arc<Vec<Arc<str>>>,
    /// Parallel value array. `values[i]` corresponds to `schema[i]`.
    values: Vec<Value>,
    /// Late-arriving / out-of-schema columns. Lazily allocated.
    overflow: Option<HashMap<Arc<str>, Value>>,
    /// Matched nodes (for graph data)
    pub nodes: HashMap<String, MatchedNode>,
    /// Matched edges (for graph data)
    pub edges: HashMap<String, MatchedEdge>,
    /// Paths (for path queries)
    pub paths: Vec<GraphPath>,
    /// Vector search results
    pub vector_results: Vec<VectorSearchResult>,
}

/// Process-wide empty schema sentinel. Schemaless records share
/// this `Arc` so the `Default` and `new()` paths cost only an
/// atomic refcount bump rather than a fresh `Vec` allocation per
/// record.
fn empty_schema() -> Arc<Vec<Arc<str>>> {
    static EMPTY: std::sync::OnceLock<Arc<Vec<Arc<str>>>> = std::sync::OnceLock::new();
    Arc::clone(EMPTY.get_or_init(|| Arc::new(Vec::new())))
}

impl Default for UnifiedRecord {
    fn default() -> Self {
        Self {
            schema: empty_schema(),
            values: Vec::new(),
            overflow: None,
            nodes: HashMap::new(),
            edges: HashMap::new(),
            paths: Vec::new(),
            vector_results: Vec::new(),
        }
    }
}

/// Schema-shared value layout for scan rows.
///
/// Wire encoders use this view to resolve column-name → index **once**
/// per response, then index into `values[]` per row instead of paying
/// an `rposition` scan on every cell.
#[derive(Debug, Clone, Copy)]
pub struct ColumnarRow<'a> {
    pub schema: &'a Arc<Vec<Arc<str>>>,
    pub values: &'a [Value],
}

/// Interned system-field column name. For a 4500-row `SELECT *` scan
/// these keys used to allocate 13.5k fresh `String`s per query; now
/// the pool of three stays resident and each record pays only an
/// atomic refcount bump.
pub fn sys_key_rid() -> Arc<str> {
    static KEY: std::sync::OnceLock<Arc<str>> = std::sync::OnceLock::new();
    Arc::clone(KEY.get_or_init(|| Arc::from("rid")))
}

pub fn sys_key_red_entity_id() -> Arc<str> {
    static KEY: std::sync::OnceLock<Arc<str>> = std::sync::OnceLock::new();
    Arc::clone(KEY.get_or_init(|| Arc::from("red_entity_id")))
}

pub fn sys_key_collection() -> Arc<str> {
    static KEY: std::sync::OnceLock<Arc<str>> = std::sync::OnceLock::new();
    Arc::clone(KEY.get_or_init(|| Arc::from("collection")))
}

pub fn sys_key_kind() -> Arc<str> {
    static KEY: std::sync::OnceLock<Arc<str>> = std::sync::OnceLock::new();
    Arc::clone(KEY.get_or_init(|| Arc::from("kind")))
}

pub fn sys_key_tenant() -> Arc<str> {
    static KEY: std::sync::OnceLock<Arc<str>> = std::sync::OnceLock::new();
    Arc::clone(KEY.get_or_init(|| Arc::from("tenant")))
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
    /// Create an empty schemaless record. Any `set*` calls will land
    /// in the overflow HashMap.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a schemaless record with pre-allocated capacity for the
    /// overflow HashMap. Use this when you know the writer will push
    /// many fields with no shared schema available.
    pub fn with_capacity(capacity: usize) -> Self {
        let mut rec = Self::default();
        if capacity > 0 {
            rec.overflow = Some(HashMap::with_capacity(capacity));
        }
        rec
    }

    /// Build a record from a shared schema and a parallel value
    /// array. Scan hot paths use this — the schema `Arc` is reused
    /// across millions of records, so each row pays only a refcount
    /// bump on the schema and a `Vec<Value>` allocation.
    ///
    /// The two slices must be the same length; mismatch is treated
    /// the same way the legacy HashMap path treated it (excess
    /// schema entries become absent columns; excess values are
    /// dropped). Callers in scan paths always supply a length-matched
    /// pair.
    pub fn with_schema(schema: Arc<Vec<Arc<str>>>, values: Vec<Value>) -> Self {
        Self {
            schema,
            values,
            overflow: None,
            nodes: HashMap::new(),
            edges: HashMap::new(),
            paths: Vec::new(),
            vector_results: Vec::new(),
        }
    }

    /// Backwards-compatible alias for [`with_schema`]. Older callers
    /// (and the wire-encoding fast path) used `from_columnar`.
    #[inline]
    pub fn from_columnar(schema: Arc<Vec<Arc<str>>>, values: Vec<Value>) -> Self {
        Self::with_schema(schema, values)
    }

    /// Build a schemaless record that materialises fields directly in
    /// the overflow HashMap. Useful for ad-hoc inserts where no schema
    /// is known up front.
    pub fn schemaless() -> Self {
        Self::default()
    }

    /// Schema slice — the column names known up front for this
    /// record. Returns an empty slice for schemaless records.
    #[inline]
    pub fn columns(&self) -> &[Arc<str>] {
        self.schema.as_ref()
    }

    /// Parallel value slice (same length as [`columns`] for
    /// schema-bearing records).
    #[inline]
    pub fn schema_values(&self) -> &[Value] {
        &self.values
    }

    /// Schemaless overflow map, if any.
    #[inline]
    pub fn overflow(&self) -> Option<&HashMap<Arc<str>, Value>> {
        self.overflow.as_ref()
    }

    /// Look up the slot index of `column` in the shared schema.
    /// Falls back to a linear scan if the schema isn't sorted (most
    /// scan-built schemas aren't, since they preserve user-supplied
    /// column order). When the schema has duplicate names the LAST
    /// occurrence wins, matching the legacy `HashMap::insert`
    /// overwrite semantics for late-arriving values.
    #[inline]
    fn schema_index(&self, column: &str) -> Option<usize> {
        self.schema.iter().rposition(|k| &**k == column)
    }

    /// Set a column value. Allocates an `Arc<str>` for the key; hot-path
    /// callers with a pre-interned key should prefer [`set_arc`].
    pub fn set(&mut self, column: &str, value: Value) {
        if let Some(idx) = self.schema_index(column) {
            if idx < self.values.len() {
                self.values[idx] = value;
                return;
            }
        }
        self.overflow_mut().insert(Arc::from(column), value);
    }

    /// Set a column value from an already-owned String key. If the
    /// key is in the shared schema the value lands in the parallel
    /// `Vec<Value>`; otherwise it promotes to the overflow HashMap.
    #[inline]
    pub fn set_owned(&mut self, column: String, value: Value) {
        if let Some(idx) = self.schema_index(&column) {
            if idx < self.values.len() {
                self.values[idx] = value;
                return;
            }
        }
        self.overflow_mut()
            .insert(Arc::from(column.into_boxed_str()), value);
    }

    /// Set a column value using a pre-interned key. Zero allocation
    /// for the key in the schema-hit path — just an atomic refcount
    /// bump on the existing schema entry. Used by the scan lean path
    /// for system fields like `red_entity_id`.
    #[inline]
    pub fn set_arc(&mut self, column: Arc<str>, value: Value) {
        if let Some(idx) = self.schema_index(&column) {
            if idx < self.values.len() {
                self.values[idx] = value;
                return;
            }
        }
        self.overflow_mut().insert(column, value);
    }

    /// Get a column value. Checks the schema-shared layout first
    /// (scan fast-path), then the overflow HashMap so late-arriving
    /// columns still resolve.
    ///
    /// When the schema has duplicate names — e.g. the sys key
    /// `created_at` plus a user column also named `created_at` — the
    /// LAST occurrence wins. This mirrors the pre-refactor
    /// `HashMap::insert` behaviour where a later `set(name, value)`
    /// overwrote an earlier one, so scan output still shows the
    /// user-provided value in preference to the system timestamp.
    pub fn get(&self, column: &str) -> Option<&Value> {
        if let Some(idx) = self.schema_index(column) {
            if let Some(v) = self.values.get(idx) {
                return Some(v);
            }
        }
        self.overflow.as_ref().and_then(|m| m.get(column))
    }

    /// Number of visible fields across both representations.
    pub fn field_count(&self) -> usize {
        self.values.len() + self.overflow.as_ref().map(|m| m.len()).unwrap_or(0)
    }

    /// Whether the record has a field with this column name.
    pub fn contains_column(&self, column: &str) -> bool {
        self.get(column).is_some()
    }

    /// Iterate `(name, value)` pairs across schema-shared + overflow.
    /// Schema rows come first in their schema order; overflow rows
    /// follow in arbitrary order. Consumers that need deterministic
    /// ordering should sort by name.
    pub fn iter_fields(&self) -> Box<dyn Iterator<Item = (&Arc<str>, &Value)> + '_> {
        let schema_iter = self.schema.iter().zip(self.values.iter());
        let overflow_iter = self.overflow.iter().flat_map(|m| m.iter());
        Box::new(schema_iter.chain(overflow_iter))
    }

    /// Mutable iteration over every visible value. Used by the few
    /// in-place rewriters (e.g. secret-payload decryption). The
    /// schema slot is rewritten in-place; overflow values likewise.
    pub fn values_mut(&mut self) -> Box<dyn Iterator<Item = &mut Value> + '_> {
        let schema_iter = self.values.iter_mut();
        let overflow_iter = self.overflow.iter_mut().flat_map(|m| m.values_mut());
        Box::new(schema_iter.chain(overflow_iter))
    }

    /// Collect column names (both representations) in a Vec.
    pub fn column_names(&self) -> Vec<Arc<str>> {
        let mut out: Vec<Arc<str>> = Vec::with_capacity(self.field_count());
        out.extend(self.schema.iter().cloned());
        if let Some(m) = &self.overflow {
            out.extend(m.keys().cloned());
        }
        out
    }

    /// Borrow the columnar view, if the record carries a non-empty
    /// shared schema. Hot-path wire encoders use this to resolve
    /// column-name → index **once** per response, then index into
    /// `values[]` per row instead of paying an `rposition` scan on
    /// every cell. Returns `None` when the record is purely
    /// schemaless (overflow only).
    #[inline]
    pub fn columnar(&self) -> Option<ColumnarRow<'_>> {
        if self.schema.is_empty() {
            None
        } else {
            Some(ColumnarRow {
                schema: &self.schema,
                values: &self.values,
            })
        }
    }

    /// Return the `Arc<Vec<Arc<str>>>` schema pointer (for identity
    /// comparison) when the record carries a schema. Callers that
    /// want to cache a schema-specific column-index map across
    /// records can use `Arc::as_ptr(...)` on this value as the cache
    /// key.
    #[inline]
    pub fn columnar_schema(&self) -> Option<&Arc<Vec<Arc<str>>>> {
        if self.schema.is_empty() {
            None
        } else {
            Some(&self.schema)
        }
    }

    /// Mutable handle to the overflow HashMap, allocating it on
    /// first use. Callers that need to bulk-insert raw HashMap
    /// entries (legacy code that hasn't been migrated) can use this.
    pub fn overflow_mut(&mut self) -> &mut HashMap<Arc<str>, Value> {
        self.overflow.get_or_insert_with(HashMap::new)
    }

    /// Insert into overflow with a borrowed `&str` key. Useful for
    /// migrating legacy `record.values.entry(k).or_insert(v)` calls.
    pub fn overflow_entry_or_insert(&mut self, column: Arc<str>, value: Value) {
        self.overflow_mut().entry(column).or_insert(value);
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
    /// Category label string (e.g. `"host"`, `"order"`).
    pub node_label: String,
    pub properties: HashMap<String, Value>,
}

impl MatchedNode {
    /// Create from a stored node, resolving its `label_id` to the
    /// canonical category string when available.
    pub fn from_stored(node: &StoredNode) -> Self {
        Self {
            id: node.id.clone(),
            label: node.label.clone(),
            node_label: node.node_type.as_str().to_string(),
            properties: HashMap::new(),
        }
    }
}

/// A matched edge from graph query
#[derive(Debug, Clone)]
pub struct MatchedEdge {
    pub from: String,
    pub to: String,
    /// Category label string for the edge.
    pub edge_label: String,
    pub weight: f32,
    pub properties: HashMap<String, Value>,
}

impl MatchedEdge {
    /// Create from `(source, edge_label, target, weight)` with the edge label
    /// already resolved to its canonical string form.
    pub fn from_tuple(
        source: &str,
        edge_label: impl Into<String>,
        target: &str,
        weight: f32,
    ) -> Self {
        Self {
            from: source.to_string(),
            to: target.to_string(),
            edge_label: edge_label.into(),
            weight,
            properties: HashMap::new(),
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

#[cfg(test)]
mod record_layout_tests {
    //! Property tests for the schema-shared `UnifiedRecord` layout
    //! introduced in issue #156. The invariants under test:
    //!
    //!   1. **get/set round-trip.** For any sequence of `set_owned`
    //!      calls, `get(k)` returns the most recently inserted value.
    //!   2. **Overflow promotion.** Keys that aren't in the shared
    //!      schema land in the overflow HashMap; keys that *are* in
    //!      the schema land in the parallel `Vec<Value>` slot.
    //!   3. **Schema-mismatch handling.** A schema-built record
    //!      reads exactly the same `(name, value)` pairs back out as
    //!      a HashMap-built reference of the same data.
    //!   4. **Multi-row scan parity.** A vector of records built with
    //!      a shared schema produces identical projections to a
    //!      vector built via per-row `set_owned` (the legacy
    //!      HashMap-per-row shape).
    //!
    //! Schemaless records are also exercised by setting `proptest`
    //! to drive empty-schema generators alongside the schemaful path.

    use super::*;
    use crate::storage::schema::Value;
    use proptest::collection::vec;
    use proptest::prelude::*;

    fn arb_value() -> impl Strategy<Value = Value> {
        prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Boolean),
            any::<i64>().prop_map(Value::Integer),
            any::<f64>()
                .prop_filter("nan-free", |v| !v.is_nan())
                .prop_map(Value::Float),
            "[a-zA-Z0-9 ]{0,16}".prop_map(Value::text),
        ]
    }

    fn arb_column_name() -> impl Strategy<Value = String> {
        // Bounded ASCII so duplicate-key generation actually
        // collides (broad alphabets just spew unique names).
        "[a-d]{1,3}".prop_map(|s| s.to_string())
    }

    fn arb_schema() -> impl Strategy<Value = Vec<String>> {
        // Up to 5 distinct columns. We dedupe so the schema slot
        // semantics (one position per column) match the HashMap
        // reference.
        vec(arb_column_name(), 0..6).prop_map(|mut v| {
            v.sort();
            v.dedup();
            v
        })
    }

    fn arb_writes() -> impl Strategy<Value = Vec<(String, Value)>> {
        vec((arb_column_name(), arb_value()), 0..16)
    }

    /// Build a `UnifiedRecord` via the schema-shared path: take the
    /// caller's columns, intern them as `Arc<str>`, and insert each
    /// `(k, v)` write through `set_owned`. Schema hits land in the
    /// `Vec<Value>` slot; misses fall to overflow.
    fn record_with_schema(schema: &[String], writes: &[(String, Value)]) -> UnifiedRecord {
        let arc_schema: Arc<Vec<Arc<str>>> =
            Arc::new(schema.iter().map(|s| Arc::from(s.as_str())).collect());
        // Schema-bearing record starts with `Null` placeholders so
        // unset columns are observable as `Some(Null)` rather than
        // `None`. This mirrors how scan paths construct rows.
        let mut rec =
            UnifiedRecord::with_schema(Arc::clone(&arc_schema), vec![Value::Null; schema.len()]);
        for (k, v) in writes {
            rec.set_owned(k.clone(), v.clone());
        }
        rec
    }

    /// Reference: build the equivalent `HashMap<String, Value>` by
    /// applying the same writes (last-write-wins). Then layer in any
    /// schema entries that were never written so absent columns show
    /// up as `Null` — same convention as `record_with_schema`.
    fn hashmap_reference(
        schema: &[String],
        writes: &[(String, Value)],
    ) -> std::collections::HashMap<String, Value> {
        let mut map: std::collections::HashMap<String, Value> =
            schema.iter().map(|c| (c.clone(), Value::Null)).collect();
        for (k, v) in writes {
            map.insert(k.clone(), v.clone());
        }
        map
    }

    proptest! {
        /// `get(k)` returns the last value written under `k`,
        /// regardless of whether `k` is in the schema or only in
        /// overflow.
        #[test]
        fn get_set_round_trip(schema in arb_schema(), writes in arb_writes()) {
            let rec = record_with_schema(&schema, &writes);
            let reference = hashmap_reference(&schema, &writes);
            for (k, expected) in &reference {
                prop_assert_eq!(rec.get(k), Some(expected), "key {}", k);
            }
        }

        /// Keys outside the schema must promote to overflow; keys
        /// inside the schema must land in the parallel vec (so the
        /// overflow map either doesn't contain them or hasn't even
        /// been allocated for the all-schema-hit case).
        #[test]
        fn overflow_promotion_only_for_missing_keys(
            schema in arb_schema(),
            writes in arb_writes(),
        ) {
            let rec = record_with_schema(&schema, &writes);

            // Schema columns: never appear in the overflow map.
            for col in &schema {
                if let Some(over) = rec.overflow() {
                    prop_assert!(
                        !over.contains_key(col.as_str()),
                        "schema column {} leaked into overflow",
                        col
                    );
                }
            }

            // Out-of-schema writes: must appear in the overflow map.
            for (k, _) in &writes {
                if !schema.iter().any(|c| c == k) {
                    let over = rec.overflow().expect("overflow allocated");
                    prop_assert!(
                        over.contains_key(k.as_str()),
                        "out-of-schema key {} missing from overflow",
                        k
                    );
                }
            }
        }

        /// `iter_fields` yields the same `(name, value)` multiset as
        /// the HashMap reference. Order isn't asserted (overflow is
        /// arbitrary), but multiset equality catches drops/dupes.
        #[test]
        fn iter_fields_matches_hashmap(
            schema in arb_schema(),
            writes in arb_writes(),
        ) {
            let rec = record_with_schema(&schema, &writes);
            let reference = hashmap_reference(&schema, &writes);

            let mut got: Vec<(String, Value)> = rec
                .iter_fields()
                .map(|(k, v)| (k.as_ref().to_string(), v.clone()))
                .collect();
            let mut want: Vec<(String, Value)> = reference
                .into_iter()
                .collect();

            got.sort_by(|a, b| a.0.cmp(&b.0));
            want.sort_by(|a, b| a.0.cmp(&b.0));
            prop_assert_eq!(got, want);
        }

        /// A vector of records that share one `Arc<Vec<Arc<str>>>`
        /// schema produces the same projected output as a vector of
        /// schemaless records that received the same writes via the
        /// legacy `set_owned`-on-empty-schema path. This is the
        /// scan-parity invariant.
        #[test]
        fn shared_schema_scan_parity(
            schema in arb_schema().prop_filter("non-empty schema", |s| !s.is_empty()),
            rows in vec(arb_writes(), 0..8),
        ) {
            // Build the shared-schema scan:
            let arc_schema: Arc<Vec<Arc<str>>> =
                Arc::new(schema.iter().map(|s| Arc::from(s.as_str())).collect());
            let shared: Vec<UnifiedRecord> = rows
                .iter()
                .map(|row_writes| {
                    let mut r = UnifiedRecord::with_schema(
                        Arc::clone(&arc_schema),
                        vec![Value::Null; schema.len()],
                    );
                    for (k, v) in row_writes {
                        r.set_owned(k.clone(), v.clone());
                    }
                    r
                })
                .collect();

            // Build the schemaless reference (no shared schema, every
            // write goes straight to overflow):
            let schemaless: Vec<UnifiedRecord> = rows
                .iter()
                .map(|row_writes| {
                    let mut r = UnifiedRecord::schemaless();
                    // Pre-populate the schema columns to Null so the
                    // observable columns match the shared-schema path.
                    for col in &schema {
                        r.set_owned(col.clone(), Value::Null);
                    }
                    for (k, v) in row_writes {
                        r.set_owned(k.clone(), v.clone());
                    }
                    r
                })
                .collect();

            // Project both vectors over the schema columns — this is
            // what wire/listener.rs does in the scan hot path. Same
            // bytes in, same bytes out.
            for (s_row, h_row) in shared.iter().zip(schemaless.iter()) {
                for col in &schema {
                    prop_assert_eq!(
                        s_row.get(col),
                        h_row.get(col),
                        "column {} diverged on shared vs schemaless build",
                        col
                    );
                }
            }

            // The shared-schema path keeps one `Arc` clone per row;
            // sanity-check that we're actually exercising it.
            for s_row in &shared {
                prop_assert!(s_row.columnar().is_some() || schema.is_empty());
            }
        }

        /// A schema with duplicate names resolves the LAST occurrence
        /// — matching legacy `HashMap::insert` overwrite semantics.
        #[test]
        fn duplicate_schema_last_write_wins(
            col in arb_column_name(),
            v1 in arb_value(),
            v2 in arb_value(),
        ) {
            let arc_schema: Arc<Vec<Arc<str>>> = Arc::new(vec![
                Arc::from(col.as_str()),
                Arc::from(col.as_str()),
            ]);
            let rec = UnifiedRecord::with_schema(arc_schema, vec![v1.clone(), v2.clone()]);
            // The last slot wins on read.
            prop_assert_eq!(rec.get(&col), Some(&v2));
        }

        /// Schemaless inserts (no schema known up front) materialise
        /// every write in the overflow HashMap and read back via
        /// `get`. The schema slice stays empty.
        #[test]
        fn schemaless_path_uses_overflow_only(writes in arb_writes()) {
            let mut rec = UnifiedRecord::schemaless();
            for (k, v) in &writes {
                rec.set_owned(k.clone(), v.clone());
            }

            prop_assert!(rec.columns().is_empty());
            prop_assert!(rec.schema_values().is_empty());

            // Last-write-wins multiset: rebuild the reference.
            let mut reference: std::collections::HashMap<String, Value> =
                std::collections::HashMap::new();
            for (k, v) in &writes {
                reference.insert(k.clone(), v.clone());
            }
            for (k, v) in reference {
                prop_assert_eq!(rec.get(&k), Some(&v));
            }
        }
    }
}
