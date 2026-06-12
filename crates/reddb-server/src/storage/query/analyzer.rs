//! DDL analyzer — re-export shim.
//!
//! The CREATE TABLE analyzer — the pass that validates declared columns and
//! resolves each declared `SqlTypeName` into a concrete storage `DataType`
//! (`analyze_create_table`, `resolve_declared_data_type`, `resolve_sql_type_name`,
//! and the `Analyzed*` result types) — was re-homed into the `reddb-io-rql`
//! language front-end crate (#1105, ADR 0053). It consumes the canonical AST
//! that already lives there (#1113) and produces its typed representation using
//! `reddb-io-types` for the logical type vocabulary, so the crate graph stays
//! acyclic — no `reddb-io-rql -> reddb-server` back-edge.
//!
//! This shim preserves the historical `crate::storage::query::analyzer::*`
//! import path — and the `crate::storage::query::{analyze_create_table, …}`
//! re-exports in the parent module — so every runtime and application
//! call-site across the workspace keeps resolving unchanged. A byte-faithful
//! move with zero call-site edits.

pub use reddb_rql::analyzer::*;
