//! Query rewriter — re-export shim.
//!
//! The multi-pass `QueryExpr` rewriter (`QueryRewriter`, `RewriteRule`,
//! `RewriteContext`, and the normalize / cached-property / filter-simplify /
//! predicate-pushdown passes) is storage-agnostic — it transforms the
//! canonical AST and consumes no storage statistics, index metadata, or
//! executor capabilities — so it moved into the `reddb-io-rql` language
//! front-end crate alongside the AST it rewrites (#1106, ADR 0053). This shim
//! preserves the historical `crate::storage::query::planner::rewriter::*`
//! import path so existing call-sites keep resolving unchanged.

pub use reddb_rql::planner::rewriter::*;
