//! DDL Parser for CREATE INDEX and DROP INDEX

use super::super::ast::{CreateIndexQuery, DropIndexQuery, IndexMethod, QueryExpr};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;

impl<'a> Parser<'a> {
    /// Parse: CREATE [UNIQUE] INDEX [IF NOT EXISTS] name ON table (col1, ...) [USING method]
    ///
    /// Called after `Token::Create` has been consumed and we've peeked `Token::Index`
    /// or `Token::Unique`.
    pub fn parse_create_index_query(&mut self) -> Result<QueryExpr, ParseError> {
        // CREATE has already been consumed by the dispatcher

        let unique = self.consume(&Token::Unique)?;

        self.expect(Token::Index)?;

        let if_not_exists = self.match_if_not_exists()?;

        let name = self.expect_ident()?;

        self.expect(Token::On)?;

        let table = self.expect_ident()?;

        // Parse column list: (col1, col2, ...)
        self.expect(Token::LParen)?;
        let mut columns = Vec::new();
        loop {
            columns.push(self.expect_ident()?);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        self.expect(Token::RParen)?;

        // Parse optional USING method
        let method = if self.consume(&Token::Using)? {
            self.parse_index_method()?
        } else {
            IndexMethod::BTree // default
        };

        Ok(QueryExpr::CreateIndex(CreateIndexQuery {
            name,
            table,
            columns,
            method,
            unique,
            if_not_exists,
        }))
    }

    /// Parse: DROP INDEX [IF EXISTS] name ON table
    ///
    /// Called after `Token::Drop` has been consumed and we've peeked `Token::Index`.
    pub fn parse_drop_index_query(&mut self) -> Result<QueryExpr, ParseError> {
        // DROP has already been consumed by the dispatcher

        self.expect(Token::Index)?;

        let if_exists = self.match_if_exists()?;

        let name = self.expect_ident()?;

        self.expect(Token::On)?;

        let table = self.expect_ident()?;

        Ok(QueryExpr::DropIndex(DropIndexQuery {
            name,
            table,
            if_exists,
        }))
    }

    /// Parse index method identifier: HASH | BTREE | BITMAP | RTREE
    fn parse_index_method(&mut self) -> Result<IndexMethod, ParseError> {
        match self.peek().clone() {
            Token::Ident(ref name) => {
                let method = match name.to_ascii_uppercase().as_str() {
                    "HASH" => IndexMethod::Hash,
                    "BTREE" => IndexMethod::BTree,
                    "BITMAP" => IndexMethod::Bitmap,
                    "RTREE" => IndexMethod::RTree,
                    _ => {
                        return Err(ParseError::new(
                            format!(
                                "unknown index method '{}', expected HASH, BTREE, BITMAP, or RTREE",
                                name
                            ),
                            self.position(),
                        ));
                    }
                };
                self.advance()?;
                Ok(method)
            }
            other => Err(ParseError::expected(
                vec!["HASH", "BTREE", "BITMAP", "RTREE"],
                &other,
                self.position(),
            )),
        }
    }
}
