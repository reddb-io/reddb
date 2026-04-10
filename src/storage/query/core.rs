use std::fmt;

use super::builders::{GraphQueryBuilder, PathQueryBuilder, TableQueryBuilder};
pub use crate::storage::engine::distance::DistanceMetric;
pub use crate::storage::engine::graph_store::{GraphEdgeType, GraphNodeType};
pub use crate::storage::engine::vector_metadata::MetadataFilter;
use crate::storage::schema::Value;

/// Root query expression
#[derive(Debug, Clone)]
pub enum QueryExpr {
    /// Pure table query: SELECT ... FROM ...
    Table(TableQuery),
    /// Pure graph query: MATCH ... RETURN ...
    Graph(GraphQuery),
    /// Join between table and graph
    Join(JoinQuery),
    /// Path query: PATH FROM ... TO ...
    Path(PathQuery),
    /// Vector similarity search
    Vector(VectorQuery),
    /// Hybrid query combining structured and vector search
    Hybrid(HybridQuery),
    /// INSERT INTO table (cols) VALUES (vals)
    Insert(InsertQuery),
    /// UPDATE table SET col=val WHERE filter
    Update(UpdateQuery),
    /// DELETE FROM table WHERE filter
    Delete(DeleteQuery),
    /// CREATE TABLE name (columns)
    CreateTable(CreateTableQuery),
    /// DROP TABLE name
    DropTable(DropTableQuery),
    /// ALTER TABLE name ADD/DROP/RENAME COLUMN
    AlterTable(AlterTableQuery),
    /// GRAPH subcommand (NEIGHBORHOOD, SHORTEST_PATH, etc.)
    GraphCommand(GraphCommand),
    /// SEARCH subcommand (SIMILAR, TEXT, HYBRID)
    SearchCommand(SearchCommand),
    /// ASK 'question' — RAG query with LLM synthesis
    Ask(AskQuery),
    /// CREATE INDEX name ON table (columns) USING type
    CreateIndex(CreateIndexQuery),
    /// DROP INDEX name ON table
    DropIndex(DropIndexQuery),
    /// Probabilistic data structure commands (HLL, SKETCH, FILTER)
    ProbabilisticCommand(ProbabilisticCommand),
    /// CREATE TIMESERIES name [RETENTION duration] [CHUNK_SIZE n]
    CreateTimeSeries(CreateTimeSeriesQuery),
    /// DROP TIMESERIES name
    DropTimeSeries(DropTimeSeriesQuery),
    /// CREATE QUEUE name [MAX_SIZE n] [PRIORITY] [WITH TTL duration]
    CreateQueue(CreateQueueQuery),
    /// DROP QUEUE name
    DropQueue(DropQueueQuery),
    /// QUEUE subcommand (PUSH, POP, PEEK, LEN, PURGE, GROUP, READ, ACK, NACK)
    QueueCommand(QueueCommand),
    /// SET CONFIG key = value
    SetConfig { key: String, value: Value },
    /// SHOW CONFIG [prefix]
    ShowConfig { prefix: Option<String> },
}

/// Probabilistic data structure commands
#[derive(Debug, Clone)]
pub enum ProbabilisticCommand {
    // HyperLogLog
    CreateHll {
        name: String,
        if_not_exists: bool,
    },
    HllAdd {
        name: String,
        elements: Vec<String>,
    },
    HllCount {
        names: Vec<String>,
    },
    HllMerge {
        dest: String,
        sources: Vec<String>,
    },
    HllInfo {
        name: String,
    },
    DropHll {
        name: String,
        if_exists: bool,
    },

    // Count-Min Sketch (Fase 7)
    CreateSketch {
        name: String,
        width: usize,
        depth: usize,
        if_not_exists: bool,
    },
    SketchAdd {
        name: String,
        element: String,
        count: u64,
    },
    SketchCount {
        name: String,
        element: String,
    },
    SketchMerge {
        dest: String,
        sources: Vec<String>,
    },
    SketchInfo {
        name: String,
    },
    DropSketch {
        name: String,
        if_exists: bool,
    },

    // Cuckoo Filter (Fase 8)
    CreateFilter {
        name: String,
        capacity: usize,
        if_not_exists: bool,
    },
    FilterAdd {
        name: String,
        element: String,
    },
    FilterCheck {
        name: String,
        element: String,
    },
    FilterDelete {
        name: String,
        element: String,
    },
    FilterCount {
        name: String,
    },
    FilterInfo {
        name: String,
    },
    DropFilter {
        name: String,
        if_exists: bool,
    },
}

/// Index type for CREATE INDEX ... USING <type>
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexMethod {
    BTree,
    Hash,
    Bitmap,
    RTree,
}

