//! Storage-agnostic logical planning + optimization (ADR 0053, #1106).
//!
//! These passes complete the crate's contract — **text in → typed logical
//! plan out** — by owning the half of query planning that consumes nothing
//! from storage: multi-pass AST rewriting ([`rewriter`]) and the pluggable
//! optimization-pass pipeline ([`optimizer`]), plus the two pure planner
//! companions they lean on — projection routing ([`projections`]) and pathkey
//! / sort-order reasoning ([`pathkeys`]).
//!
//! The cut line is the typed logical plan. Anything that consumes storage
//! statistics, index metadata, or executor capabilities — cost estimation,
//! histograms, the stats catalog/provider, partition/hypertable pruning,
//! join-order DP, the plan cache, and the catalog-bound `CanonicalPlanner` —
//! stays in `reddb-server` and consumes this crate's output. Every module here
//! depends only on the canonical AST ([`crate::ast`]), its SQL lowering
//! helpers ([`crate::sql_lowering`]), and the neutral keystone type vocabulary
//! ([`reddb_types`], ADR 0052), so the crate graph stays acyclic — no
//! `reddb-io-rql -> reddb-server` back-edge.

pub mod optimizer;
pub mod pathkeys;
pub mod projections;
pub mod rewriter;

pub use optimizer::{OptimizationPass, QueryOptimizer};
pub use rewriter::{QueryRewriter, RewriteContext, RewriteRule};
