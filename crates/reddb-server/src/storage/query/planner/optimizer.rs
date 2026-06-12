//! Query optimizer — re-export shim.
//!
//! The pluggable optimization-pass pipeline (`QueryOptimizer`,
//! `OptimizationPass`, and the predicate-pushdown / join-reordering /
//! projection-pushdown passes) is storage-agnostic — it rewrites the canonical
//! AST without consuming storage statistics, index metadata, or executor
//! capabilities — so it moved into `reddb-io-rql` (#1106, ADR 0053). This shim
//! preserves the historical `crate::storage::query::planner::optimizer::*`
//! import path so existing call-sites keep resolving unchanged.

pub use reddb_rql::planner::optimizer::*;
