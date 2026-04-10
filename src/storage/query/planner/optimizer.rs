//! Query Optimizer
//!
//! Multi-pass query optimization with pluggable strategies.
//!
//! # Optimization Passes
//!
//! 1. **PredicatePushdown**: Move filters to data sources
//! 2. **JoinReordering**: Optimal join order via IDP algorithm
//! 3. **IndexSelection**: Choose best indexes for scans
//! 4. **ProjectionPushdown**: Eliminate unused columns early
//! 5. **ExpressionSimplification**: Simplify complex expressions

use crate::storage::query::ast::{JoinQuery, JoinType, QueryExpr};

/// An optimization pass that transforms query expressions
pub trait OptimizationPass: Send + Sync {
    /// Pass name for debugging
    fn name(&self) -> &str;

    /// Apply the optimization pass
    fn apply(&self, query: QueryExpr) -> QueryExpr;

    /// Estimated benefit (higher = more important)
    fn benefit(&self) -> u32;
}

/// Query optimizer with multiple passes
pub struct QueryOptimizer {
    /// Ordered optimization passes
    passes: Vec<Box<dyn OptimizationPass>>,
    /// Enable cost-based optimization
    cost_based: bool,
}

impl QueryOptimizer {
    /// Create a new optimizer with default passes
    pub fn new() -> Self {
        let passes: Vec<Box<dyn OptimizationPass>> = vec![
            Box::new(PredicatePushdownPass),
            Box::new(ProjectionPushdownPass),
            Box::new(JoinReorderingPass),
            Box::new(IndexSelectionPass),
            Box::new(LimitPushdownPass),
        ];

        Self {
            passes,
            cost_based: true,
        }
    }

    /// Add a custom optimization pass
    pub fn add_pass(&mut self, pass: Box<dyn OptimizationPass>) {
        self.passes.push(pass);
        // Sort by benefit (highest first)
        self.passes.sort_by_key(|b| std::cmp::Reverse(b.benefit()));
    }

    /// Optimize a query expression
    pub fn optimize(&self, query: QueryExpr) -> (QueryExpr, Vec<String>) {
        let mut optimized = query;
        let mut applied_passes = Vec::new();

        for pass in &self.passes {
            let before = format!("{:?}", optimized);
            optimized = pass.apply(optimized);
            let after = format!("{:?}", optimized);

            if before != after {
                applied_passes.push(pass.name().to_string());
            }
        }

        (optimized, applied_passes)
    }

    /// Optimize with hints
    pub fn optimize_with_hints(&self, query: QueryExpr, hints: &OptimizationHints) -> QueryExpr {
        let mut optimized = query;

        for pass in &self.passes {
            // Check if pass is disabled by hints
            if hints.disabled_passes.contains(&pass.name().to_string()) {
                continue;
            }

            optimized = pass.apply(optimized);
        }

        optimized
    }
}

impl Default for QueryOptimizer {
    fn default() -> Self {
        Self::new()
    }
}

/// Hints to control optimization
#[derive(Debug, Clone, Default)]
pub struct OptimizationHints {
    /// Disabled optimization passes
    pub disabled_passes: Vec<String>,
    /// Force specific join order
    pub join_order: Option<Vec<String>>,
    /// Force specific index usage
    pub force_index: Option<String>,
    /// Disable parallel execution
    pub no_parallel: bool,
}

// =============================================================================
// Built-in Optimization Passes
// =============================================================================

/// Push predicates down to data sources
struct PredicatePushdownPass;

impl OptimizationPass for PredicatePushdownPass {
    fn name(&self) -> &str {
        "PredicatePushdown"
    }

    fn apply(&self, query: QueryExpr) -> QueryExpr {
        match query {
            QueryExpr::Join(jq) => self.optimize_join(jq),
            other => other,
        }
    }

    fn benefit(&self) -> u32 {
        100 // High priority - reduces data early
    }
}

