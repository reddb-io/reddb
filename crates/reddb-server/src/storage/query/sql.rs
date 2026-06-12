//! Canonical SQL frontend — re-export shim.
//!
//! The SQL frontend command surface (`SqlStatement` / `FrontendStatement` /
//! `SqlCommand` and their lowering to `QueryExpr`) and the bulk of the parser
//! dispatch (`Parser::parse_frontend_statement`, `parse_sql_statement`,
//! `parse_sql_command`, …) moved with the parser family into the
//! `reddb-io-rql` front-end crate (#1103, ADR 0053). It consumes the lexer +
//! canonical AST already living there (#1102, #1113) and depends only on
//! `reddb-io-types`, so the crate graph stays acyclic.
//!
//! This shim preserves the historical `crate::storage::query::sql::*` import
//! path so the `storage::query` re-exports and any call-site keep resolving
//! unchanged — a byte-faithful move with zero call-site edits.

pub use reddb_rql::sql::*;
