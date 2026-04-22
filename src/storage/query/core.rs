use std::fmt;

use super::builders::{GraphQueryBuilder, PathQueryBuilder, TableQueryBuilder};
pub use crate::storage::engine::distance::DistanceMetric;
pub use crate::storage::engine::graph_store::{GraphEdgeType, GraphNodeType};
pub use crate::storage::engine::vector_metadata::MetadataFilter;
use crate::storage::schema::{SqlTypeName, Value};

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
    /// CREATE TREE name IN collection ROOT ... MAX_CHILDREN n
    CreateTree(CreateTreeQuery),
    /// DROP TREE name IN collection
    DropTree(DropTreeQuery),
    /// TREE subcommand (INSERT, MOVE, DELETE, VALIDATE, REBALANCE)
    TreeCommand(TreeCommand),
    /// SET CONFIG key = value
    SetConfig { key: String, value: Value },
    /// SHOW CONFIG [prefix]
    ShowConfig { prefix: Option<String> },
    /// `SET TENANT 'id'` / `SET TENANT = 'id'` / `RESET TENANT`
    ///
    /// Session-scoped multi-tenancy handle. Populates a per-connection
    /// thread-local that `CURRENT_TENANT()` reads and that RLS
    /// policies combine with via `USING (tenant_id = CURRENT_TENANT())`.
    /// `None` clears the current tenant (RESET TENANT or SET TENANT
    /// NULL). Unlike `SetConfig` this is *not* persisted to red_config —
    /// it lives for the connection's lifetime only.
    SetTenant(Option<String>),
    /// `SHOW TENANT` — returns the thread-local tenant id (or NULL).
    ShowTenant,
    /// EXPLAIN ALTER FOR CREATE TABLE name (...) [FORMAT JSON]
    ///
    /// Pure read command that diffs the embedded `CREATE TABLE`
    /// statement against the live `CollectionContract` of the
    /// table with the same name and returns the `ALTER TABLE`
    /// operations that would close the gap. Never executes
    /// anything — output is text (default) or JSON depending on
    /// the optional `FORMAT JSON` suffix. Powers the Purple
    /// framework's migration generator and any other client that
    /// wants reddb to own the schema-diff rules.
    ExplainAlter(ExplainAlterQuery),
    /// Transaction control: BEGIN, COMMIT, ROLLBACK, SAVEPOINT, RELEASE, ROLLBACK TO.
    ///
    /// Phase 1.1 (PG parity): parser + dispatch are wired so clients (psql, JDBC, etc.)
    /// can issue these statements without errors. Real isolation/atomicity semantics
    /// arrive with Phase 2.3 MVCC. Until then statements behave as autocommit (each
    /// DML is its own transaction); BEGIN/COMMIT/ROLLBACK return success but do NOT
    /// provide rollback-on-failure guarantees across multiple statements.
    TransactionControl(TxnControl),
    /// Maintenance commands: VACUUM [FULL] [table], ANALYZE [table].
    ///
    /// Phase 1.2 (PG parity): `VACUUM` triggers segment/page flush + planner stats
    /// refresh. `ANALYZE` refreshes planner statistics (histograms, null counts,
    /// distinct estimates). Both accept an optional table target; omitting the
    /// target iterates every collection.
    MaintenanceCommand(MaintenanceCommand),
    /// `CREATE SCHEMA [IF NOT EXISTS] name`
    ///
    /// Phase 1.3 (PG parity): schemas are logical namespaces stored in
    /// `red_config` under the key `schema.{name}`. Tables created inside a
    /// schema use `schema.table` qualified names (collection name = "schema.table").
    CreateSchema(CreateSchemaQuery),
    /// `DROP SCHEMA [IF EXISTS] name [CASCADE]`
    DropSchema(DropSchemaQuery),
    /// `CREATE SEQUENCE [IF NOT EXISTS] name [START [WITH] n] [INCREMENT [BY] n]`
    ///
    /// Phase 1.3 (PG parity): sequences are 64-bit monotonic counters persisted
    /// in `red_config` under the key `sequence.{name}`. Values are produced by
    /// the scalar functions `nextval('name')` and `currval('name')`.
    CreateSequence(CreateSequenceQuery),
    /// `DROP SEQUENCE [IF EXISTS] name`
    DropSequence(DropSequenceQuery),
    /// `COPY table FROM 'path' [WITH ...]` — CSV import (Phase 1.5 PG parity).
    ///
    /// Supported options: `DELIMITER c`, `HEADER [true|false]`. Rows stream
    /// into the named collection via the `CsvImporter`.
    CopyFrom(CopyFromQuery),
    /// `CREATE [OR REPLACE] [MATERIALIZED] VIEW [IF NOT EXISTS] name AS SELECT ...`
    ///
    /// Phase 2.1 (PG parity): views are stored as `view.{name}` entries in
    /// `red_config`. Materialized views additionally allocate a slot in the
    /// shared `MaterializedViewCache`; `REFRESH MATERIALIZED VIEW` re-runs
    /// the underlying query and repopulates the cache.
    CreateView(CreateViewQuery),
    /// `DROP [MATERIALIZED] VIEW [IF EXISTS] name`
    DropView(DropViewQuery),
    /// `REFRESH MATERIALIZED VIEW name`
    ///
    /// Re-executes the view's query and writes the result into the cache.
    RefreshMaterializedView(RefreshMaterializedViewQuery),
    /// `CREATE POLICY name ON table [FOR action] [TO role] USING (filter)`
    ///
    /// Phase 2.5 (PG parity): row-level security policy definition.
    /// Evaluated at read time — when the table has RLS enabled, all
    /// matching policies for the current role are combined with OR and
    /// AND-ed into the query's WHERE clause.
    CreatePolicy(CreatePolicyQuery),
    /// `DROP POLICY [IF EXISTS] name ON table`
    DropPolicy(DropPolicyQuery),
    /// `CREATE SERVER name FOREIGN DATA WRAPPER kind OPTIONS (...)`
    /// (Phase 3.2 PG parity). Registers a named foreign-data-wrapper
    /// instance in the runtime's `ForeignTableRegistry`.
    CreateServer(CreateServerQuery),
    /// `DROP SERVER [IF EXISTS] name [CASCADE]`
    DropServer(DropServerQuery),
    /// `CREATE FOREIGN TABLE name (cols) SERVER srv OPTIONS (...)`
    /// (Phase 3.2 PG parity). Makes `name` resolvable as a foreign table
    /// via the parent server's `ForeignDataWrapper`.
    CreateForeignTable(CreateForeignTableQuery),
    /// `DROP FOREIGN TABLE [IF EXISTS] name`
    DropForeignTable(DropForeignTableQuery),
}

