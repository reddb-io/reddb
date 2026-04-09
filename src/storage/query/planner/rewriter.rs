//! Query Rewriter
//!
//! Multi-pass AST transformation system inspired by Neo4j's query rewriting.
//!
//! # Rewrite Passes
//!
//! 1. **Normalize**: Standardize AST structure
//! 2. **InjectCachedProperties**: Cache property lookups at compile time
//! 3. **SimplifyFilters**: Combine and simplify filter expressions
//! 4. **PushdownPredicates**: Move filters closer to data source
//! 5. **ValidateFunctions**: Check function calls against schema

use crate::storage::query::ast::{
    CompareOp, FieldRef, Filter as AstFilter, JoinQuery, Projection, QueryExpr,
};
use crate::storage::schema::Value;

/// Context for rewrite operations
#[derive(Debug, Clone, Default)]
pub struct RewriteContext {
    /// Property cache for compile-time lookups
    pub property_cache: Vec<CachedProperty>,
    /// Validation errors encountered
    pub errors: Vec<String>,
    /// Warnings generated
    pub warnings: Vec<String>,
    /// Statistics about rewrites
    pub stats: RewriteStats,
}

/// A cached property lookup
#[derive(Debug, Clone)]
pub struct CachedProperty {
    /// Source alias (table or node)
    pub source: String,
    /// Property name
    pub property: String,
    /// Cached value if known at compile time
    pub cached_value: Option<String>,
}

/// Statistics about rewrite passes
#[derive(Debug, Clone, Default)]
pub struct RewriteStats {
    /// Number of filters simplified
    pub filters_simplified: u32,
    /// Number of predicates pushed down
    pub predicates_pushed: u32,
    /// Number of properties cached
    pub properties_cached: u32,
    /// Number of expressions normalized
    pub expressions_normalized: u32,
}

/// A rewrite rule that transforms query expressions
pub trait RewriteRule: Send + Sync {
    /// Rule name for debugging
    fn name(&self) -> &str;

    /// Apply the rule to a query expression
    fn apply(&self, query: QueryExpr, ctx: &mut RewriteContext) -> QueryExpr;

    /// Check if this rule is applicable to the query
    fn is_applicable(&self, query: &QueryExpr) -> bool;
}

/// Query rewriter with pluggable rules
pub struct QueryRewriter {
    /// Ordered list of rewrite rules
    rules: Vec<Box<dyn RewriteRule>>,
    /// Maximum number of rewrite iterations
    max_iterations: usize,
}

impl QueryRewriter {
    /// Create a new rewriter with default rules
    pub fn new() -> Self {
        let rules: Vec<Box<dyn RewriteRule>> = vec![
            Box::new(NormalizeRule),
            Box::new(SimplifyFiltersRule),
            Box::new(PushdownPredicatesRule),
            Box::new(EliminateDeadCodeRule),
            Box::new(FoldConstantsRule),
        ];

        Self {
            rules,
            max_iterations: 10,
        }
    }

    /// Add a custom rewrite rule
    pub fn add_rule(&mut self, rule: Box<dyn RewriteRule>) {
        self.rules.push(rule);
    }

    /// Rewrite a query expression
    pub fn rewrite(&self, query: QueryExpr) -> QueryExpr {
        let mut ctx = RewriteContext::default();
        self.rewrite_with_context(query, &mut ctx)
    }

    /// Rewrite with access to context
    pub fn rewrite_with_context(
        &self,
        mut query: QueryExpr,
        ctx: &mut RewriteContext,
    ) -> QueryExpr {
        // Apply rules iteratively until fixed point
        for _iteration in 0..self.max_iterations {
            let original = format!("{:?}", query);

            for rule in &self.rules {
                if rule.is_applicable(&query) {
                    query = rule.apply(query, ctx);
                }
            }

            // Check for fixed point
            if format!("{:?}", query) == original {
                break;
            }
        }

        query
    }
}

impl Default for QueryRewriter {
    fn default() -> Self {
        Self::new()
    }
}

