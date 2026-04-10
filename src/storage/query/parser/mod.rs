//! RQL Parser
//!
//! Parses RQL (RedDB Query Language) tokens into a unified AST.
//! Supports SQL-like table queries, Cypher-like graph patterns, and joins.
//!
//! # Supported Syntax
//!
//! ## Table Queries
//! ```text
//! SELECT [columns] FROM table [WHERE condition] [ORDER BY ...] [LIMIT n]
//! ```
//!
//! ## Graph Queries
//! ```text
//! MATCH pattern [WHERE condition] RETURN projection
//! ```
//!
//! ## Join Queries
//! ```text
//! FROM table [alias] JOIN GRAPH pattern ON condition [RETURN projection]
//! ```
//!
//! ## Path Queries
//! ```text
//! PATH FROM selector TO selector [VIA edge_types] [RETURN projection]
//! ```
//!
//! ## Vector Queries
//! ```text
//! VECTOR SEARCH collection SIMILAR TO [...] [WHERE ...] [METRIC ...] LIMIT k
//! ```
//!
//! ## Hybrid Queries
//! ```text
//! HYBRID (structured query) VECTOR SEARCH ... FUSION strategy LIMIT n
//! ```

mod cte;
mod ddl;
mod dml;
mod error;
mod filter;
mod graph;
mod graph_commands;
mod helpers;
mod hybrid;
mod index_ddl;
mod join;
mod path;
mod probabilistic_commands;
mod search_commands;
mod table;
mod vector;

#[cfg(test)]
mod tests;

pub use error::ParseError;

use super::ast::QueryExpr;
use super::lexer::{Lexer, Position, Spanned, Token};
use crate::storage::schema::Value;

/// RQL Parser
pub struct Parser<'a> {
    lexer: Lexer<'a>,
    /// Current token
    pub(crate) current: Spanned,
}

impl<'a> Parser<'a> {
    /// Create a new parser
    pub fn new(input: &'a str) -> Result<Self, ParseError> {
        let mut lexer = Lexer::new(input);
        let current = lexer.next_token()?;
        Ok(Self { lexer, current })
    }

    /// Get current position
    pub fn position(&self) -> Position {
        self.current.start
    }

    /// Advance to next token
    pub fn advance(&mut self) -> Result<Token, ParseError> {
        let old = std::mem::replace(&mut self.current, self.lexer.next_token()?);
        Ok(old.token)
    }

    /// Peek at current token
    pub fn peek(&self) -> &Token {
        &self.current.token
    }

    /// Check if current token matches
    pub fn check(&self, expected: &Token) -> bool {
        std::mem::discriminant(&self.current.token) == std::mem::discriminant(expected)
    }

    /// Check if current token is a specific keyword
    pub fn check_keyword(&self, keyword: &Token) -> bool {
        self.check(keyword)
    }

    /// Consume a specific token or error
    pub fn expect(&mut self, expected: Token) -> Result<Token, ParseError> {
        if self.check(&expected) {
            self.advance()
        } else {
            Err(ParseError::expected(
                vec![&expected.to_string()],
                &self.current.token,
                self.position(),
            ))
        }
    }

    /// Consume an identifier and return its value
    pub fn expect_ident(&mut self) -> Result<String, ParseError> {
        match &self.current.token {
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
    pub fn expect_ident_or_keyword(&mut self) -> Result<String, ParseError> {
        // Get the string representation of the current token
        let name = match &self.current.token {
            Token::Ident(name) => name.clone(),
            // Keywords that can be type names (convert to uppercase for type matching)
            Token::Contains => "CONTAINS".to_string(),
            Token::Left => "LEFT".to_string(),
            Token::Right => "RIGHT".to_string(),
            Token::First => "FIRST".to_string(),
            Token::Last => "LAST".to_string(),
            Token::In => "IN".to_string(),
            Token::By => "BY".to_string(),
            // Any other keyword - use its display string
            other => other.to_string(),
        };

        // Only advance for valid type-name-like tokens
        match &self.current.token {
            // Identifiers are always valid
            Token::Ident(_) => {
                self.advance()?;
                Ok(name)
            }
            // These keywords can be type names
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
            // Reject structural tokens that can't be type names
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
                &self.current.token,
                self.position(),
            )),
            // All other keywords can potentially be type names
            _ => {
                self.advance()?;
                Ok(name)
            }
        }
    }

