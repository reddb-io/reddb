//! Expression typer — re-export shim.
//!
//! The Fase 3 expression typer — the pass that walks an `ast::Expr` tree and
//! assigns a concrete `DataType` to every node (`type_expr`, `TypedExpr`,
//! `TypedExprKind`, `TypeError`, `Scope`) — was re-homed into the
//! `reddb-io-rql` language front-end crate (#1105, ADR 0053). It consumes the
//! canonical AST that already lives there (#1113) and types it through the cast
//! and function catalogs in `reddb-io-types`, so the crate graph stays acyclic
//! — no `reddb-io-rql -> reddb-server` back-edge.
//!
//! This shim preserves the historical `crate::storage::query::expr_typing::*`
//! import path so every call-site across the workspace keeps resolving
//! unchanged. A byte-faithful move with zero call-site edits.

pub use reddb_rql::expr_typing::*;
