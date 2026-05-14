//! Parser for CREATE/DROP TIMESERIES

use super::super::ast::{
    CreateTableQuery, CreateTimeSeriesQuery, DropTimeSeriesQuery, HypertableDdl, QueryExpr,
};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;
use crate::catalog::CollectionModel;

impl<'a> Parser<'a> {
    /// Parse CREATE TIMESERIES body (after CREATE TIMESERIES consumed)
    pub fn parse_create_timeseries_body(&mut self) -> Result<QueryExpr, ParseError> {
        let if_not_exists = self.match_if_not_exists()?;
        let name = self.expect_ident()?;

        let mut retention_ms = None;
        let mut chunk_size = None;
        let mut downsample_policies = Vec::new();

        // Parse optional clauses in any order
        loop {
            if self.consume(&Token::Retention)? {
                let value = self.parse_float()?;
                let unit = self.parse_duration_unit()?;
                retention_ms = Some((value * unit) as u64);
            } else if self.consume_ident_ci("CHUNK_SIZE")? || self.consume_ident_ci("CHUNKSIZE")? {
                chunk_size = Some(self.parse_integer()? as usize);
            } else if self.consume_ident_ci("DOWNSAMPLE")? {
                downsample_policies.push(self.parse_downsample_policy_spec()?);
                while self.consume(&Token::Comma)? {
                    downsample_policies.push(self.parse_downsample_policy_spec()?);
                }
            } else {
                break;
            }
        }

        Ok(QueryExpr::CreateTimeSeries(CreateTimeSeriesQuery {
            name,
            retention_ms,
            chunk_size,
            downsample_policies,
            if_not_exists,
            hypertable: None,
        }))
    }

    /// Parse CREATE METRICS body (after CREATE METRICS consumed).
    ///
    /// v0 intentionally establishes only the collection contract. Ingestion,
    /// series registry, and Prometheus adapter slices build on this metadata.
    pub fn parse_create_metrics_body(&mut self) -> Result<QueryExpr, ParseError> {
        let if_not_exists = self.match_if_not_exists()?;
        let name = self.expect_ident()?;

        let mut raw_retention_ms = None;
        let mut tenant_by = None;
        let mut downsample_policies = Vec::new();

        loop {
            if self.consume(&Token::Retention)? {
                let value = self.parse_float()?;
                let unit = self.parse_duration_unit()?;
                raw_retention_ms = Some((value * unit) as u64);
            } else if self.consume_ident_ci("DOWNSAMPLE")? {
                downsample_policies.push(self.parse_downsample_policy_spec()?);
                while self.consume(&Token::Comma)? {
                    downsample_policies.push(self.parse_downsample_policy_spec()?);
                }
            } else if tenant_by.is_none() && self.consume_ident_ci("TENANT")? {
                self.expect(Token::By)?;
                self.expect(Token::LParen)?;
                let mut path = self.expect_ident_or_keyword()?;
                while self.consume(&Token::Dot)? {
                    let next = self.expect_ident_or_keyword()?;
                    path = format!("{path}.{next}");
                }
                self.expect(Token::RParen)?;
                tenant_by = Some(path);
            } else {
                break;
            }
        }

        Ok(QueryExpr::CreateTable(CreateTableQuery {
            collection_model: CollectionModel::Metrics,
            name,
            columns: Vec::new(),
            if_not_exists,
            default_ttl_ms: raw_retention_ms,
            metrics_rollup_policies: downsample_policies,
            context_index_fields: Vec::new(),
            context_index_enabled: false,
            timestamps: false,
            partition_by: None,
            tenant_by,
            append_only: true,
            subscriptions: Vec::new(),
            vault_own_master_key: false,
        }))
    }

    /// Parse CREATE HYPERTABLE body — TimescaleDB-style.
    ///
    ///   CREATE HYPERTABLE metrics
    ///     TIME_COLUMN ts
    ///     CHUNK_INTERVAL '1d'
    ///     [TTL '90d']
    ///     [RETENTION 90 DAYS]          -- collection-level TTL (ms)
    ///
    /// Produces the same `CreateTimeSeriesQuery` AST as `CREATE
    /// TIMESERIES`, with the `hypertable` field populated. The
    /// runtime dispatcher registers the spec on the RedDB-wide
    /// `HypertableRegistry` alongside creating the collection.
    pub fn parse_create_hypertable_body(&mut self) -> Result<QueryExpr, ParseError> {
        let if_not_exists = self.match_if_not_exists()?;
        let name = self.expect_ident()?;

        let mut time_column: Option<String> = None;
        let mut chunk_interval_ns: Option<u64> = None;
        let mut ttl_ns: Option<u64> = None;
        let mut retention_ms = None;

        loop {
            if self.consume_ident_ci("TIME_COLUMN")? {
                time_column = Some(self.expect_ident()?);
            } else if self.consume_ident_ci("CHUNK_INTERVAL")? {
                chunk_interval_ns = Some(self.parse_duration_ns_literal("CHUNK_INTERVAL")?);
            } else if self.consume_ident_ci("TTL")? {
                ttl_ns = Some(self.parse_duration_ns_literal("TTL")?);
            } else if self.consume(&Token::Retention)? {
                let value = self.parse_float()?;
                let unit = self.parse_duration_unit()?;
                retention_ms = Some((value * unit) as u64);
            } else {
                break;
            }
        }

        let time_column = time_column.ok_or_else(|| {
            ParseError::new(
                "CREATE HYPERTABLE requires TIME_COLUMN <ident>".to_string(),
                self.position(),
            )
        })?;
        let chunk_interval_ns = chunk_interval_ns.ok_or_else(|| {
            ParseError::new(
                "CREATE HYPERTABLE requires CHUNK_INTERVAL '<duration>' (e.g. '1d')".to_string(),
                self.position(),
            )
        })?;

        Ok(QueryExpr::CreateTimeSeries(CreateTimeSeriesQuery {
            name,
            retention_ms,
            chunk_size: None,
            downsample_policies: Vec::new(),
            if_not_exists,
            hypertable: Some(HypertableDdl {
                time_column,
                chunk_interval_ns,
                default_ttl_ns: ttl_ns,
            }),
        }))
    }

