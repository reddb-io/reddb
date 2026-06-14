//! Front-end DoS limits.
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
//! | `max_depth`           | 32      | Recursive descent + Pratt; above typical        |
//! |                       |         | hand-written queries (≤ 12).                     |
//! | `max_input_bytes`     | 1 MiB   | Hard cap on the token stream input.              |
//! | `max_identifier_chars`| 256     | Long enough for legitimate UUID-tagged names,    |
//! |                       |         | short enough to bound HashMap pressure.          |
//! | `max_tokens`          | 8192    | Bounds token-driven parser work even when input  |
//! |                       |         | bytes and recursion depth stay below their caps. |
//!
//! `ParserLimits` is consumed both by the [`crate::lexer`] (identifier and
//! input-byte caps, checked during tokenization) and by the parser proper
//! (recursion-depth cap), which still lives in `reddb-server` and reaches
//! this type through its re-export shim.

/// Hard limits enforced by the front-end.
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
    /// Maximum number of tokens the parser may consume. This bounds
    /// flat adversarial inputs such as long operator chains that do
    /// not trip byte, identifier, or recursion-depth limits.
    pub max_tokens: usize,
}

impl Default for ParserLimits {
    fn default() -> Self {
        Self {
            max_depth: 32,
            max_input_bytes: 1024 * 1024, // 1 MiB
            max_identifier_chars: 256,
            max_tokens: 8192,
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
            max_tokens: 65_536,
        }
    }
}
