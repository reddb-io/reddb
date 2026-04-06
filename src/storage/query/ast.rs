//! Unified Query AST
//!
//! Defines the abstract syntax tree for unified table+graph queries.
//! Supports:
//! - Pure table queries (SELECT ... FROM ...)
//! - Pure graph queries (MATCH (a)-[r]->(b) ...)
//! - Table-graph joins (FROM t JOIN GRAPH ...)
//! - Path queries (PATH FROM ... TO ... VIA ...)
//!
//! # Examples
//!
//! ```text
//! -- Table query
//! SELECT ip, ports FROM hosts WHERE os = 'Linux'
//!
//! -- Graph query
//! MATCH (h:Host)-[:HAS_SERVICE]->(s:Service)
//! WHERE h.ip STARTS WITH '192.168'
//! RETURN h, s
//!
//! -- Join query
//! FROM hosts h
//! JOIN GRAPH (h)-[:HAS_VULN]->(v:Vulnerability) AS g
//! WHERE h.criticality > 7
//! RETURN h.ip, h.hostname, v.cve
//!
//! -- Path query
//! PATH FROM host('192.168.1.1') TO host('10.0.0.1')
//! VIA [:AUTH_ACCESS, :CONNECTS_TO]
//! RETURN path
//! ```

use std::fmt;

pub use crate::storage::engine::distance::DistanceMetric;
use crate::storage::engine::graph_store::{GraphEdgeType, GraphNodeType};
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
    /// Order by clauses
    pub order_by: Vec<OrderByClause>,
    /// Limit
    pub limit: Option<u64>,
    /// Offset
    pub offset: Option<u64>,
}

impl TableQuery {
    /// Create a new table query
    pub fn new(table: &str) -> Self {
        Self {
            table: table.to_string(),
            alias: None,
            columns: Vec::new(),
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
        }
    }
}

// ============================================================================
// Graph Query
// ============================================================================

/// Graph query: MATCH pattern WHERE filter RETURN projection
#[derive(Debug, Clone)]
pub struct GraphQuery {
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
            pattern,
            filter: None,
            return_: Vec::new(),
        }
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
}

