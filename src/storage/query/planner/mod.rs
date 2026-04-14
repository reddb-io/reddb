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
pub mod histogram;
pub mod optimizer;
pub mod rewriter;
pub mod stats_provider;

mod logical;
mod types;

pub use cache::{CachedPlan, PlanCache};
pub use cost::{CardinalityEstimate, ColumnStats, CostEstimator, PlanCost, TableStats};
pub use histogram::{Bucket, ColumnValue, Histogram, MostCommonValues};
pub use optimizer::{OptimizationPass, QueryOptimizer};
pub use rewriter::{QueryRewriter, RewriteContext, RewriteRule};
pub use stats_provider::{NullProvider, RegistryProvider, StaticProvider, StatsProvider};
pub use types::{
    build_canonical_plan, AccessPathDecision, CacheStats, CanonicalLogicalNode,
    CanonicalLogicalPlan, CanonicalPlanner, QueryPlan, QueryPlanner,
};
