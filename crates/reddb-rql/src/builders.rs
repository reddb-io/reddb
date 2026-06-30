use super::*;
use crate::sql_lowering::{filter_to_expr, projection_to_select_item};

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
        let field = FieldRef::column(
            self.query.alias.as_deref().unwrap_or(&self.query.table),
            column,
        );
        self.query.select_items.push(SelectItem::Expr {
            expr: Expr::col(field.clone()),
            alias: None,
        });
        self.query.columns.push(Projection::from_field(field));
        self
    }

    /// Add all columns
    pub fn select_all(mut self) -> Self {
        self.query.select_items = vec![SelectItem::Wildcard];
        self.query.columns.clear();
        self
    }

    /// Add filter
    pub fn filter(mut self, f: Filter) -> Self {
        let f_expr = filter_to_expr(&f);
        self.query.where_expr = Some(match self.query.where_expr.take() {
            Some(existing) => Expr::binop(BinOp::And, existing, f_expr),
            None => f_expr,
        });
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
            return_items: Vec::new(),
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
            return_items: Vec::new(),
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
            return_items: Vec::new(),
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
            return_items: Vec::new(),
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
            return_items: Vec::new(),
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

    /// Set row limit
    pub fn limit(mut self, n: u64) -> Self {
        self.query.limit = Some(n);
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
    return_items: Vec<SelectItem>,
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
            | QueryExpr::CreateCollection(_)
            | QueryExpr::CreateVector(_)
            | QueryExpr::DropTable(_)
            | QueryExpr::DropGraph(_)
            | QueryExpr::DropVector(_)
            | QueryExpr::DropDocument(_)
            | QueryExpr::DropKv(_)
            | QueryExpr::DropCollection(_)
            | QueryExpr::Truncate(_)
            | QueryExpr::AlterTable(_)
            | QueryExpr::CreateVcsRef(_)
            | QueryExpr::DropVcsRef(_)
            | QueryExpr::GraphCommand(_)
            | QueryExpr::SearchCommand(_)
            | QueryExpr::CreateIndex(_)
            | QueryExpr::DropIndex(_)
            | QueryExpr::ProbabilisticCommand(_)
            | QueryExpr::Ask(_)
            | QueryExpr::SetConfig { .. }
            | QueryExpr::ShowConfig { .. }
            | QueryExpr::SetSecret { .. }
            | QueryExpr::DeleteSecret { .. }
            | QueryExpr::ShowSecrets { .. }
            | QueryExpr::SetTenant(_)
            | QueryExpr::ShowTenant
            | QueryExpr::CreateTimeSeries(_)
            | QueryExpr::CreateMetric(_)
            | QueryExpr::AlterMetric(_)
            | QueryExpr::CreateSlo(_)
            | QueryExpr::DropTimeSeries(_)
            | QueryExpr::CreateQueue(_)
            | QueryExpr::AlterQueue(_)
            | QueryExpr::DropQueue(_)
            | QueryExpr::QueueSelect(_)
            | QueryExpr::QueueCommand(_)
            | QueryExpr::KvCommand(_)
            | QueryExpr::ConfigCommand(_)
            | QueryExpr::CreateTree(_)
            | QueryExpr::DropTree(_)
            | QueryExpr::TreeCommand(_)
            | QueryExpr::ExplainAlter(_)
            | QueryExpr::TransactionControl(_)
            | QueryExpr::MaintenanceCommand(_)
            | QueryExpr::VcsCommand(_)
            | QueryExpr::CreateSchema(_)
            | QueryExpr::DropSchema(_)
            | QueryExpr::CreateSequence(_)
            | QueryExpr::DropSequence(_)
            | QueryExpr::CopyFrom(_)
            | QueryExpr::CreateView(_)
            | QueryExpr::DropView(_)
            | QueryExpr::RefreshMaterializedView(_)
            | QueryExpr::CreatePolicy(_)
            | QueryExpr::DropPolicy(_)
            | QueryExpr::CreateServer(_)
            | QueryExpr::DropServer(_)
            | QueryExpr::CreateForeignTable(_)
            | QueryExpr::DropForeignTable(_)
            | QueryExpr::Grant(_)
            | QueryExpr::Revoke(_)
            | QueryExpr::AlterUser(_)
            | QueryExpr::CreateUser(_)
            | QueryExpr::CreateIamPolicy { .. }
            | QueryExpr::DropIamPolicy { .. }
            | QueryExpr::AttachPolicy { .. }
            | QueryExpr::DetachPolicy { .. }
            | QueryExpr::ShowPolicies { .. }
            | QueryExpr::ShowEffectivePermissions { .. }
            | QueryExpr::RankOf(_)
            | QueryExpr::ApproxRankOf(_)
            | QueryExpr::RankRange(_)
            | QueryExpr::SimulatePolicy { .. }
            | QueryExpr::LintPolicy { .. }
            | QueryExpr::MigratePolicyMode { .. }
            | QueryExpr::CreateMigration(_)
            | QueryExpr::ApplyMigration(_)
            | QueryExpr::RollbackMigration(_)
            | QueryExpr::ExplainMigration(_)
            | QueryExpr::EventsBackfill(_)
            | QueryExpr::EventsBackfillStatus { .. } => {}
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
        let projection = Projection::from_field(field);
        if let Some(item) = projection_to_select_item(&projection) {
            self.return_items.push(item);
        }
        self.return_.push(projection);
        self
    }

    /// Add post-join projected column
    pub fn select(mut self, column: &str) -> Self {
        let projection = Projection::from_field(FieldRef::column("", column));
        if let Some(item) = projection_to_select_item(&projection) {
            self.return_items.push(item);
        }
        self.return_.push(projection);
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
            return_items: self.return_items,
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

    /// Add edge label string to traverse (preferred).
    pub fn via_label(mut self, label: impl Into<String>) -> Self {
        self.query.via.push(label.into());
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
    // Builder method appending a CTE to the WITH clause; unrelated to
    // `std::ops::Add`, so that trait is intentionally not implemented.
    #[allow(clippy::should_implement_trait)]
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

#[cfg(test)]
mod tests {
    use super::*;

    fn eq_filter(column: &str, value: Value) -> Filter {
        Filter::compare(FieldRef::column("", column), CompareOp::Eq, value)
    }

    fn join_condition() -> JoinCondition {
        JoinCondition::new(
            FieldRef::column("left", "id"),
            FieldRef::column("right", "id"),
        )
    }

    #[test]
    fn table_builder_covers_selection_filters_order_limit_and_offset() {
        let query = TableQueryBuilder::new("hosts")
            .alias("h")
            .select("ip")
            .filter(eq_filter("os", Value::text("linux")))
            .filter(eq_filter("active", Value::Boolean(true)))
            .order_by(OrderByClause::desc(FieldRef::column("h", "last_seen")))
            .limit(10)
            .offset(5)
            .build();

        let QueryExpr::Table(table) = query else {
            panic!("expected table query");
        };
        assert_eq!(table.table, "hosts");
        assert_eq!(table.alias.as_deref(), Some("h"));
        assert_eq!(table.select_items.len(), 1);
        assert_eq!(table.columns.len(), 1);
        assert!(matches!(table.filter, Some(Filter::And(_, _))));
        assert!(matches!(
            table.where_expr,
            Some(Expr::BinaryOp { op: BinOp::And, .. })
        ));
        assert_eq!(table.order_by.len(), 1);
        assert_eq!(table.limit, Some(10));
        assert_eq!(table.offset, Some(5));

        let QueryExpr::Table(table) = TableQueryBuilder::new("hosts").select_all().build() else {
            panic!("expected table query");
        };
        assert_eq!(table.select_items, vec![SelectItem::Wildcard]);
        assert!(table.columns.is_empty());
    }

    #[test]
    fn graph_builder_combines_filters_alias_limit_and_returns() {
        let query = GraphQueryBuilder::new()
            .node(NodePattern::new("h").of_label("Host"))
            .edge(EdgePattern::new("h", "s").of_label("HAS_SERVICE"))
            .filter(eq_filter("critical", Value::Boolean(true)))
            .filter(eq_filter("active", Value::Boolean(true)))
            .alias("g")
            .limit(3)
            .return_field(FieldRef::node_prop("h", "ip"))
            .build();

        let QueryExpr::Graph(graph) = query else {
            panic!("expected graph query");
        };
        assert_eq!(graph.alias.as_deref(), Some("g"));
        assert_eq!(graph.pattern.nodes.len(), 1);
        assert_eq!(graph.pattern.edges.len(), 1);
        assert!(matches!(graph.filter, Some(Filter::And(_, _))));
        assert_eq!(graph.limit, Some(3));
        assert_eq!(graph.return_.len(), 1);
    }

    #[test]
    fn join_builder_aliases_supported_right_sources_and_builds_options() {
        let condition = join_condition();
        let cases = vec![
            TableQueryBuilder::new("hosts").join_table("services", condition.clone()),
            TableQueryBuilder::new("hosts").join_graph(GraphPattern::new(), condition.clone()),
            TableQueryBuilder::new("hosts").join_path(
                PathQuery::new(NodeSelector::by_id("a"), NodeSelector::by_id("b")),
                condition.clone(),
            ),
            TableQueryBuilder::new("hosts").join_vector(
                VectorQuery::new("embeddings", VectorSource::text("ssh")),
                condition.clone(),
            ),
            TableQueryBuilder::new("hosts").join_hybrid(
                HybridQuery::new(
                    QueryExpr::Table(TableQuery::new("hosts")),
                    VectorQuery::new("embeddings", VectorSource::text("ssh")),
                ),
                condition.clone(),
            ),
        ];

        for builder in cases {
            let query = builder
                .right_alias("rhs")
                .join_type(JoinType::FullOuter)
                .filter(eq_filter("ok", Value::Boolean(true)))
                .order_by(OrderByClause::asc(FieldRef::column("", "id")))
                .limit(4)
                .offset(2)
                .return_field(FieldRef::column("hosts", "id"))
                .select("name")
                .build();

            let QueryExpr::Join(join) = query else {
                panic!("expected join query");
            };
            assert_eq!(join.join_type, JoinType::FullOuter);
            assert!(join.filter.is_some());
            assert_eq!(join.order_by.len(), 1);
            assert_eq!(join.limit, Some(4));
            assert_eq!(join.offset, Some(2));
            assert_eq!(join.return_.len(), 2);
            assert_eq!(join.return_items.len(), 2);
            match *join.right {
                QueryExpr::Table(table) => assert_eq!(table.alias.as_deref(), Some("rhs")),
                QueryExpr::Graph(graph) => assert_eq!(graph.alias.as_deref(), Some("rhs")),
                QueryExpr::Path(path) => assert_eq!(path.alias.as_deref(), Some("rhs")),
                QueryExpr::Vector(vector) => assert_eq!(vector.alias.as_deref(), Some("rhs")),
                QueryExpr::Hybrid(hybrid) => assert_eq!(hybrid.alias.as_deref(), Some("rhs")),
                other => panic!("unexpected right source: {other:?}"),
            }
        }
    }

    #[test]
    fn join_builder_right_alias_ignores_non_source_variants() {
        let builder = JoinQueryBuilder {
            left: QueryExpr::Table(TableQuery::new("left")),
            right: QueryExpr::SetTenant(Some("acme".to_string())),
            on: join_condition(),
            join_type: JoinType::Inner,
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            return_items: Vec::new(),
            return_: Vec::new(),
        };
        let query = builder.right_alias("ignored").build();
        let QueryExpr::Join(join) = query else {
            panic!("expected join query");
        };
        assert!(matches!(*join.right, QueryExpr::SetTenant(Some(ref tenant)) if tenant == "acme"));
    }

    #[test]
    fn path_builder_sets_alias_via_filter_and_length() {
        let query = PathQueryBuilder::new(NodeSelector::by_id("a"), NodeSelector::by_id("b"))
            .via_label("CONNECTS_TO")
            .max_length(7)
            .filter(eq_filter("kind", Value::text("vpn")))
            .alias("p")
            .build();

        let QueryExpr::Path(path) = query else {
            panic!("expected path query");
        };
        assert_eq!(path.alias.as_deref(), Some("p"));
        assert_eq!(path.via, vec!["CONNECTS_TO"]);
        assert_eq!(path.max_length, 7);
        assert!(path.filter.is_some());
    }

    #[test]
    fn cte_helpers_track_recursive_state_and_lookup() {
        let base = QueryExpr::Table(TableQuery::new("hosts"));
        let cte = CteDefinition::new("active", base.clone())
            .with_columns(vec!["id".to_string(), "ip".to_string()]);
        assert_eq!(cte.name, "active");
        assert_eq!(cte.columns, vec!["id", "ip"]);
        assert!(!cte.recursive);

        let recursive = CteDefinition::recursive("walk", base.clone());
        assert!(recursive.recursive);

        let clause = WithClause::new().add(cte).add(recursive);
        assert!(!clause.is_empty());
        assert!(clause.has_recursive);
        assert!(clause.get("active").is_some());
        assert!(clause.get("missing").is_none());

        let simple = QueryWithCte::simple(base.clone());
        assert!(simple.with_clause.is_none());
        assert!(matches!(simple.query, QueryExpr::Table(_)));

        let with_ctes = QueryWithCte::with_ctes(clause.clone(), base.clone());
        assert!(with_ctes.with_clause.is_some());

        let built = CteQueryBuilder::new()
            .cte("one", base.clone())
            .recursive_cte("two", base.clone())
            .cte_with_columns("three", vec!["id".to_string()], base.clone())
            .build(base);
        let clause = built.with_clause.expect("with clause");
        assert_eq!(clause.ctes.len(), 3);
        assert!(clause.has_recursive);
        assert_eq!(clause.get("three").expect("cte").columns, vec!["id"]);
    }
}
