//! Search Command Parser: SEARCH SIMILAR | TEXT | HYBRID

use super::super::ast::{QueryExpr, SearchCommand};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;
use crate::storage::schema::Value;

impl<'a> Parser<'a> {
    /// Parse: SEARCH subcommand ...
    pub fn parse_search_command(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::Search)?;
        match self.peek().clone() {
            Token::Similar => self.parse_search_similar(),
            Token::Text => self.parse_search_text(),
            Token::Hybrid => self.parse_search_hybrid(),
            _ => Err(ParseError::expected(
                vec!["SIMILAR", "TEXT", "HYBRID"],
                self.peek(),
                self.position(),
            )),
        }
    }

    /// Parse: SEARCH SIMILAR [0.1, 0.2, 0.3] COLLECTION col [LIMIT n] [MIN_SCORE f]
    fn parse_search_similar(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume SIMILAR

        // Parse vector literal
        let vector = self.parse_vector_literal()?;

        // Parse COLLECTION
        self.expect(Token::Collection)?;
        let collection = self.expect_ident()?;

        // Optional LIMIT
        let limit = if self.consume(&Token::Limit)? {
            self.parse_integer()? as usize
        } else {
            10
        };

        // Optional MIN_SCORE
        let min_score = if self.consume(&Token::MinScore)? {
            self.parse_float()? as f32
        } else {
            0.0
        };

        Ok(QueryExpr::SearchCommand(SearchCommand::Similar {
            vector,
            collection,
            limit,
            min_score,
        }))
    }

    /// Parse: SEARCH TEXT 'query string' [COLLECTION col] [LIMIT n] [FUZZY]
    fn parse_search_text(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume TEXT

        let query = self.parse_string()?;

        // Optional COLLECTION
        let collection = if self.consume(&Token::Collection)? {
            Some(self.expect_ident()?)
        } else {
            None
        };

        // Optional LIMIT
        let limit = if self.consume(&Token::Limit)? {
            self.parse_integer()? as usize
        } else {
            10
        };

        // Optional FUZZY
        let fuzzy = self.consume(&Token::Fuzzy)?;

        Ok(QueryExpr::SearchCommand(SearchCommand::Text {
            query,
            collection,
            limit,
            fuzzy,
        }))
    }

    /// Parse: SEARCH HYBRID [SIMILAR [v1, v2, ...]] [TEXT 'query'] COLLECTION col [LIMIT n]
    fn parse_search_hybrid(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume HYBRID

        // Optional SIMILAR vector
        let vector = if self.consume(&Token::Similar)? {
            Some(self.parse_vector_literal()?)
        } else {
            None
        };

        // Optional TEXT query
        let query = if self.consume(&Token::Text)? {
            Some(self.parse_string()?)
        } else {
            None
        };

        // Require at least one of vector or text
        if vector.is_none() && query.is_none() {
            return Err(ParseError::new(
                "SEARCH HYBRID requires at least SIMILAR or TEXT".to_string(),
                self.position(),
            ));
        }

        // Parse COLLECTION
        self.expect(Token::Collection)?;
        let collection = self.expect_ident()?;

        // Optional LIMIT
        let limit = if self.consume(&Token::Limit)? {
            self.parse_integer()? as usize
        } else {
            10
        };

        Ok(QueryExpr::SearchCommand(SearchCommand::Hybrid {
            vector,
            query,
            collection,
            limit,
        }))
    }

    /// Parse a vector literal: [0.1, 0.2, 0.3]
    fn parse_vector_literal(&mut self) -> Result<Vec<f32>, ParseError> {
        self.expect(Token::LBracket)?;
        let mut items = Vec::new();
        if !self.check(&Token::RBracket) {
            loop {
                let val = self.parse_float()? as f32;
                items.push(val);
                if !self.consume(&Token::Comma)? {
                    break;
                }
            }
        }
        self.expect(Token::RBracket)?;
        Ok(items)
    }
}
