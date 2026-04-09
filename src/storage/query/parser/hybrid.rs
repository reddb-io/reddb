//! Hybrid query parsing (combining structured and vector search)

use super::super::ast::{FusionStrategy, HybridQuery, QueryExpr, VectorQuery};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;

impl<'a> Parser<'a> {
    /// Parse HYBRID query combining structured and vector search
    ///
    /// Syntax:
    /// ```text
    /// HYBRID
    ///   FROM table [WHERE ...] | MATCH pattern [WHERE ...]
    /// VECTOR SEARCH collection
    ///   SIMILAR TO ...
    /// FUSION RERANK(weight) | FILTER_THEN_SEARCH | SEARCH_THEN_FILTER | RRF(k) | INTERSECTION | UNION(sw, vw)
    /// [LIMIT n]
    /// ```
    pub fn parse_hybrid_query(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::Hybrid)?;

        // Parse structured part (table or graph query)
        let structured = match self.peek() {
            Token::From => {
                // Table query or join
                self.parse_from_query()?
            }
            Token::Match => self.parse_match_query()?,
            Token::Select => self.parse_select_query()?,
            other => {
                return Err(ParseError::expected(
                    vec!["FROM", "MATCH", "SELECT"],
                    other,
                    self.position(),
                ));
            }
        };

        // Parse vector part
        self.expect(Token::Vector)?;
        self.expect(Token::Search)?;

        let collection = self.expect_ident()?;

        self.expect(Token::Similar)?;
        self.expect(Token::To)?;

        let query_vector = self.parse_vector_source()?;

        // Parse vector filter
        let filter = if self.consume(&Token::Where)? {
            Some(self.parse_metadata_filter()?)
        } else {
            None
        };

        // Parse optional metric
        let metric = if self.consume(&Token::Metric)? {
            Some(self.parse_distance_metric()?)
        } else {
            None
        };

        let vector = VectorQuery {
            alias: None,
            collection,
            query_vector,
            k: 10, // Will be overridden by limit
            filter,
            metric,
            include_vectors: false,
            include_metadata: true,
            threshold: None,
        };

        // Parse fusion strategy
        self.expect(Token::Fusion)?;
        let fusion = self.parse_fusion_strategy()?;

        // Parse limit
        let limit = if self.consume(&Token::Limit)? {
            Some(self.parse_integer()? as usize)
        } else {
            None
        };

        Ok(QueryExpr::Hybrid(HybridQuery {
            alias: None,
            structured: Box::new(structured),
            vector,
            fusion,
            limit,
        }))
    }

    /// Parse fusion strategy
    fn parse_fusion_strategy(&mut self) -> Result<FusionStrategy, ParseError> {
        match self.peek() {
            Token::Rerank => {
                self.advance()?;
                // Optional weight in parentheses
                let weight = if self.consume(&Token::LParen)? {
                    let w = self.parse_float()? as f32;
                    self.expect(Token::RParen)?;
                    w
                } else {
                    0.5 // Default weight
                };
                Ok(FusionStrategy::Rerank { weight })
            }
            Token::Rrf => {
                self.advance()?;
                // Optional k in parentheses
                let k = if self.consume(&Token::LParen)? {
                    let k = self.parse_integer()? as u32;
                    self.expect(Token::RParen)?;
                    k
                } else {
                    60 // Default RRF k
                };
                Ok(FusionStrategy::RRF { k })
            }
            Token::Intersection => {
                self.advance()?;
                Ok(FusionStrategy::Intersection)
            }
            Token::Union => {
                self.advance()?;
                // Optional weights in parentheses
                let (sw, vw) = if self.consume(&Token::LParen)? {
                    let sw = self.parse_float()? as f32;
                    self.expect(Token::Comma)?;
                    let vw = self.parse_float()? as f32;
                    self.expect(Token::RParen)?;
                    (sw, vw)
                } else {
                    (0.5, 0.5) // Default equal weights
                };
                Ok(FusionStrategy::Union {
                    structured_weight: sw,
                    vector_weight: vw,
                })
            }
            Token::Ident(name) => {
                let name_upper = name.to_uppercase();
                let name_clone = name.clone();
                self.advance()?;
                match name_upper.as_str() {
                    "FILTER_THEN_SEARCH" | "FILTERTHEN" => {
                        Ok(FusionStrategy::FilterThenSearch)
                    }
                    "SEARCH_THEN_FILTER" | "SEARCHTHEN" => {
                        Ok(FusionStrategy::SearchThenFilter)
                    }
                    "RERANK" => {
                        let weight = if self.consume(&Token::LParen)? {
                            let w = self.parse_float()? as f32;
                            self.expect(Token::RParen)?;
                            w
                        } else {
                            0.5
                        };
                        Ok(FusionStrategy::Rerank { weight })
                    }
                    _ => Err(ParseError::new(
                        format!("Unknown fusion strategy: {}. Valid: RERANK, RRF, FILTER_THEN_SEARCH, SEARCH_THEN_FILTER, INTERSECTION, UNION", name_clone),
                        self.position(),
                    )),
                }
            }
            other => Err(ParseError::expected(
                vec![
                    "RERANK",
                    "RRF",
                    "FILTER_THEN_SEARCH",
                    "SEARCH_THEN_FILTER",
                    "INTERSECTION",
                    "UNION",
                ],
                other,
                self.position(),
            )),
        }
    }
}