impl fmt::Display for IndexMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BTree => write!(f, "BTREE"),
            Self::Hash => write!(f, "HASH"),
            Self::Bitmap => write!(f, "BITMAP"),
            Self::RTree => write!(f, "RTREE"),
        }
    }
}

/// CREATE INDEX [UNIQUE] [IF NOT EXISTS] name ON table (col1, col2, ...) [USING method]
#[derive(Debug, Clone)]
pub struct CreateIndexQuery {
    pub name: String,
    pub table: String,
    pub columns: Vec<String>,
    pub method: IndexMethod,
    pub unique: bool,
    pub if_not_exists: bool,
}

/// DROP INDEX [IF EXISTS] name ON table
#[derive(Debug, Clone)]
pub struct DropIndexQuery {
    pub name: String,
    pub table: String,
    pub if_exists: bool,
}

/// ASK 'question' [USING provider] [MODEL 'model'] [DEPTH n] [LIMIT n] [COLLECTION col]
#[derive(Debug, Clone)]
pub struct AskQuery {
    pub question: String,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub depth: Option<usize>,
    pub limit: Option<usize>,
    pub collection: Option<String>,
}

impl QueryExpr {
    /// Create a table query
    pub fn table(name: &str) -> TableQueryBuilder {
        TableQueryBuilder::new(name)
    }

    /// Create a graph query
    pub fn graph() -> GraphQueryBuilder {
        GraphQueryBuilder::new()
    }

    /// Create a path query
    pub fn path(from: NodeSelector, to: NodeSelector) -> PathQueryBuilder {
        PathQueryBuilder::new(from, to)
    }
}

// ============================================================================
// Table Query
// ============================================================================

/// Table query: SELECT columns FROM table WHERE filter ORDER BY ... LIMIT ...
#[derive(Debug, Clone)]
pub struct TableQuery {
    /// Table name
    pub table: String,
    /// Optional table alias
    pub alias: Option<String>,
    /// Columns to select (empty = all)
    pub columns: Vec<Projection>,
    /// Filter condition
    pub filter: Option<Filter>,
    /// GROUP BY fields
    pub group_by: Vec<String>,
    /// HAVING filter (applied after grouping)
    pub having: Option<Filter>,
    /// Order by clauses
    pub order_by: Vec<OrderByClause>,
    /// Limit
    pub limit: Option<u64>,
    /// Offset
    pub offset: Option<u64>,
    /// WITH EXPAND options (graph traversal, cross-ref following)
    pub expand: Option<ExpandOptions>,
}

/// Options for WITH EXPAND clause on SELECT queries.
#[derive(Debug, Clone, Default)]
pub struct ExpandOptions {
    /// Expand via graph edges (WITH EXPAND GRAPH)
    pub graph: bool,
    /// Graph expansion depth (DEPTH n)
    pub graph_depth: usize,
    /// Expand via cross-references (WITH EXPAND CROSS_REFS)
    pub cross_refs: bool,
    /// Index hint from the optimizer (which index to prefer for this query)
    pub index_hint: Option<crate::storage::query::planner::optimizer::IndexHint>,
}

impl TableQuery {
    /// Create a new table query
    pub fn new(table: &str) -> Self {
        Self {
            table: table.to_string(),
            alias: None,
            columns: Vec::new(),
            filter: None,
            group_by: Vec::new(),
            having: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            expand: None,
        }
    }
}

// ============================================================================
// Graph Query
// ============================================================================

/// Graph query: MATCH pattern WHERE filter RETURN projection
#[derive(Debug, Clone)]
pub struct GraphQuery {
    /// Optional outer alias when used as a join source
    pub alias: Option<String>,
    /// Graph pattern to match
    pub pattern: GraphPattern,
    /// Filter condition
    pub filter: Option<Filter>,
    /// Return projections
    pub return_: Vec<Projection>,
}

impl GraphQuery {
    /// Create a new graph query
    pub fn new(pattern: GraphPattern) -> Self {
        Self {
            alias: None,
            pattern,
            filter: None,
            return_: Vec::new(),
        }
    }

    /// Set outer alias
    pub fn alias(mut self, alias: &str) -> Self {
        self.alias = Some(alias.to_string());
        self
    }
}

/// Graph pattern: collection of node and edge patterns
#[derive(Debug, Clone, Default)]
pub struct GraphPattern {
    /// Node patterns
    pub nodes: Vec<NodePattern>,
    /// Edge patterns connecting nodes
    pub edges: Vec<EdgePattern>,
}

impl GraphPattern {
    /// Create an empty pattern
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a node pattern
    pub fn node(mut self, pattern: NodePattern) -> Self {
        self.nodes.push(pattern);
        self
    }

