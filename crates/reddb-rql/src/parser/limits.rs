//! Parser DoS limits.
//!
//! These limits are uniformly applied at parser entry points so a
//! malicious query string can't exhaust recursion stack, RAM, or
//! identifier bookkeeping. Limit values are documented in
//! `docs/security/parser-limits.md` (issue #87).
//!
//! # Defaults
//!
//! | Limit                 | Default | Rationale                                       |
//! |-----------------------|---------|-------------------------------------------------|
//! | `max_depth`           | 16      | Recursive descent + Pratt; above typical        |
//! |                       |         | hand-written queries (≤ 12).                     |
//! | `max_input_bytes`     | 1 MiB   | Hard cap on the token stream input.              |
//! | `max_identifier_chars`| 256     | Long enough for legitimate UUID-tagged names,    |
//! |                       |         | short enough to bound HashMap pressure.          |
//! | `max_tokens`          | 8192    | Bounds token-driven parser work even when input  |
//! |                       |         | bytes and recursion depth stay below their caps. |
//!
//! Callers that need different limits (replication apply, admin DDL
//! migrations) construct a custom [`ParserLimits`] and pass it to
//! [`Parser::with_limits`](super::Parser::with_limits).

/// Hard limits enforced by the front-end — re-export.
///
/// `ParserLimits` lives at the crate root ([`crate::limits`]), re-homed there
/// with the lexer that consumes its identifier and input-byte caps (#1102,
/// ADR 0053). This re-export preserves the historical `parser::ParserLimits` /
/// `parser::limits::ParserLimits` paths so every call-site (and the
/// `tests/support/parser_hardening` harness) keeps resolving unchanged. The
/// recursion-depth machinery below is parser-internal and moves with the
/// parser into this crate (#1103).
pub use crate::limits::ParserLimits;

/// Internal recursion-depth tracker. RAII-style: a guard
/// [`DepthGuard`] increments on construction and decrements on
/// drop, so early returns/`?` propagation can't leak depth.
#[derive(Debug)]
pub(crate) struct DepthCounter {
    pub(crate) depth: usize,
    pub(crate) max_depth: usize,
}

impl DepthCounter {
    pub(crate) fn new(max_depth: usize) -> Self {
        Self {
            depth: 0,
            max_depth,
        }
    }
}
