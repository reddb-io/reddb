//! ClickHouse-style projection routing — re-export shim.
//!
//! The projection metadata + matcher (`ProjectionSpec`, `pick_projection`, …)
//! models only query shape over the canonical AST — no storage statistics,
//! index metadata, or executor capabilities — so it moved into `reddb-io-rql`
//! (#1106, ADR 0053). This shim preserves the historical
//! `crate::storage::query::planner::projections::*` import path so existing
//! call-sites keep resolving unchanged.

pub use reddb_rql::planner::projections::*;