    /// Add an edge pattern
    pub fn edge(mut self, pattern: EdgePattern) -> Self {
        self.edges.push(pattern);
        self
    }
}

/// Node pattern: (alias:Type {properties})
#[derive(Debug, Clone)]
pub struct NodePattern {
    /// Variable alias for this node
    pub alias: String,
    /// Optional node type filter
    pub node_type: Option<GraphNodeType>,
    /// Property filters
    pub properties: Vec<PropertyFilter>,
}

impl NodePattern {
    /// Create a new node pattern
    pub fn new(alias: &str) -> Self {
        Self {
            alias: alias.to_string(),
            node_type: None,
            properties: Vec::new(),
        }
    }

    /// Set node type
    pub fn of_type(mut self, node_type: GraphNodeType) -> Self {
        self.node_type = Some(node_type);
        self
    }

    /// Add property filter
    pub fn with_property(mut self, name: &str, op: CompareOp, value: Value) -> Self {
        self.properties.push(PropertyFilter {
            name: name.to_string(),
            op,
            value,
        });
        self
    }
}

/// Edge pattern: -[alias:Type*min..max]->
#[derive(Debug, Clone)]
pub struct EdgePattern {
    /// Optional alias for this edge
    pub alias: Option<String>,
    /// Source node alias
    pub from: String,
    /// Target node alias
    pub to: String,
    /// Optional edge type filter
    pub edge_type: Option<GraphEdgeType>,
    /// Edge direction
    pub direction: EdgeDirection,
    /// Minimum hops (for variable-length patterns)
    pub min_hops: u32,
    /// Maximum hops (for variable-length patterns)
    pub max_hops: u32,
}

impl EdgePattern {
    /// Create a new edge pattern
    pub fn new(from: &str, to: &str) -> Self {
        Self {
            alias: None,
            from: from.to_string(),
            to: to.to_string(),
            edge_type: None,
            direction: EdgeDirection::Outgoing,
            min_hops: 1,
            max_hops: 1,
        }
    }

    /// Set edge type
    pub fn of_type(mut self, edge_type: GraphEdgeType) -> Self {
        self.edge_type = Some(edge_type);
        self
    }

    /// Set direction
    pub fn direction(mut self, dir: EdgeDirection) -> Self {
        self.direction = dir;
        self
    }

    /// Set hop range for variable-length patterns
    pub fn hops(mut self, min: u32, max: u32) -> Self {
        self.min_hops = min;
        self.max_hops = max;
        self
    }

    /// Set alias
    pub fn alias(mut self, alias: &str) -> Self {
        self.alias = Some(alias.to_string());
        self
    }
}

/// Edge direction
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeDirection {
    /// Outgoing: (a)-[r]->(b)
    Outgoing,
    /// Incoming: (a)<-[r]-(b)
    Incoming,
    /// Both: (a)-[r]-(b)
    Both,
}

/// Property filter: name op value
#[derive(Debug, Clone)]
pub struct PropertyFilter {
    pub name: String,
    pub op: CompareOp,
    pub value: Value,
}

// ============================================================================
// Join Query
// ============================================================================

/// Join query: combines table and graph queries
#[derive(Debug, Clone)]
pub struct JoinQuery {
    /// Left side (typically table)
    pub left: Box<QueryExpr>,
    /// Right side (typically graph)
    pub right: Box<QueryExpr>,
    /// Join type
    pub join_type: JoinType,
    /// Join condition
    pub on: JoinCondition,
    /// Post-join filter condition
    pub filter: Option<Filter>,
    /// Post-join ordering
    pub order_by: Vec<OrderByClause>,
    /// Post-join limit
    pub limit: Option<u64>,
    /// Post-join offset
    pub offset: Option<u64>,
    /// Post-join projection
    pub return_: Vec<Projection>,
}

impl JoinQuery {
    /// Create a new join query
    pub fn new(left: QueryExpr, right: QueryExpr, on: JoinCondition) -> Self {
        Self {
            left: Box::new(left),
            right: Box::new(right),
            join_type: JoinType::Inner,
            on,
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            return_: Vec::new(),
        }
    }

    /// Set join type
    pub fn join_type(mut self, jt: JoinType) -> Self {
        self.join_type = jt;
        self
    }
}

/// Join type
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinType {
    /// Inner join
    Inner,
    /// Left outer join
    LeftOuter,
    /// Right outer join
    RightOuter,
}

/// Join condition: how to match rows with nodes
#[derive(Debug, Clone)]
pub struct JoinCondition {
    /// Left field (table side)
    pub left_field: FieldRef,
    /// Right field (graph side)
    pub right_field: FieldRef,
}

