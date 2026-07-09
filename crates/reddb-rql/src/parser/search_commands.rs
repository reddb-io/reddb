//! Search Command Parser: SEARCH SIMILAR | TEXT | HYBRID

use super::error::ParseError;
use super::Parser;
use crate::ast::{QueryExpr, SearchCommand};
use crate::lexer::Token;
use reddb_types::types::Value;

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

        // Parse vector literal OR text for semantic search OR positional placeholder.
        let mut vector_param: Option<usize> = None;
        let mut text_param: Option<usize> = None;
        let (vector, text) = if self.consume(&Token::Text)? {
            // SEARCH SIMILAR TEXT ('query' | $N) — semantic search
            if matches!(self.peek(), Token::Dollar | Token::Question) {
                text_param = Some(self.parse_param_slot("SEARCH SIMILAR TEXT")?);
                (Vec::new(), None)
            } else {
                let query_text = self.parse_string()?;
                (Vec::new(), Some(query_text))
            }
        } else if matches!(self.peek(), Token::Dollar | Token::Question) {
            // SEARCH SIMILAR $N / ? / ?N — parameterized vector
            vector_param = Some(self.parse_param_slot("SEARCH SIMILAR vector")?);
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
            text_param,
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
            limit_param,
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
        let mut limit_param: Option<usize> = None;
        for _ in 0..2 {
            if self.consume(&Token::Limit)? {
                if matches!(self.peek(), Token::Dollar | Token::Question) {
                    limit_param = Some(self.parse_param_slot("LIMIT")?);
                    limit = 0;
                } else {
                    limit = self.parse_integer()? as usize;
                }
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
            limit_param,
        }))
    }

    /// Parse: SEARCH SPATIAL (RADIUS | BBOX | WITHIN POLYGON | NEAREST) ...
    ///
    /// Syntax:
    /// - SEARCH SPATIAL RADIUS lat lon radius_km COLLECTION col COLUMN col [LIMIT n]
    /// - SEARCH SPATIAL BBOX min_lat min_lon max_lat max_lon COLLECTION col COLUMN col [LIMIT n]
    /// - SEARCH SPATIAL WITHIN POLYGON ((lat lon), ...) COLLECTION col COLUMN col [LIMIT n]
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
                let column = self.parse_search_spatial_column()?;

                let mut limit_param: Option<usize> = None;
                let limit = if self.consume(&Token::Limit)? {
                    if matches!(self.peek(), Token::Dollar | Token::Question) {
                        limit_param = Some(self.parse_param_slot("LIMIT")?);
                        0
                    } else {
                        self.parse_integer()? as usize
                    }
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
                    limit_param,
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
                let column = self.parse_search_spatial_column()?;

                let mut limit_param: Option<usize> = None;
                let limit = if self.consume(&Token::Limit)? {
                    if matches!(self.peek(), Token::Dollar | Token::Question) {
                        limit_param = Some(self.parse_param_slot("LIMIT")?);
                        0
                    } else {
                        self.parse_integer()? as usize
                    }
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
                    limit_param,
                }))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("WITHIN") => {
                self.advance()?; // consume WITHIN
                self.expect_search_ident("POLYGON")?;
                let vertices = self.parse_search_spatial_polygon_vertices()?;
                validate_search_spatial_polygon(&vertices, self.position())?;

                self.expect(Token::Collection)?;
                let collection = self.expect_ident()?;

                let _ = self.consume(&Token::Column)? || self.consume_search_ident("COLUMN")?;
                let column = self.parse_search_spatial_column()?;

                let mut limit_param: Option<usize> = None;
                let limit = if self.consume(&Token::Limit)? {
                    if matches!(self.peek(), Token::Dollar | Token::Question) {
                        limit_param = Some(self.parse_param_slot("LIMIT")?);
                        0
                    } else {
                        self.parse_integer()? as usize
                    }
                } else {
                    100
                };

                Ok(QueryExpr::SearchCommand(
                    SearchCommand::SpatialWithinPolygon {
                        vertices,
                        collection,
                        column,
                        limit,
                        limit_param,
                    },
                ))
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
                let column = self.parse_search_spatial_column()?;

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
                vec!["RADIUS", "BBOX", "WITHIN", "NEAREST"],
                self.peek(),
                self.position(),
            )),
        }
    }

    fn parse_search_spatial_polygon_vertices(&mut self) -> Result<Vec<(f64, f64)>, ParseError> {
        self.expect(Token::LParen)?;
        let mut vertices = Vec::new();
        loop {
            self.expect(Token::LParen)?;
            let lat = self.parse_float()?;
            let lon = self.parse_float()?;
            self.expect(Token::RParen)?;
            vertices.push((lat, lon));
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        self.expect(Token::RParen)?;
        Ok(vertices)
    }

    fn parse_search_spatial_column(&mut self) -> Result<String, ParseError> {
        let mut segments = vec![self.expect_spatial_column_segment()?];
        while self.consume(&Token::Dot)? {
            segments.push(self.expect_spatial_column_segment()?);
        }
        Ok(segments.join("."))
    }

    /// One dotted-path segment of a `SEARCH SPATIAL ... COLUMN` argument.
    ///
    /// Column names are user data, not grammar: a document field named
    /// `current` lexes (case-insensitively) into the window-frame keyword
    /// token, whose display form would silently rewrite the column to
    /// `CURRENT` and break the case-sensitive body-field lookup. Recover
    /// the typed spelling by slicing the token's source span; only
    /// word-shaped tokens qualify as segments.
    fn expect_spatial_column_segment(&mut self) -> Result<String, ParseError> {
        if let Token::Ident(name) = &self.current.token {
            let name = name.clone();
            self.advance()?;
            return Ok(name);
        }
        let start = self.current.start.offset as usize;
        let end = self.current.end.offset as usize;
        let raw = self.lexer.source().get(start..end).unwrap_or("");
        let word_shaped = !raw.is_empty()
            && raw.chars().all(|c| c.is_alphanumeric() || c == '_')
            && !raw.chars().next().is_some_and(|c| c.is_ascii_digit());
        if word_shaped {
            let name = raw.to_string();
            self.advance()?;
            return Ok(name);
        }
        Err(ParseError::new(
            format!("expected column name, found {}", self.current.token),
            self.position(),
        ))
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

pub(super) fn validate_search_spatial_polygon(
    vertices: &[(f64, f64)],
    pos: crate::lexer::Position,
) -> Result<(), ParseError> {
    if vertices.len() < 3 {
        return Err(ParseError::new(
            "SEARCH SPATIAL WITHIN POLYGON requires at least 3 vertices".to_string(),
            pos,
        ));
    }
    for (lat, lon) in vertices {
        if !lat.is_finite() || !(-90.0..=90.0).contains(lat) {
            return Err(ParseError::value_out_of_range(
                "lat",
                "must be in -90.0..=90.0",
                pos,
            ));
        }
        if !lon.is_finite() || !(-180.0..=180.0).contains(lon) {
            return Err(ParseError::value_out_of_range(
                "lon",
                "must be in -180.0..=180.0",
                pos,
            ));
        }
    }
    let (min_lon, max_lon) = vertices.iter().map(|(_, lon)| *lon).fold(
        (f64::INFINITY, f64::NEG_INFINITY),
        |(min_lon, max_lon), lon| (min_lon.min(lon), max_lon.max(lon)),
    );
    if max_lon - min_lon > 180.0 {
        return Err(ParseError::new(
            "SEARCH SPATIAL WITHIN POLYGON does not support polygons crossing the antimeridian"
                .to_string(),
            pos,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_query(input: &str) -> QueryExpr {
        crate::parser::parse(input).unwrap().query
    }

    fn assert_parse_err(input: &str) {
        assert!(crate::parser::parse(input).is_err(), "{input}");
    }

    #[test]
    fn parses_search_similar_text_vector_and_limit_parameters() {
        let query = parse_query(
            "SEARCH SIMILAR TEXT 'semantic query' COLLECTION docs LIMIT 7 MIN_SCORE 0.42 USING openai",
        );
        let QueryExpr::SearchCommand(SearchCommand::Similar {
            vector,
            text,
            provider,
            collection,
            limit,
            min_score,
            vector_param,
            limit_param,
            min_score_param,
            text_param,
        }) = query
        else {
            panic!("Expected SearchCommand::Similar");
        };
        assert!(vector.is_empty());
        assert_eq!(text, Some("semantic query".to_string()));
        assert_eq!(provider, Some("openai".to_string()));
        assert_eq!(collection, "docs");
        assert_eq!(limit, 7);
        assert!((min_score - 0.42).abs() < 0.01);
        assert_eq!(vector_param, None);
        assert_eq!(limit_param, None);
        assert_eq!(min_score_param, None);
        assert_eq!(text_param, None);

        let query = parse_query("SEARCH SIMILAR TEXT $1 COLLECTION docs LIMIT $2 MIN_SCORE $3");
        let QueryExpr::SearchCommand(SearchCommand::Similar {
            vector,
            text,
            limit,
            min_score,
            vector_param,
            limit_param,
            min_score_param,
            text_param,
            ..
        }) = query
        else {
            panic!("Expected parameterized SearchCommand::Similar");
        };
        assert!(vector.is_empty());
        assert_eq!(text, None);
        assert_eq!(limit, 0);
        assert!((min_score).abs() < 0.01);
        assert_eq!(vector_param, None);
        assert_eq!(limit_param, Some(1));
        assert_eq!(min_score_param, Some(2));
        assert_eq!(text_param, Some(0));

        let query = parse_query("SEARCH SIMILAR $1 COLLECTION embeddings");
        let QueryExpr::SearchCommand(SearchCommand::Similar {
            vector,
            vector_param,
            limit,
            min_score,
            ..
        }) = query
        else {
            panic!("Expected vector parameter SearchCommand::Similar");
        };
        assert!(vector.is_empty());
        assert_eq!(vector_param, Some(0));
        assert_eq!(limit, 10);
        assert!((min_score).abs() < 0.01);
    }

    #[test]
    fn parses_search_text_in_collection_with_question_limit() {
        let query = parse_query("SEARCH TEXT 'needle' IN docs LIMIT ? FUZZY");
        let QueryExpr::SearchCommand(SearchCommand::Text {
            query,
            collection,
            limit,
            fuzzy,
            limit_param,
        }) = query
        else {
            panic!("Expected SearchCommand::Text");
        };
        assert_eq!(query, "needle");
        assert_eq!(collection, Some("docs".to_string()));
        assert_eq!(limit, 0);
        assert!(fuzzy);
        assert_eq!(limit_param, Some(0));
    }

    #[test]
    fn parses_search_hybrid_vector_keyword_k_equals_and_keyword_collection() {
        let query = parse_query("SEARCH HYBRID VECTOR [1, 2] TEXT 'needle' IN TEXT K = $1");
        let QueryExpr::SearchCommand(SearchCommand::Hybrid {
            vector,
            query,
            collection,
            limit,
            limit_param,
        }) = query
        else {
            panic!("Expected SearchCommand::Hybrid");
        };
        assert_eq!(vector, Some(vec![1.0, 2.0]));
        assert_eq!(query, Some("needle".to_string()));
        assert_eq!(collection, "text");
        assert_eq!(limit, 0);
        assert_eq!(limit_param, Some(0));
    }

    #[test]
    fn parses_multimodal_and_index_parameterized_limits() {
        let query = parse_query("SEARCH MULTIMODAL 'image query' COLLECTION assets LIMIT $1");
        let QueryExpr::SearchCommand(SearchCommand::Multimodal {
            query,
            collection,
            limit,
            limit_param,
        }) = query
        else {
            panic!("Expected SearchCommand::Multimodal");
        };
        assert_eq!(query, "image query");
        assert_eq!(collection, Some("assets".to_string()));
        assert_eq!(limit, 0);
        assert_eq!(limit_param, Some(0));

        let query = parse_query(
            "SEARCH INDEX email VALUE 'a@example.test' COLLECTION users LIMIT $1 EXACT",
        );
        let QueryExpr::SearchCommand(SearchCommand::Index {
            index,
            value,
            collection,
            limit,
            exact,
            limit_param,
        }) = query
        else {
            panic!("Expected SearchCommand::Index");
        };
        assert_eq!(index, "email");
        assert_eq!(value, "a@example.test");
        assert_eq!(collection, Some("users".to_string()));
        assert_eq!(limit, 0);
        assert!(exact);
        assert_eq!(limit_param, Some(0));
    }

    #[test]
    fn parses_search_context_depth_before_parameterized_limit() {
        let query =
            parse_query("SEARCH CONTEXT 'who' FIELD subject COLLECTION docs DEPTH 3 LIMIT $1");
        let QueryExpr::SearchCommand(SearchCommand::Context {
            query,
            field,
            collection,
            limit,
            depth,
            limit_param,
        }) = query
        else {
            panic!("Expected SearchCommand::Context");
        };
        assert_eq!(query, "who");
        assert_eq!(field, Some("subject".to_string()));
        assert_eq!(collection, Some("docs".to_string()));
        assert_eq!(limit, 0);
        assert_eq!(depth, 3);
        assert_eq!(limit_param, Some(0));
    }

    #[test]
    fn parses_search_spatial_bbox_and_nearest_parameters() {
        let query =
            parse_query("SEARCH SPATIAL BBOX -10 -20 10 20 COLLECTION sites COLUMN geog LIMIT $1");
        let QueryExpr::SearchCommand(SearchCommand::SpatialBbox {
            min_lat,
            min_lon,
            max_lat,
            max_lon,
            collection,
            column,
            limit,
            limit_param,
        }) = query
        else {
            panic!("Expected SearchCommand::SpatialBbox");
        };
        assert!((min_lat + 10.0).abs() < 0.001);
        assert!((min_lon + 20.0).abs() < 0.001);
        assert!((max_lat - 10.0).abs() < 0.001);
        assert!((max_lon - 20.0).abs() < 0.001);
        assert_eq!(collection, "sites");
        assert_eq!(column, "geog");
        assert_eq!(limit, 0);
        assert_eq!(limit_param, Some(0));

        let query =
            parse_query("SEARCH SPATIAL NEAREST 48.85 2.35 K ?2 COLLECTION sites COLUMN geog");
        let QueryExpr::SearchCommand(SearchCommand::SpatialNearest {
            lat,
            lon,
            k,
            collection,
            column,
            k_param,
        }) = query
        else {
            panic!("Expected SearchCommand::SpatialNearest");
        };
        assert!((lat - 48.85).abs() < 0.001);
        assert!((lon - 2.35).abs() < 0.001);
        assert_eq!(k, 0);
        assert_eq!(collection, "sites");
        assert_eq!(column, "geog");
        assert_eq!(k_param, Some(1));
    }

    #[test]
    fn rejects_invalid_search_command_forms() {
        for input in [
            "SEARCH AUDIO 'needle'",
            "SEARCH HYBRID TEXT 'needle'",
            "SEARCH SPATIAL WITHIN 0 0 COLLECTION sites COLUMN geog",
            "SEARCH SPATIAL RADIUS 91 0 1 COLLECTION sites COLUMN geog",
            "SEARCH SPATIAL RADIUS 45 181 1 COLLECTION sites COLUMN geog",
            "SEARCH SPATIAL RADIUS 45 90 0 COLLECTION sites COLUMN geog",
            "SEARCH SPATIAL BBOX -91 0 1 1 COLLECTION sites COLUMN geog",
            "SEARCH SPATIAL BBOX 0 -181 1 1 COLLECTION sites COLUMN geog",
            "SEARCH SPATIAL BBOX 0 0 91 1 COLLECTION sites COLUMN geog",
            "SEARCH SPATIAL BBOX 0 0 1 181 COLLECTION sites COLUMN geog",
            "SEARCH SPATIAL NEAREST 91 0 K 1 COLLECTION sites COLUMN geog",
            "SEARCH SPATIAL NEAREST 0 181 K 1 COLLECTION sites COLUMN geog",
            "SEARCH SPATIAL NEAREST 0 0 K 0 COLLECTION sites COLUMN geog",
        ] {
            assert_parse_err(input);
        }
    }
}