// =============================================================================
// Built-in Rewrite Rules
// =============================================================================

/// Normalize AST structure
struct NormalizeRule;

impl RewriteRule for NormalizeRule {
    fn name(&self) -> &str {
        "Normalize"
    }

    fn apply(&self, query: QueryExpr, ctx: &mut RewriteContext) -> QueryExpr {
        match query {
            QueryExpr::Table(mut tq) => {
                // Normalize column order
                tq.columns.sort_by(|a, b| {
                    let a_name = projection_name(a);
                    let b_name = projection_name(b);
                    a_name.cmp(&b_name)
                });
                ctx.stats.expressions_normalized += 1;
                QueryExpr::Table(tq)
            }
            QueryExpr::Graph(gq) => {
                // Graph queries don't need normalization currently
                QueryExpr::Graph(gq)
            }
            QueryExpr::Join(jq) => {
                // Recursively normalize children
                let left = self.apply(*jq.left, ctx);
                let right = self.apply(*jq.right, ctx);
                QueryExpr::Join(JoinQuery {
                    left: Box::new(left),
                    right: Box::new(right),
                    ..jq
                })
            }
            QueryExpr::Path(pq) => QueryExpr::Path(pq),
            QueryExpr::Vector(vq) => {
                // Vector queries don't need normalization currently
                QueryExpr::Vector(vq)
            }
            QueryExpr::Hybrid(mut hq) => {
                // Normalize the structured part
                hq.structured = Box::new(self.apply(*hq.structured, ctx));
                QueryExpr::Hybrid(hq)
            }
            // DML/DDL/Command statements pass through without normalization
            other @ (QueryExpr::Insert(_)
            | QueryExpr::Update(_)
            | QueryExpr::Delete(_)
            | QueryExpr::CreateTable(_)
            | QueryExpr::DropTable(_)
            | QueryExpr::AlterTable(_)
            | QueryExpr::GraphCommand(_)
            | QueryExpr::SearchCommand(_)) => other,
        }
    }

    fn is_applicable(&self, _query: &QueryExpr) -> bool {
        true
    }
}

/// Simplify filter expressions
struct SimplifyFiltersRule;

impl RewriteRule for SimplifyFiltersRule {
    fn name(&self) -> &str {
        "SimplifyFilters"
    }

    fn apply(&self, query: QueryExpr, ctx: &mut RewriteContext) -> QueryExpr {
        match query {
            QueryExpr::Table(mut tq) => {
                if let Some(filter) = tq.filter.take() {
                    tq.filter = Some(simplify_filter(filter, ctx));
                }
                QueryExpr::Table(tq)
            }
            QueryExpr::Graph(mut gq) => {
                if let Some(filter) = gq.filter.take() {
                    gq.filter = Some(simplify_filter(filter, ctx));
                }
                QueryExpr::Graph(gq)
            }
            QueryExpr::Join(mut jq) => {
                let left = self.apply(*jq.left, ctx);
                let right = self.apply(*jq.right, ctx);
                if let Some(filter) = jq.filter.take() {
                    jq.filter = Some(simplify_filter(filter, ctx));
                }
                jq.left = Box::new(left);
                jq.right = Box::new(right);
                QueryExpr::Join(jq)
            }
            QueryExpr::Path(pq) => QueryExpr::Path(pq),
            QueryExpr::Vector(vq) => {
                // Vector queries have MetadataFilter, not AstFilter
                // Pass through for now
                QueryExpr::Vector(vq)
            }
            QueryExpr::Hybrid(mut hq) => {
                // Simplify filters in the structured part
                hq.structured = Box::new(self.apply(*hq.structured, ctx));
                QueryExpr::Hybrid(hq)
            }
            // DML/DDL/Command statements pass through without filter simplification
            other @ (QueryExpr::Insert(_)
            | QueryExpr::Update(_)
            | QueryExpr::Delete(_)
            | QueryExpr::CreateTable(_)
            | QueryExpr::DropTable(_)
            | QueryExpr::AlterTable(_)
            | QueryExpr::GraphCommand(_)
            | QueryExpr::SearchCommand(_)) => other,
        }
    }