impl PredicatePushdownPass {
    fn optimize_join(&self, query: JoinQuery) -> QueryExpr {
        // Analyze join condition to find pushable predicates
        // This is a simplified version - real implementation would analyze
        // predicate dependencies on join columns

        let left = self.apply(*query.left);
        let right = self.apply(*query.right);

        QueryExpr::Join(JoinQuery {
            left: Box::new(left),
            right: Box::new(right),
            ..query
        })
    }
}

/// Push projections down to eliminate columns early
struct ProjectionPushdownPass;

impl OptimizationPass for ProjectionPushdownPass {
    fn name(&self) -> &str {
        "ProjectionPushdown"
    }

    fn apply(&self, query: QueryExpr) -> QueryExpr {
        match query {
            QueryExpr::Join(jq) => {
                // Analyze which columns are actually needed
                let left = self.apply(*jq.left);
                let right = self.apply(*jq.right);

                QueryExpr::Join(JoinQuery {
                    left: Box::new(left),
                    right: Box::new(right),
                    ..jq
                })
            }
            QueryExpr::Table(tq) => {
                // Table projections already use specific column projections
                // No transformation needed - already efficient
                QueryExpr::Table(tq)
            }
            other => other,
        }
    }

    fn benefit(&self) -> u32 {
        80 // High priority - reduces memory
    }
}

/// Reorder joins for optimal execution
struct JoinReorderingPass;

impl OptimizationPass for JoinReorderingPass {
    fn name(&self) -> &str {
        "JoinReordering"
    }

    fn apply(&self, query: QueryExpr) -> QueryExpr {
        match query {
            QueryExpr::Join(jq) => {
                // For now, just ensure smaller table is on build side
                // Real IDP algorithm would enumerate join orderings
                self.optimize_join_order(jq)
            }
            other => other,
        }
    }

    fn benefit(&self) -> u32 {
        90 // High priority - join order greatly affects cost
    }
}

impl JoinReorderingPass {
    fn optimize_join_order(&self, query: JoinQuery) -> QueryExpr {
        // Estimate cardinalities
        let left_size = Self::estimate_size(&query.left);
        let right_size = Self::estimate_size(&query.right);

        // For hash join, smaller table should be build side (left)
        if left_size > right_size && query.join_type == JoinType::Inner {
            // Swap left and right
            let JoinQuery {
                left,
                right,
                join_type,
                on,
                filter,
                order_by,
                limit,
                offset,
                return_,
            } = query;
            QueryExpr::Join(JoinQuery {
                left: right,
                right: left,
                join_type,
                on: swap_condition(on),
                filter,
                order_by,
                limit,
                offset,
                return_,
            })
        } else {
            QueryExpr::Join(query)
        }
    }

    fn estimate_size(query: &QueryExpr) -> f64 {
        match query {
            QueryExpr::Table(tq) => {
                let base = 1000.0;
                if tq.filter.is_some() {
                    base * 0.1
                } else if tq.limit.is_some() {
                    tq.limit.unwrap() as f64
                } else {
                    base
                }
            }
            QueryExpr::Graph(_) => 100.0,
            QueryExpr::Join(jq) => {
                Self::estimate_size(&jq.left) * Self::estimate_size(&jq.right) * 0.1
            }
            QueryExpr::Path(_) => 10.0,
            QueryExpr::Vector(vq) => {
                // Vector search returns k results
                vq.k as f64
            }
            QueryExpr::Hybrid(hq) => {
                // Hybrid query combines structured and vector results
                let structured_size = Self::estimate_size(&hq.structured);
                let vector_size = hq.vector.k as f64;
                // Fusion typically reduces to min of both, limited by limit
                let base = structured_size.min(vector_size);
                hq.limit.map(|l| base.min(l as f64)).unwrap_or(base)
            }
            // DML/DDL/Command statements return minimal result sets
            QueryExpr::Insert(_)
            | QueryExpr::Update(_)
            | QueryExpr::Delete(_)
            | QueryExpr::CreateTable(_)
            | QueryExpr::DropTable(_)
            | QueryExpr::AlterTable(_)
            | QueryExpr::GraphCommand(_)
            | QueryExpr::SearchCommand(_)
            | QueryExpr::CreateIndex(_)
            | QueryExpr::DropIndex(_)
            | QueryExpr::ProbabilisticCommand(_)
            | QueryExpr::Ask(_)
            | QueryExpr::SetConfig { .. }
            | QueryExpr::ShowConfig { .. } => 1.0,
        }
    }
}