impl JoinCondition {
    /// Create a new join condition
    pub fn new(left: FieldRef, right: FieldRef) -> Self {
        Self {
            left_field: left,
            right_field: right,
        }
    }
}

/// Reference to a field (table column, node property, or edge property)
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FieldRef {
    /// Table column: table.column
    TableColumn { table: String, column: String },
    /// Node property: alias.property
    NodeProperty { alias: String, property: String },
    /// Edge property: alias.property
    EdgeProperty { alias: String, property: String },
    /// Node ID: alias.id
    NodeId { alias: String },
}

impl FieldRef {
    /// Create a table column reference
    pub fn column(table: &str, column: &str) -> Self {
        Self::TableColumn {
            table: table.to_string(),
            column: column.to_string(),
        }
    }

    /// Create a node property reference
    pub fn node_prop(alias: &str, property: &str) -> Self {
        Self::NodeProperty {
            alias: alias.to_string(),
            property: property.to_string(),
        }
    }

    /// Create a node ID reference
    pub fn node_id(alias: &str) -> Self {
        Self::NodeId {
            alias: alias.to_string(),
        }
    }

    /// Create an edge property reference
    pub fn edge_prop(alias: &str, property: &str) -> Self {
        Self::EdgeProperty {
            alias: alias.to_string(),
            property: property.to_string(),
        }
    }
}

// ============================================================================
// Path Query
// ============================================================================

/// Path query: find paths between nodes
#[derive(Debug, Clone)]
pub struct PathQuery {
    /// Optional outer alias when used as a join source
    pub alias: Option<String>,
    /// Source node selector
    pub from: NodeSelector,
    /// Target node selector
    pub to: NodeSelector,
    /// Edge types to traverse (empty = any)
    pub via: Vec<GraphEdgeType>,
    /// Maximum path length
    pub max_length: u32,
    /// Filter on paths
    pub filter: Option<Filter>,
    /// Return projections
    pub return_: Vec<Projection>,
}

impl PathQuery {
    /// Create a new path query
    pub fn new(from: NodeSelector, to: NodeSelector) -> Self {
        Self {
            alias: None,
            from,
            to,
            via: Vec::new(),
            max_length: 10,
            filter: None,
            return_: Vec::new(),
        }
    }

    /// Set outer alias
    pub fn alias(mut self, alias: &str) -> Self {
        self.alias = Some(alias.to_string());
        self
    }

    /// Add an edge type constraint to traverse
    pub fn via(mut self, edge_type: GraphEdgeType) -> Self {
        self.via.push(edge_type);
        self
    }
}

/// Node selector for path queries
#[derive(Debug, Clone)]
pub enum NodeSelector {
    /// By node ID
    ById(String),
    /// By node type and property
    ByType {
        node_type: GraphNodeType,
        filter: Option<PropertyFilter>,
    },
    /// By table row (linked node)
    ByRow { table: String, row_id: u64 },
}

impl NodeSelector {
    /// Select by node ID
    pub fn by_id(id: &str) -> Self {
        Self::ById(id.to_string())
    }

    /// Select by type
    pub fn by_type(node_type: GraphNodeType) -> Self {
        Self::ByType {
            node_type,
            filter: None,
        }
    }

    /// Select by table row
    pub fn by_row(table: &str, row_id: u64) -> Self {
        Self::ByRow {
            table: table.to_string(),
            row_id,
        }
    }
}

// ============================================================================
// Vector Query
// ============================================================================

/// Vector similarity search query
///
/// ```text
/// VECTOR SEARCH embeddings
/// SIMILAR TO [0.1, 0.2, ..., 0.5]
/// WHERE metadata.source = 'nmap'
/// LIMIT 10
/// ```
#[derive(Debug, Clone)]
pub struct VectorQuery {
    /// Optional outer alias when used as a join source
    pub alias: Option<String>,
    /// Collection name to search
    pub collection: String,
    /// Query vector (or reference to get vector from)
    pub query_vector: VectorSource,
    /// Number of results to return
    pub k: usize,
    /// Metadata filter
    pub filter: Option<MetadataFilter>,
    /// Distance metric to use (defaults to collection's metric)
    pub metric: Option<DistanceMetric>,
    /// Include vectors in results
    pub include_vectors: bool,
    /// Include metadata in results
    pub include_metadata: bool,
    /// Minimum similarity threshold (optional)
    pub threshold: Option<f32>,
}

impl VectorQuery {
    /// Create a new vector query
    pub fn new(collection: &str, query: VectorSource) -> Self {
        Self {
            alias: None,
            collection: collection.to_string(),
            query_vector: query,
            k: 10,
            filter: None,
            metric: None,
            include_vectors: false,
            include_metadata: true,
            threshold: None,
        }
    }