impl JoinQuery {
    /// Create a new join query
    pub fn new(left: QueryExpr, right: QueryExpr, on: JoinCondition) -> Self {
        Self {
            left: Box::new(left),
            right: Box::new(right),
            join_type: JoinType::Inner,
            on,
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
            from,
            to,
            via: Vec::new(),
            max_length: 10,
            filter: None,
            return_: Vec::new(),
        }
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
// Builders (Fluent API)
// ============================================================================

/// Builder for table queries
pub struct TableQueryBuilder {
    query: TableQuery,
}

impl TableQueryBuilder {
    /// Create a new builder
    pub fn new(table: &str) -> Self {
        Self {
            query: TableQuery::new(table),
        }
    }

    /// Set alias
    pub fn alias(mut self, alias: &str) -> Self {
        self.query.alias = Some(alias.to_string());
        self
    }

    /// Add column to select
    pub fn select(mut self, column: &str) -> Self {
        self.query
            .columns
            .push(Projection::from_field(FieldRef::column(
                self.query.alias.as_deref().unwrap_or(&self.query.table),
                column,
            )));
        self
    }

    /// Add all columns
    pub fn select_all(mut self) -> Self {
        self.query.columns.clear();
        self
    }

    /// Add filter
    pub fn filter(mut self, f: Filter) -> Self {
        self.query.filter = Some(match self.query.filter.take() {
            Some(existing) => existing.and(f),
            None => f,
        });
        self
    }

    /// Add order by
    pub fn order_by(mut self, clause: OrderByClause) -> Self {
        self.query.order_by.push(clause);
        self
    }

    /// Set limit
    pub fn limit(mut self, n: u64) -> Self {
        self.query.limit = Some(n);
        self
    }

    /// Set offset
    pub fn offset(mut self, n: u64) -> Self {
        self.query.offset = Some(n);
        self
    }

    /// Join with a graph pattern
    pub fn join_graph(self, pattern: GraphPattern, on: JoinCondition) -> JoinQueryBuilder {
        JoinQueryBuilder {
            left: QueryExpr::Table(self.query),
            pattern,
            on,
            join_type: JoinType::Inner,
        }
    }

    /// Build the query expression
    pub fn build(self) -> QueryExpr {
        QueryExpr::Table(self.query)
    }
}

/// Builder for graph queries
pub struct GraphQueryBuilder {
    query: GraphQuery,
}

impl GraphQueryBuilder {
    /// Create a new builder
    pub fn new() -> Self {
        Self {
            query: GraphQuery::new(GraphPattern::new()),
        }
    }

    /// Add node pattern
    pub fn node(mut self, pattern: NodePattern) -> Self {
        self.query.pattern.nodes.push(pattern);
        self
    }

    /// Add edge pattern
    pub fn edge(mut self, pattern: EdgePattern) -> Self {
        self.query.pattern.edges.push(pattern);
        self
    }

    /// Add filter
    pub fn filter(mut self, f: Filter) -> Self {
        self.query.filter = Some(match self.query.filter.take() {
            Some(existing) => existing.and(f),
            None => f,
        });
        self
    }

    /// Add return projection
    pub fn return_field(mut self, field: FieldRef) -> Self {
        self.query.return_.push(Projection::from_field(field));
        self
    }

    /// Build the query expression
    pub fn build(self) -> QueryExpr {
        QueryExpr::Graph(self.query)
    }
}

impl Default for GraphQueryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for join queries
pub struct JoinQueryBuilder {
    left: QueryExpr,
    pattern: GraphPattern,
    on: JoinCondition,
    join_type: JoinType,
}

impl JoinQueryBuilder {
    /// Set join type
    pub fn join_type(mut self, jt: JoinType) -> Self {
        self.join_type = jt;
        self
    }

    /// Build the query expression
    pub fn build(self) -> QueryExpr {
        let right = QueryExpr::Graph(GraphQuery::new(self.pattern));
        QueryExpr::Join(JoinQuery {
            left: Box::new(self.left),
            right: Box::new(right),
            join_type: self.join_type,
            on: self.on,
        })
    }
}

/// Builder for path queries
pub struct PathQueryBuilder {
    query: PathQuery,
}

impl PathQueryBuilder {
    /// Create a new builder
    pub fn new(from: NodeSelector, to: NodeSelector) -> Self {
        Self {
            query: PathQuery::new(from, to),
        }
    }

    /// Add edge type to traverse
    pub fn via(mut self, edge_type: GraphEdgeType) -> Self {
        self.query.via.push(edge_type);
        self
    }

    /// Set max length
    pub fn max_length(mut self, n: u32) -> Self {
        self.query.max_length = n;
        self
    }

    /// Add filter
    pub fn filter(mut self, f: Filter) -> Self {
        self.query.filter = Some(f);
        self
    }

    /// Build the query expression
    pub fn build(self) -> QueryExpr {
        QueryExpr::Path(self.query)
    }
}

// ============================================================================
// Common Table Expressions (CTEs)
// ============================================================================

/// A Common Table Expression (CTE) definition
///
/// CTEs provide named subqueries that can be referenced multiple times
/// within the main query. Recursive CTEs enable hierarchical queries.
///
/// # Examples
///
/// ```text
/// -- Non-recursive CTE
/// WITH active_hosts AS (
///     SELECT * FROM hosts WHERE last_seen > now() - interval '1 hour'
/// )
/// SELECT * FROM active_hosts WHERE criticality > 5
///
/// -- Recursive CTE for attack paths
/// WITH RECURSIVE attack_path AS (
///     -- Base case: starting host
///     SELECT id, ip, 0 as depth FROM hosts WHERE ip = '192.168.1.1'
///     UNION ALL
///     -- Recursive case: follow connections
///     SELECT h.id, h.ip, ap.depth + 1
///     FROM attack_path ap
///     JOIN connections c ON c.source_id = ap.id
///     JOIN hosts h ON h.id = c.target_id
///     WHERE ap.depth < 10
/// )
/// SELECT * FROM attack_path
/// ```
#[derive(Debug, Clone)]
pub struct CteDefinition {
    /// Name of the CTE (used to reference it in the main query)
    pub name: String,
    /// Optional column aliases for the CTE result
    pub columns: Vec<String>,
    /// The query that defines this CTE
    pub query: Box<QueryExpr>,
    /// Whether this is a recursive CTE
    pub recursive: bool,
}

impl CteDefinition {
    /// Create a new non-recursive CTE
    pub fn new(name: &str, query: QueryExpr) -> Self {
        Self {
            name: name.to_string(),
            columns: Vec::new(),
            query: Box::new(query),
            recursive: false,
        }
    }

    /// Create a recursive CTE
    pub fn recursive(name: &str, query: QueryExpr) -> Self {
        Self {
            name: name.to_string(),
            columns: Vec::new(),
            query: Box::new(query),
            recursive: true,
        }
    }

    /// Add column aliases
    pub fn with_columns(mut self, columns: Vec<String>) -> Self {
        self.columns = columns;
        self
    }
}

/// WITH clause containing one or more CTEs
#[derive(Debug, Clone, Default)]
pub struct WithClause {
    /// List of CTE definitions
    pub ctes: Vec<CteDefinition>,
    /// Whether any CTE in the clause is recursive
    pub has_recursive: bool,
}

impl WithClause {
    /// Create a new WITH clause
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a CTE definition
    pub fn add(mut self, cte: CteDefinition) -> Self {
        if cte.recursive {
            self.has_recursive = true;
        }
        self.ctes.push(cte);
        self
    }

    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.ctes.is_empty()
    }

    /// Get a CTE by name
    pub fn get(&self, name: &str) -> Option<&CteDefinition> {
        self.ctes.iter().find(|c| c.name == name)
    }
}

/// Query with optional WITH clause
#[derive(Debug, Clone)]
pub struct QueryWithCte {
    /// Optional WITH clause
    pub with_clause: Option<WithClause>,
    /// The main query
    pub query: QueryExpr,
}

impl QueryWithCte {
    /// Create a query without CTEs
    pub fn simple(query: QueryExpr) -> Self {
        Self {
            with_clause: None,
            query,
        }
    }