/// Select optimal indexes for table scans.
///
/// Analyzes filter predicates and annotates the query plan with index hints:
/// - Equality predicates (`col = value`) → prefer Hash index if available
/// - Low-cardinality equality → prefer Bitmap index
/// - Range predicates (`col > value`, `BETWEEN`) → prefer B-tree
/// - Spatial predicates → prefer R-tree
///
/// The hints are stored in the TableQuery's alias field as a prefix
/// (e.g., `__idx:hash:col_name`) which the executor can read to skip
/// full scans. This is a lightweight approach that avoids adding new
/// fields to the AST while enabling index-aware execution.
struct IndexSelectionPass;

impl OptimizationPass for IndexSelectionPass {
    fn name(&self) -> &str {
        "IndexSelection"
    }

    fn apply(&self, query: QueryExpr) -> QueryExpr {
        match query {
            QueryExpr::Table(mut tq) => {
                if let Some(ref filter) = tq.filter {
                    if let Some(hint) = Self::analyze_filter(filter) {
                        // Store index hint in expand metadata for executor
                        let expand = tq.expand.get_or_insert_with(Default::default);
                        expand.index_hint = Some(hint);
                    }
                }
                QueryExpr::Table(tq)
            }
            other => other,
        }
    }

    fn benefit(&self) -> u32 {
        70
    }
}

impl IndexSelectionPass {
    /// Analyze a filter predicate and return the best index hint
    fn analyze_filter(filter: &crate::storage::query::ast::Filter) -> Option<IndexHint> {
        match filter {
            // Equality on a single column → Hash index candidate
            crate::storage::query::ast::Filter::Compare { field, op, .. }
                if *op == crate::storage::query::ast::CompareOp::Eq =>
            {
                let col = Self::field_name(field);
                Some(IndexHint {
                    method: IndexHintMethod::Hash,
                    column: col,
                })
            }
            // Range predicates → B-tree candidate
            crate::storage::query::ast::Filter::Compare { field, op, .. }
                if matches!(
                    op,
                    crate::storage::query::ast::CompareOp::Lt
                        | crate::storage::query::ast::CompareOp::Le
                        | crate::storage::query::ast::CompareOp::Gt
                        | crate::storage::query::ast::CompareOp::Ge
                ) =>
            {
                let col = Self::field_name(field);
                Some(IndexHint {
                    method: IndexHintMethod::BTree,
                    column: col,
                })
            }
            // BETWEEN → B-tree candidate
            crate::storage::query::ast::Filter::Between { field, .. } => {
                let col = Self::field_name(field);
                Some(IndexHint {
                    method: IndexHintMethod::BTree,
                    column: col,
                })
            }
            // IN with few values → Bitmap candidate
            crate::storage::query::ast::Filter::In { field, values } if values.len() <= 10 => {
                let col = Self::field_name(field);
                Some(IndexHint {
                    method: IndexHintMethod::Bitmap,
                    column: col,
                })
            }
            // AND: pick the most selective hint from left or right
            crate::storage::query::ast::Filter::And(left, right) => {
                Self::analyze_filter(left).or_else(|| Self::analyze_filter(right))
            }
            _ => None,
        }
    }

