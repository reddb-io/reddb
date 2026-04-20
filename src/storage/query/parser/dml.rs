//! DML SQL Parser: INSERT, UPDATE, DELETE

use super::super::ast::{
    AskQuery, DeleteQuery, Expr, Filter, InsertEntityType, InsertQuery, QueryExpr, ReturningItem,
    UpdateQuery,
};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;
use crate::storage::query::sql_lowering::{filter_to_expr, fold_expr_to_value};
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
        let mut all_value_exprs = Vec::new();
        loop {
            self.expect(Token::LParen)?;
            let row_exprs = self.parse_dml_expr_list()?;
            self.expect(Token::RParen)?;
            let row_values = row_exprs
                .iter()
                .cloned()
                .map(fold_expr_to_value)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|msg| ParseError::new(msg, self.position()))?;
            all_value_exprs.push(row_exprs);
            all_values.push(row_values);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }

        // Parse optional WITH clauses
        let (ttl_ms, expires_at_ms, with_metadata, auto_embed) = self.parse_with_clauses()?;

        let returning = self.parse_returning_clause()?;

        Ok(QueryExpr::Insert(InsertQuery {
            table,
            entity_type,
            columns,
            value_exprs: all_value_exprs,
            values: all_values,
            returning,
            ttl_ms,
            expires_at_ms,
            with_metadata,
            auto_embed,
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

    /// Parse WITH clauses: WITH TTL | EXPIRES AT | METADATA | AUTO EMBED
    /// Returns (ttl_ms, expires_at_ms, metadata, auto_embed)
    pub fn parse_with_clauses(
        &mut self,
    ) -> Result<
        (
            Option<u64>,
            Option<u64>,
            Vec<(String, Value)>,
            Option<crate::storage::query::ast::AutoEmbedConfig>,
        ),
        ParseError,
    > {
        let mut ttl_ms = None;
        let mut expires_at_ms = None;
        let mut with_metadata = Vec::new();
        let mut auto_embed = None;

        while self.consume(&Token::With)? {
            if self.consume_ident_ci("TTL")? {
                ttl_ms = Some(self.parse_ttl_duration()?);
            } else if self.consume_ident_ci("EXPIRES")? {
                self.expect_ident_ci("AT")?;
                let ts = self.parse_expires_at_value()?;
                expires_at_ms = Some(ts);
            } else if self.consume(&Token::Metadata)? || self.consume_ident_ci("METADATA")? {
                with_metadata = self.parse_with_metadata_pairs()?;
            } else if self.consume_ident_ci("AUTO")? {
                // WITH AUTO EMBED (field1, field2) [USING provider] [MODEL 'model']
                self.consume_ident_ci("EMBED")?;
                self.expect(Token::LParen)?;
                let mut fields = Vec::new();
                loop {
                    fields.push(self.expect_ident()?);
                    if !self.consume(&Token::Comma)? {
                        break;
                    }
                }
                self.expect(Token::RParen)?;
                let provider = if self.consume_ident_ci("USING")? {
                    self.expect_ident()?
                } else {
                    "openai".to_string()
                };
                let model = if self.consume_ident_ci("MODEL")? {
                    Some(self.parse_string()?)
                } else {
                    None
                };
                auto_embed = Some(crate::storage::query::ast::AutoEmbedConfig {
                    fields,
                    provider,
                    model,
                });
            } else {
                return Err(ParseError::expected(
                    vec!["TTL", "EXPIRES AT", "METADATA", "AUTO EMBED"],
                    self.peek(),
                    self.position(),
                ));
            }
        }

        Ok((ttl_ms, expires_at_ms, with_metadata, auto_embed))
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
                let key = self.expect_ident_or_keyword()?.to_ascii_lowercase();
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
        let mut assignment_exprs = Vec::new();
        loop {
            let col = self.expect_ident()?;
            self.expect(Token::Eq)?;
            let expr = self.parse_expr()?;
            let folded = fold_expr_to_value(expr.clone()).ok();
            assignment_exprs.push((col.clone(), expr));
            if let Some(val) = folded {
                assignments.push((col.clone(), val));
            }
            if !self.consume(&Token::Comma)? {
                break;
            }
        }

        let filter = if self.consume(&Token::Where)? {
            Some(self.parse_filter()?)
        } else {
            None
        };
        let where_expr = filter.as_ref().map(filter_to_expr);

        let (ttl_ms, expires_at_ms, with_metadata, _auto_embed) = self.parse_with_clauses()?;

        let returning = self.parse_returning_clause()?;

        Ok(QueryExpr::Update(UpdateQuery {
            table,
            assignment_exprs,
            assignments,
            where_expr,
            filter,
            ttl_ms,
            expires_at_ms,
            with_metadata,
            returning,
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

        let where_expr = filter.as_ref().map(filter_to_expr);

        let returning = self.parse_returning_clause()?;

        Ok(QueryExpr::Delete(DeleteQuery {
            table,
            where_expr,
            filter,
            returning,
        }))
    }

    /// Parse optional `RETURNING (* | col [, col ...])` clause.
    /// Returns `None` if no RETURNING token, errors if RETURNING is present
    /// but not followed by `*` or a non-empty column list.
    fn parse_returning_clause(&mut self) -> Result<Option<Vec<ReturningItem>>, ParseError> {
        if !self.consume(&Token::Returning)? {
            return Ok(None);
        }
        if self.consume(&Token::Star)? {
            return Ok(Some(vec![ReturningItem::All]));
        }
        let mut items = Vec::new();
        loop {
            let col = self.expect_ident_or_keyword()?;
            items.push(ReturningItem::Column(col));
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        if items.is_empty() {
            return Err(ParseError::expected(
                vec!["*", "column name"],
                self.peek(),
                self.position(),
            ));
        }
        Ok(Some(items))
    }

    /// Parse: ASK 'question' [USING provider] [MODEL 'model'] [DEPTH n] [LIMIT n] [COLLECTION col]
    pub fn parse_ask_query(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume ASK

        let question = self.parse_string()?;

        let mut provider = None;
        let mut model = None;
        let mut depth = None;
        let mut limit = None;
        let mut collection = None;

        // Parse optional clauses in any order
        for _ in 0..5 {
            if self.consume_ident_ci("USING")? {
                provider = Some(self.expect_ident()?);
            } else if self.consume_ident_ci("MODEL")? {
                model = Some(self.parse_string()?);
            } else if self.consume(&Token::Depth)? {
                depth = Some(self.parse_integer()? as usize);
            } else if self.consume(&Token::Limit)? {
                limit = Some(self.parse_integer()? as usize);
            } else if self.consume(&Token::Collection)? {
                collection = Some(self.expect_ident()?);
            } else {
                break;
            }
        }

        Ok(QueryExpr::Ask(AskQuery {
            question,
            provider,
            model,
            depth,
            limit,
            collection,
        }))
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
        Ok(self
            .parse_dml_expr_list()?
            .into_iter()
            .map(fold_expr_to_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|msg| ParseError::new(msg, self.position()))?)
    }

    fn parse_dml_expr_list(&mut self) -> Result<Vec<Expr>, ParseError> {
        let mut values = Vec::new();
        loop {
            values.push(self.parse_expr()?);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        Ok(values)
    }

    /// Parse a single literal value (string, number, true, false, null, array)
    pub(crate) fn parse_literal_value(&mut self) -> Result<Value, ParseError> {
        // Recognize PASSWORD('plaintext') and SECRET('plaintext') as
        // typed literal constructors. The parser stores them as
        // sentinel-prefixed values so that the INSERT executor can
        // apply the crypto transform (argon2id hash / AES-256-GCM
        // encrypt) without the parser depending on auth or crypto
        // subsystems.
        if let Token::Ident(name) = self.peek().clone() {
            let upper = name.to_uppercase();
            if upper == "PASSWORD" || upper == "SECRET" {
                self.advance()?; // consume ident
                self.expect(Token::LParen)?;
                let plaintext = self.parse_string()?;
                self.expect(Token::RParen)?;
                return Ok(match upper.as_str() {
                    "PASSWORD" => Value::Password(format!("@@plain@@{plaintext}")),
                    "SECRET" => Value::Secret(format!("@@plain@@{plaintext}").into_bytes()),
                    _ => unreachable!(),
                });
            }
        }

        match self.peek().clone() {
            Token::String(s) => {
                let s = s.clone();
                self.advance()?;
                Ok(Value::text(s))
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
                            Value::Text(s) => crate::json::Value::String(s.to_string()),
                            _ => crate::json::Value::Null,
                        })
                        .collect();
                    let json_val = crate::json::Value::Array(json_arr);
                    let bytes = crate::json::to_vec(&json_val).unwrap_or_default();
                    Ok(Value::Json(bytes))
                }
            }
            Token::LBrace => {
                // Parse JSON object literal {key: value, ...}
                self.advance()?; // consume '{'
                let mut map = crate::json::Map::new();
                if !self.check(&Token::RBrace) {
                    loop {
                        // Key: string or identifier. Reserved-word
                        // keys (`level`, `msg`, `type`, …) fall through
                        // to `expect_ident_or_keyword`, which returns
                        // the canonical UPPERCASE spelling; lowercase
                        // that path so the JSON object preserves the
                        // source casing.
                        let key = match self.peek().clone() {
                            Token::String(s) => {
                                self.advance()?;
                                s
                            }
                            Token::Ident(s) => {
                                self.advance()?;
                                s
                            }
                            _ => self.expect_ident_or_keyword()?.to_ascii_lowercase(),
                        };
                        // Separator: ':' or '='
                        if !self.consume(&Token::Colon)? {
                            self.expect(Token::Eq)?;
                        }
                        // Value: recursive
                        let val = self.parse_literal_value()?;
                        let json_val = match val {
                            Value::Null => crate::json::Value::Null,
                            Value::Boolean(b) => crate::json::Value::Bool(b),
                            Value::Integer(i) => crate::json::Value::Number(i as f64),
                            Value::Float(f) => crate::json::Value::Number(f),
                            Value::Text(s) => crate::json::Value::String(s.to_string()),
                            Value::Json(ref bytes) => {
                                crate::json::from_slice(bytes).unwrap_or(crate::json::Value::Null)
                            }
                            _ => crate::json::Value::Null,
                        };
                        map.insert(key, json_val);
                        if !self.consume(&Token::Comma)? {
                            break;
                        }
                    }
                }
                self.expect(Token::RBrace)?;
                let json_val = crate::json::Value::Object(map);
                let bytes = crate::json::to_vec(&json_val).unwrap_or_default();
                Ok(Value::Json(bytes))
            }
            ref other => Err(ParseError::expected(
                vec!["string", "number", "true", "false", "null", "[", "{"],
                other,
                self.position(),
            )),
        }
    }
}
