//! Parser error types

use std::fmt::{self, Write};

use super::super::lexer::{LexerError, LexerLimitHit, Position, Token};

/// Parse error
#[derive(Debug, Clone)]
pub struct ParseError {
    /// Error message
    pub message: String,
    /// Position where error occurred
    pub position: Position,
    /// Expected tokens (for better error messages)
    pub expected: Vec<String>,
    /// Optional structured kind for hardening / DoS errors
    pub kind: ParseErrorKind,
}

/// Categorical kind for a parse error.
///
/// Most parse errors are plain `Syntax` failures; the variants
/// below carry structured information for the parser-hardening
/// layer (issue #87) so callers can distinguish DoS-style refusals
/// from grammar errors without string matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseErrorKind {
    /// Generic syntax / semantic error.
    Syntax,
    /// Recursion-depth limit exceeded during parsing.
    DepthLimit {
        limit_name: &'static str,
        value: usize,
    },
    /// Input larger than the configured byte cap.
    InputTooLarge {
        limit_name: &'static str,
        value: usize,
    },
    /// Identifier longer than the configured character cap.
    IdentifierTooLong {
        limit_name: &'static str,
        value: usize,
    },
    /// A literal value (integer / float) parsed cleanly but lies
    /// outside the semantic range expected for its slot — e.g.
    /// `MAX_SIZE 0`, `lat = 91.0`, `K = 0`, or a negative integer
    /// where a positive one is required. The structured payload lets
    /// the snapshot/property harness distinguish these from generic
    /// syntax errors without string matching.
    ValueOutOfRange {
        /// Stable slot name, e.g. `"MAX_SIZE"`, `"lat"`, `"radius"`.
        field: &'static str,
        /// Free-text constraint, e.g. `"must be > 0"`,
        /// `"must be in -90.0..=90.0"`.
        constraint: &'static str,
    },
}

impl ParseError {
    /// Create a new parse error
    pub fn new(message: impl Into<String>, position: Position) -> Self {
        Self {
            message: message.into(),
            position,
            expected: Vec::new(),
            kind: ParseErrorKind::Syntax,
        }
    }

    /// Create error with expected tokens
    ///
    /// `found` is rendered through [`SafeTokenDisplay`] so caller-controlled
    /// bytes inside `Token::Ident` / `Token::String` / `Token::JsonLiteral` /
    /// `Token::Float` / `Token::Integer` payloads are escaped via Rust's
    /// `escape_debug` rules (CR / LF / NUL / quote bytes become `\n`,
    /// `\r`, `\0`, `\"`, …). Static keyword and punctuation arms keep their
    /// existing UPPER-CASE rendering so error messages and snapshot tests
    /// stay readable. This prevents F-05 smuggling through the downstream
    /// JSON / audit / log / gRPC sinks that embed `ParseError::message`.
    pub fn expected(expected: Vec<&str>, found: &Token, position: Position) -> Self {
        Self {
            message: format!("Unexpected token: {}", SafeTokenDisplay(found)),
            position,
            expected: expected.into_iter().map(|s| s.to_string()).collect(),
            kind: ParseErrorKind::Syntax,
        }
    }

    /// Recursion depth limit hit. The structured `kind` carries the
    /// name + numeric value so the snapshot/property harness can
    /// pattern-match without string slicing.
    pub fn depth_limit(limit_name: &'static str, value: usize, position: Position) -> Self {
        Self {
            message: format!(
                "recursion depth limit exceeded ({} = {})",
                limit_name, value
            ),
            position,
            expected: Vec::new(),
            kind: ParseErrorKind::DepthLimit { limit_name, value },
        }
    }

    /// Input bytes exceeded the configured cap.
    pub fn input_too_large(limit_name: &'static str, value: usize, position: Position) -> Self {
        Self {
            message: format!(
                "input exceeds maximum size ({} = {} bytes)",
                limit_name, value
            ),
            position,
            expected: Vec::new(),
            kind: ParseErrorKind::InputTooLarge { limit_name, value },
        }
    }

