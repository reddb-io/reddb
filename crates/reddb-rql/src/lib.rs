//! `reddb-io-rql` — the RedDB Query Language (RQL) front-end + conformance
//! authority (ADR 0053).
//!
//! This crate is being stood up in phases. The eventual home of the RQL
//! language front-end (lexer → parser → AST → mode translators → analyzer →
//! typing → optimizer) is here, but in this **S1 tracer-bullet slice** the
//! language front-end still lives in `reddb-server`. What ships first is the
//! half of ADR 0053 that is portable today: ownership of the
//! **correctness specification of execution** — an sqllogictest-format
//! conformance suite whose truth comes from the public SQLite corpus, run
//! end-to-end against the current in-server engine.
//!
//! The crate sits near the bottom of the workspace graph: it depends only on
//! [`reddb_types`] (the neutral keystone, ADR 0052) and on nothing else of the
//! workspace. The conformance harness reaches the engine from the
//! integration-test target alone (a dev-dependency on `reddb-io-server`), so
//! the published crate graph keeps its single edge `reddb-io-rql ->
//! reddb-io-types`.
//!
//! The one piece of conformance machinery that is genuinely storage-agnostic —
//! and therefore lives in the library rather than the test harness — is the
//! rendering of an engine [`reddb_types::Value`] into the textual cell the
//! sqllogictest comparator sees. [`conformance`] owns that rendering.

pub mod conformance;
pub mod lexer;
pub mod limits;

pub use conformance::{render_cell, CellType};
pub use lexer::{Lexer, LexerError, Position, Spanned, Token};
pub use limits::ParserLimits;