    /// Create a query with CTEs
    pub fn with_ctes(with_clause: WithClause, query: QueryExpr) -> Self {
        Self {
            with_clause: Some(with_clause),
            query,
        }
    }
}

/// Builder for constructing queries with CTEs
pub struct CteQueryBuilder {
    with_clause: WithClause,
}

impl CteQueryBuilder {
    /// Start building a WITH clause
    pub fn new() -> Self {
        Self {
            with_clause: WithClause::new(),
        }
    }

    /// Add a non-recursive CTE
    pub fn cte(mut self, name: &str, query: QueryExpr) -> Self {
        self.with_clause = self.with_clause.add(CteDefinition::new(name, query));
        self
    }

    /// Add a recursive CTE
    pub fn recursive_cte(mut self, name: &str, query: QueryExpr) -> Self {
        self.with_clause = self.with_clause.add(CteDefinition::recursive(name, query));
        self
    }

    /// Add a CTE with column aliases
    pub fn cte_with_columns(mut self, name: &str, columns: Vec<String>, query: QueryExpr) -> Self {
        let cte = CteDefinition::new(name, query).with_columns(columns);
        self.with_clause = self.with_clause.add(cte);
        self
    }

    /// Build the query with the main query expression
    pub fn build(self, main_query: QueryExpr) -> QueryWithCte {
        QueryWithCte::with_ctes(self.with_clause, main_query)
    }
}

impl Default for CteQueryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_table_query_builder() {
        let query = QueryExpr::table("hosts")
            .alias("h")
            .select("ip")
            .select("hostname")
            .filter(Filter::compare(
                FieldRef::column("h", "os"),
                CompareOp::Eq,
                Value::Text("Linux".to_string()),
            ))
            .limit(100)
            .build();

        if let QueryExpr::Table(tq) = query {
            assert_eq!(tq.table, "hosts");
            assert_eq!(tq.alias, Some("h".to_string()));
            assert_eq!(tq.columns.len(), 2);
            assert_eq!(tq.limit, Some(100));
        } else {
            panic!("Expected TableQuery");
        }
    }