    /// Identifier exceeded the configured character cap.
    pub fn identifier_too_long(limit_name: &'static str, value: usize, position: Position) -> Self {
        Self {
            message: format!(
                "identifier exceeds maximum length ({} = {} chars)",
                limit_name, value
            ),
            position,
            expected: Vec::new(),
            kind: ParseErrorKind::IdentifierTooLong { limit_name, value },
        }
    }

    /// A literal value lies outside the allowed range for its slot.
    /// The free-text `constraint` is included verbatim in the message
    /// so callers can render a single line without re-formatting.
    pub fn value_out_of_range(
        field: &'static str,
        constraint: &'static str,
        position: Position,
    ) -> Self {
        Self {
            message: format!("{} {}", field, constraint),
            position,
            expected: Vec::new(),
            kind: ParseErrorKind::ValueOutOfRange { field, constraint },
        }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Parse error at {}: {}", self.position, self.message)?;
        if !self.expected.is_empty() {
            write!(f, " (expected: {})", self.expected.join(", "))?;
        }
        Ok(())
    }
}

impl std::error::Error for ParseError {}

/// `Display` adapter that emits a `Token` while escaping the
/// caller-controlled byte payload of `Ident` / `String` / `JsonLiteral` /
/// `Integer` / `Float` arms.
///
/// F-05 (serialization-boundary audit, 2026-05-06): SQL parser error
/// messages flow into JSON HTTP bodies, JSONL audit rows, gRPC
/// `Status::message`, PG3 `ErrorResponse`, and `tracing::warn!` log
/// lines. The default `Token` Display arms emit raw user bytes for
/// `Token::Ident("foo\nbar")` etc., which lets a tenant smuggle CR /
/// LF / NUL / quote bytes through every downstream sink at once.
///
/// This adapter renders user-controlled arms via `escape_debug` (the
/// same rules `{:?}` applies to a `&str`) and leaves keyword /
/// punctuation arms untouched so existing snapshot tests and operator
/// log readability are preserved.
pub struct SafeTokenDisplay<'a>(pub &'a Token);

impl fmt::Display for SafeTokenDisplay<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            // User-controlled byte payloads. Render via `escape_debug`
            // so embedded CR / LF / NUL / quote bytes do not reach
            // downstream serialization sinks unescaped.
            Token::Ident(s) => write_escaped(f, s),
            Token::String(s) => {
                f.write_str("'")?;
                write_escaped(f, s)?;
                f.write_str("'")
            }
            Token::JsonLiteral(s) => write_escaped(f, s),
            // Numeric tokens come straight from the lexer; their
            // canonical Display form is bounded ASCII, but the lexer
            // builds them via `to_string` so they cannot carry control
            // bytes. Pass through Display.
            Token::Integer(_) | Token::Float(_) => fmt::Display::fmt(self.0, f),
            // Static keyword / punctuation arms — fall back to the
            // existing Display output verbatim.
            other => fmt::Display::fmt(other, f),
        }
    }
}

fn write_escaped(f: &mut fmt::Formatter<'_>, s: &str) -> fmt::Result {
    for ch in s.chars() {
        // `escape_debug` matches Rust's Debug rules: ASCII control
        // bytes become `\n`, `\r`, `\0`, `\t`, …; non-ASCII printable
        // characters pass through; backslash and double-quote are
        // escaped.
        for esc in ch.escape_debug() {
            f.write_char(esc)?;
        }
    }
    Ok(())
}

impl From<LexerError> for ParseError {
    fn from(e: LexerError) -> Self {
        let kind = match &e.limit_hit {
            Some(LexerLimitHit::IdentifierTooLong { limit_name, value }) => {
                ParseErrorKind::IdentifierTooLong {
                    limit_name,
                    value: *value,
                }
            }
            None => ParseErrorKind::Syntax,
        };
        ParseError {
            message: e.message,
            position: e.position,
            expected: Vec::new(),
            kind,
        }
    }
}