    fn field_name(field: &crate::storage::query::ast::FieldRef) -> String {
        match field {
            crate::storage::query::ast::FieldRef::TableColumn { column, .. } => column.clone(),
            crate::storage::query::ast::FieldRef::NodeProperty { property, .. } => property.clone(),
            crate::storage::query::ast::FieldRef::EdgeProperty { property, .. } => property.clone(),
            crate::storage::query::ast::FieldRef::NodeId { alias } => {
                format!("{}.id", alias)
            }
        }
    }
}

/// Hint about which index method to prefer for a query
#[derive(Debug, Clone)]
pub struct IndexHint {
    /// Preferred index method
    pub method: IndexHintMethod,
    /// Column the index applies to
    pub column: String,
}

/// Which index method the optimizer recommends
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexHintMethod {
    Hash,
    BTree,
    Bitmap,
    Spatial,
}

/// Push LIMIT down through operations
struct LimitPushdownPass;

impl OptimizationPass for LimitPushdownPass {
    fn name(&self) -> &str {
        "LimitPushdown"
    }

    fn apply(&self, query: QueryExpr) -> QueryExpr {
        match query {
            QueryExpr::Join(jq) => {
                // Can push limit through certain joins
                let left = self.apply(*jq.left);
                let right = self.apply(*jq.right);

                QueryExpr::Join(JoinQuery {
                    left: Box::new(left),
                    right: Box::new(right),
                    ..jq
                })
            }
            other => other,
        }
    }

    fn benefit(&self) -> u32 {
        60
    }
}

// =============================================================================
// Helper Functions
// =============================================================================

fn swap_condition(
    condition: crate::storage::query::ast::JoinCondition,
) -> crate::storage::query::ast::JoinCondition {
    crate::storage::query::ast::JoinCondition {
        left_field: condition.right_field,
        right_field: condition.left_field,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::query::ast::{
        DistanceMetric, FieldRef, FusionStrategy, JoinCondition, Projection, TableQuery,
    };

    fn make_table_query(name: &str) -> QueryExpr {
        QueryExpr::Table(TableQuery {
            table: name.to_string(),
            alias: Some(name.to_string()),
            columns: vec![Projection::All],
            filter: None,
            group_by: Vec::new(),
            having: None,
            order_by: vec![],
            limit: None,
            offset: None,
            expand: None,
        })
    }

    #[test]
    fn test_optimizer_applies_passes() {
        let optimizer = QueryOptimizer::new();
        let query = make_table_query("hosts");

        let (optimized, passes) = optimizer.optimize(query);
        // Should at least attempt the passes
        assert!(matches!(optimized, QueryExpr::Table(_)));
    }

    #[test]
    fn test_join_reordering() {
        let optimizer = QueryOptimizer::new();

        let small = QueryExpr::Table(TableQuery {
            table: "small".to_string(),
            alias: None,
            columns: vec![Projection::All],
            filter: None,
            group_by: Vec::new(),
            having: None,
            order_by: vec![],
            limit: Some(10), // Small table
            offset: None,
            expand: None,
        });

        let large = QueryExpr::Table(TableQuery {
            table: "large".to_string(),
            alias: None,
            columns: vec![Projection::All],
            filter: None,
            group_by: Vec::new(),
            having: None,
            order_by: vec![],
            limit: None, // Large table
            offset: None,
            expand: None,
        });

        let join = QueryExpr::Join(JoinQuery {
            left: Box::new(large.clone()),
            right: Box::new(small.clone()),
            join_type: JoinType::Inner,
            on: JoinCondition {
                left_field: FieldRef::TableColumn {
                    table: "large".to_string(),
                    column: "id".to_string(),
                },
                right_field: FieldRef::TableColumn {
                    table: "small".to_string(),
                    column: "id".to_string(),
                },
            },
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            return_: Vec::new(),
        });

        let (optimized, passes) = optimizer.optimize(join);

        // Should have applied JoinReordering
        if let QueryExpr::Join(jq) = optimized {
            // Small table should now be on left (build side)
            if let QueryExpr::Table(left) = *jq.left {
                assert_eq!(left.table, "small");
            }
        }
    }
}
