//! Vector query parsing (VECTOR SEARCH ... SIMILAR TO ...)

use super::error::ParseError;
use super::Parser;
use crate::ast::{QueryExpr, VectorQuery, VectorSource};
use crate::lexer::Token;
use reddb_types::distance::DistanceMetric;
use reddb_types::vector_metadata::{MetadataFilter, MetadataValue};

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
            // Parenthesized source: subquery or (collection, vector_id) reference
            Token::LParen => {
                self.advance()?;
                if self.vector_source_starts_subquery() {
                    let expr = self.parse_query_expr()?;
                    self.expect(Token::RParen)?;
                    Ok(VectorSource::Subquery(Box::new(expr)))
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

    fn vector_source_starts_subquery(&self) -> bool {
        matches!(
            self.peek(),
            Token::Select
                | Token::Match
                | Token::Path
                | Token::From
                | Token::Vector
                | Token::Hybrid
        )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_query(input: &str) -> Result<QueryExpr, ParseError> {
        crate::parser::parse(input).map(|query| query.query)
    }

    #[test]
    fn vector_query_uses_defaults_for_bare_identifier_source() {
        let query = parse_query("VECTOR SEARCH embeddings SIMILAR TO nearest_neighbor").unwrap();

        let QueryExpr::Vector(vector) = query else {
            panic!("expected vector query");
        };
        assert_eq!(vector.collection, "embeddings");
        assert_eq!(vector.k, 10);
        assert!(vector.filter.is_none());
        assert_eq!(vector.metric, None);
        assert_eq!(vector.threshold, None);
        assert!(!vector.include_vectors);
        assert!(!vector.include_metadata);
        assert!(matches!(
            vector.query_vector,
            VectorSource::Text(text) if text == "nearest_neighbor"
        ));
    }

    #[test]
    fn vector_query_parses_reference_sources_and_k_alias() {
        let query =
            parse_query("VECTOR SEARCH embeddings SIMILAR TO docs(42) INCLUDE METADATA K = 7")
                .unwrap();
        let QueryExpr::Vector(vector) = query else {
            panic!("expected vector query");
        };
        assert_eq!(vector.k, 7);
        assert!(vector.include_metadata);
        assert!(matches!(
            vector.query_vector,
            VectorSource::Reference {
                collection,
                vector_id,
            } if collection == "docs" && vector_id == 42
        ));

        let query =
            parse_query("VECTOR SEARCH embeddings SIMILAR TO (archive, 99) LIMIT 4").unwrap();
        let QueryExpr::Vector(vector) = query else {
            panic!("expected vector query");
        };
        assert_eq!(vector.k, 4);
        assert!(matches!(
            vector.query_vector,
            VectorSource::Reference {
                collection,
                vector_id,
            } if collection == "archive" && vector_id == 99
        ));
    }

    #[test]
    fn vector_query_parses_subquery_source() {
        let query =
            parse_query("VECTOR SEARCH docs SIMILAR TO (SELECT id FROM seeds) LIMIT 2").unwrap();

        let QueryExpr::Vector(vector) = query else {
            panic!("expected vector query");
        };
        assert_eq!(vector.collection, "docs");
        assert_eq!(vector.k, 2);
        match vector.query_vector {
            VectorSource::Subquery(expr) => match *expr {
                QueryExpr::Table(table) => assert_eq!(table.table, "seeds"),
                other => panic!("expected table subquery, got {other:?}"),
            },
            other => panic!("expected subquery source, got {other:?}"),
        }
    }

    #[test]
    fn vector_query_parses_filter_sets_metric_threshold_and_includes() {
        let query = parse_query(
            "VECTOR SEARCH docs SIMILAR TO [0.1, 0.2] \
             WHERE (source IN ('nmap', 'nessus') OR severity NOT IN (1, 2)) \
             AND archived = false METRIC DOT THRESHOLD 0.25 INCLUDE VECTORS LIMIT 3",
        )
        .unwrap();

        let QueryExpr::Vector(vector) = query else {
            panic!("expected vector query");
        };
        assert_eq!(vector.k, 3);
        assert_eq!(vector.metric, Some(DistanceMetric::InnerProduct));
        assert_eq!(vector.threshold, Some(0.25));
        assert!(vector.include_vectors);
        assert!(
            matches!(vector.query_vector, VectorSource::Literal(values) if values == vec![0.1, 0.2])
        );

        let Some(MetadataFilter::And(and_parts)) = vector.filter else {
            panic!("expected AND filter");
        };
        assert_eq!(and_parts.len(), 2);
        match &and_parts[0] {
            MetadataFilter::Or(or_parts) => {
                assert_eq!(or_parts.len(), 2);
                assert!(matches!(
                    &or_parts[0],
                    MetadataFilter::In(field, values)
                        if field == "source"
                            && values == &vec![
                                MetadataValue::String("nmap".to_string()),
                                MetadataValue::String("nessus".to_string())
                            ]
                ));
                assert!(matches!(
                    &or_parts[1],
                    MetadataFilter::NotIn(field, values)
                        if field == "severity"
                            && values == &vec![MetadataValue::Integer(1), MetadataValue::Integer(2)]
                ));
            }
            other => panic!("expected OR filter, got {other:?}"),
        }
        assert!(matches!(
            &and_parts[1],
            MetadataFilter::Eq(field, MetadataValue::Bool(false)) if field == "archived"
        ));
    }

    #[test]
    fn metadata_filter_parses_comparisons_and_contains() {
        let query = parse_query(
            "VECTOR SEARCH docs SIMILAR TO [0.3] \
             WHERE score < 0.7 OR rank >= 10 AND title CONTAINS 'redis'",
        )
        .unwrap();

        let QueryExpr::Vector(vector) = query else {
            panic!("expected vector query");
        };
        let Some(MetadataFilter::Or(or_parts)) = vector.filter else {
            panic!("expected OR filter");
        };
        assert_eq!(or_parts.len(), 2);
        assert!(matches!(
            &or_parts[0],
            MetadataFilter::Lt(field, MetadataValue::Float(value))
                if field == "score" && (*value - 0.7).abs() < f64::EPSILON
        ));
        match &or_parts[1] {
            MetadataFilter::And(and_parts) => {
                assert_eq!(and_parts.len(), 2);
                assert!(matches!(
                    &and_parts[0],
                    MetadataFilter::Gte(field, MetadataValue::Integer(10)) if field == "rank"
                ));
                assert!(matches!(
                    &and_parts[1],
                    MetadataFilter::Contains(field, value)
                        if field == "title" && value == "redis"
                ));
            }
            other => panic!("expected AND filter, got {other:?}"),
        }
    }

    #[test]
    fn vector_parser_reports_malformed_queries() {
        for sql in [
            "VECTOR SEARCH docs SIMILAR TO []",
            "VECTOR SEARCH docs SIMILAR TO [0.1] INCLUDE SCORES",
            "VECTOR SEARCH docs SIMILAR TO [0.1] METRIC MANHATTAN",
            "VECTOR SEARCH docs SIMILAR TO [0.1] WHERE source",
            "VECTOR SEARCH docs SIMILAR TO (docs)",
        ] {
            assert!(parse_query(sql).is_err(), "{sql} should not parse");
        }
    }
}
