//! Pathkey / sort-order reasoning — re-export shim.
//!
//! The pathkey model (`PathKey`, `PathKeys`, `plan_sort`, `SortStrategy`, …)
//! reasons about output-order guarantees purely over the canonical AST's
//! `FieldRef`s — no storage statistics, index metadata, or executor
//! capabilities — so it moved into `reddb-io-rql` (#1106, ADR 0053). This shim
//! preserves the historical `crate::storage::query::planner::pathkeys::*`
//! import path so existing call-sites keep resolving unchanged.

pub use reddb_rql::planner::pathkeys::*;