    /// Set the number of results
    pub fn limit(mut self, k: usize) -> Self {
        self.k = k;
        self
    }

    /// Set metadata filter
    pub fn with_filter(mut self, filter: MetadataFilter) -> Self {
        self.filter = Some(filter);
        self
    }

    /// Include vectors in results
    pub fn with_vectors(mut self) -> Self {
        self.include_vectors = true;
        self
    }

    /// Set similarity threshold
    pub fn min_similarity(mut self, threshold: f32) -> Self {
        self.threshold = Some(threshold);
        self
    }

    /// Set outer alias
    pub fn alias(mut self, alias: &str) -> Self {
        self.alias = Some(alias.to_string());
        self
    }
}

/// Source of query vector
#[derive(Debug, Clone)]
pub enum VectorSource {
    /// Literal vector values
    Literal(Vec<f32>),
    /// Text to embed (requires embedding function)
    Text(String),
    /// Reference to another vector by ID
    Reference { collection: String, vector_id: u64 },
    /// From a subquery result
    Subquery(Box<QueryExpr>),
}

impl VectorSource {
    /// Create from literal vector
    pub fn literal(values: Vec<f32>) -> Self {
        Self::Literal(values)
    }

    /// Create from text (to be embedded)
    pub fn text(s: &str) -> Self {
        Self::Text(s.to_string())
    }

    /// Reference another vector
    pub fn reference(collection: &str, vector_id: u64) -> Self {
        Self::Reference {
            collection: collection.to_string(),
            vector_id,
        }
    }
}

// ============================================================================
// Hybrid Query
// ============================================================================

/// Hybrid query combining structured (table/graph) and vector search
///
/// ```text
/// FROM hosts h
/// JOIN VECTOR embeddings e ON h.id = e.metadata.host_id
/// SIMILAR TO 'ssh vulnerability'
/// WHERE h.os = 'Linux'
/// RETURN h.*, e.distance
/// ```
#[derive(Debug, Clone)]
pub struct HybridQuery {
    /// Optional outer alias when used as a join source
    pub alias: Option<String>,
    /// Structured query part (table/graph)
    pub structured: Box<QueryExpr>,
    /// Vector search part
    pub vector: VectorQuery,
    /// How to combine results
    pub fusion: FusionStrategy,
    /// Final result limit
    pub limit: Option<usize>,
}

impl HybridQuery {
    /// Create a new hybrid query
    pub fn new(structured: QueryExpr, vector: VectorQuery) -> Self {
        Self {
            alias: None,
            structured: Box::new(structured),
            vector,
            fusion: FusionStrategy::Rerank { weight: 0.5 },
            limit: None,
        }
    }

    /// Set fusion strategy
    pub fn with_fusion(mut self, fusion: FusionStrategy) -> Self {
        self.fusion = fusion;
        self
    }

    /// Set result limit
    pub fn limit(mut self, limit: usize) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Set outer alias
    pub fn alias(mut self, alias: &str) -> Self {
        self.alias = Some(alias.to_string());
        self
    }
}

/// Strategy for combining structured and vector search results
#[derive(Debug, Clone)]
pub enum FusionStrategy {
    /// Vector similarity re-ranks structured results
    /// weight: 0.0 = pure structured, 1.0 = pure vector
    Rerank { weight: f32 },
    /// Filter with structured query, then search vectors among filtered
    FilterThenSearch,
    /// Search vectors first, then filter with structured query
    SearchThenFilter,
    /// Reciprocal Rank Fusion
    /// k: RRF constant (typically 60)
    RRF { k: u32 },
    /// Intersection: only return results that match both
    Intersection,
    /// Union: return results from either (with combined scores)
    Union {
        structured_weight: f32,
        vector_weight: f32,
    },
}

impl Default for FusionStrategy {
    fn default() -> Self {
        Self::Rerank { weight: 0.5 }
    }
}

// ============================================================================
// DML/DDL Query Types
// ============================================================================

/// Entity type qualifier for INSERT statements
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InsertEntityType {
    /// Default: plain row
    #[default]
    Row,
    /// INSERT INTO t NODE (...)
    Node,
    /// INSERT INTO t EDGE (...)
    Edge,
    /// INSERT INTO t VECTOR (...)
    Vector,
    /// INSERT INTO t DOCUMENT (...)
    Document,
    /// INSERT INTO t KV (...)
    Kv,
}

