//! Vector query parsing (VECTOR SEARCH ... SIMILAR TO ...)

use super::super::ast::{QueryExpr, TableQuery, VectorQuery, VectorSource};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;
use crate::storage::engine::distance::DistanceMetric;
use crate::storage::engine::vector_metadata::{MetadataFilter, MetadataValue};

impl<'a> Parser<'a> {
    /// Parse VECTOR SEARCH ... SIMILAR TO ... query
    ///
    /// Syntax:
    /// ```text
    /// VECTOR SEARCH collection
    /// SIMILAR TO [0.1, 0.2, ...] | 'text query' | (subquery)
    /// [WHERE metadata conditions]
    /// [METRIC L2|COSINE|INNER_PRODUCT]
    /// [THRESHOLD 0.5]
    /// [INCLUDE VECTORS] [INCLUDE METADATA]
    /// [LIMIT k]
    /// ```
    pub fn parse_vector_query(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::Vector)?;
        self.expect(Token::Search)?;

        // Collection name
        let collection = self.expect_ident()?;

        // SIMILAR TO clause
        self.expect(Token::Similar)?;
        self.expect(Token::To)?;

        let query_vector = self.parse_vector_source()?;

        // Parse optional clauses
        let mut filter: Option<MetadataFilter> = None;
        let mut metric: Option<DistanceMetric> = None;
        let mut threshold: Option<f32> = None;
        let mut include_vectors = false;
        let mut include_metadata = false;
        let mut k: usize = 10; // Default

        // Parse optional clauses in any order
        loop {
            if self.consume(&Token::Where)? {
                filter = Some(self.parse_metadata_filter()?);
            } else if self.consume(&Token::Metric)? {
                metric = Some(self.parse_distance_metric()?);
            } else if self.consume(&Token::Threshold)? {
                threshold = Some(self.parse_float()? as f32);
            } else if self.consume(&Token::Include)? {
                if self.consume(&Token::Vectors)? {
                    include_vectors = true;
                } else if self.consume(&Token::Metadata)? {
                    include_metadata = true;
                } else {
                    return Err(ParseError::expected(
                        vec!["VECTORS", "METADATA"],
                        self.peek(),
                        self.position(),
                    ));
                }
            } else if self.consume(&Token::Limit)? {
                k = self.parse_integer()? as usize;
            } else if self.consume(&Token::K)? {
                // Alternative: K = 10
                self.expect(Token::Eq)?;
                k = self.parse_integer()? as usize;
            } else {
                break;
            }
        }

