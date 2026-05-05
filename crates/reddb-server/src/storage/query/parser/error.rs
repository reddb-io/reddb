//! Parser error types

use std::fmt;

use super::super::lexer::{LexerError, Position, Token};

/// Parser error
#[derive(Debug, Clone)]
pub struct ParseError {
    /// Error message
    pub message: String,
    /// Position where error occurred
    pub position: Position,
    /// Expected tokens (for better error messages)
    pub expected: Vec<String>,
}

impl ParseError {
    /// Create a new parse error
    pub fn new(message: impl Into<String>, position: Position) -> Self {
        Self {
            message: message.into(),
            position,
            expected: Vec::new(),
        }
    }

    /// Create error with expected tokens
    pub fn expected(expected: Vec<&str>, found: &Token, position: Position) -> Self {
        Self {
            message: format!("Unexpected token: {}", found),
            position,
            expected: expected.into_iter().map(|s| s.to_string()).collect(),
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
        ParseError::new(e.message, e.position)
    }
}