/// INSERT INTO table (columns) VALUES (row1), (row2), ... [WITH TTL duration] [WITH METADATA (k=v)]
#[derive(Debug, Clone)]
pub struct InsertQuery {
    /// Target table name
    pub table: String,
    /// Entity type qualifier
    pub entity_type: InsertEntityType,
    /// Column names
    pub columns: Vec<String>,
    /// Rows of values (each inner Vec is one row)
    pub values: Vec<Vec<Value>>,
    /// Whether to return inserted rows
    pub returning: bool,
    /// Optional TTL in milliseconds (from WITH TTL clause)
    pub ttl_ms: Option<u64>,
    /// Optional absolute expiration (from WITH EXPIRES AT clause)
    pub expires_at_ms: Option<u64>,
    /// Optional metadata key-value pairs (from WITH METADATA clause)
    pub with_metadata: Vec<(String, Value)>,
    /// Auto-embed fields on insert (from WITH AUTO EMBED clause)
    pub auto_embed: Option<AutoEmbedConfig>,
}

/// Configuration for automatic embedding generation on INSERT.
#[derive(Debug, Clone)]
pub struct AutoEmbedConfig {
    /// Fields to extract text from for embedding
    pub fields: Vec<String>,
    /// AI provider (e.g. "openai")
    pub provider: String,
    /// Optional model override
    pub model: Option<String>,
}

/// UPDATE table SET col=val, ... WHERE filter [WITH TTL duration] [WITH METADATA (...)]
#[derive(Debug, Clone)]
pub struct UpdateQuery {
    /// Target table name
    pub table: String,
    /// Column-value assignments
    pub assignments: Vec<(String, Value)>,
    /// Optional WHERE filter
    pub filter: Option<Filter>,
    /// Optional TTL in milliseconds (from WITH TTL clause)
    pub ttl_ms: Option<u64>,
    /// Optional absolute expiration (from WITH EXPIRES AT clause)
    pub expires_at_ms: Option<u64>,
    /// Optional metadata key-value pairs (from WITH METADATA clause)
    pub with_metadata: Vec<(String, Value)>,
}

/// DELETE FROM table WHERE filter
#[derive(Debug, Clone)]
pub struct DeleteQuery {
    /// Target table name
    pub table: String,
    /// Optional WHERE filter
    pub filter: Option<Filter>,
}

/// CREATE TABLE name (columns)
#[derive(Debug, Clone)]
pub struct CreateTableQuery {
    /// Table name
    pub name: String,
    /// Column definitions
    pub columns: Vec<CreateColumnDef>,
    /// IF NOT EXISTS flag
    pub if_not_exists: bool,
    /// Optional default TTL applied to newly inserted items in this collection.
    pub default_ttl_ms: Option<u64>,
    /// Fields to prioritize in the context index (WITH CONTEXT INDEX ON (f1, f2))
    pub context_index_fields: Vec<String>,
}

/// Column definition for CREATE TABLE
#[derive(Debug, Clone)]
pub struct CreateColumnDef {
    /// Column name
    pub name: String,
    /// Data type (e.g. TEXT, INTEGER, EMAIL, ENUM(...), ARRAY(TEXT), DECIMAL(2))
    pub data_type: String,
    /// NOT NULL constraint
    pub not_null: bool,
    /// DEFAULT value expression
    pub default: Option<String>,
    /// Compression level (COMPRESS:N)
    pub compress: Option<u8>,
    /// UNIQUE constraint
    pub unique: bool,
    /// PRIMARY KEY constraint
    pub primary_key: bool,
    /// Enum variant names (for ENUM type)
    pub enum_variants: Vec<String>,
    /// Array element type (for ARRAY type)
    pub array_element: Option<String>,
    /// Decimal precision (for DECIMAL type)
    pub decimal_precision: Option<u8>,
}

/// DROP TABLE name
#[derive(Debug, Clone)]
pub struct DropTableQuery {
    /// Table name
    pub name: String,
    /// IF EXISTS flag
    pub if_exists: bool,
}

/// ALTER TABLE name operations
#[derive(Debug, Clone)]
pub struct AlterTableQuery {
    /// Table name
    pub name: String,
    /// Alter operations
    pub operations: Vec<AlterOperation>,
}

/// Single ALTER TABLE operation
#[derive(Debug, Clone)]
pub enum AlterOperation {
    /// ADD COLUMN definition
    AddColumn(CreateColumnDef),
    /// DROP COLUMN name
    DropColumn(String),
    /// RENAME COLUMN from TO to
    RenameColumn { from: String, to: String },
}

// ============================================================================
// Shared Types
// ============================================================================

/// Column/field projection
#[derive(Debug, Clone, PartialEq)]
pub enum Projection {
    /// Select all columns (*)
    All,
    /// Single column by name
    Column(String),
    /// Column with alias
    Alias(String, String),
    /// Function call (name, args)
    Function(String, Vec<Projection>),
    /// Expression with optional alias
    Expression(Box<Filter>, Option<String>),
    /// Field reference (for graph properties)
    Field(FieldRef, Option<String>),
}