    /// Try to consume a token, returning true if consumed
    pub fn consume(&mut self, expected: &Token) -> Result<bool, ParseError> {
        if self.check(expected) {
            self.advance()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    /// Try to consume an identifier case-insensitively (for keywords not in Token enum)
    pub fn consume_ident_ci(&mut self, expected: &str) -> Result<bool, ParseError> {
        match self.peek().clone() {
            Token::Ident(name) if name.eq_ignore_ascii_case(expected) => {
                self.advance()?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Parse a complete query
    pub fn parse(&mut self) -> Result<QueryExpr, ParseError> {
        let query = self.parse_query_expr()?;

        // Expect end of input
        if !self.check(&Token::Eof) {
            return Err(ParseError::new(
                format!("Unexpected token after query: {}", self.current.token),
                self.position(),
            ));
        }

        Ok(query)
    }

    /// Parse the main query expression (without CTEs)
    pub fn parse_query_expr(&mut self) -> Result<QueryExpr, ParseError> {
        match self.peek() {
            Token::Select => self.parse_select_query(),
            Token::Match => self.parse_match_query(),
            Token::Path => self.parse_path_query(),
            Token::From => self.parse_from_query(),
            Token::Vector => self.parse_vector_query(),
            Token::Hybrid => self.parse_hybrid_query(),
            Token::Insert => self.parse_insert_query(),
            Token::Update => self.parse_update_query(),
            Token::Delete => self.parse_delete_query(),
            Token::Create => {
                let pos = self.position();
                self.advance()?; // consume CREATE
                if self.check(&Token::Index) || self.check(&Token::Unique) {
                    self.parse_create_index_query()
                } else if self.check(&Token::Table) {
                    self.expect(Token::Table)?;
                    self.parse_create_table_body()
                } else {
                    Err(ParseError::expected(
                        vec!["TABLE", "INDEX", "UNIQUE"],
                        self.peek(),
                        pos,
                    ))
                }
            }
            Token::Drop => {
                let pos = self.position();
                self.advance()?; // consume DROP
                if self.check(&Token::Index) {
                    self.parse_drop_index_query()
                } else if self.check(&Token::Table) {
                    self.expect(Token::Table)?;
                    self.parse_drop_table_body()
                } else {
                    Err(ParseError::expected(
                        vec!["TABLE", "INDEX"],
                        self.peek(),
                        pos,
                    ))
                }
            }
            Token::Alter => self.parse_alter_table_query(),
            Token::Graph => self.parse_graph_command(),
            Token::Search => self.parse_search_command(),
            Token::Ident(ref name) if name.eq_ignore_ascii_case("ASK") => self.parse_ask_query(),
            other => Err(ParseError::expected(
                vec![
                    "SELECT", "MATCH", "PATH", "FROM", "VECTOR", "HYBRID", "INSERT", "UPDATE",
                    "DELETE", "CREATE", "DROP", "ALTER", "GRAPH", "SEARCH", "ASK",
                ],
                other,
                self.position(),
            )),
        }
    }

    /// Parse an integer literal
    pub fn parse_integer(&mut self) -> Result<i64, ParseError> {
        match &self.current.token {
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
    pub fn parse_float(&mut self) -> Result<f64, ParseError> {
        match &self.current.token {
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
    pub fn parse_string(&mut self) -> Result<String, ParseError> {
        match &self.current.token {
            Token::String(s) => {
                let s = s.clone();
                self.advance()?;
                Ok(s)
            }
            other => Err(ParseError::expected(vec!["string"], other, self.position())),
        }
    }

    /// Parse a value
    pub fn parse_value(&mut self) -> Result<Value, ParseError> {
        match &self.current.token {
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
    pub fn parse_value_list(&mut self) -> Result<Vec<Value>, ParseError> {
        let mut values = Vec::new();
        loop {
            values.push(self.parse_value()?);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        Ok(values)
    }
}

/// Parse an RQL query string
pub fn parse(input: &str) -> Result<QueryExpr, ParseError> {
    let mut parser = Parser::new(input)?;
    parser.parse()
}
