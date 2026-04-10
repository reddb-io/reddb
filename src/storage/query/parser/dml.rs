//! DML SQL Parser: INSERT, UPDATE, DELETE

use super::super::ast::{
    DeleteQuery, Filter, InsertEntityType, InsertQuery, QueryExpr, UpdateQuery,
};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;
use crate::storage::schema::Value;

impl<'a> Parser<'a> {
    /// Parse: INSERT INTO table [NODE|EDGE|VECTOR|DOCUMENT|KV] (col1, col2) VALUES (val1, val2), (val3, val4) [RETURNING]
    pub fn parse_insert_query(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::Insert)?;
        self.expect(Token::Into)?;
        let table = self.expect_ident()?;

        // Check for entity type keyword
        let entity_type = match self.peek().clone() {
            Token::Node => {
                self.advance()?;
                InsertEntityType::Node
            }
            Token::Edge => {
                self.advance()?;
                InsertEntityType::Edge
            }
            Token::Vector => {
                self.advance()?;
                InsertEntityType::Vector
            }
            Token::Document => {
                self.advance()?;
                InsertEntityType::Document
            }
            Token::Kv => {
                self.advance()?;
                InsertEntityType::Kv
            }
            _ => InsertEntityType::Row,
        };

        // Parse column list
        self.expect(Token::LParen)?;
        let columns = self.parse_ident_list()?;
        self.expect(Token::RParen)?;

        // Parse VALUES
        self.expect(Token::Values)?;
        let mut all_values = Vec::new();
        loop {
            self.expect(Token::LParen)?;
            let row_values = self.parse_dml_value_list()?;
            self.expect(Token::RParen)?;
            all_values.push(row_values);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }

        let returning = self.consume(&Token::Returning)?;

        // Parse optional WITH clauses
        let (ttl_ms, expires_at_ms, with_metadata) = self.parse_with_clauses()?;

