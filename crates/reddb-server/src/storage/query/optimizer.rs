//! Query optimizer (filter ranking, decorrelation, stats collection) —
//! re-export shim.
//!
//! Filter ranking (`FilterRanker`, `RankedFilter`, `RankingConfig`), subquery
//! decorrelation (`Decorrelator`, `SubqueryAnalysis`, …), and the in-memory
//! stats-collection model (`StatsCollector`, `ColumnStats`, `TableStats`) are
//! storage-agnostic — self-contained ranking/cost models that consume no
//! storage statistics, index metadata, or executor capabilities — so they
//! moved into the `reddb-io-rql` language front-end crate (#1106, ADR 0053).
//! This shim preserves the historical `crate::storage::query::optimizer::*`
//! import path (and the `crate::storage::query::optimizer::{stats, …}`
//! submodule paths) so existing call-sites keep resolving unchanged.

pub use reddb_rql::optimizer::*;
pub use reddb_rql::optimizer::{decorrelate, filter_rank, stats};