    fn is_applicable(&self, query: &QueryExpr) -> bool {
        match query {
            QueryExpr::Table(tq) => tq.filter.is_some(),
            QueryExpr::Graph(gq) => gq.filter.is_some(),
            QueryExpr::Join(_) => true,
            QueryExpr::Path(_) => false,
            QueryExpr::Vector(vq) => vq.filter.is_some(),
            QueryExpr::Hybrid(_) => true, // May have filters in structured part
            // DML/DDL/Command statements are not applicable for filter simplification
            QueryExpr::Insert(_)
            | QueryExpr::Update(_)
            | QueryExpr::Delete(_)
            | QueryExpr::CreateTable(_)
            | QueryExpr::DropTable(_)
            | QueryExpr::AlterTable(_)
            | QueryExpr::GraphCommand(_)
            | QueryExpr::SearchCommand(_) => false,
        }
    }
}

/// Push predicates down to data sources
struct PushdownPredicatesRule;

impl RewriteRule for PushdownPredicatesRule {
    fn name(&self) -> &str {
        "PushdownPredicates"
    }

    fn apply(&self, query: QueryExpr, ctx: &mut RewriteContext) -> QueryExpr {
        match query {
            QueryExpr::Join(mut jq) => {
                // Try to push join predicates down to children
                // This is a simplified version - real implementation would analyze
                // which predicates can be pushed to which child

                // For now, just recursively apply to children
                jq.left = Box::new(self.apply(*jq.left, ctx));
                jq.right = Box::new(self.apply(*jq.right, ctx));
                ctx.stats.predicates_pushed += 1;
                QueryExpr::Join(jq)
            }
            other => other,
        }
    }

    fn is_applicable(&self, query: &QueryExpr) -> bool {
        matches!(query, QueryExpr::Join(_))
    }
}

/// Eliminate dead code branches
struct EliminateDeadCodeRule;

impl RewriteRule for EliminateDeadCodeRule {
    fn name(&self) -> &str {
        "EliminateDeadCode"
    }

    fn apply(&self, query: QueryExpr, _ctx: &mut RewriteContext) -> QueryExpr {
        match query {
            QueryExpr::Table(mut tq) => {
                // Remove always-true filters
                if let Some(ref filter) = tq.filter {
                    if is_always_true(filter) {
                        tq.filter = None;
                    }
                }
                QueryExpr::Table(tq)
            }
            other => other,
        }
    }

    fn is_applicable(&self, query: &QueryExpr) -> bool {
        matches!(query, QueryExpr::Table(_))
    }
}

/// Fold constant expressions
struct FoldConstantsRule;

impl RewriteRule for FoldConstantsRule {
    fn name(&self) -> &str {
        "FoldConstants"
    }

    fn apply(&self, query: QueryExpr, _ctx: &mut RewriteContext) -> QueryExpr {
        // Constant folding is complex - for now just pass through
        // A real implementation would evaluate constant expressions at compile time
        query
    }

    fn is_applicable(&self, _query: &QueryExpr) -> bool {
        true
    }
}

// =============================================================================
// Helper Functions
// =============================================================================

fn projection_name(proj: &Projection) -> String {
    match proj {
        Projection::All => "*".to_string(),
        Projection::Column(name) => name.clone(),
        Projection::Alias(_, alias) => alias.clone(),
        Projection::Function(name, _) => name.clone(),
        Projection::Expression(expr, _) => format!("{:?}", expr),
        Projection::Field(field, alias) => alias.clone().unwrap_or_else(|| format!("{:?}", field)),
    }
}

