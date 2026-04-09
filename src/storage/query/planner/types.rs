use std::collections::BTreeMap;

use crate::storage::query::ast::QueryExpr;
use crate::storage::RedDB;

use super::logical::logical_plan_node_with_catalog;
use super::{CachedPlan, CostEstimator, PlanCache, PlanCost, QueryOptimizer, QueryRewriter};

/// Query plan ready for execution
#[derive(Debug, Clone)]
pub struct QueryPlan {
    /// Original query expression
    pub original: QueryExpr,
    /// Optimized query expression
    pub optimized: QueryExpr,
    /// Estimated cost
    pub cost: PlanCost,
    /// Optimization passes applied
    pub passes_applied: Vec<String>,
}

impl QueryPlan {
    /// Create a new query plan
    pub fn new(original: QueryExpr, optimized: QueryExpr, cost: PlanCost) -> Self {
        Self {
            original,
            optimized,
            cost,
            passes_applied: Vec::new(),
        }
    }

    /// Add an optimization pass record
    pub fn add_pass(&mut self, pass_name: &str) {
        self.passes_applied.push(pass_name.to_string());
    }
}

/// Canonical logical plan used for explain/introspection during the planner transition.
#[derive(Debug, Clone, Default)]
pub struct CanonicalLogicalPlan {
    pub root: CanonicalLogicalNode,
}

/// Canonical logical operator node.
#[derive(Debug, Clone, Default)]
pub struct CanonicalLogicalNode {
    pub operator: String,
    pub source: Option<String>,
    pub details: BTreeMap<String, String>,
    pub estimated_rows: f64,
    pub estimated_selectivity: f64,
    pub estimated_confidence: f64,
    pub operator_cost: f64,
    pub children: Vec<CanonicalLogicalNode>,
}

#[derive(Debug, Clone)]
pub struct AccessPathDecision {
    pub path: &'static str,
    pub index_hint: Option<String>,
    pub reason: String,
    pub warning: Option<String>,
}

/// Query planner that combines rewriting, caching, and cost estimation
pub struct QueryPlanner {
    /// Query rewriter for AST transformations
    rewriter: QueryRewriter,
    /// Plan cache for avoiding recompilation
    cache: PlanCache,
    /// Cost estimator for plan selection
    cost_estimator: CostEstimator,
    /// Query optimizer
    optimizer: QueryOptimizer,
}

impl QueryPlanner {
    /// Create a new query planner
    pub fn new() -> Self {
        Self {
            rewriter: QueryRewriter::new(),
            cache: PlanCache::new(1000), // 1000 entry cache
            cost_estimator: CostEstimator::new(),
            optimizer: QueryOptimizer::new(),
        }
    }

    /// Create with custom cache size
    pub fn with_cache_size(cache_size: usize) -> Self {
        Self {
            rewriter: QueryRewriter::new(),
            cache: PlanCache::new(cache_size),
            cost_estimator: CostEstimator::new(),
            optimizer: QueryOptimizer::new(),
        }
    }

    /// Plan a query - returns cached plan if available
    pub fn plan(&mut self, query: QueryExpr) -> QueryPlan {
        // Check cache first
        let cache_key = self.compute_cache_key(&query);
        if let Some(cached) = self.cache.get(&cache_key) {
            return cached.plan.clone();
        }

        // Rewrite the query
        let rewritten = self.rewriter.rewrite(query.clone());

        // Optimize the query
        let (optimized, passes) = self.optimizer.optimize(rewritten);

        // Estimate cost
        let cost = self.cost_estimator.estimate(&optimized);

        // Create plan
        let mut plan = QueryPlan::new(query, optimized, cost);
        for pass in passes {
            plan.add_pass(&pass);
        }

        // Cache the plan
        self.cache.insert(cache_key, CachedPlan::new(plan.clone()));

        plan
    }

    /// Invalidate cache entries matching a predicate
    pub fn invalidate_cache<F>(&mut self, predicate: F)
    where
        F: Fn(&str) -> bool,
    {
        self.cache.invalidate(predicate);
    }

    /// Clear the entire cache
    pub fn clear_cache(&mut self) {
        self.cache.clear();
    }

    /// Get cache statistics
    pub fn cache_stats(&self) -> CacheStats {
        self.cache.stats()
    }

    /// Compute cache key for a query
    fn compute_cache_key(&self, query: &QueryExpr) -> String {
        // Use debug representation as cache key
        // In production, use a hash function
        format!("{:?}", query)
    }
}

impl Default for QueryPlanner {
    fn default() -> Self {
        Self::new()
    }
}

/// Builder for canonical logical plans used by explain/introspection.
pub struct CanonicalPlanner<'a> {
    db: &'a RedDB,
}

impl<'a> CanonicalPlanner<'a> {
    /// Create a canonical planner bound to a database handle.
    pub fn new(db: &'a RedDB) -> Self {
        Self { db }
    }

    /// Build a canonical logical plan from an optimized query expression.
    pub fn build(&self, expr: &QueryExpr) -> CanonicalLogicalPlan {
        CanonicalLogicalPlan {
            root: logical_plan_node_with_catalog(self.db, expr),
        }
    }
}

/// Build a canonical logical plan for explain/introspection.
pub fn build_canonical_plan(db: &RedDB, expr: &QueryExpr) -> CanonicalLogicalPlan {
    CanonicalPlanner::new(db).build(expr)
}

/// Cache statistics
#[derive(Debug, Clone, Default)]
pub struct CacheStats {
    /// Number of cache hits
    pub hits: u64,
    /// Number of cache misses
    pub misses: u64,
    /// Current cache size
    pub size: usize,
    /// Maximum cache size
    pub capacity: usize,
}

impl CacheStats {
    /// Hit rate as percentage
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            (self.hits as f64 / total as f64) * 100.0
        }
    }
}