impl Projection {
    /// Create a projection from a field reference
    pub fn from_field(field: FieldRef) -> Self {
        Projection::Field(field, None)
    }

    /// Create a column projection
    pub fn column(name: &str) -> Self {
        Projection::Column(name.to_string())
    }

    /// Create an aliased projection
    pub fn with_alias(column: &str, alias: &str) -> Self {
        Projection::Alias(column.to_string(), alias.to_string())
    }
}

/// Filter condition
#[derive(Debug, Clone, PartialEq)]
pub enum Filter {
    /// Comparison: field op value
    Compare {
        field: FieldRef,
        op: CompareOp,
        value: Value,
    },
    /// Logical AND
    And(Box<Filter>, Box<Filter>),
    /// Logical OR
    Or(Box<Filter>, Box<Filter>),
    /// Logical NOT
    Not(Box<Filter>),
    /// IS NULL
    IsNull(FieldRef),
    /// IS NOT NULL
    IsNotNull(FieldRef),
    /// IN (value1, value2, ...)
    In { field: FieldRef, values: Vec<Value> },
    /// BETWEEN low AND high
    Between {
        field: FieldRef,
        low: Value,
        high: Value,
    },
    /// LIKE pattern
    Like { field: FieldRef, pattern: String },
    /// STARTS WITH prefix
    StartsWith { field: FieldRef, prefix: String },
    /// ENDS WITH suffix
    EndsWith { field: FieldRef, suffix: String },
    /// CONTAINS substring
    Contains { field: FieldRef, substring: String },
}

impl Filter {
    /// Create a comparison filter
    pub fn compare(field: FieldRef, op: CompareOp, value: Value) -> Self {
        Self::Compare { field, op, value }
    }

    /// Combine with AND
    pub fn and(self, other: Filter) -> Self {
        Self::And(Box::new(self), Box::new(other))
    }

    /// Combine with OR
    pub fn or(self, other: Filter) -> Self {
        Self::Or(Box::new(self), Box::new(other))
    }

    /// Negate
    pub fn not(self) -> Self {
        Self::Not(Box::new(self))
    }
}

/// Comparison operator
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareOp {
    /// Equal (=)
    Eq,
    /// Not equal (<> or !=)
    Ne,
    /// Less than (<)
    Lt,
    /// Less than or equal (<=)
    Le,
    /// Greater than (>)
    Gt,
    /// Greater than or equal (>=)
    Ge,
}

impl fmt::Display for CompareOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CompareOp::Eq => write!(f, "="),
            CompareOp::Ne => write!(f, "<>"),
            CompareOp::Lt => write!(f, "<"),
            CompareOp::Le => write!(f, "<="),
            CompareOp::Gt => write!(f, ">"),
            CompareOp::Ge => write!(f, ">="),
        }
    }
}

/// Order by clause
#[derive(Debug, Clone)]
pub struct OrderByClause {
    /// Field to order by
    pub field: FieldRef,
    /// Ascending or descending
    pub ascending: bool,
    /// Nulls first or last
    pub nulls_first: bool,
}

impl OrderByClause {
    /// Create ascending order
    pub fn asc(field: FieldRef) -> Self {
        Self {
            field,
            ascending: true,
            nulls_first: false,
        }
    }

    /// Create descending order
    pub fn desc(field: FieldRef) -> Self {
        Self {
            field,
            ascending: false,
            nulls_first: true,
        }
    }
}

// ============================================================================
// Graph Commands
// ============================================================================

/// Graph analytics command issued via SQL-like syntax
#[derive(Debug, Clone)]
pub enum GraphCommand {
    /// GRAPH NEIGHBORHOOD 'source' [DEPTH n] [DIRECTION dir]
    Neighborhood {
        source: String,
        depth: u32,
        direction: String,
    },
    /// GRAPH SHORTEST_PATH 'source' TO 'target' [ALGORITHM alg] [DIRECTION dir]
    ShortestPath {
        source: String,
        target: String,
        algorithm: String,
        direction: String,
    },
    /// GRAPH TRAVERSE 'source' [STRATEGY bfs|dfs] [DEPTH n] [DIRECTION dir]
    Traverse {
        source: String,
        strategy: String,
        depth: u32,
        direction: String,
    },
    /// GRAPH CENTRALITY [ALGORITHM alg]
    Centrality { algorithm: String },
    /// GRAPH COMMUNITY [ALGORITHM alg] [MAX_ITERATIONS n]
    Community {
        algorithm: String,
        max_iterations: u32,
    },
    /// GRAPH COMPONENTS [MODE connected|weak|strong]
    Components { mode: String },
    /// GRAPH CYCLES [MAX_LENGTH n]
    Cycles { max_length: u32 },
    /// GRAPH CLUSTERING
    Clustering,
    /// GRAPH TOPOLOGICAL_SORT
    TopologicalSort,
}