    #[test]
    fn test_graph_query_builder() {
        let query = QueryExpr::graph()
            .node(NodePattern::new("h").of_type(GraphNodeType::Host))
            .node(NodePattern::new("s").of_type(GraphNodeType::Service))
            .edge(EdgePattern::new("h", "s").of_type(GraphEdgeType::HasService))
            .return_field(FieldRef::node_id("h"))
            .build();

        if let QueryExpr::Graph(gq) = query {
            assert_eq!(gq.pattern.nodes.len(), 2);
            assert_eq!(gq.pattern.edges.len(), 1);
            assert_eq!(gq.return_.len(), 1);
        } else {
            panic!("Expected GraphQuery");
        }
    }

    #[test]
    fn test_path_query_builder() {
        let query = QueryExpr::path(
            NodeSelector::by_id("host:192.168.1.1"),
            NodeSelector::by_id("host:10.0.0.1"),
        )
        .via(GraphEdgeType::AuthAccess)
        .via(GraphEdgeType::ConnectsTo)
        .max_length(5)
        .build();

        if let QueryExpr::Path(pq) = query {
            assert_eq!(pq.via.len(), 2);
            assert_eq!(pq.max_length, 5);
        } else {
            panic!("Expected PathQuery");
        }
    }

    #[test]
    fn test_join_query_builder() {
        let query = QueryExpr::table("hosts")
            .alias("h")
            .select("ip")
            .join_graph(
                GraphPattern::new()
                    .node(NodePattern::new("n").of_type(GraphNodeType::Host))
                    .edge(EdgePattern::new("n", "v").of_type(GraphEdgeType::AffectedBy)),
                JoinCondition::new(
                    FieldRef::column("h", "ip"),
                    FieldRef::node_prop("n", "label"),
                ),
            )
            .build();

        if let QueryExpr::Join(jq) = query {
            assert!(matches!(*jq.left, QueryExpr::Table(_)));
            assert!(matches!(*jq.right, QueryExpr::Graph(_)));
        } else {
            panic!("Expected JoinQuery");
        }
    }

    #[test]
    fn test_cte_builder() {
        // Build a query with a non-recursive CTE
        let inner_query = QueryExpr::table("hosts")
            .filter(Filter::compare(
                FieldRef::column("", "os"),
                CompareOp::Eq,
                Value::Text("Linux".to_string()),
            ))
            .build();

        let main_query = QueryExpr::table("linux_hosts").select("ip").build();

        let query_with_cte = CteQueryBuilder::new()
            .cte("linux_hosts", inner_query)
            .build(main_query);

        assert!(query_with_cte.with_clause.is_some());
        let with_clause = query_with_cte.with_clause.unwrap();
        assert_eq!(with_clause.ctes.len(), 1);
        assert_eq!(with_clause.ctes[0].name, "linux_hosts");
        assert!(!with_clause.ctes[0].recursive);
        assert!(!with_clause.has_recursive);
    }

    #[test]
    fn test_recursive_cte() {
        // Build a recursive CTE for hierarchical data
        let base_query = QueryExpr::table("hosts")
            .filter(Filter::compare(
                FieldRef::column("", "ip"),
                CompareOp::Eq,
                Value::Text("192.168.1.1".to_string()),
            ))
            .build();

        let main_query = QueryExpr::table("reachable").select("ip").build();

        let query_with_cte = CteQueryBuilder::new()
            .recursive_cte("reachable", base_query)
            .build(main_query);

        assert!(query_with_cte.with_clause.is_some());
        let with_clause = query_with_cte.with_clause.unwrap();
        assert!(with_clause.has_recursive);
        assert!(with_clause.ctes[0].recursive);
    }

    #[test]
    fn test_cte_with_columns() {
        let inner = QueryExpr::table("hosts").build();
        let main = QueryExpr::table("h").build();

        let cte =
            CteDefinition::new("h", inner).with_columns(vec!["id".to_string(), "name".to_string()]);

        assert_eq!(cte.columns.len(), 2);
        assert_eq!(cte.columns[0], "id");
        assert_eq!(cte.columns[1], "name");

        let query = QueryWithCte::with_ctes(WithClause::new().add(cte), main);
        assert!(query.with_clause.is_some());
    }
}
