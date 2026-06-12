//! RQL Parser — re-export shim.
//!
//! The whole parser family — the monolithic [`Parser`] struct and every
//! SQL/DML/DDL/expression/filter/join/CTE/graph/vector submodule, plus the
//! embedded parser unit tests — was re-homed into the `reddb-io-rql` language
//! front-end crate (#1103, ADR 0053). It now consumes the lexer and canonical
//! AST that already live in that crate (#1102, #1113) and reaches the in-house
//! JSON stack + `parse_duration_ns` through `reddb-io-types` (#1118), so the
//! crate graph stays acyclic — no `reddb-io-rql -> reddb-server` back-edge.
//!
//! This shim preserves the historical `crate::storage::query::parser::*`
//! import path so every runtime, mode-detector, and `sql.rs` call-site across
//! the workspace keeps resolving unchanged — a byte-faithful move with zero
//! call-site edits.

pub use reddb_rql::parser::*;
