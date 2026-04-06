//! Helper methods for parsing primitives

use super::super::lexer::Token;
use super::error::ParseError;
use crate::storage::schema::Value;

/// Trait for parsers that can parse primitive values
pub trait PrimitiveParser {
    fn peek(&self) -> &Token;
    fn advance(&mut self) -> Result<Token, ParseError>;
    fn position(&self) -> super::super::lexer::Position;
    fn current_token(&self) -> &Token;
    fn expect(&mut self, expected: Token) -> Result<Token, ParseError>;
    fn consume(&mut self, expected: &Token) -> Result<bool, ParseError>;
    fn check(&self, expected: &Token) -> bool;

    /// Parse an integer literal
    fn parse_integer(&mut self) -> Result<i64, ParseError> {
        match self.current_token() {
            Token::Integer(n) => {
                let n = *n;
                self.advance()?;
                Ok(n)
            }
            other => Err(ParseError::expected(
                vec!["integer"],
                other,
                self.position(),
            )),
        }
    }

    /// Parse float literal
    fn parse_float(&mut self) -> Result<f64, ParseError> {
        match self.current_token() {
            Token::Float(n) => {
                let n = *n;
                self.advance()?;
                Ok(n)
            }
            Token::Integer(n) => {
                let n = *n as f64;
                self.advance()?;
                Ok(n)
            }
            other => Err(ParseError::expected(vec!["number"], other, self.position())),
        }
    }

    /// Parse a string literal
    fn parse_string(&mut self) -> Result<String, ParseError> {
        match self.current_token() {
            Token::String(s) => {
                let s = s.clone();
                self.advance()?;
                Ok(s)
            }
            other => Err(ParseError::expected(vec!["string"], other, self.position())),
        }
    }

    /// Parse a value
    fn parse_value(&mut self) -> Result<Value, ParseError> {
        match self.current_token() {
            Token::String(s) => {
                let s = s.clone();
                self.advance()?;
                Ok(Value::Text(s))
            }
            Token::Integer(n) => {
                let n = *n;
                self.advance()?;
                Ok(Value::Integer(n))
            }
            Token::Float(n) => {
                let n = *n;
                self.advance()?;
                Ok(Value::Float(n))
            }
            Token::True => {
                self.advance()?;
                Ok(Value::Boolean(true))
            }
            Token::False => {
                self.advance()?;
                Ok(Value::Boolean(false))
            }
            Token::Null => {
                self.advance()?;
                Ok(Value::Null)
            }
            other => Err(ParseError::expected(
                vec!["string", "number", "true", "false", "null"],
                other,
                self.position(),
            )),
        }
    }

    /// Parse value list for IN clause
    fn parse_value_list(&mut self) -> Result<Vec<Value>, ParseError> {
        let mut values = Vec::new();
        loop {
            values.push(self.parse_value()?);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        Ok(values)
    }

    /// Consume an identifier and return its value
    fn expect_ident(&mut self) -> Result<String, ParseError> {
        match self.current_token() {
            Token::Ident(name) => {
                let name = name.clone();
                self.advance()?;
                Ok(name)
            }
            other => Err(ParseError::expected(
                vec!["identifier"],
                other,
                self.position(),
            )),
        }
    }

    /// Consume an identifier or keyword (for type names where keywords are valid)
    fn expect_ident_or_keyword(&mut self) -> Result<String, ParseError> {
        let name = match self.current_token() {
            Token::Ident(name) => name.clone(),
            Token::Contains => "CONTAINS".to_string(),
            Token::Left => "LEFT".to_string(),
            Token::Right => "RIGHT".to_string(),
            Token::First => "FIRST".to_string(),
            Token::Last => "LAST".to_string(),
            Token::In => "IN".to_string(),
            Token::By => "BY".to_string(),
            other => other.to_string(),
        };

        match self.current_token() {
            Token::Ident(_) => {
                self.advance()?;
                Ok(name)
            }
            Token::Contains
            | Token::Left
            | Token::Right
            | Token::First
            | Token::Last
            | Token::In
            | Token::By => {
                self.advance()?;
                Ok(name)
            }
            Token::Eof
            | Token::LParen
            | Token::RParen
            | Token::LBracket
            | Token::RBracket
            | Token::Comma
            | Token::Dot
            | Token::Eq
            | Token::Lt
            | Token::Gt
            | Token::Le
            | Token::Ge
            | Token::Arrow
            | Token::ArrowLeft
            | Token::Dash
            | Token::Colon
            | Token::Semi
            | Token::Star
            | Token::Plus
            | Token::Slash => Err(ParseError::expected(
                vec!["identifier or type name"],
                self.current_token(),
                self.position(),
            )),
            _ => {
                self.advance()?;
                Ok(name)
            }
        }
    }
}
