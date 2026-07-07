//! Parser for CREATE/DROP TIMESERIES

use super::error::ParseError;
use super::Parser;
use crate::ast::{
    CreateSloQuery, CreateTableQuery, CreateTimeSeriesQuery, DropTimeSeriesQuery, HypertableDdl,
    QueryExpr,
};
use crate::lexer::Token;
use reddb_types::catalog::CollectionModel;

impl<'a> Parser<'a> {
    /// Parse CREATE TIMESERIES body (after CREATE TIMESERIES consumed)
    pub fn parse_create_timeseries_body(&mut self) -> Result<QueryExpr, ParseError> {
        let if_not_exists = self.match_if_not_exists()?;
        let name = self.expect_ident()?;

        let mut retention_ms = None;
        let mut chunk_size = None;
        let mut downsample_policies = Vec::new();
        let mut session_key: Option<String> = None;
        let mut session_gap_ms: Option<u64> = None;
        let mut columnar = true;

        // Parse optional clauses in any order
        loop {
            if self.consume(&Token::Retention)? {
                let value = self.parse_float()?;
                let unit = self.parse_duration_unit()?;
                retention_ms = Some((value * unit) as u64);
            } else if self.consume_ident_ci("CHUNK_SIZE")? || self.consume_ident_ci("CHUNKSIZE")? {
                chunk_size = Some(self.parse_integer()? as usize);
            } else if self.consume_ident_ci("NO")? {
                if !self.consume_ident_ci("COLUMNAR")? {
                    return Err(ParseError::expected(
                        vec!["COLUMNAR"],
                        self.peek(),
                        self.position(),
                    ));
                }
                columnar = false;
            } else if self.consume_ident_ci("COLUMNAR")? {
                return Err(retired_columnar_keyword_error(self.position()));
            } else if self.consume_ident_ci("DOWNSAMPLE")? {
                downsample_policies.push(self.parse_downsample_policy_spec()?);
                while self.consume(&Token::Comma)? {
                    downsample_policies.push(self.parse_downsample_policy_spec()?);
                }
            } else if self.consume(&Token::With)? {
                // `WITH SESSION_KEY <col> SESSION_GAP <duration>` — both
                // clauses are paired so the SESSIONIZE operator (slice
                // 2+) has a complete default. Order is fixed
                // (SESSION_KEY first) to keep the grammar simple; one
                // without the other is a parse error.
                self.parse_with_session_clause(&mut session_key, &mut session_gap_ms)?;
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
            session_key,
            session_gap_ms,
            columnar,
        }))
    }

    /// Parse `SESSION_KEY <ident> SESSION_GAP <duration>` after a
    /// `WITH` token has been consumed. Both clauses are required; a
    /// SESSION_KEY without a SESSION_GAP (or vice-versa) is rejected
    /// at parse time so the descriptor never carries half a pairing.
    fn parse_with_session_clause(
        &mut self,
        session_key: &mut Option<String>,
        session_gap_ms: &mut Option<u64>,
    ) -> Result<(), ParseError> {
        if !self.consume_ident_ci("SESSION_KEY")? {
            return Err(ParseError::new(
                "expected SESSION_KEY after WITH on CREATE TIMESERIES".to_string(),
                self.position(),
            ));
        }
        let key = self.expect_ident()?;
        if !self.consume_ident_ci("SESSION_GAP")? {
            return Err(ParseError::new(
                "WITH SESSION_KEY requires a paired SESSION_GAP <duration>".to_string(),
                self.position(),
            ));
        }
        let value = self.parse_float()?;
        let unit = self.parse_duration_unit()?;
        *session_key = Some(key);
        *session_gap_ms = Some((value * unit) as u64);
        Ok(())
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
            analytics_config: Vec::new(),
            vault_own_master_key: false,
            ai_policy: None,
        }))
    }

    /// Parse CREATE METRIC body (after CREATE METRIC consumed).
    pub fn parse_create_metric_body(&mut self) -> Result<QueryExpr, ParseError> {
        let mut path = self.expect_ident_or_keyword()?.to_ascii_lowercase();
        while self.consume(&Token::Dot)? {
            let next = self.expect_ident_or_keyword()?.to_ascii_lowercase();
            path = format!("{path}.{next}");
        }

        let mut kind = None;
        let mut role = None;
        let mut source: Option<String> = None;
        let mut query: Option<String> = None;
        let mut window_ms: Option<u64> = None;
        let mut time_field: Option<String> = None;
        loop {
            if self.consume_ident_ci("TYPE")? || self.consume_ident_ci("KIND")? {
                kind = Some(self.expect_ident_or_keyword()?.to_ascii_lowercase());
            } else if self.consume_ident_ci("ROLE")? {
                role = Some(self.expect_ident_or_keyword()?.to_ascii_lowercase());
            } else if self.consume_ident_ci("SOURCE")? {
                source = Some(self.expect_ident_or_keyword()?);
            } else if self.consume_ident_ci("QUERY")? {
                let value = self.parse_literal_value()?;
                match value {
                    reddb_types::types::Value::Text(s) => query = Some(s.to_string()),
                    other => {
                        return Err(ParseError::new(
                            format!("derived metric QUERY expects a string literal, got {other:?}"),
                            self.position(),
                        ));
                    }
                }
            } else if self.consume_ident_ci("WINDOW")? {
                let value = self.parse_float()?;
                let unit = self.parse_duration_unit()?;
                window_ms = Some((value * unit) as u64);
            } else if self.consume_ident_ci("TIME_FIELD")? {
                time_field = Some(self.expect_ident_or_keyword()?);
            } else {
                break;
            }
        }

        Ok(QueryExpr::CreateMetric(crate::ast::CreateMetricQuery {
            path,
            kind: kind.ok_or_else(|| {
                ParseError::new(
                    "metric descriptor requires TYPE or KIND".to_string(),
                    self.position(),
                )
            })?,
            role: role.ok_or_else(|| {
                ParseError::new(
                    "metric descriptor requires ROLE".to_string(),
                    self.position(),
                )
            })?,
            source,
            query,
            window_ms,
            time_field,
        }))
    }

    /// Parse ALTER METRIC body (after ALTER METRIC consumed).
    ///
    /// Grammar:
    ///   ALTER METRIC <dotted.path> SET ROLE <ident>
    ///   ALTER METRIC <dotted.path> SET KIND <ident>      -- rejected at runtime
    ///   ALTER METRIC <dotted.path> SET TYPE <ident>      -- rejected at runtime
    ///   ALTER METRIC <dotted.path> SET PATH <dotted>     -- rejected at runtime
    ///
    /// Immutable-field attempts parse so the runtime can return a
    /// structured "field X cannot be changed" error explaining *why*.
    pub fn parse_alter_metric_body(&mut self) -> Result<QueryExpr, ParseError> {
        let mut path = self.expect_ident_or_keyword()?.to_ascii_lowercase();
        while self.consume(&Token::Dot)? {
            let next = self.expect_ident_or_keyword()?.to_ascii_lowercase();
            path = format!("{path}.{next}");
        }

        if !self.consume(&Token::Set)? && !self.consume_ident_ci("SET")? {
            return Err(ParseError::expected(
                vec!["SET"],
                self.peek(),
                self.position(),
            ));
        }

        let mut set_role = None;
        let mut attempted_kind = None;
        let mut attempted_path = None;

        if self.consume_ident_ci("ROLE")? {
            set_role = Some(self.expect_ident_or_keyword()?.to_ascii_lowercase());
        } else if self.consume_ident_ci("KIND")? || self.consume_ident_ci("TYPE")? {
            attempted_kind = Some(self.expect_ident_or_keyword()?.to_ascii_lowercase());
        } else if self.consume(&Token::Path)? || self.consume_ident_ci("PATH")? {
            let mut new_path = self.expect_ident_or_keyword()?.to_ascii_lowercase();
            while self.consume(&Token::Dot)? {
                let next = self.expect_ident_or_keyword()?.to_ascii_lowercase();
                new_path = format!("{new_path}.{next}");
            }
            attempted_path = Some(new_path);
        } else {
            return Err(ParseError::expected(
                vec!["ROLE", "KIND", "TYPE", "PATH"],
                self.peek(),
                self.position(),
            ));
        }

        Ok(QueryExpr::AlterMetric(crate::ast::AlterMetricQuery {
            path,
            set_role,
            attempted_kind,
            attempted_path,
        }))
    }

    /// Parse CREATE SLO body (after CREATE SLO consumed).
    ///
    /// Grammar:
    ///   CREATE SLO <dotted.path>
    ///     ON <metric.dotted.path>
    ///     TARGET <number>
    ///     WINDOW <number> <duration_unit>
    ///
    /// Clauses are positional after the SLO path so the grammar stays
    /// tight; the parser leaves semantic validation (metric exists, role
    /// = sli, target in range) to the runtime catalog where the error
    /// wording can reference the live catalog state.
    pub fn parse_create_slo_body(&mut self) -> Result<QueryExpr, ParseError> {
        let mut path = self.expect_ident_or_keyword()?.to_ascii_lowercase();
        while self.consume(&Token::Dot)? {
            let next = self.expect_ident_or_keyword()?.to_ascii_lowercase();
            path = format!("{path}.{next}");
        }

        if !self.consume(&Token::On)? {
            return Err(ParseError::expected(
                vec!["ON"],
                self.peek(),
                self.position(),
            ));
        }

        let mut metric_path = self.expect_ident_or_keyword()?.to_ascii_lowercase();
        while self.consume(&Token::Dot)? {
            let next = self.expect_ident_or_keyword()?.to_ascii_lowercase();
            metric_path = format!("{metric_path}.{next}");
        }

        let mut target: Option<f64> = None;
        let mut window_ms: Option<u64> = None;

        loop {
            if self.consume_ident_ci("TARGET")? {
                target = Some(self.parse_float()?);
            } else if self.consume_ident_ci("WINDOW")? {
                let value = self.parse_float()?;
                let unit = self.parse_duration_unit()?;
                window_ms = Some((value * unit) as u64);
            } else {
                break;
            }
        }

        Ok(QueryExpr::CreateSlo(CreateSloQuery {
            path,
            metric_path,
            target: target.ok_or_else(|| {
                ParseError::new(
                    "SLO descriptor requires TARGET <number>".to_string(),
                    self.position(),
                )
            })?,
            window_ms: window_ms.ok_or_else(|| {
                ParseError::new(
                    "SLO descriptor requires WINDOW <duration>".to_string(),
                    self.position(),
                )
            })?,
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
        let mut columnar = true;

        loop {
            if self.consume_ident_ci("TIME_COLUMN")? {
                time_column = Some(self.expect_ident()?);
            } else if self.consume_ident_ci("CHUNK_INTERVAL")? {
                chunk_interval_ns = Some(self.parse_duration_ns_literal("CHUNK_INTERVAL")?);
            } else if self.consume_ident_ci("NO")? {
                if !self.consume_ident_ci("COLUMNAR")? {
                    return Err(ParseError::expected(
                        vec!["COLUMNAR"],
                        self.peek(),
                        self.position(),
                    ));
                }
                columnar = false;
            } else if self.consume_ident_ci("COLUMNAR")? {
                return Err(retired_columnar_keyword_error(self.position()));
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
            session_key: None,
            session_gap_ms: None,
            columnar,
        }))
    }

    /// Accept a string-literal duration (`'1d'`, `'5m'`, `'30s'`, …) and
    /// resolve it to nanoseconds using the shared retention grammar.
    fn parse_duration_ns_literal(&mut self, clause: &str) -> Result<u64, ParseError> {
        let pos = self.position();
        let value = self.parse_literal_value()?;
        match value {
            reddb_types::types::Value::Text(s) => {
                reddb_types::duration::parse_duration_ns(&s).ok_or_else(|| {
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
    pub fn parse_duration_unit(&mut self) -> Result<f64, ParseError> {
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

fn retired_columnar_keyword_error(position: crate::lexer::Position) -> ParseError {
    ParseError::new(
        "COLUMNAR is no longer accepted; columnar projection is automatic for in-scope \
         collections, use NO COLUMNAR to opt out"
            .to_string(),
        position,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use reddb_types::catalog::CollectionModel;

    fn parse_query(input: &str) -> Result<QueryExpr, ParseError> {
        crate::parser::parse(input).map(|query| query.query)
    }

    #[test]
    fn create_timeseries_accepts_clause_order_and_defaults_to_columnar() {
        let query = parse_query(
            "CREATE TIMESERIES IF NOT EXISTS readings DOWNSAMPLE 1h:raw \
             RETENTION 2 h CHUNKSIZE 64",
        )
        .unwrap();

        let QueryExpr::CreateTimeSeries(timeseries) = query else {
            panic!("expected create timeseries");
        };
        assert_eq!(timeseries.name, "readings");
        assert!(timeseries.if_not_exists);
        assert!(timeseries.columnar);
        assert_eq!(timeseries.retention_ms, Some(2 * 3_600_000));
        assert_eq!(timeseries.chunk_size, Some(64));
        assert_eq!(timeseries.downsample_policies, vec!["1h:raw:avg"]);
        assert_eq!(timeseries.session_key, None);
        assert_eq!(timeseries.session_gap_ms, None);
        assert!(timeseries.hypertable.is_none());
    }

    #[test]
    fn create_timeseries_accepts_no_columnar_opt_out() {
        let query = parse_query("CREATE TIMESERIES readings NO COLUMNAR").unwrap();
        let QueryExpr::CreateTimeSeries(timeseries) = query else {
            panic!("expected create timeseries");
        };
        assert!(!timeseries.columnar);
    }

    #[test]
    fn create_timeseries_rejects_retired_columnar_keyword() {
        for sql in [
            "CREATE TIMESERIES readings COLUMNAR",
            "CREATE HYPERTABLE readings TIME_COLUMN ts CHUNK_INTERVAL '1h' COLUMNAR",
        ] {
            let err = parse_query(sql)
                .expect_err("COLUMNAR is retired")
                .to_string();
            assert!(
                err.contains("COLUMNAR is no longer accepted"),
                "{sql}: {err}"
            );
            assert!(err.contains("automatic"), "{sql}: {err}");
        }
    }

    #[test]
    fn create_metrics_sets_collection_defaults_and_optional_clauses() {
        let query = parse_query(
            "CREATE METRICS IF NOT EXISTS telemetry RETENTION 30 m \
             DOWNSAMPLE 5m:raw:max TENANT BY (ctx.tenant)",
        )
        .unwrap();

        let QueryExpr::CreateTable(metrics) = query else {
            panic!("expected metrics collection");
        };
        assert_eq!(metrics.collection_model, CollectionModel::Metrics);
        assert_eq!(metrics.name, "telemetry");
        assert!(metrics.if_not_exists);
        assert_eq!(metrics.default_ttl_ms, Some(30 * 60_000));
        assert_eq!(metrics.metrics_rollup_policies, vec!["5m:raw:max"]);
        assert_eq!(metrics.tenant_by.as_deref(), Some("ctx.tenant"));
        assert!(metrics.append_only);
        assert!(metrics.columns.is_empty());
    }

    #[test]
    fn create_metric_alter_metric_and_slo_parse_descriptor_forms() {
        let query = parse_query(
            "CREATE METRIC Svc.Latency.P99 TYPE gauge ROLE sli SOURCE rollups \
             QUERY 'SELECT p99 FROM rollups' WINDOW 5 min TIME_FIELD observed_at",
        )
        .unwrap();
        let QueryExpr::CreateMetric(metric) = query else {
            panic!("expected create metric");
        };
        assert_eq!(metric.path, "svc.latency.p99");
        assert_eq!(metric.kind, "gauge");
        assert_eq!(metric.role, "sli");
        assert_eq!(metric.source.as_deref(), Some("rollups"));
        assert_eq!(metric.query.as_deref(), Some("SELECT p99 FROM rollups"));
        assert_eq!(metric.window_ms, Some(5 * 60_000));
        assert_eq!(metric.time_field.as_deref(), Some("observed_at"));

        let query = parse_query("ALTER METRIC Svc.Latency.P99 SET PATH svc.latency.p95").unwrap();
        let QueryExpr::AlterMetric(alter) = query else {
            panic!("expected alter metric");
        };
        assert_eq!(alter.path, "svc.latency.p99");
        assert_eq!(alter.set_role, None);
        assert_eq!(alter.attempted_kind, None);
        assert_eq!(alter.attempted_path.as_deref(), Some("svc.latency.p95"));

        let query =
            parse_query("CREATE SLO Api.Availability ON Svc.Latency.P99 TARGET 0.999 WINDOW 28 d")
                .unwrap();
        let QueryExpr::CreateSlo(slo) = query else {
            panic!("expected create slo");
        };
        assert_eq!(slo.path, "api.availability");
        assert_eq!(slo.metric_path, "svc.latency.p99");
        assert!((slo.target - 0.999).abs() < f64::EPSILON);
        assert_eq!(slo.window_ms, 28 * 86_400_000);
    }

    #[test]
    fn create_hypertable_and_drop_timeseries_parse_variants() {
        let query = parse_query(
            "CREATE HYPERTABLE IF NOT EXISTS events TIME_COLUMN ts \
             CHUNK_INTERVAL '30m' TTL '10s' RETENTION 1 h",
        )
        .unwrap();
        let QueryExpr::CreateTimeSeries(timeseries) = query else {
            panic!("expected hypertable as timeseries");
        };
        let hypertable = timeseries.hypertable.expect("hypertable ddl");
        assert_eq!(timeseries.name, "events");
        assert!(timeseries.if_not_exists);
        assert!(timeseries.columnar);
        assert_eq!(timeseries.retention_ms, Some(3_600_000));
        assert_eq!(hypertable.time_column, "ts");
        assert_eq!(hypertable.chunk_interval_ns, 30 * 60 * 1_000_000_000);
        assert_eq!(hypertable.default_ttl_ns, Some(10 * 1_000_000_000));

        let query = parse_query(
            "CREATE HYPERTABLE cold_events TIME_COLUMN ts CHUNK_INTERVAL '1h' NO COLUMNAR",
        )
        .unwrap();
        let QueryExpr::CreateTimeSeries(timeseries) = query else {
            panic!("expected hypertable as timeseries");
        };
        assert!(!timeseries.columnar);

        let query = parse_query("DROP TIMESERIES IF EXISTS tenant.metrics.*").unwrap();
        assert!(matches!(
            query,
            QueryExpr::DropTimeSeries(drop) if drop.name == "tenant.metrics.*" && drop.if_exists
        ));
    }

    #[test]
    fn timeseries_metric_and_slo_errors_are_reported() {
        for sql in [
            "CREATE TIMESERIES events WITH RETENTION 1 d",
            "CREATE METRICS telemetry TENANT (ctx.tenant)",
            "CREATE METRIC svc.latency TYPE gauge",
            "CREATE METRIC svc.latency TYPE gauge ROLE sli QUERY 42",
            "ALTER METRIC svc.latency ROLE sli",
            "CREATE SLO api.availability ON svc.latency WINDOW 1 h",
            "CREATE HYPERTABLE events TIME_COLUMN ts CHUNK_INTERVAL 'not-duration'",
        ] {
            assert!(parse_query(sql).is_err(), "{sql} should not parse");
        }
    }
}