// ============================================================================
// Search Commands
// ============================================================================

/// Search command issued via SQL-like syntax
#[derive(Debug, Clone)]
pub enum SearchCommand {
    /// SEARCH SIMILAR [v1, v2, ...] | TEXT 'query' [COLLECTION col] [LIMIT n] [MIN_SCORE f] [USING provider]
    Similar {
        vector: Vec<f32>,
        text: Option<String>,
        provider: Option<String>,
        collection: String,
        limit: usize,
        min_score: f32,
    },
    /// SEARCH TEXT 'query' [COLLECTION col] [LIMIT n] [FUZZY]
    Text {
        query: String,
        collection: Option<String>,
        limit: usize,
        fuzzy: bool,
    },
    /// SEARCH HYBRID [vector] [TEXT 'query'] COLLECTION col [LIMIT n]
    Hybrid {
        vector: Option<Vec<f32>>,
        query: Option<String>,
        collection: String,
        limit: usize,
    },
    /// SEARCH MULTIMODAL 'key_or_query' [COLLECTION col] [LIMIT n]
    Multimodal {
        query: String,
        collection: Option<String>,
        limit: usize,
    },
    /// SEARCH INDEX index VALUE 'value' [COLLECTION col] [LIMIT n] [EXACT]
    Index {
        index: String,
        value: String,
        collection: Option<String>,
        limit: usize,
        exact: bool,
    },
    /// SEARCH CONTEXT 'query' [FIELD field] [COLLECTION col] [LIMIT n] [DEPTH n]
    Context {
        query: String,
        field: Option<String>,
        collection: Option<String>,
        limit: usize,
        depth: usize,
    },
    /// SEARCH SPATIAL RADIUS lat lon radius_km COLLECTION col COLUMN col [LIMIT n]
    SpatialRadius {
        center_lat: f64,
        center_lon: f64,
        radius_km: f64,
        collection: String,
        column: String,
        limit: usize,
    },
    /// SEARCH SPATIAL BBOX min_lat min_lon max_lat max_lon COLLECTION col COLUMN col [LIMIT n]
    SpatialBbox {
        min_lat: f64,
        min_lon: f64,
        max_lat: f64,
        max_lon: f64,
        collection: String,
        column: String,
        limit: usize,
    },
    /// SEARCH SPATIAL NEAREST lat lon K n COLLECTION col COLUMN col
    SpatialNearest {
        lat: f64,
        lon: f64,
        k: usize,
        collection: String,
        column: String,
    },
}

// ============================================================================
// Time-Series DDL
// ============================================================================

/// CREATE TIMESERIES name [RETENTION duration] [CHUNK_SIZE n]
#[derive(Debug, Clone)]
pub struct CreateTimeSeriesQuery {
    pub name: String,
    pub retention_ms: Option<u64>,
    pub chunk_size: Option<usize>,
    pub if_not_exists: bool,
}

/// DROP TIMESERIES [IF EXISTS] name
#[derive(Debug, Clone)]
pub struct DropTimeSeriesQuery {
    pub name: String,
    pub if_exists: bool,
}

// ============================================================================
// Queue DDL & Commands
// ============================================================================

/// CREATE QUEUE name [MAX_SIZE n] [PRIORITY] [WITH TTL duration]
#[derive(Debug, Clone)]
pub struct CreateQueueQuery {
    pub name: String,
    pub priority: bool,
    pub max_size: Option<usize>,
    pub ttl_ms: Option<u64>,
    pub if_not_exists: bool,
}

/// DROP QUEUE [IF EXISTS] name
#[derive(Debug, Clone)]
pub struct DropQueueQuery {
    pub name: String,
    pub if_exists: bool,
}

/// Which end of the queue
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueSide {
    Left,
    Right,
}

/// Queue operation commands
#[derive(Debug, Clone)]
pub enum QueueCommand {
    Push {
        queue: String,
        value: String,
        side: QueueSide,
        priority: Option<i32>,
    },
    Pop {
        queue: String,
        side: QueueSide,
        count: usize,
    },
    Peek {
        queue: String,
        count: usize,
    },
    Len {
        queue: String,
    },
    Purge {
        queue: String,
    },
    GroupCreate {
        queue: String,
        group: String,
    },
    GroupRead {
        queue: String,
        group: String,
        consumer: String,
        count: usize,
    },
    Ack {
        queue: String,
        group: String,
        message_id: String,
    },
    Nack {
        queue: String,
        group: String,
        message_id: String,
    },
}

// ============================================================================
// Builders (Fluent API)
// ============================================================================
