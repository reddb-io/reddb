//! Parser error types

use std::fmt;

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
    pub fn expected(expected: Vec<&str>, found: &Token, position: Position) -> Self {
        Self {
            message: format!("Unexpected token: {}", found),
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
