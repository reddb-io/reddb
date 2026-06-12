//! Filter ⇄ Expr lowering — re-export shim.
//!
//! The lowering helpers (`filter_to_expr`, `expr_to_filter`,
//! `projection_to_*`, the `effective_*` accessors, …) moved into the
//! `reddb-io-rql` crate alongside the canonical SQL AST they operate on
//! (#1113, ADR 0053): they depend only on the AST plus `reddb-io-types`, so
//! they belong in the same dependency-closed cluster. This shim preserves the
//! historical `crate::storage::query::sql_lowering::*` import path so every
//! parser, planner, and runtime call-site keeps resolving unchanged.

pub use reddb_rql::sql_lowering::*;
