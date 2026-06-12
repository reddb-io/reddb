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

// The parser family (#1103) and the AST cluster (#1113) were re-homed
// byte-faithfully from `reddb-server`, which carries a crate-level
// `#![allow(dead_code, unused_imports, unused_variables)]`. Carrying the same
// blanket here keeps the relocation a pure move — the parser's helper methods,
// keyword-import lists, and parse-loop bindings stay exactly as authored
// (matching the precedent set by `reddb-io-types`, ADR 0052).
#![allow(dead_code, unused_imports, unused_variables)]

pub mod ast;
pub mod conformance;
pub mod filter_optimizer;
pub mod lexer;
pub mod limits;
pub mod parser;
pub mod sql;
pub mod sql_lowering;

pub use conformance::{render_cell, CellType};
pub use lexer::{Lexer, LexerError, Position, Spanned, Token};
pub use limits::ParserLimits;
pub use parser::{parse, ParseError, ParseErrorKind, Parser, SafeTokenDisplay};
pub use sql::{parse_frontend, FrontendStatement, SqlCommand, SqlStatement};