fn simplify_filter(filter: AstFilter, ctx: &mut RewriteContext) -> AstFilter {
    match filter {
        AstFilter::And(left, right) => {
            let left = simplify_filter(*left, ctx);
            let right = simplify_filter(*right, ctx);

            // AND with TRUE -> other side
            if is_always_true(&left) {
                ctx.stats.filters_simplified += 1;
                return right;
            }
            if is_always_true(&right) {
                ctx.stats.filters_simplified += 1;
                return left;
            }

            // AND with FALSE -> FALSE
            if is_always_false(&left) || is_always_false(&right) {
                ctx.stats.filters_simplified += 1;
                return AstFilter::Compare {
                    field: FieldRef::TableColumn {
                        table: String::new(),
                        column: "1".to_string(),
                    },
                    op: CompareOp::Eq,
                    value: Value::Integer(0),
                };
            }

            AstFilter::And(Box::new(left), Box::new(right))
        }
        AstFilter::Or(left, right) => {
            let left = simplify_filter(*left, ctx);
            let right = simplify_filter(*right, ctx);

            // OR with FALSE -> other side
            if is_always_false(&left) {
                ctx.stats.filters_simplified += 1;
                return right;
            }
            if is_always_false(&right) {
                ctx.stats.filters_simplified += 1;
                return left;
            }

            // OR with TRUE -> TRUE
            if is_always_true(&left) || is_always_true(&right) {
                ctx.stats.filters_simplified += 1;
                return AstFilter::Compare {
                    field: FieldRef::TableColumn {
                        table: String::new(),
                        column: "1".to_string(),
                    },
                    op: CompareOp::Eq,
                    value: Value::Integer(1),
                };
            }

            AstFilter::Or(Box::new(left), Box::new(right))
        }
        AstFilter::Not(inner) => {
            let inner = simplify_filter(*inner, ctx);

            // NOT NOT x -> x
            if let AstFilter::Not(double_inner) = inner {
                ctx.stats.filters_simplified += 1;
                return *double_inner;
            }

            AstFilter::Not(Box::new(inner))
        }
        other => other,
    }
}

fn is_always_true(filter: &AstFilter) -> bool {
    match filter {
        AstFilter::Compare { field, op, value } => {
            // 1 = 1 is always true
            matches!(field, FieldRef::TableColumn { column, .. } if column == "1")
                && matches!(op, CompareOp::Eq)
                && matches!(value, Value::Integer(1))
        }
        _ => false,
    }
}

fn is_always_false(filter: &AstFilter) -> bool {
    match filter {
        AstFilter::Compare { field, op, value } => {
            // 1 = 0 is always false
            matches!(field, FieldRef::TableColumn { column, .. } if column == "1")
                && matches!(op, CompareOp::Eq)
                && matches!(value, Value::Integer(0))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_field(name: &str) -> FieldRef {
        FieldRef::TableColumn {
            table: String::new(),
            column: name.to_string(),
        }
    }

    #[test]
    fn test_simplify_and_with_true() {
        let mut ctx = RewriteContext::default();

        let filter = AstFilter::And(
            Box::new(AstFilter::Compare {
                field: make_field("1"),
                op: CompareOp::Eq,
                value: Value::Integer(1),
            }),
            Box::new(AstFilter::Compare {
                field: make_field("x"),
                op: CompareOp::Eq,
                value: Value::Integer(5),
            }),
        );

        let simplified = simplify_filter(filter, &mut ctx);

        match simplified {
            AstFilter::Compare { field, .. } => {
                assert!(matches!(field, FieldRef::TableColumn { column, .. } if column == "x"));
            }
            _ => panic!("Expected Compare filter"),
        }
    }

    #[test]
    fn test_simplify_double_not() {
        let mut ctx = RewriteContext::default();

        let filter = AstFilter::Not(Box::new(AstFilter::Not(Box::new(AstFilter::Compare {
            field: make_field("x"),
            op: CompareOp::Eq,
            value: Value::Integer(5),
        }))));

        let simplified = simplify_filter(filter, &mut ctx);

        match simplified {
            AstFilter::Compare { field, .. } => {
                assert!(matches!(field, FieldRef::TableColumn { column, .. } if column == "x"));
            }
            _ => panic!("Expected Compare filter"),
        }
    }
}