#[derive(Debug, Clone)]
pub struct CreateServerQuery {
    pub name: String,
    /// Wrapper kind declared in `FOREIGN DATA WRAPPER <kind>`.
    pub wrapper: String,
    /// Generic `(key 'value', ...)` option bag.
    pub options: Vec<(String, String)>,
    pub if_not_exists: bool,
}

#[derive(Debug, Clone)]
pub struct DropServerQuery {
    pub name: String,
    pub if_exists: bool,
    pub cascade: bool,
}

#[derive(Debug, Clone)]
pub struct CreateForeignTableQuery {
    pub name: String,
    pub server: String,
    pub columns: Vec<ForeignColumnDef>,
    pub options: Vec<(String, String)>,
    pub if_not_exists: bool,
}

#[derive(Debug, Clone)]
pub struct ForeignColumnDef {
    pub name: String,
    pub data_type: String,
    pub not_null: bool,
}

#[derive(Debug, Clone)]
pub struct DropForeignTableQuery {
    pub name: String,
    pub if_exists: bool,
}

/// Row-level security policy definition.
#[derive(Debug, Clone)]
pub struct CreatePolicyQuery {
    pub name: String,
    pub table: String,
    /// Which action this policy gates. `None` = `ALL` (applies to all four).
    pub action: Option<PolicyAction>,
    /// Role the policy applies to. `None` = all roles.
    pub role: Option<String>,
    /// Boolean predicate the row must satisfy.
    pub using: Box<Filter>,
    /// Entity kind this policy targets (Phase 2.5.5 RLS universal).
    /// `CREATE POLICY p ON t ...` defaults to `Table`; writing
    /// `ON NODES OF g` / `ON VECTORS OF v` / `ON MESSAGES OF q` /
    /// `ON POINTS OF ts` / `ON EDGES OF g` targets the matching
    /// non-tabular kind. The evaluator filters polices by kind so
    /// a graph policy only gates graph reads, vector policy only
    /// gates vector reads, etc.
    pub target_kind: PolicyTargetKind,
}