        Ok(QueryExpr::Insert(InsertQuery {
            table,
            entity_type,
            columns,
            values: all_values,
            returning,
            ttl_ms,
            expires_at_ms,
            with_metadata,
        }))
    }

    /// Parse TTL duration value using the same logic as CREATE TABLE ... WITH TTL.
    fn parse_ttl_duration(&mut self) -> Result<u64, ParseError> {
        // Reuse the DDL TTL parser: expects a number followed by optional unit
        let ttl_value = self.parse_float()?;
        let ttl_unit = match self.peek() {
            Token::Ident(unit) => {
                let unit = unit.clone();
                self.advance()?;
                unit
            }
            _ => "s".to_string(),
        };

        let multiplier_ms = match ttl_unit.to_ascii_lowercase().as_str() {
            "ms" | "msec" | "millisecond" | "milliseconds" => 1.0,
            "s" | "sec" | "secs" | "second" | "seconds" => 1_000.0,
            "m" | "min" | "mins" | "minute" | "minutes" => 60_000.0,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3_600_000.0,
            "d" | "day" | "days" => 86_400_000.0,
            other => {
                return Err(ParseError::new(
                    format!("unsupported TTL unit '{other}'"),
                    self.position(),
                ))
            }
        };

        Ok((ttl_value * multiplier_ms) as u64)
    }

    /// Parse WITH clauses: WITH TTL <duration> | WITH EXPIRES AT <timestamp> | WITH METADATA (k=v)
    /// Returns (ttl_ms, expires_at_ms, metadata)
    pub fn parse_with_clauses(
        &mut self,
    ) -> Result<(Option<u64>, Option<u64>, Vec<(String, Value)>), ParseError> {
        let mut ttl_ms = None;
        let mut expires_at_ms = None;
        let mut with_metadata = Vec::new();

        while self.consume(&Token::With)? {
            if self.consume_ident_ci("TTL")? {
                ttl_ms = Some(self.parse_ttl_duration()?);
            } else if self.consume_ident_ci("EXPIRES")? {
                // WITH EXPIRES AT <timestamp>
                self.expect_ident_ci("AT")?;
                let ts = self.parse_expires_at_value()?;
                expires_at_ms = Some(ts);
            } else if self.consume(&Token::Metadata)? || self.consume_ident_ci("METADATA")? {
                with_metadata = self.parse_with_metadata_pairs()?;
            } else {
                return Err(ParseError::expected(
                    vec!["TTL", "EXPIRES AT", "METADATA"],
                    self.peek(),
                    self.position(),
                ));
            }
        }

        Ok((ttl_ms, expires_at_ms, with_metadata))
    }

    /// Expect a case-insensitive identifier (error if not found)
    fn expect_ident_ci(&mut self, expected: &str) -> Result<(), ParseError> {
        if self.consume_ident_ci(expected)? {
            Ok(())
        } else {
            Err(ParseError::expected(
                vec![expected],
                self.peek(),
                self.position(),
            ))
        }
    }

    /// Parse an absolute expiration timestamp (unix ms or string date)
    fn parse_expires_at_value(&mut self) -> Result<u64, ParseError> {
        // Try integer (unix timestamp in ms)
        if let Ok(value) = self.parse_integer() {
            return Ok(value as u64);
        }
        // Try string like '2026-12-31' — convert to unix ms
        if let Ok(text) = self.parse_string() {
            // Simple ISO date parsing: YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS
            let trimmed = text.trim();
            if let Ok(ts) = trimmed.parse::<u64>() {
                return Ok(ts);
            }
            // Basic date parsing — delegate to chrono if available, or simple heuristic
            return Err(ParseError::new(
                format!("EXPIRES AT requires a unix timestamp in milliseconds, got '{trimmed}'"),
                self.position(),
            ));
        }
        Err(ParseError::expected(
            vec!["timestamp (unix ms) or 'YYYY-MM-DD'"],
            self.peek(),
            self.position(),
        ))
    }

    /// Parse WITH METADATA (key1 = 'value1', key2 = 42)
    fn parse_with_metadata_pairs(&mut self) -> Result<Vec<(String, Value)>, ParseError> {
        self.expect(Token::LParen)?;
        let mut pairs = Vec::new();
        if !self.check(&Token::RParen) {
            loop {
                let key = self.expect_ident()?;
                self.expect(Token::Eq)?;
                let value = self.parse_literal_value()?;
                pairs.push((key, value));
                if !self.consume(&Token::Comma)? {
                    break;
                }
            }
        }
        self.expect(Token::RParen)?;
        Ok(pairs)
    }

    /// Parse: UPDATE table SET col1=val1, col2=val2 [WHERE filter] [WITH TTL|EXPIRES AT|METADATA]
    pub fn parse_update_query(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::Update)?;
        let table = self.expect_ident()?;
        self.expect(Token::Set)?;

        let mut assignments = Vec::new();
        loop {
            let col = self.expect_ident()?;
            self.expect(Token::Eq)?;
            let val = self.parse_literal_value()?;
            assignments.push((col, val));
            if !self.consume(&Token::Comma)? {
                break;
            }
        }

        let filter = if self.consume(&Token::Where)? {
            Some(self.parse_filter()?)
        } else {
            None
        };

        let (ttl_ms, expires_at_ms, with_metadata) = self.parse_with_clauses()?;

        Ok(QueryExpr::Update(UpdateQuery {
            table,
            assignments,
            filter,
            ttl_ms,
            expires_at_ms,
            with_metadata,
        }))
    }

    /// Parse: DELETE FROM table [WHERE filter]
    pub fn parse_delete_query(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::Delete)?;
        self.expect(Token::From)?;
        let table = self.expect_ident()?;

        let filter = if self.consume(&Token::Where)? {
            Some(self.parse_filter()?)
        } else {
            None
        };

        Ok(QueryExpr::Delete(DeleteQuery { table, filter }))
    }

    /// Parse comma-separated identifiers (accepts keywords as column names in DML context)
    fn parse_ident_list(&mut self) -> Result<Vec<String>, ParseError> {
        let mut idents = Vec::new();
        loop {
            idents.push(self.expect_ident_or_keyword()?);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        Ok(idents)
    }

    /// Parse comma-separated literal values for DML statements
    fn parse_dml_value_list(&mut self) -> Result<Vec<Value>, ParseError> {
        let mut values = Vec::new();
        loop {
            values.push(self.parse_literal_value()?);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        Ok(values)
    }

    /// Parse a single literal value (string, number, true, false, null, array)
    pub(crate) fn parse_literal_value(&mut self) -> Result<Value, ParseError> {
        match self.peek().clone() {
            Token::String(s) => {
                let s = s.clone();
                self.advance()?;
                Ok(Value::Text(s))
            }
            Token::Integer(n) => {
                self.advance()?;
                Ok(Value::Integer(n))
            }
            Token::Float(n) => {
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
            Token::LBracket => {
                // Parse array literal [val1, val2, ...]
                // For numeric arrays, produce Value::Vector; for others, produce Value::Json
                self.advance()?; // consume '['
                let mut items = Vec::new();
                if !self.check(&Token::RBracket) {
                    loop {
                        items.push(self.parse_literal_value()?);
                        if !self.consume(&Token::Comma)? {
                            break;
                        }
                    }
                }
                self.expect(Token::RBracket)?;

                // Check if all items are numeric (Integer or Float) -> Value::Vector
                let all_numeric = items
                    .iter()
                    .all(|v| matches!(v, Value::Integer(_) | Value::Float(_)));
                if all_numeric && !items.is_empty() {
                    let floats: Vec<f32> = items
                        .iter()
                        .map(|v| match v {
                            Value::Float(f) => *f as f32,
                            Value::Integer(i) => *i as f32,
                            _ => 0.0,
                        })
                        .collect();
                    Ok(Value::Vector(floats))
                } else {
                    // Encode as JSON bytes
                    let json_arr: Vec<crate::json::Value> = items
                        .iter()
                        .map(|v| match v {
                            Value::Null => crate::json::Value::Null,
                            Value::Boolean(b) => crate::json::Value::Bool(*b),
                            Value::Integer(i) => crate::json::Value::Number(*i as f64),
                            Value::Float(f) => crate::json::Value::Number(*f),
                            Value::Text(s) => crate::json::Value::String(s.clone()),
                            _ => crate::json::Value::Null,
                        })
                        .collect();
                    let json_val = crate::json::Value::Array(json_arr);
                    let bytes = crate::json::to_vec(&json_val).unwrap_or_default();
                    Ok(Value::Json(bytes))
                }
            }
            ref other => Err(ParseError::expected(
                vec!["string", "number", "true", "false", "null", "["],
                other,
                self.position(),
            )),
        }
    }
}
