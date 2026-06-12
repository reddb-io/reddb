//! RQL Lexer — re-export shim.
//!
//! The lexer was re-homed into the `reddb-io-rql` language front-end crate
//! (#1102, ADR 0053). This shim preserves the historical
//! `crate::storage::query::lexer::*` import path so every parser, planner,
//! and runtime call-site across the workspace keeps resolving unchanged —
//! a byte-faithful move with zero call-site edits. The lexer's embedded
//! unit tests moved verbatim alongside it and run in the crate.

pub use reddb_rql::lexer::*;