/// Which flavour of entity a policy governs (Phase 2.5.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PolicyTargetKind {
    Table,
    Nodes,
    Edges,
    Vectors,
    Messages,
    Points,
    Documents,
}

impl PolicyTargetKind {
    /// Lowercase identifier for UX — used in messages and the
    /// `red_config.rls.policies.*` persistence key.
    pub fn as_ident(&self) -> &'static str {
        match self {
            Self::Table => "table",
            Self::Nodes => "nodes",
            Self::Edges => "edges",
            Self::Vectors => "vectors",
            Self::Messages => "messages",
            Self::Points => "points",
            Self::Documents => "documents",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyAction {
    Select,
    Insert,
    Update,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropPolicyQuery {
    pub name: String,
    pub table: String,
    pub if_exists: bool,
}

#[derive(Debug, Clone)]
pub struct CreateViewQuery {
    pub name: String,
    /// Parsed `SELECT ...` body. Stored as a boxed `QueryExpr` so the
    /// runtime can substitute the tree directly when a query references
    /// this view (no re-parsing per read).
    pub query: Box<QueryExpr>,
    pub materialized: bool,
    pub if_not_exists: bool,
    /// `CREATE OR REPLACE VIEW` — overwrites any existing definition.
    pub or_replace: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropViewQuery {
    pub name: String,
    pub materialized: bool,
    pub if_exists: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefreshMaterializedViewQuery {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyFromQuery {
    pub table: String,
    pub path: String,
    pub format: CopyFormat,
    pub delimiter: Option<char>,
    pub has_header: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyFormat {
    Csv,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateSchemaQuery {
    pub name: String,
    pub if_not_exists: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropSchemaQuery {
    pub name: String,
    pub if_exists: bool,
    pub cascade: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateSequenceQuery {
    pub name: String,
    pub if_not_exists: bool,
    /// First value produced by `nextval`. Default 1.
    pub start: i64,
    /// Added to the current value on each `nextval`. Default 1.
    pub increment: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropSequenceQuery {
    pub name: String,
    pub if_exists: bool,
}

/// Transaction-control statement variants. See [`QueryExpr::TransactionControl`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TxnControl {
    /// `BEGIN [WORK | TRANSACTION]`, `START TRANSACTION`
    Begin,
    /// `COMMIT [WORK | TRANSACTION]`, `END`
    Commit,
    /// `ROLLBACK [WORK | TRANSACTION]`
    Rollback,
    /// `SAVEPOINT name`
    Savepoint(String),
    /// `RELEASE [SAVEPOINT] name`
    ReleaseSavepoint(String),
    /// `ROLLBACK TO [SAVEPOINT] name`
    RollbackToSavepoint(String),
}

/// Maintenance command variants. See [`QueryExpr::MaintenanceCommand`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MaintenanceCommand {
    /// `VACUUM [FULL] [table]`
    ///
    /// Triggers segment compaction and planner stats refresh. `FULL` additionally
    /// forces a full pager sync. Target `None` applies to every collection.
    Vacuum { target: Option<String>, full: bool },
    /// `ANALYZE [table]`
    ///
    /// Refreshes planner statistics (histogram, distinct estimates, null counts).
    /// Target `None` re-analyzes every collection.
    Analyze { target: Option<String> },
}

/// AST node for `EXPLAIN ALTER FOR <CreateTableStmt> [FORMAT JSON]`.
///
/// `target` carries the CREATE TABLE structure exactly as the
/// parser produces it for a regular CREATE — full reuse of
/// `parse_create_table_body`. `format` determines whether the
/// executor emits a `ALTER TABLE …;`-flavored text payload
/// (the default — copy-paste friendly into the REPL) or a
/// structured JSON object (machine-friendly).
#[derive(Debug, Clone)]
pub struct ExplainAlterQuery {
    pub target: CreateTableQuery,
    pub format: ExplainFormat,
}

/// Output format requested for an `EXPLAIN ALTER` command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplainFormat {
    /// Plain SQL text — `ALTER TABLE …;` lines plus header
    /// comments and rename hints. Default; copy-paste friendly.
    Sql,
    /// Structured JSON object with `operations`,
    /// `rename_candidates`, `summary`. Machine-friendly for
    /// driver code (Purple migration generator, dashboards,
    /// CLI tools).
    Json,
}

impl Default for ExplainFormat {
    fn default() -> Self {
        Self::Sql
    }
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
    /// Table name. Legacy slot — still populated even when `source`
    /// is set to a subquery so existing call sites that read
    /// `query.table.as_str()` keep compiling. When `source` is
    /// `Some(TableSource::Subquery(…))`, this field holds a synthetic
    /// sentinel name (`"__subq_NNNN"`) that runtime code must never
    /// resolve against the real schema registry.
    pub table: String,
    /// Fase 2 Week 3: structured table source. `None` means the
    /// legacy `table` field is authoritative. `Some(Name)` is the
    /// same information as `table` but in typed form. `Some(Subquery)`
    /// wires a `(SELECT …) AS alias` in a FROM position — the Fase
    /// 1.7 unlock.
    pub source: Option<TableSource>,
    /// Optional table alias
    pub alias: Option<String>,
    /// Canonical SQL select list.
    pub select_items: Vec<SelectItem>,
    /// Columns to select (empty = all)
    pub columns: Vec<Projection>,
    /// Canonical SQL WHERE clause.
    pub where_expr: Option<super::Expr>,
    /// Filter condition
    pub filter: Option<Filter>,
    /// Canonical SQL GROUP BY items.
    pub group_by_exprs: Vec<super::Expr>,
    /// GROUP BY fields
    pub group_by: Vec<String>,
    /// Canonical SQL HAVING clause.
    pub having_expr: Option<super::Expr>,
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

/// Structured FROM source for a `TableQuery`. Additive alongside the
/// legacy `TableQuery.table: String` slot — callers that understand
/// this type can branch on subqueries; callers that only read `table`
/// fall back to the synthetic sentinel name and, for subqueries,
/// produce an "unknown table" error until they migrate.
#[derive(Debug, Clone)]
pub enum TableSource {
    /// Plain table reference — equivalent to the legacy `String` form.
    Name(String),
    /// A subquery in FROM position: `FROM (SELECT …) AS alias`.
    Subquery(Box<QueryExpr>),
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
            source: None,
            alias: None,
            select_items: Vec::new(),
            columns: Vec::new(),
            where_expr: None,
            filter: None,
            group_by_exprs: Vec::new(),
            group_by: Vec::new(),
            having_expr: None,
            having: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            expand: None,
        }
    }

    /// Create a TableQuery that wraps a subquery in FROM position.
    /// The legacy `table` slot holds a synthetic sentinel so code that
    /// only reads `table.as_str()` errors loudly with a
    /// recognisable marker instead of silently treating it as a
    /// real collection.
    pub fn from_subquery(subquery: QueryExpr, alias: Option<String>) -> Self {
        let sentinel = match &alias {
            Some(a) => format!("__subq_{a}"),
            None => "__subq_anon".to_string(),
        };
        Self {
            table: sentinel,
            source: Some(TableSource::Subquery(Box::new(subquery))),
            alias,
            select_items: Vec::new(),
            columns: Vec::new(),
            where_expr: None,
            filter: None,
            group_by_exprs: Vec::new(),
            group_by: Vec::new(),
            having_expr: None,
            having: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            expand: None,
        }
    }
}

/// Canonical SQL select item for table queries.
#[derive(Debug, Clone, PartialEq)]
pub enum SelectItem {
    Wildcard,
    Expr {
        expr: super::Expr,
        alias: Option<String>,
    },
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
    /// Canonical SQL RETURN projection.
    pub return_items: Vec<SelectItem>,
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
            return_items: Vec::new(),
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
    /// Inner join — only matching pairs emitted
    Inner,
    /// Left outer join — every left row, matched or padded with nulls on the right
    LeftOuter,
    /// Right outer join — every right row, matched or padded with nulls on the left
    RightOuter,
    /// Full outer join — LeftOuter ∪ RightOuter, each unmatched side padded
    FullOuter,
    /// Cross join — Cartesian product, no predicate
    Cross,
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

/// An item in a RETURNING clause: either `*` (all columns) or a named column.
#[derive(Debug, Clone, PartialEq)]
pub enum ReturningItem {
    /// RETURNING *
    All,
    /// RETURNING col
    Column(String),
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
    /// Canonical SQL rows of expressions.
    pub value_exprs: Vec<Vec<super::Expr>>,
    /// Rows of values (each inner Vec is one row)
    pub values: Vec<Vec<Value>>,
    /// Optional RETURNING clause items.
    pub returning: Option<Vec<ReturningItem>>,
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
    /// Canonical SQL assignments.
    pub assignment_exprs: Vec<(String, super::Expr)>,
    /// Best-effort literal-only cache of assignments. Non-foldable expressions
    /// are preserved exclusively in `assignment_exprs` and evaluated later
    /// against the row pre-image by the runtime.
    pub assignments: Vec<(String, Value)>,
    /// Canonical SQL WHERE clause.
    pub where_expr: Option<super::Expr>,
    /// Optional WHERE filter
    pub filter: Option<Filter>,
    /// Optional TTL in milliseconds (from WITH TTL clause)
    pub ttl_ms: Option<u64>,
    /// Optional absolute expiration (from WITH EXPIRES AT clause)
    pub expires_at_ms: Option<u64>,
    /// Optional metadata key-value pairs (from WITH METADATA clause)
    pub with_metadata: Vec<(String, Value)>,
    /// Optional RETURNING clause items.
    pub returning: Option<Vec<ReturningItem>>,
}

/// DELETE FROM table WHERE filter
#[derive(Debug, Clone)]
pub struct DeleteQuery {
    /// Target table name
    pub table: String,
    /// Canonical SQL WHERE clause.
    pub where_expr: Option<super::Expr>,
    /// Optional WHERE filter
    pub filter: Option<Filter>,
    /// Optional RETURNING clause items.
    pub returning: Option<Vec<ReturningItem>>,
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
    /// Enables the global context index for this table
    /// (`WITH context_index = true`). Default false — pure OLTP tables
    /// skip the tokenisation / 3-way RwLock write storm on every insert.
    /// Having `context_index_fields` non-empty also enables it implicitly.
    pub context_index_enabled: bool,
    /// When true, CREATE TABLE implicitly adds two user-visible columns
    /// `created_at` and `updated_at` (BIGINT unix-ms). The runtime
    /// populates them from `UnifiedEntity::created_at/updated_at` on
    /// every write; `created_at` is immutable after insert.
    /// Enabled via `WITH timestamps = true` in the DDL.
    pub timestamps: bool,
    /// Partitioning spec (Phase 2.2 PG parity).
    ///
    /// When present the table is the *parent* of a partition tree — every
    /// child partition is registered via `ALTER TABLE ... ATTACH PARTITION`.
    /// Phase 2.2 stops at registry-only: queries against a partitioned
    /// parent don't auto-rewrite as UNION yet (Phase 4 adds pruning).
    pub partition_by: Option<PartitionSpec>,
    /// Table-scoped multi-tenancy declaration (Phase 2.5.4).
    ///
    /// Syntax: `CREATE TABLE t (...) WITH (tenant_by = 'col_name')` or
    /// the shorthand `CREATE TABLE t (...) TENANT BY (col_name)`. The
    /// runtime treats the named column as the tenant discriminator and
    /// automatically:
    ///
    /// 1. Registers the table → column mapping so INSERTs that omit the
    ///    column get `CURRENT_TENANT()` auto-filled.
    /// 2. Installs an implicit RLS policy equivalent to
    ///    `USING (col = CURRENT_TENANT())` for SELECT/UPDATE/DELETE/INSERT.
    /// 3. Flips `rls_enabled_tables` on so the policy actually applies.
    ///
    /// None leaves the table non-tenant-scoped — callers manage tenancy
    /// manually via explicit CREATE POLICY if they want it.
    pub tenant_by: Option<String>,
    /// When true, UPDATE and DELETE on this table are rejected at
    /// parse time. Corresponds to `CREATE TABLE ... APPEND ONLY` or
    /// `WITH (append_only = true)`. Default false (mutable).
    pub append_only: bool,
}

/// `PARTITION BY RANGE|LIST|HASH (column)` clause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartitionSpec {
    pub kind: PartitionKind,
    /// Partition key column(s). Simple single-column for Phase 2.2.
    pub column: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionKind {
    /// `PARTITION BY RANGE(col)` — children bind `FOR VALUES FROM (a) TO (b)`.
    Range,
    /// `PARTITION BY LIST(col)` — children bind `FOR VALUES IN (v1, v2, ...)`.
    List,
    /// `PARTITION BY HASH(col)` — children bind `FOR VALUES WITH (MODULUS m, REMAINDER r)`.
    Hash,
}

/// Column definition for CREATE TABLE
#[derive(Debug, Clone)]
pub struct CreateColumnDef {
    /// Column name
    pub name: String,
    /// Legacy declared type string preserved for the runtime/storage pipeline.
    pub data_type: String,
    /// Structured SQL type used by the semantic layer.
    pub sql_type: SqlTypeName,
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
    /// `ATTACH PARTITION child FOR VALUES ...` (Phase 2.2 PG parity).
    ///
    /// Binds an existing child table to the parent partitioned table.
    /// The `bound` string captures the raw bound expression so the
    /// runtime can round-trip it back into `red_config` without a
    /// dedicated per-kind AST.
    AttachPartition {
        child: String,
        /// Human-readable bound string, e.g. `FROM (2024-01-01) TO (2025-01-01)`
        /// or `IN (1, 2, 3)` or `WITH (MODULUS 4, REMAINDER 0)`.
        bound: String,
    },
    /// `DETACH PARTITION child`
    DetachPartition { child: String },
    /// `ENABLE ROW LEVEL SECURITY` (Phase 2.5 PG parity).
    ///
    /// Flips the table into RLS-enforced mode. Reads against the table
    /// will be filtered by every matching `CREATE POLICY` (for the
    /// current role) combined with `AND`.
    EnableRowLevelSecurity,
    /// `DISABLE ROW LEVEL SECURITY` — disables enforcement; policies
    /// remain defined but are ignored until re-enabled.
    DisableRowLevelSecurity,
    /// `ENABLE TENANCY ON (col)` (Phase 2.5.4 PG parity-ish).
    ///
    /// Retrofit a tenant-scoped declaration onto an existing table —
    /// registers the column, installs the auto `__tenant_iso` RLS
    /// policy, and flips RLS on. Equivalent to re-running
    /// `CREATE TABLE ... TENANT BY (col)` minus the schema creation.
    EnableTenancy { column: String },
    /// `DISABLE TENANCY` — tears down the auto-policy and clears the
    /// tenancy registration. User-defined policies on the table are
    /// untouched; RLS stays enabled if any survive.
    DisableTenancy,
    /// `SET APPEND_ONLY = true|false` — flips the catalog flag.
    /// Setting `true` rejects all future UPDATE/DELETE at parse-time
    /// guard; setting `false` re-enables them. Existing rows are
    /// untouched either way — this is a purely declarative switch.
    SetAppendOnly(bool),
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
    /// Field-to-field comparison: left.field op right.field. Used when
    /// WHERE / BETWEEN operands reference another column instead of a
    /// literal — the pre-Fase-2-parser-v2 shim for column-to-column
    /// predicates. Once the Expr-rewrite lands, this collapses into
    /// `Compare { left: Expr, op, right: Expr }`.
    CompareFields {
        left: FieldRef,
        op: CompareOp,
        right: FieldRef,
    },
    /// Expression-to-expression comparison: `lhs op rhs` where either
    /// side may be an arbitrary `Expr` tree (function call, CAST,
    /// arithmetic, nested CASE). This is the most general compare
    /// variant — `Compare` and `CompareFields` stay as fast-path
    /// specialisations because the planner / cost model / index
    /// selector all pattern-match on the simpler shapes. The parser
    /// only emits this variant when a simpler one cannot express the
    /// predicate.
    CompareExpr {
        lhs: super::Expr,
        op: CompareOp,
        rhs: super::Expr,
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

    /// Bottom-up AST rewrites: OR-of-equalities → IN, AND/OR flatten.
    /// Inspired by MongoDB's `MatchExpression::optimize()`.
    /// Call on the result of `effective_table_filter()` before evaluation.
    pub fn optimize(self) -> Self {
        crate::storage::query::filter_optimizer::optimize(self)
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

/// Order by clause.
///
/// Fase 2 migration: `field` is the legacy bare column reference and
/// remains populated for back-compat with existing callers (SPARQL /
/// Gremlin / Cypher translators, the planner cost model, etc.). The
/// new `expr` slot carries an arbitrary `Expr` tree — when present,
/// runtime comparators prefer it over `field`, so the parser can
/// emit `ORDER BY CAST(a AS INT)`, `ORDER BY a + b * 2`, etc. without
/// breaking the rest of the codebase.
///
/// When `expr` is `None`, the clause behaves exactly like before.
/// When `expr` is `Some(Expr::Column(f))`, runtime code may still use
/// the legacy path — it's equivalent. Constructors default `expr` to
/// `None` so all existing call sites stay source-compatible.
#[derive(Debug, Clone)]
pub struct OrderByClause {
    /// Field to order by. Left populated even when `expr` is set so
    /// legacy consumers (planner cardinality estimate, cost model,
    /// mode translators) that still pattern-match on `field` keep
    /// working during the Fase 2 migration.
    pub field: FieldRef,
    /// Fase 2 expression-aware sort key. When `Some`, runtime order
    /// comparators evaluate this expression per row and sort on the
    /// resulting values — unlocks `ORDER BY expr` (Fase 1.6).
    pub expr: Option<super::Expr>,
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
            expr: None,
            ascending: true,
            nulls_first: false,
        }
    }

    /// Create descending order
    pub fn desc(field: FieldRef) -> Self {
        Self {
            field,
            expr: None,
            ascending: false,
            nulls_first: true,
        }
    }

    /// Attach an `Expr` sort key to an existing clause. Leaves `field`
    /// untouched so back-compat match sites keep their pattern.
    pub fn with_expr(mut self, expr: super::Expr) -> Self {
        self.expr = Some(expr);
        self
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
    /// GRAPH PROPERTIES
    Properties,
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

/// CREATE TIMESERIES name [RETENTION duration] [CHUNK_SIZE n] [DOWNSAMPLE spec[, spec...]]
///
/// `CREATE HYPERTABLE` lands on the same AST with `hypertable` populated.
/// The TimescaleDB-style syntax (time column + chunk_interval) gives the
/// runtime enough to register a `HypertableSpec` alongside the
/// underlying collection contract, so chunk routing and TTL sweeps can
/// address the table without a separate DDL.
#[derive(Debug, Clone)]
pub struct CreateTimeSeriesQuery {
    pub name: String,
    pub retention_ms: Option<u64>,
    pub chunk_size: Option<usize>,
    pub downsample_policies: Vec<String>,
    pub if_not_exists: bool,
    /// When `Some`, the DDL was spelled `CREATE HYPERTABLE` and the
    /// runtime must register the spec with the hypertable registry.
    pub hypertable: Option<HypertableDdl>,
}

/// Hypertable-specific DDL fields — set only when the caller used
/// `CREATE HYPERTABLE`.
#[derive(Debug, Clone)]
pub struct HypertableDdl {
    /// Column that carries the nanosecond timestamp axis.
    pub time_column: String,
    /// Chunk width in nanoseconds.
    pub chunk_interval_ns: u64,
    /// Per-chunk default TTL in nanoseconds (`None` = no TTL).
    pub default_ttl_ns: Option<u64>,
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

/// CREATE QUEUE name [MAX_SIZE n] [PRIORITY] [WITH TTL duration] [WITH DLQ name] [MAX_ATTEMPTS n]
#[derive(Debug, Clone)]
pub struct CreateQueueQuery {
    pub name: String,
    pub priority: bool,
    pub max_size: Option<usize>,
    pub ttl_ms: Option<u64>,
    pub dlq: Option<String>,
    pub max_attempts: u32,
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
        value: Value,
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
    Pending {
        queue: String,
        group: String,
    },
    Claim {
        queue: String,
        group: String,
        consumer: String,
        min_idle_ms: u64,
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
// Tree DDL & Commands
// ============================================================================

#[derive(Debug, Clone)]
pub struct TreeNodeSpec {
    pub label: String,
    pub node_type: Option<String>,
    pub properties: Vec<(String, Value)>,
    pub metadata: Vec<(String, Value)>,
    pub max_children: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreePosition {
    First,
    Last,
    Index(usize),
}

#[derive(Debug, Clone)]
pub struct CreateTreeQuery {
    pub collection: String,
    pub name: String,
    pub root: TreeNodeSpec,
    pub default_max_children: usize,
    pub if_not_exists: bool,
}

#[derive(Debug, Clone)]
pub struct DropTreeQuery {
    pub collection: String,
    pub name: String,
    pub if_exists: bool,
}

#[derive(Debug, Clone)]
pub enum TreeCommand {
    Insert {
        collection: String,
        tree_name: String,
        parent_id: u64,
        node: TreeNodeSpec,
        position: TreePosition,
    },
    Move {
        collection: String,
        tree_name: String,
        node_id: u64,
        parent_id: u64,
        position: TreePosition,
    },
    Delete {
        collection: String,
        tree_name: String,
        node_id: u64,
    },
    Validate {
        collection: String,
        tree_name: String,
    },
    Rebalance {
        collection: String,
        tree_name: String,
        dry_run: bool,
    },
}

// ============================================================================
// Builders (Fluent API)
// ============================================================================
