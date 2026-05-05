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
//! | `max_depth`           | 128     | Recursive descent + Pratt; well above hand-     |
//! |                       |         | written queries (typical ≤ 12).                  |
//! | `max_input_bytes`     | 1 MiB   | Hard cap on the token stream input.              |
//! | `max_identifier_chars`| 256     | Long enough for legitimate UUID-tagged names,    |
//! |                       |         | short enough to bound HashMap pressure.          |
//!
//! Callers that need different limits (replication apply, admin DDL
//! migrations) construct a custom [`ParserLimits`] and pass it to
//! [`Parser::with_limits`](super::Parser::with_limits).

/// Hard limits enforced by the parser.
///
/// The fields are public so the harness module (used by tests in
/// `tests/support/parser_hardening`) can mutate them inline. Default
/// values match production defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParserLimits {
    /// Maximum recursion depth across recursive descent points
    /// (expressions, parenthesised sub-queries, JOIN chains).
    pub max_depth: usize,
    /// Maximum input length in bytes. Checked at the lexer entry
    /// before tokenization begins.
    pub max_input_bytes: usize,
    /// Maximum identifier length in characters. Checked when an
    /// identifier token is constructed in the lexer.
    pub max_identifier_chars: usize,
}

impl Default for ParserLimits {
    fn default() -> Self {
        Self {
            max_depth: 128,
            max_input_bytes: 1024 * 1024, // 1 MiB
            max_identifier_chars: 256,
        }
    }
}

impl ParserLimits {
    /// Permissive limits for tests that intentionally probe deep
    /// nesting or long inputs without tripping DoS guards.
    pub fn permissive() -> Self {
        Self {
            max_depth: 1024,
            max_input_bytes: 16 * 1024 * 1024,
            max_identifier_chars: 4096,
        }
    }
}

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
