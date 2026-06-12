//! Unified Query AST — re-export shim.
//!
//! The canonical SQL AST — `QueryExpr` and the whole query-type family
//! (`core.rs`), the query builders (`builders.rs`), and the Phase-2 `Expr`
//! tree (`Expr`/`Span`/`UnaryOp`/`ExprSubquery`) — was re-homed into the
//! `reddb-io-rql` language front-end crate (#1113, ADR 0053). Its lowering
//! (`sql_lowering`) and filter optimizer (`filter_optimizer`) moved with it
//! as the dependency-closed cluster; all of it depends only on
//! `reddb-io-types`, so the crate graph stays acyclic.
//!
//! This shim preserves the historical `crate::storage::query::ast::*` import
//! path so every parser, planner, evaluator, and runtime call-site across the
//! workspace keeps resolving unchanged — a byte-faithful move with zero
//! call-site edits. The embedded AST unit tests moved verbatim alongside the
//! types and run in the crate.

pub use reddb_rql::ast::*;