        Ok(QueryExpr::Vector(VectorQuery {
            alias: None,
            collection,
            query_vector,
            k,
            filter,
            metric,
            include_vectors,
            include_metadata,
            threshold,
        }))
    }

    /// Parse vector source: literal array, text, reference, or subquery
    pub fn parse_vector_source(&mut self) -> Result<VectorSource, ParseError> {
        match self.peek() {
            // Literal vector: [0.1, 0.2, 0.3]
            Token::LBracket => {
                self.advance()?;
                let mut values = Vec::new();
                loop {
                    let value = self.parse_float()?;
                    values.push(value as f32);
                    if !self.consume(&Token::Comma)? {
                        break;
                    }
                }
                self.expect(Token::RBracket)?;
                Ok(VectorSource::Literal(values))
            }
            // Text query: 'find similar vulnerabilities'
            Token::String(_) => {
                let text = self.parse_string()?;
                Ok(VectorSource::Text(text))
            }
            // Subquery: (SELECT embedding FROM ...)
            Token::LParen => {
                self.advance()?;
                // For now, parse as reference if it's an identifier
                // Full subquery support would require recursive parsing
                if let Token::Select = self.peek() {
                    // Skip the subquery for now - just consume until )
                    let mut depth = 1;
                    while depth > 0 {
                        match self.advance()? {
                            Token::LParen => depth += 1,
                            Token::RParen => depth -= 1,
                            Token::Eof => {
                                return Err(ParseError::new(
                                    "Unterminated subquery",
                                    self.position(),
                                ));
                            }
                            _ => {}
                        }
                    }
                    Ok(VectorSource::Subquery(Box::new(QueryExpr::Table(
                        TableQuery {
                            table: "subquery".to_string(),
                            alias: None,
                            columns: Vec::new(),
                            filter: None,
                            group_by: Vec::new(),
                            having: None,
                            order_by: Vec::new(),
                            limit: None,
                            offset: None,
                            expand: None,
                        },
                    ))))
                } else {
                    // Reference: (collection, vector_id)
                    let collection = self.expect_ident()?;
                    self.expect(Token::Comma)?;
                    let vector_id = self.parse_integer()? as u64;
                    self.expect(Token::RParen)?;
                    Ok(VectorSource::Reference {
                        collection,
                        vector_id,
                    })
                }
            }
            // Reference by name: embedding_name
            Token::Ident(_) => {
                let name = self.expect_ident()?;
                // Check for (collection, id) format
                if self.consume(&Token::LParen)? {
                    let vector_id = self.parse_integer()? as u64;
                    self.expect(Token::RParen)?;
                    Ok(VectorSource::Reference {
                        collection: name,
                        vector_id,
                    })
                } else {
                    // Just a name reference, treat as text
                    Ok(VectorSource::Text(name))
                }
            }
            other => Err(ParseError::expected(
                vec!["vector literal [...]", "string", "reference"],
                other,
                self.position(),
            )),
        }
    }

    /// Parse metadata filter for vector queries
    pub fn parse_metadata_filter(&mut self) -> Result<MetadataFilter, ParseError> {
        self.parse_metadata_or_expr()
    }

    /// Parse OR expression in metadata filter
    fn parse_metadata_or_expr(&mut self) -> Result<MetadataFilter, ParseError> {
        let mut left = self.parse_metadata_and_expr()?;

        while self.consume(&Token::Or)? {
            let right = self.parse_metadata_and_expr()?;
            left = MetadataFilter::Or(vec![left, right]);
        }

        Ok(left)
    }

    /// Parse AND expression in metadata filter
    fn parse_metadata_and_expr(&mut self) -> Result<MetadataFilter, ParseError> {
        let mut left = self.parse_metadata_primary()?;

        while self.consume(&Token::And)? {
            let right = self.parse_metadata_primary()?;
            left = MetadataFilter::And(vec![left, right]);
        }

        Ok(left)
    }

    /// Parse primary metadata filter
    fn parse_metadata_primary(&mut self) -> Result<MetadataFilter, ParseError> {
        // Parenthesized expression
        if self.consume(&Token::LParen)? {
            let expr = self.parse_metadata_filter()?;
            self.expect(Token::RParen)?;
            return Ok(expr);
        }

        // field op value
        let field = self.expect_ident()?;

        // Handle different operators
        if self.consume(&Token::Eq)? {
            let value = self.parse_metadata_value()?;
            Ok(MetadataFilter::Eq(field, value))
        } else if self.consume(&Token::Ne)? {
            let value = self.parse_metadata_value()?;
            Ok(MetadataFilter::Ne(field, value))
        } else if self.consume(&Token::Lt)? {
            let value = self.parse_metadata_value()?;
            Ok(MetadataFilter::Lt(field, value))
        } else if self.consume(&Token::Le)? {
            let value = self.parse_metadata_value()?;
            Ok(MetadataFilter::Lte(field, value))
        } else if self.consume(&Token::Gt)? {
            let value = self.parse_metadata_value()?;
            Ok(MetadataFilter::Gt(field, value))
        } else if self.consume(&Token::Ge)? {
            let value = self.parse_metadata_value()?;
            Ok(MetadataFilter::Gte(field, value))
        } else if self.consume(&Token::In)? {
            self.expect(Token::LParen)?;
            let values = self.parse_metadata_value_list()?;
            self.expect(Token::RParen)?;
            Ok(MetadataFilter::In(field, values))
        } else if self.consume(&Token::Not)? {
            self.expect(Token::In)?;
            self.expect(Token::LParen)?;
            let values = self.parse_metadata_value_list()?;
            self.expect(Token::RParen)?;
            Ok(MetadataFilter::NotIn(field, values))
        } else if self.consume(&Token::Contains)? {
            let value = self.parse_string()?;
            Ok(MetadataFilter::Contains(field, value))
        } else {
            Err(ParseError::expected(
                vec!["=", "<>", "<", "<=", ">", ">=", "IN", "NOT IN", "CONTAINS"],
                self.peek(),
                self.position(),
            ))
        }
    }

    /// Parse metadata value
    fn parse_metadata_value(&mut self) -> Result<MetadataValue, ParseError> {
        match self.peek() {
            Token::String(_) => {
                let s = self.parse_string()?;
                Ok(MetadataValue::String(s))
            }
            Token::Integer(_) => {
                let n = self.parse_integer()?;
                Ok(MetadataValue::Integer(n))
            }
            Token::Float(_) => {
                let n = self.parse_float()?;
                Ok(MetadataValue::Float(n))
            }
            Token::True => {
                self.advance()?;
                Ok(MetadataValue::Bool(true))
            }
            Token::False => {
                self.advance()?;
                Ok(MetadataValue::Bool(false))
            }
            other => Err(ParseError::expected(
                vec!["string", "number", "true", "false"],
                other,
                self.position(),
            )),
        }
    }

    /// Parse list of metadata values
    fn parse_metadata_value_list(&mut self) -> Result<Vec<MetadataValue>, ParseError> {
        let mut values = Vec::new();
        loop {
            values.push(self.parse_metadata_value()?);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        Ok(values)
    }

    /// Parse distance metric
    pub fn parse_distance_metric(&mut self) -> Result<DistanceMetric, ParseError> {
        match self.peek() {
            Token::L2 => {
                self.advance()?;
                Ok(DistanceMetric::L2)
            }
            Token::Cosine => {
                self.advance()?;
                Ok(DistanceMetric::Cosine)
            }
            Token::InnerProduct => {
                self.advance()?;
                Ok(DistanceMetric::InnerProduct)
            }
            Token::Ident(name) => {
                let name_upper = name.to_uppercase();
                let name_clone = name.clone();
                self.advance()?;
                match name_upper.as_str() {
                    "L2" | "EUCLIDEAN" => Ok(DistanceMetric::L2),
                    "COSINE" | "COS" => Ok(DistanceMetric::Cosine),
                    "INNER_PRODUCT" | "IP" | "DOT" => Ok(DistanceMetric::InnerProduct),
                    _ => Err(ParseError::new(
                        format!(
                            "Unknown distance metric: {}. Valid: L2, COSINE, INNER_PRODUCT",
                            name_clone
                        ),
                        self.position(),
                    )),
                }
            }
            other => Err(ParseError::expected(
                vec!["L2", "COSINE", "INNER_PRODUCT"],
                other,
                self.position(),
            )),
        }
    }
}
