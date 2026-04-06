//! Query Planning Layer
//!
//! Multi-pass query rewriting and optimization inspired by Neo4j's planning architecture.
//!
//! # Components
//!
//! - **rewriter**: Multi-pass query AST transformation
//! - **cache**: LRU cache for compiled query plans
//! - **cost**: Cost-based plan selection with cardinality estimation
//! - **optimizer**: Query optimization strategies

pub mod cache;
pub mod cost;
pub mod optimizer;
pub mod rewriter;

pub use cache::{CachedPlan, PlanCache};
pub use cost::{CardinalityEstimate, CostEstimator, PlanCost};
pub use optimizer::{OptimizationPass, QueryOptimizer};
pub use rewriter::{QueryRewriter, RewriteContext, RewriteRule};

use crate::storage::query::ast::QueryExpr;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::query::ast::{Projection, QueryExpr, TableQuery};

    fn make_simple_query() -> QueryExpr {
        QueryExpr::Table(TableQuery {
            table: "hosts".to_string(),
            alias: None,
            columns: vec![Projection::All],
            filter: None,
            order_by: vec![],
            limit: None,
            offset: None,
        })
    }

    #[test]
    fn test_planner_creates_plan() {
        let mut planner = QueryPlanner::new();
        let query = make_simple_query();
        let plan = planner.plan(query);
        assert!(plan.cost.total > 0.0);
    }

    #[test]
    fn test_planner_caches_plans() {
        let mut planner = QueryPlanner::new();
        let query = make_simple_query();

        // First call - cache miss
        let _ = planner.plan(query.clone());
        assert_eq!(planner.cache_stats().misses, 1);
        assert_eq!(planner.cache_stats().hits, 0);

        // Second call - cache hit
        let _ = planner.plan(query);
        assert_eq!(planner.cache_stats().hits, 1);
    }

    #[test]
    fn test_cache_invalidation() {
        let mut planner = QueryPlanner::new();
        let query = make_simple_query();

        let _ = planner.plan(query.clone());
        assert_eq!(planner.cache_stats().size, 1);

        planner.clear_cache();
        assert_eq!(planner.cache_stats().size, 0);
    }
}
