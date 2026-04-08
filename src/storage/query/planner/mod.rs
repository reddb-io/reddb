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

mod logical;
mod types;

pub use cache::{CachedPlan, PlanCache};
pub use cost::{CardinalityEstimate, CostEstimator, PlanCost};
pub use optimizer::{OptimizationPass, QueryOptimizer};
pub use rewriter::{QueryRewriter, RewriteContext, RewriteRule};
pub use types::{
    build_canonical_plan, CacheStats, CanonicalLogicalNode, CanonicalLogicalPlan,
    CanonicalPlanner, QueryPlan, QueryPlanner,
};
