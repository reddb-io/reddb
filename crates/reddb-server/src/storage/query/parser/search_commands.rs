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

        // Parse vector literal OR text for semantic search OR $N placeholder
        let mut vector_param: Option<usize> = None;
        let (vector, text) = if self.consume(&Token::Text)? {
            // SEARCH SIMILAR TEXT 'query' — semantic search
            let query_text = self.parse_string()?;
            (Vec::new(), Some(query_text))
        } else if matches!(self.peek(), Token::Dollar) {
            // SEARCH SIMILAR $N — parameterized vector
            if self.placeholder_mode == super::PlaceholderMode::Question {
                return Err(ParseError::new(
                    "cannot mix `?` and `$N` placeholders in one statement".to_string(),
                    self.position(),
                ));
            }
            self.advance()?;
            let idx = match *self.peek() {
                Token::Integer(n) if n >= 1 => {
                    self.advance()?;
                    (n - 1) as usize
                }
                _ => {
                    return Err(ParseError::new(
                        "expected `$N` (N >= 1) for SEARCH SIMILAR vector parameter".to_string(),
                        self.position(),
                    ));
                }
            };
            self.placeholder_mode = super::PlaceholderMode::Dollar;
            vector_param = Some(idx);
            (Vec::new(), None)
        } else {
            // SEARCH SIMILAR [0.1, 0.2] — classic vector search
            (self.parse_vector_literal()?, None)
        };

        // Parse COLLECTION
        self.expect(Token::Collection)?;
        let collection = self.expect_ident()?;

        // Optional LIMIT — accepts an integer literal or `$N` placeholder (#361).
        let mut limit_param: Option<usize> = None;
        let limit = if self.consume(&Token::Limit)? {
            if matches!(self.peek(), Token::Dollar | Token::Question) {
                limit_param = Some(self.parse_param_slot("LIMIT")?);
                0
            } else {
                self.parse_integer()? as usize
            }
        } else {
            10
        };

        // Optional MIN_SCORE — accepts a float literal or `$N` placeholder (#361).
        let mut min_score_param: Option<usize> = None;
        let min_score = if self.consume(&Token::MinScore)? {
            if matches!(self.peek(), Token::Dollar | Token::Question) {
                min_score_param = Some(self.parse_param_slot("MIN_SCORE")?);
                0.0
            } else {
                self.parse_float()? as f32
            }
        } else {
            0.0
        };

        // Optional USING provider. `USING` is a reserved keyword
        // (`Token::Using`), so `consume_search_ident` (which only
        // matches `Token::Ident`) would never fire. Use the typed
        // consumer. See bug #108.
        let provider = if self.consume(&Token::Using)? {
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
            vector_param,
            limit_param,
            min_score_param,
        }))
    }

    /// Parse: SEARCH TEXT 'query string' [COLLECTION|IN col] [LIMIT n] [FUZZY]
    fn parse_search_text(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume TEXT

        let query = self.parse_string()?;

        // Optional COLLECTION
        let collection = if self.consume(&Token::Collection)? || self.consume(&Token::In)? {
            Some(self.expect_ident()?)
        } else {
            None
        };

        // Optional LIMIT — accepts an integer literal or `$N` placeholder (#361).
        let mut limit_param: Option<usize> = None;
        let limit = if self.consume(&Token::Limit)? {
            if matches!(self.peek(), Token::Dollar | Token::Question) {
                limit_param = Some(self.parse_param_slot("LIMIT")?);
                0
            } else {
                self.parse_integer()? as usize
            }
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
            limit_param,
        }))
    }

    /// Parse: SEARCH HYBRID [SIMILAR|VECTOR [v1, v2, ...]] [TEXT 'query'] COLLECTION|IN col [LIMIT|K n]
    fn parse_search_hybrid(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume HYBRID

        let mut vector = None;
        let mut query = None;

        loop {
            if self.consume(&Token::Similar)? || self.consume(&Token::Vector)? {
                vector = Some(self.parse_vector_literal()?);
            } else if self.consume(&Token::Text)? {
                query = Some(self.parse_string()?);
            } else {
                break;
            }
        }

        // Require at least one of vector or text
        if vector.is_none() && query.is_none() {
            return Err(ParseError::new(
                "SEARCH HYBRID requires at least SIMILAR or TEXT".to_string(),
                self.position(),
            ));
        }

        // Parse COLLECTION/IN — tolerate collection names that collide
        // with reserved keywords (e.g. `data`, `text`, `nodes`) by
        // falling back to `expect_ident_or_keyword` and lowercasing the
        // keyword form so the stored name matches the source casing.
        if !(self.consume(&Token::Collection)? || self.consume(&Token::In)?) {
            return Err(ParseError::expected(
                vec!["COLLECTION", "IN"],
                self.peek(),
                self.position(),
            ));
        }
        let collection = self.expect_collection_name()?;

        // Optional LIMIT / K — accepts an integer literal or `$N` placeholder (#361).
        let mut limit_param: Option<usize> = None;
        let limit = if self.consume(&Token::Limit)? || self.consume(&Token::K)? {
            let _ = self.consume(&Token::Eq)?;
            if matches!(self.peek(), Token::Dollar | Token::Question) {
                limit_param = Some(self.parse_param_slot("LIMIT")?);
                0
            } else {
                self.parse_integer()? as usize
            }
        } else {
            10
        };

        Ok(QueryExpr::SearchCommand(SearchCommand::Hybrid {
            vector,
            query,
            collection,
            limit,
            limit_param,
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

        // Optional LIMIT — accepts an integer literal or `$N` placeholder (#361).
        let mut limit_param: Option<usize> = None;
        let limit = if self.consume(&Token::Limit)? {
            if matches!(self.peek(), Token::Dollar | Token::Question) {
                limit_param = Some(self.parse_param_slot("LIMIT")?);
                0
            } else {
                self.parse_integer()? as usize
            }
        } else {
            25
        };

        Ok(QueryExpr::SearchCommand(SearchCommand::Multimodal {
            query,
            collection,
            limit,
            limit_param,
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

    /// Collection/index names frequently collide with reserved words
    /// (`data`, `text`, `nodes`, `edges`, …). Accept either a plain
    /// identifier or a keyword, lowercasing the keyword form so the
    /// stored name matches the source spelling.
    fn expect_collection_name(&mut self) -> Result<String, ParseError> {
        let was_ident = matches!(self.peek(), Token::Ident(_));
        let raw = self.expect_ident_or_keyword()?;
        Ok(if was_ident {
            raw
        } else {
            raw.to_ascii_lowercase()
        })
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
                let lat_pos = self.position();
                let center_lat = self.parse_float()?;
                if !(-90.0..=90.0).contains(&center_lat) {
                    return Err(ParseError::value_out_of_range(
                        "lat",
                        "must be in -90.0..=90.0",
                        lat_pos,
                    ));
                }
                let lon_pos = self.position();
                let center_lon = self.parse_float()?;
                if !(-180.0..=180.0).contains(&center_lon) {
                    return Err(ParseError::value_out_of_range(
                        "lon",
                        "must be in -180.0..=180.0",
                        lon_pos,
                    ));
                }
                let r_pos = self.position();
                let radius_km = self.parse_float()?;
                if radius_km.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
                    return Err(ParseError::value_out_of_range(
                        "radius",
                        "must be a positive number",
                        r_pos,
                    ));
                }

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
                let p = self.position();
                let min_lat = self.parse_float()?;
                if !(-90.0..=90.0).contains(&min_lat) {
                    return Err(ParseError::value_out_of_range(
                        "lat",
                        "must be in -90.0..=90.0",
                        p,
                    ));
                }
                let p = self.position();
                let min_lon = self.parse_float()?;
                if !(-180.0..=180.0).contains(&min_lon) {
                    return Err(ParseError::value_out_of_range(
                        "lon",
                        "must be in -180.0..=180.0",
                        p,
                    ));
                }
                let p = self.position();
                let max_lat = self.parse_float()?;
                if !(-90.0..=90.0).contains(&max_lat) {
                    return Err(ParseError::value_out_of_range(
                        "lat",
                        "must be in -90.0..=90.0",
                        p,
                    ));
                }
                let p = self.position();
                let max_lon = self.parse_float()?;
                if !(-180.0..=180.0).contains(&max_lon) {
                    return Err(ParseError::value_out_of_range(
                        "lon",
                        "must be in -180.0..=180.0",
                        p,
                    ));
                }

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
                let lat_pos = self.position();
                let lat = self.parse_float()?;
                if !(-90.0..=90.0).contains(&lat) {
                    return Err(ParseError::value_out_of_range(
                        "lat",
                        "must be in -90.0..=90.0",
                        lat_pos,
                    ));
                }
                let lon_pos = self.position();
                let lon = self.parse_float()?;
                if !(-180.0..=180.0).contains(&lon) {
                    return Err(ParseError::value_out_of_range(
                        "lon",
                        "must be in -180.0..=180.0",
                        lon_pos,
                    ));
                }

                self.expect(Token::K)?;
                // K accepts a positive integer literal OR `$N` placeholder (#361).
                let mut k_param: Option<usize> = None;
                let k = if matches!(self.peek(), Token::Dollar | Token::Question) {
                    k_param = Some(self.parse_param_slot("K")?);
                    0
                } else {
                    self.parse_positive_integer("K")? as usize
                };

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
                    k_param,
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
