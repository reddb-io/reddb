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
            Token::Index => self.parse_search_index(),
            Token::Ident(name) if name.eq_ignore_ascii_case("MULTIMODAL") => {
                self.parse_search_multimodal()
            }
            Token::Ident(name) if name.eq_ignore_ascii_case("CONTEXT") => {
                self.parse_search_context()
            }
            Token::Ident(name) if name.eq_ignore_ascii_case("SPATIAL") => {
                self.parse_search_spatial()
            }
            _ => Err(ParseError::expected(
                vec![
                    "SIMILAR",
                    "TEXT",
                    "HYBRID",
                    "MULTIMODAL",
                    "INDEX",
                    "CONTEXT",
                    "SPATIAL",
                ],
                self.peek(),
                self.position(),
            )),
        }
    }

    /// Parse: SEARCH SIMILAR ([v1, v2] | TEXT 'query') COLLECTION col [LIMIT n] [MIN_SCORE f] [USING provider]
    fn parse_search_similar(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume SIMILAR

        // Parse vector literal OR text for semantic search
        let (vector, text) = if self.consume(&Token::Text)? {
            // SEARCH SIMILAR TEXT 'query' — semantic search
            let query_text = self.parse_string()?;
            (Vec::new(), Some(query_text))
        } else {
            // SEARCH SIMILAR [0.1, 0.2] — classic vector search
            (self.parse_vector_literal()?, None)
        };

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

        // Optional USING provider
        let provider = if self.consume_search_ident("USING")? {
            Some(self.expect_ident()?)
        } else {
            None
        };

        Ok(QueryExpr::SearchCommand(SearchCommand::Similar {
            vector,
            text,
            provider,
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

    /// Parse: SEARCH MULTIMODAL 'query' [COLLECTION col] [LIMIT n]
    fn parse_search_multimodal(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume MULTIMODAL identifier

        let query = self.parse_string()?;

        let collection = if self.consume(&Token::Collection)? {
            Some(self.expect_ident()?)
        } else {
            None
        };

        let limit = if self.consume(&Token::Limit)? {
            self.parse_integer()? as usize
        } else {
            25
        };

        Ok(QueryExpr::SearchCommand(SearchCommand::Multimodal {
            query,
            collection,
            limit,
        }))
    }

    /// Parse: SEARCH INDEX index VALUE 'value' [COLLECTION col] [LIMIT n] [EXACT|FUZZY]
    fn parse_search_index(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume INDEX keyword

        let index = self.expect_ident()?;
        self.expect_search_ident("VALUE")?;
        let value = self.parse_string()?;

        let collection = if self.consume(&Token::Collection)? {
            Some(self.expect_ident()?)
        } else {
            None
        };

        let limit = if self.consume(&Token::Limit)? {
            self.parse_integer()? as usize
        } else {
            25
        };

        let fuzzy = self.consume(&Token::Fuzzy)? || self.consume_search_ident("FUZZY")?;
        if !fuzzy {
            let _ = self.consume_search_ident("EXACT")?;
        }
        let exact = !fuzzy;

        Ok(QueryExpr::SearchCommand(SearchCommand::Index {
            index,
            value,
            collection,
            limit,
            exact,
        }))
    }

    fn expect_search_ident(&mut self, expected: &str) -> Result<(), ParseError> {
        if self.consume_search_ident(expected)? {
            Ok(())
        } else {
            Err(ParseError::expected(
                vec![expected],
                self.peek(),
                self.position(),
            ))
        }
    }

    fn consume_search_ident(&mut self, expected: &str) -> Result<bool, ParseError> {
        match self.peek().clone() {
            Token::Ident(name) if name.eq_ignore_ascii_case(expected) => {
                self.advance()?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// Parse: SEARCH CONTEXT 'query' [FIELD field] [COLLECTION col] [DEPTH n] [LIMIT n]
    fn parse_search_context(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume CONTEXT keyword

        let query = self.parse_string()?;

        let field = if self.consume_search_ident("FIELD")? {
            Some(self.expect_ident()?)
        } else {
            None
        };

        let collection = if self.consume(&Token::Collection)? {
            Some(self.expect_ident()?)
        } else {
            None
        };

        // Parse optional clauses in any order
        let mut limit = 25usize;
        let mut depth = 1usize;
        for _ in 0..2 {
            if self.consume(&Token::Limit)? {
                limit = self.parse_integer()? as usize;
            } else if self.consume(&Token::Depth)? {
                depth = self.parse_integer()? as usize;
            }
        }

        Ok(QueryExpr::SearchCommand(SearchCommand::Context {
            query,
            field,
            collection,
            limit,
            depth,
        }))
    }

    /// Parse: SEARCH SPATIAL (RADIUS | BBOX | NEAREST) ...
    ///
    /// Syntax:
    /// - SEARCH SPATIAL RADIUS lat lon radius_km COLLECTION col COLUMN col [LIMIT n]
    /// - SEARCH SPATIAL BBOX min_lat min_lon max_lat max_lon COLLECTION col COLUMN col [LIMIT n]
    /// - SEARCH SPATIAL NEAREST lat lon K n COLLECTION col COLUMN col
    fn parse_search_spatial(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume SPATIAL

        match self.peek().clone() {
            Token::Ident(ref name) if name.eq_ignore_ascii_case("RADIUS") => {
                self.advance()?; // consume RADIUS
                let center_lat = self.parse_float()?;
                let center_lon = self.parse_float()?;
                let radius_km = self.parse_float()?;

                self.expect(Token::Collection)?;
                let collection = self.expect_ident()?;

                let _ = self.consume(&Token::Column)? || self.consume_search_ident("COLUMN")?;
                let column = self.expect_ident()?;

                let limit = if self.consume(&Token::Limit)? {
                    self.parse_integer()? as usize
                } else {
                    100
                };

                Ok(QueryExpr::SearchCommand(SearchCommand::SpatialRadius {
                    center_lat,
                    center_lon,
                    radius_km,
                    collection,
                    column,
                    limit,
                }))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("BBOX") => {
                self.advance()?; // consume BBOX
                let min_lat = self.parse_float()?;
                let min_lon = self.parse_float()?;
                let max_lat = self.parse_float()?;
                let max_lon = self.parse_float()?;

                self.expect(Token::Collection)?;
                let collection = self.expect_ident()?;

                let _ = self.consume(&Token::Column)? || self.consume_search_ident("COLUMN")?;
                let column = self.expect_ident()?;

                let limit = if self.consume(&Token::Limit)? {
                    self.parse_integer()? as usize
                } else {
                    100
                };

                Ok(QueryExpr::SearchCommand(SearchCommand::SpatialBbox {
                    min_lat,
                    min_lon,
                    max_lat,
                    max_lon,
                    collection,
                    column,
                    limit,
                }))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("NEAREST") => {
                self.advance()?; // consume NEAREST
                let lat = self.parse_float()?;
                let lon = self.parse_float()?;

                self.expect(Token::K)?;
                let k = self.parse_integer()? as usize;

                self.expect(Token::Collection)?;
                let collection = self.expect_ident()?;

                let _ = self.consume(&Token::Column)? || self.consume_search_ident("COLUMN")?;
                let column = self.expect_ident()?;

                Ok(QueryExpr::SearchCommand(SearchCommand::SpatialNearest {
                    lat,
                    lon,
                    k,
                    collection,
                    column,
                }))
            }
            _ => Err(ParseError::expected(
                vec!["RADIUS", "BBOX", "NEAREST"],
                self.peek(),
                self.position(),
            )),
        }
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