    /// Accept a string-literal duration (`'1d'`, `'5m'`, `'30s'`, …) and
    /// resolve it to nanoseconds using the shared retention grammar.
    fn parse_duration_ns_literal(&mut self, clause: &str) -> Result<u64, ParseError> {
        let pos = self.position();
        let value = self.parse_literal_value()?;
        match value {
            crate::storage::schema::Value::Text(s) => {
                crate::storage::timeseries::retention::parse_duration_ns(&s).ok_or_else(|| {
                    ParseError::new(
                        // F-05: `s` is caller-controlled string-literal bytes.
                        // Render via `{:?}` so CR/LF/NUL/quotes are escaped
                        // before reaching downstream serialization sinks.
                        // `clause` is a static internal label and stays bare.
                        format!("{clause} duration {s:?} is not a valid duration literal"),
                        pos,
                    )
                })
            }
            other => Err(ParseError::new(
                format!("{clause} expects a string duration literal, got {other:?}"),
                pos,
            )),
        }
    }

    /// Parse DROP TIMESERIES body (after DROP TIMESERIES consumed)
    pub fn parse_drop_timeseries_body(&mut self) -> Result<QueryExpr, ParseError> {
        let if_exists = self.match_if_exists()?;
        let name = self.parse_drop_collection_name()?;
        Ok(QueryExpr::DropTimeSeries(DropTimeSeriesQuery {
            name,
            if_exists,
        }))
    }

    /// Parse a duration unit and return the multiplier in milliseconds
    fn parse_duration_unit(&mut self) -> Result<f64, ParseError> {
        // Aggregate-function keywords (`MIN`, `MAX`, `AVG`) lex as
        // dedicated tokens, not `Token::Ident`, so they need their
        // own arms. `MIN` is the minute alias; `MAX` and `AVG` have
        // no canonical duration meaning today but were silently
        // falling through to the seconds default — surface a clear
        // error instead.
        match self.peek().clone() {
            Token::Ident(ref unit) => {
                let mult = match unit.to_ascii_lowercase().as_str() {
                    "ms" | "msec" | "millisecond" | "milliseconds" => 1.0,
                    "s" | "sec" | "secs" | "second" | "seconds" => 1_000.0,
                    "m" | "min" | "mins" | "minute" | "minutes" => 60_000.0,
                    "h" | "hr" | "hrs" | "hour" | "hours" => 3_600_000.0,
                    "d" | "day" | "days" => 86_400_000.0,
                    other => {
                        return Err(ParseError::new(
                            // F-05: `other` is caller-controlled identifier
                            // text. Render via `{:?}` so embedded CR/LF/NUL/
                            // quotes are escaped before the message reaches
                            // downstream serialization sinks.
                            format!("unknown duration unit {other:?}, expected s/m/h/d"),
                            self.position(),
                        ));
                    }
                };
                self.advance()?;
                Ok(mult)
            }
            Token::Min => {
                // `MIN` keyword used as the minute alias.
                self.advance()?;
                Ok(60_000.0)
            }
            Token::Max | Token::Avg => {
                // These keywords have no duration semantics; reject
                // explicitly so a stray aggregate keyword does not
                // silently default to seconds.
                let kw = self.peek().clone();
                Err(ParseError::new(
                    format!("unknown duration unit '{}', expected s/m/h/d", kw),
                    self.position(),
                ))
            }
            _ => Ok(1_000.0), // default: seconds
        }
    }

    fn parse_downsample_policy_spec(&mut self) -> Result<String, ParseError> {
        let target = self.parse_resolution_spec()?;
        self.expect(Token::Colon)?;
        let source = self.parse_resolution_spec()?;
        let aggregation = if self.consume(&Token::Colon)? {
            self.expect_ident_or_keyword()?.to_ascii_lowercase()
        } else {
            "avg".to_string()
        };
        Ok(format!("{target}:{source}:{aggregation}"))
    }

    fn parse_resolution_spec(&mut self) -> Result<String, ParseError> {
        match self.peek().clone() {
            Token::Ident(value) if value.eq_ignore_ascii_case("raw") => {
                self.advance()?;
                Ok(value.to_ascii_lowercase())
            }
            Token::Integer(value) => {
                self.advance()?;
                let unit = self.expect_ident_or_keyword()?.to_ascii_lowercase();
                Ok(format!("{value}{unit}"))
            }
            Token::Float(value) => {
                self.advance()?;
                let unit = self.expect_ident_or_keyword()?.to_ascii_lowercase();
                let number = if value.fract().abs() < f64::EPSILON {
                    format!("{}", value as i64)
                } else {
                    value.to_string()
                };
                Ok(format!("{number}{unit}"))
            }
            other => Err(ParseError::new(
                format!(
                    "expected duration literal for downsample policy, got {}",
                    other
                ),
                self.position(),
            )),
        }
    }
}
