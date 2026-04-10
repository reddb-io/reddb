use super::*;
use crate::storage::engine::GraphEdgeType;

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
            right: QueryExpr::Graph(GraphQuery::new(pattern)),
            on,
            join_type: JoinType::Inner,
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            return_: Vec::new(),
        }
    }

    /// Join with another table source
    pub fn join_table(self, table: &str, on: JoinCondition) -> JoinQueryBuilder {
        JoinQueryBuilder {
            left: QueryExpr::Table(self.query),
            right: QueryExpr::Table(TableQuery::new(table)),
            on,
            join_type: JoinType::Inner,
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            return_: Vec::new(),
        }
    }

    /// Join with a vector query
    pub fn join_vector(self, query: VectorQuery, on: JoinCondition) -> JoinQueryBuilder {
        JoinQueryBuilder {
            left: QueryExpr::Table(self.query),
            right: QueryExpr::Vector(query),
            on,
            join_type: JoinType::Inner,
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            return_: Vec::new(),
        }
    }

    /// Join with a path query
    pub fn join_path(self, query: PathQuery, on: JoinCondition) -> JoinQueryBuilder {
        JoinQueryBuilder {
            left: QueryExpr::Table(self.query),
            right: QueryExpr::Path(query),
            on,
            join_type: JoinType::Inner,
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            return_: Vec::new(),
        }
    }

    /// Join with a hybrid query
    pub fn join_hybrid(self, query: HybridQuery, on: JoinCondition) -> JoinQueryBuilder {
        JoinQueryBuilder {
            left: QueryExpr::Table(self.query),
            right: QueryExpr::Hybrid(query),
            on,
            join_type: JoinType::Inner,
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            return_: Vec::new(),
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

    /// Set outer alias
    pub fn alias(mut self, alias: &str) -> Self {
        self.query.alias = Some(alias.to_string());
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
    right: QueryExpr,
    on: JoinCondition,
    join_type: JoinType,
    filter: Option<Filter>,
    order_by: Vec<OrderByClause>,
    limit: Option<u64>,
    offset: Option<u64>,
    return_: Vec<Projection>,
}

impl JoinQueryBuilder {
    /// Set join type
    pub fn join_type(mut self, jt: JoinType) -> Self {
        self.join_type = jt;
        self
    }

    /// Set alias for the right-hand source
    pub fn right_alias(mut self, alias: &str) -> Self {
        let alias = alias.to_string();
        match &mut self.right {
            QueryExpr::Table(table) => table.alias = Some(alias.clone()),
            QueryExpr::Graph(graph) => graph.alias = Some(alias.clone()),
            QueryExpr::Path(path) => path.alias = Some(alias.clone()),
            QueryExpr::Vector(vector) => vector.alias = Some(alias.clone()),
            QueryExpr::Hybrid(hybrid) => hybrid.alias = Some(alias.clone()),
            QueryExpr::Join(_)
            | QueryExpr::Insert(_)
            | QueryExpr::Update(_)
            | QueryExpr::Delete(_)
            | QueryExpr::CreateTable(_)
            | QueryExpr::DropTable(_)
            | QueryExpr::AlterTable(_)
            | QueryExpr::GraphCommand(_)
            | QueryExpr::SearchCommand(_)
            | QueryExpr::Ask(_) => {}
        }
        self
    }

    /// Add post-join filter
    pub fn filter(mut self, f: Filter) -> Self {
        self.filter = Some(match self.filter.take() {
            Some(existing) => existing.and(f),
            None => f,
        });
        self
    }

    /// Add post-join ordering
    pub fn order_by(mut self, clause: OrderByClause) -> Self {
        self.order_by.push(clause);
        self
    }

    /// Set post-join limit
    pub fn limit(mut self, n: u64) -> Self {
        self.limit = Some(n);
        self
    }

    /// Set post-join offset
    pub fn offset(mut self, n: u64) -> Self {
        self.offset = Some(n);
        self
    }

    /// Add post-join projected field
    pub fn return_field(mut self, field: FieldRef) -> Self {
        self.return_.push(Projection::from_field(field));
        self
    }

    /// Add post-join projected column
    pub fn select(mut self, column: &str) -> Self {
        self.return_
            .push(Projection::from_field(FieldRef::column("", column)));
        self
    }

    /// Build the query expression
    pub fn build(self) -> QueryExpr {
        QueryExpr::Join(JoinQuery {
            left: Box::new(self.left),
            right: Box::new(self.right),
            join_type: self.join_type,
            on: self.on,
            filter: self.filter,
            order_by: self.order_by,
            limit: self.limit,
            offset: self.offset,
            return_: self.return_,
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

    /// Set outer alias
    pub fn alias(mut self, alias: &str) -> Self {
        self.query.alias = Some(alias.to_string());
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
