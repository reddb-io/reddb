//! DML SQL Parser: INSERT, UPDATE, DELETE

use super::error::ParseError;
use super::Parser;
use crate::ast::{
    AskCacheClause, AskQuery, BinOp, DeleteQuery, Expr, FieldRef, Filter, InsertEntityType,
    InsertQuery, OrderByClause, QueryExpr, ReturningItem, UpdateQuery, UpdateTarget,
};
use crate::lexer::Token;
use crate::sql_lowering::{filter_to_expr, fold_expr_to_value};
use reddb_types::types::Value;

/// Maximum nesting depth for JSON object literals — shared constant
/// that now lives in the crate-level [`crate::limits`] module so every
/// depth cap is co-located with [`crate::limits::ParserLimits`].
pub(crate) use crate::limits::JSON_LITERAL_MAX_DEPTH;

/// Walk a parsed `JsonValue` tree and bail out if nesting exceeds
/// `JSON_LITERAL_MAX_DEPTH`. Iterative to avoid the very stack
/// overflow we're trying to prevent.
pub(crate) fn json_literal_depth_check(
    value: &reddb_types::utils::json::JsonValue,
) -> Result<(), String> {
    use reddb_types::utils::json::JsonValue;
    let mut stack: Vec<(&JsonValue, u32)> = vec![(value, 1)];
    while let Some((node, depth)) = stack.pop() {
        if depth > JSON_LITERAL_MAX_DEPTH {
            return Err(format!(
                "JSON object literal exceeds JSON_LITERAL_MAX_DEPTH ({})",
                JSON_LITERAL_MAX_DEPTH
            ));
        }
        match node {
            JsonValue::Object(entries) => {
                for (_, v) in entries {
                    stack.push((v, depth + 1));
                }
            }
            JsonValue::Array(items) => {
                for v in items {
                    stack.push((v, depth + 1));
                }
            }
            _ => {}
        }
    }
    Ok(())
}

impl<'a> Parser<'a> {
    /// Parse: INSERT INTO table [NODE|EDGE|VECTOR|DOCUMENT|KV] (col1, col2) VALUES (val1, val2), (val3, val4) [RETURNING]
    pub fn parse_insert_query(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::Insert)?;
        self.expect(Token::Into)?;
        // Issue #789 — Analytics v0 explicitly excludes `INSERT INTO
        // METRIC <path>` as a raw write path (PRD #782 non-goal). Raw
        // samples land in ordinary RedDB collections; the metric
        // descriptor catalog is reached through `CREATE METRIC` and
        // `red.analytics.metrics`. Reject the form here before the
        // identifier slot so the error names the actual reason, not a
        // generic "expected identifier".
        if matches!(self.peek(), Token::Metric) {
            return Err(ParseError::new(
                "INSERT INTO METRIC is not supported in Analytics v0 — \
                 write raw samples into an ordinary TABLE/DOCUMENT \
                 collection; the metric descriptor catalog is reached \
                 via CREATE METRIC and red.analytics.metrics \
                 (PRD #782 non-goal)",
                self.position(),
            ));
        }
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

        // Parse column list. ADR 0067 (#1709): the document INSERT column
        // list is removed — the canonical form is bare
        // `VALUES (<json-literal>)`. For documents we synthesize the
        // implicit `body` column so the runtime document path is unchanged,
        // and reject the legacy `(body)` / `_ttl` column lists with a
        // didactic error. Every other entity type keeps its mandatory
        // column list.
        let columns = if matches!(entity_type, InsertEntityType::Document) {
            self.parse_document_insert_columns()?
        } else if matches!(entity_type, InsertEntityType::Row) && self.check(&Token::Values) {
            // ADR 0067 (#1710): the bare `INSERT INTO c VALUES (…)` form
            // carries no column list and no model marker. The model is
            // inferred from the catalog at analysis time — an existing
            // document collection routes to document creation — so the
            // parser leaves the column list empty and defers the routing
            // decision to the runtime.
            Vec::new()
        } else {
            self.expect(Token::LParen)?;
            let columns = self.parse_ident_list()?;
            self.expect(Token::RParen)?;
            columns
        };

        // Parse VALUES
        self.expect(Token::Values)?;
        let mut all_values = Vec::new();
        let mut all_value_exprs = Vec::new();
        loop {
            self.expect(Token::LParen)?;
            let row_exprs = self.parse_dml_expr_list()?;
            self.expect(Token::RParen)?;
            // Tolerate `$N` / `?` placeholders in VALUES rows: fold to
            // Value::Null and rely on `user_params::bind` to substitute
            // the caller's values before execution. Issue #355.
            // Tolerate `$N` / `?` placeholders in VALUES rows: if fold
            // fails on an expression that contains `Expr::Parameter`,
            // emit a `Value::Null` placeholder. `user_params::bind`
            // substitutes the caller-supplied value before execution.
            // Issue #355.
            let row_values = row_exprs
                .iter()
                .map(|expr| match fold_expr_to_value(expr.clone()) {
                    Ok(value) => Ok(value),
                    Err(msg) => {
                        if crate::sql_lowering::expr_contains_parameter(expr) {
                            Ok(Value::Null)
                        } else {
                            Err(msg)
                        }
                    }
                })
                .collect::<Result<Vec<_>, _>>()
                .map_err(|msg| ParseError::new(msg, self.position()))?;
            all_value_exprs.push(row_exprs);
            all_values.push(row_values);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }

        // ADR 0067 (#1709): a document body is an inline strict-JSON
        // literal. Reject the removed quoted-string coercion
        // (`VALUES ('{…}')`) with a didactic error naming the inline
        // literal and `JSON_PARSE`.
        if matches!(entity_type, InsertEntityType::Document) {
            self.reject_document_string_body(&all_value_exprs)?;
        }

        // Parse optional WITH clauses
        let (ttl_ms, expires_at_ms, with_metadata, auto_embed) = self.parse_with_clauses()?;

        let returning = self.parse_returning_clause()?;

        let suppress_events = if self.consume_ident_ci("SUPPRESS")? {
            self.expect_ident_ci("EVENTS")?;
            true
        } else {
            false
        };

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
            suppress_events,
        }))
    }

    /// ADR 0067 (#1709): the document INSERT column list is dead. The
    /// canonical form is bare `VALUES (<json-literal>)`, so a document
    /// INSERT carries no column list and we synthesize the implicit
    /// `body` column here for the runtime document path. A leftover `(…)`
    /// column list is rejected with a didactic error: `_ttl` metadata
    /// columns point at `WITH TTL`; anything else (including the old
    /// ceremonial `body`) shows the bare `VALUES` form.
    fn parse_document_insert_columns(&mut self) -> Result<Vec<String>, ParseError> {
        if !self.check(&Token::LParen) {
            return Ok(vec!["body".to_string()]);
        }
        self.expect(Token::LParen)?;
        let columns = self.parse_ident_list()?;
        self.expect(Token::RParen)?;
        if columns.iter().any(|column| is_legacy_ttl_column(column)) {
            return Err(ParseError::document_insert_ttl_column(self.position()));
        }
        Err(ParseError::document_insert_column_list(self.position()))
    }

    /// ADR 0067 (#1709): a document body must be an inline strict-JSON
    /// literal. A bare quoted string literal in a document `VALUES` row is
    /// the removed coercion (`VALUES ('{…}')`) and is rejected with a
    /// didactic error. Parameters (`$N` / `?`) and `JSON_PARSE(<expr>)`
    /// remain valid, so only `Expr::Literal { Value::Text }` is rejected.
    fn reject_document_string_body(&self, value_exprs: &[Vec<Expr>]) -> Result<(), ParseError> {
        for row in value_exprs {
            for expr in row {
                if let Expr::Literal {
                    value: Value::Text(_),
                    ..
                } = expr
                {
                    return Err(ParseError::document_insert_quoted_body(self.position()));
                }
            }
        }
        Ok(())
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
                    // F-05: render `other` via `{:?}` so caller-controlled
                    // bytes (CR / LF / NUL / quotes) are escaped before
                    // landing in the JSON/audit/log/gRPC error sinks.
                    format!(
                        "unsupported TTL unit {other:?}; supported units: ms, s, m, h, d (e.g. `WITH TTL 30 m`)"
                    ),
                    self.position(),
                ));
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
            Option<crate::ast::AutoEmbedConfig>,
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
                // `USING` is a reserved keyword (`Token::Using`), so
                // `consume_ident_ci` would never match. Use the typed
                // consumer instead. See bug #108 (mirrors the #92 fix
                // for migration `DEPENDS ON`).
                // Empty means "no explicit provider" — the runtime resolves
                // it via the embeddings task pointer (ADR-0068 §5). Only an
                // explicit `USING` names a provider here.
                let provider = if self.consume(&Token::Using)? {
                    self.expect_ident()?
                } else {
                    String::new()
                };
                let model = if self.consume_ident_ci("MODEL")? {
                    Some(self.parse_string()?)
                } else {
                    None
                };
                auto_embed = Some(crate::ast::AutoEmbedConfig {
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
                // F-05: `trimmed` is caller-controlled string-literal bytes.
                // Render via `{:?}` so CR/LF/NUL/quotes are escaped before
                // the message reaches the JSON / audit / log / gRPC sinks.
                format!("EXPIRES AT requires a unix timestamp in milliseconds, got {trimmed:?}"),
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
        let target = self.parse_update_target()?;
        self.expect(Token::Set)?;

        let mut assignments = Vec::new();
        let mut assignment_exprs = Vec::new();
        let mut compound_assignment_ops = Vec::new();
        loop {
            let col = self.parse_update_assignment_target()?;
            let compound_op = if self.consume(&Token::Eq)? {
                None
            } else {
                let op = match self.peek() {
                    Token::Plus => BinOp::Add,
                    Token::Dash | Token::Minus => BinOp::Sub,
                    Token::Star => BinOp::Mul,
                    Token::Slash => BinOp::Div,
                    Token::Percent => BinOp::Mod,
                    _ => {
                        return Err(ParseError::expected(
                            vec!["=", "+=", "-=", "*=", "/=", "%="],
                            self.peek(),
                            self.position(),
                        ));
                    }
                };
                self.advance()?;
                self.expect(Token::Eq)?;
                Some(op)
            };
            let expr = self.parse_expr()?;
            let folded = fold_expr_to_value(expr.clone()).ok();
            assignment_exprs.push((col.clone(), expr));
            compound_assignment_ops.push(compound_op);
            if compound_op.is_none() {
                if let Some(val) = folded {
                    assignments.push((col.clone(), val));
                }
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

        let (claim_limit, claim_exact) = if self.consume_ident_ci("CLAIM")? {
            if self.consume_ident_ci("EXACT")? {
                (Some(self.parse_integer()? as u64), true)
            } else {
                self.expect(Token::Limit)?;
                (Some(self.parse_integer()? as u64), false)
            }
        } else {
            (None, false)
        };

        let mut order_by = if self.consume(&Token::Order)? {
            self.expect(Token::By)?;
            let clauses = self.parse_order_by_list()?;
            validate_update_order_by(&clauses, self.position())?;
            clauses
        } else {
            Vec::new()
        };

        // Optional `LIMIT N` — used by `BATCH N ROWS` data migrations
        // to cap a single batch. Must come after WHERE / WITH because
        // those have their own keyword tokens that the LIMIT branch
        // would otherwise mis-consume.
        let limit = if self.consume(&Token::Limit)? {
            Some(self.parse_integer()? as u64)
        } else {
            None
        };
        // CLAIM LIMIT acts as the LIMIT for the purpose of ORDER BY semantics:
        // a claim without a conventional LIMIT still has a deterministic bound.
        let effective_limit = limit.is_some() || claim_limit.is_some();
        if !order_by.is_empty() && !effective_limit {
            return Err(ParseError::new(
                "UPDATE ORDER BY requires LIMIT",
                self.position(),
            ));
        }
        if claim_limit.is_some() && order_by.is_empty() {
            return Err(ParseError::new(
                "UPDATE CLAIM requires ORDER BY",
                self.position(),
            ));
        }
        if !order_by.is_empty() && !update_order_by_mentions_rid(&order_by) {
            order_by.push(OrderByClause {
                field: FieldRef::TableColumn {
                    table: String::new(),
                    column: "rid".to_string(),
                },
                expr: None,
                ascending: true,
                nulls_first: false,
            });
        }

        let returning = self.parse_returning_clause()?;

        let suppress_events = if self.consume_ident_ci("SUPPRESS")? {
            self.expect_ident_ci("EVENTS")?;
            true
        } else {
            false
        };

        Ok(QueryExpr::Update(UpdateQuery {
            table,
            target,
            assignment_exprs,
            compound_assignment_ops,
            assignments,
            where_expr,
            filter,
            ttl_ms,
            expires_at_ms,
            with_metadata,
            returning,
            claim_limit,
            claim_exact,
            order_by,
            limit,
            suppress_events,
        }))
    }

    fn parse_update_assignment_target(&mut self) -> Result<String, ParseError> {
        // Dotted assignment targets (`SET a.b.c = …`) parse for every target
        // (ADR 0067, #1711). Whether a nested path is legal is a model
        // question the parser cannot answer — the analyzer resolves the
        // collection's model from the catalog and rejects dotted targets off a
        // document collection.
        let mut segments = vec![self.expect_column_ident()?];
        while self.consume(&Token::Dot)? {
            segments.push(self.expect_column_ident()?);
        }
        Ok(segments.join("."))
    }

    fn parse_update_target(&mut self) -> Result<UpdateTarget, ParseError> {
        // Model markers on UPDATE are removed (ADR 0067, #1711): the catalog
        // already knows every existing collection's model, so `DOCUMENTS` /
        // `ROWS` / `KV` are redundant and rejected with a didactic error that
        // points at the unmarked form. `NODES` / `EDGES` stay — a graph
        // collection holds both record kinds, so only the user can say which
        // one an UPDATE targets.
        if self.check(&Token::Kv) {
            return Err(self.removed_update_marker_error("KV"));
        }
        if self.check(&Token::Rows) {
            return Err(self.removed_update_marker_error("ROWS"));
        }
        if matches!(self.peek(), Token::Ident(name) if name.eq_ignore_ascii_case("DOCUMENTS")) {
            return Err(self.removed_update_marker_error("DOCUMENTS"));
        }
        if self.consume_ident_ci("NODES")? {
            return Ok(UpdateTarget::Nodes);
        }
        if self.consume_ident_ci("EDGES")? {
            return Ok(UpdateTarget::Edges);
        }
        Ok(UpdateTarget::Rows)
    }

    /// Build the didactic error for a removed UPDATE model marker (ADR 0067,
    /// #1711). The catalog knows the collection's model, so the marker is
    /// redundant; the message names the unmarked form and the graph exception.
    fn removed_update_marker_error(&self, marker: &str) -> ParseError {
        ParseError::new(
            format!(
                "the `{marker}` UPDATE model marker has been removed; the catalog \
                 already knows the collection's model — write `UPDATE <collection> \
                 SET …` with no marker. (Graph updates still name `NODES` or `EDGES`, \
                 since a graph collection holds both record kinds.)"
            ),
            self.position(),
        )
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

        let suppress_events = if self.consume_ident_ci("SUPPRESS")? {
            self.expect_ident_ci("EVENTS")?;
            true
        } else {
            false
        };

        Ok(QueryExpr::Delete(DeleteQuery {
            table,
            where_expr,
            filter,
            returning,
            suppress_events,
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
            if returning_expr_start(self.peek()) {
                return Err(returning_expr_not_supported(self.position()));
            }
            let col = self.expect_update_returning_column()?;
            items.push(ReturningItem::Column(col));
            if returning_expr_tail(self.peek()) {
                return Err(returning_expr_not_supported(self.position()));
            }
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

    fn expect_update_returning_column(&mut self) -> Result<String, ParseError> {
        if self.consume(&Token::Weight)? {
            return Ok("weight".to_string());
        }
        self.expect_ident_or_keyword()
    }

    /// Parse: ASK 'question' [USING provider] [MODEL 'model'] [DEPTH n]
    /// [LIMIT n] [MIN_SCORE x] [COLLECTION col] [AS RQL]
    pub fn parse_ask_query(&mut self) -> Result<QueryExpr, ParseError> {
        self.parse_ask_query_with_explain(false)
    }

    /// Parse: EXPLAIN ASK 'question' ...
    pub fn parse_explain_ask_query(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume EXPLAIN
        if !matches!(self.peek(), Token::Ident(name) if name.eq_ignore_ascii_case("ASK")) {
            return Err(ParseError::expected(
                vec!["ASK"],
                self.peek(),
                self.position(),
            ));
        }
        self.parse_ask_query_with_explain(true)
    }

    fn parse_ask_query_with_explain(&mut self, explain: bool) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume ASK

        let (question, question_param) = match self.peek() {
            Token::String(_) => (self.parse_string()?, None),
            Token::Dollar | Token::Question => {
                let index = self.parse_param_slot("ASK question")?;
                (String::new(), Some(index))
            }
            other => {
                return Err(ParseError::expected(
                    vec!["string", "$N", "?"],
                    other,
                    self.position(),
                ));
            }
        };

        let mut provider = None;
        let mut model = None;
        let mut depth = None;
        let mut limit = None;
        let mut min_score = None;
        let mut collection = None;
        let mut temperature = None;
        let mut seed = None;
        let mut strict = true;
        let mut stream = false;
        let mut cache = AskCacheClause::Default;
        let mut as_rql = false;
        let mut execute = false;
        let mut steps = None;

        // Parse optional clauses in any order. Loop bound = number of
        // clause kinds, so each can appear at most once.
        for _ in 0..15 {
            if self.consume(&Token::Using)? {
                provider = Some(match &self.current.token {
                    Token::String(_) => self.parse_string()?,
                    _ => self.expect_ident()?,
                });
            } else if self.consume_ident_ci("MODEL")? {
                model = Some(self.parse_string()?);
            } else if self.consume(&Token::Depth)? {
                depth = Some(self.parse_integer()? as usize);
            } else if self.consume(&Token::Limit)? {
                limit = Some(self.parse_integer()? as usize);
            } else if self.consume(&Token::MinScore)? {
                min_score = Some(self.parse_float()? as f32);
            } else if self.consume(&Token::Collection)? {
                collection = Some(self.expect_ident()?);
            } else if self.consume_ident_ci("TEMPERATURE")? {
                temperature = Some(self.parse_float()? as f32);
            } else if self.consume_ident_ci("SEED")? {
                seed = Some(self.parse_integer()? as u64);
            } else if self.consume_ident_ci("STRICT")? {
                let value = self.expect_ident_or_keyword()?;
                if value.eq_ignore_ascii_case("ON") {
                    strict = true;
                } else if value.eq_ignore_ascii_case("OFF") {
                    strict = false;
                } else {
                    return Err(ParseError::new(
                        "Expected ON or OFF after STRICT",
                        self.position(),
                    ));
                }
            } else if self.consume_ident_ci("STREAM")? {
                stream = true;
            } else if self.consume_ident_ci("CACHE")? {
                if !matches!(cache, AskCacheClause::Default) {
                    return Err(ParseError::new(
                        "ASK cache clause specified more than once",
                        self.position(),
                    ));
                }
                let ttl = self.expect_ident_or_keyword()?;
                if !ttl.eq_ignore_ascii_case("TTL") {
                    return Err(ParseError::new("Expected TTL after CACHE", self.position()));
                }
                cache = AskCacheClause::CacheTtl(self.parse_string()?);
            } else if self.consume_ident_ci("NOCACHE")? {
                if !matches!(cache, AskCacheClause::Default) {
                    return Err(ParseError::new(
                        "ASK cache clause specified more than once",
                        self.position(),
                    ));
                }
                cache = AskCacheClause::NoCache;
            } else if self.consume(&Token::As)? {
                if as_rql {
                    return Err(ParseError::new(
                        "ASK AS RQL specified more than once",
                        self.position(),
                    ));
                }
                let output = self.expect_ident_or_keyword()?;
                if !output.eq_ignore_ascii_case("RQL") {
                    return Err(ParseError::new(
                        "Expected RQL after ASK AS",
                        self.position(),
                    ));
                }
                as_rql = true;
            } else if self.consume_ident_ci("EXECUTE")? {
                if execute {
                    return Err(ParseError::new(
                        "ASK EXECUTE specified more than once",
                        self.position(),
                    ));
                }
                execute = true;
            } else if self.consume_ident_ci("STEPS")? {
                if steps.is_some() {
                    return Err(ParseError::new(
                        "ASK STEPS specified more than once",
                        self.position(),
                    ));
                }
                let n = self.parse_integer()?;
                if n < 1 {
                    return Err(ParseError::new(
                        "ASK STEPS must be a positive integer",
                        self.position(),
                    ));
                }
                steps = Some(n as usize);
            } else {
                break;
            }
        }

        Ok(QueryExpr::Ask(AskQuery {
            explain,
            question,
            question_param,
            provider,
            model,
            depth,
            limit,
            min_score,
            collection,
            temperature,
            seed,
            strict,
            stream,
            cache,
            as_rql,
            execute,
            steps,
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
        self.parse_dml_expr_list()?
            .into_iter()
            .map(fold_expr_to_value)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|msg| ParseError::new(msg, self.position()))
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
        // Depth guard: this function recurses for nested array `[…]`
        // and object `{…}` literals (see the LBracket / LBrace arms
        // below). Without entering the depth counter, an adversarial
        // payload like `[[[[…(10k×)…]]]]` would overflow the Rust
        // stack BEFORE `ParserLimits::max_depth` fires. The
        // `JsonLiteral` token path uses `json_literal_depth_check`
        // (iterative) — the bare `[`/`{` path needs the recursion
        // counter explicitly.
        self.enter_depth()?;
        let result = self.parse_literal_value_inner();
        self.exit_depth();
        result
    }

    fn parse_literal_value_inner(&mut self) -> Result<Value, ParseError> {
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
            if upper == "SECRET_REF" {
                self.advance()?; // consume ident
                self.expect(Token::LParen)?;
                let store = self.expect_ident_or_keyword()?.to_ascii_lowercase();
                if store != "vault" {
                    return Err(ParseError::expected(
                        vec!["vault"],
                        self.peek(),
                        self.position(),
                    ));
                }
                self.expect(Token::Comma)?;
                let (collection, key) =
                    self.parse_kv_key(reddb_types::catalog::CollectionModel::Vault)?;
                self.expect(Token::RParen)?;
                return Ok(secret_ref_value(&store, &collection, &key));
            }
        }

        match self.peek().clone() {
            Token::String(s) => {
                let s = s.clone();
                self.advance()?;
                Ok(Value::text(s))
            }
            Token::JsonLiteral(raw) => {
                // The lexer already validated brace balance and the
                // 16 MiB payload ceiling. Parse the raw text into a
                // canonical JsonValue then re-encode via `to_vec` so
                // the on-disk bytes match the quoted form.
                self.advance()?;
                let json_value = reddb_types::utils::json::parse_json(&raw).map_err(|err| {
                    ParseError::new(
                        // F-05: render the underlying parse-error string
                        // via `{:?}` so any user fragment serde echoed
                        // back (unexpected character, key text, …) is
                        // Debug-escaped before reaching the downstream
                        // JSON / audit / log / gRPC sinks.
                        format!("invalid JSON object literal: {:?}", err.to_string()),
                        self.position(),
                    )
                })?;
                json_literal_depth_check(&json_value)
                    .map_err(|err| ParseError::new(err, self.position()))?;
                let canonical = reddb_types::serde_json::Value::from(json_value);
                let bytes = reddb_types::json::to_vec(&canonical).map_err(|err| {
                    ParseError::new(
                        // F-05: escape the encoder error via `{:?}` so any
                        // user fragment it carries cannot smuggle control
                        // bytes through downstream serialization sinks.
                        format!("failed to encode JSON literal: {:?}", err.to_string()),
                        self.position(),
                    )
                })?;
                Ok(Value::Json(bytes))
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
                // Parse array literal `[val1, val2, ...]` **losslessly** into a
                // `Value::Array`, preserving each element's integer/float/other
                // identity (issue #1708, ADR 0067). The parser deliberately does
                // NOT decide here whether the array is a vector or a JSON array:
                // committing to an f32 `Value::Vector` at parse time silently
                // corrupts large integers destined for a JSON position (e.g.
                // `[9007199254740993]`). Instead the analyzer/runtime resolves
                // the concrete shape from the target's type — a vector-typed
                // position coerces `Value::Array` → `Value::Vector`, a JSON
                // position coerces it to an exact JSON array.
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
                Ok(Value::Array(items))
            }
            Token::LBrace => {
                // Parse JSON object literal {key: value, ...}
                self.advance()?; // consume '{'
                let mut map = reddb_types::json::Map::new();
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
                        map.insert(key, literal_value_to_json(&val));
                        if !self.consume(&Token::Comma)? {
                            break;
                        }
                    }
                }
                self.expect(Token::RBrace)?;
                let json_val = reddb_types::json::Value::Object(map);
                let bytes = reddb_types::json::to_vec(&json_val).unwrap_or_default();
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

/// ADR 0067 (#1709): the legacy metadata columns whose presence in a
/// document INSERT column list should steer the didactic error toward
/// `WITH TTL`. Mirrors `resolve_sql_ttl_metadata_key` in the server
/// runtime; kept local so the parser has no server dependency.
fn is_legacy_ttl_column(column: &str) -> bool {
    column.eq_ignore_ascii_case("_ttl")
        || column.eq_ignore_ascii_case("_ttl_ms")
        || column.eq_ignore_ascii_case("_expires_at")
}

/// Convert a parsed literal `Value` into a `reddb_types::json::Value`.
///
/// Array literals now parse into `Value::Array` (issue #1708). When an array
/// appears inside a JSON object-literal position (e.g.
/// `{roles: ['edge', 'cache']}`) it must serialise as a JSON array rather than
/// collapsing to `null` the way the old catch-all arm did. Recurses so nested
/// arrays and objects round-trip.
fn literal_value_to_json(val: &Value) -> reddb_types::json::Value {
    match val {
        Value::Null => reddb_types::json::Value::Null,
        Value::Boolean(b) => reddb_types::json::Value::Bool(*b),
        Value::Integer(i) => reddb_types::json::Value::Number(*i as f64),
        Value::Float(f) => reddb_types::json::Value::Number(*f),
        Value::Text(s) => reddb_types::json::Value::String(s.to_string()),
        Value::Json(bytes) => {
            reddb_types::json::from_slice(bytes).unwrap_or(reddb_types::json::Value::Null)
        }
        Value::Array(items) => {
            reddb_types::json::Value::Array(items.iter().map(literal_value_to_json).collect())
        }
        _ => reddb_types::json::Value::Null,
    }
}

fn returning_expr_start(token: &Token) -> bool {
    matches!(
        token,
        Token::Integer(_)
            | Token::Float(_)
            | Token::String(_)
            | Token::JsonLiteral(_)
            | Token::Null
            | Token::True
            | Token::False
            | Token::LParen
            | Token::Minus
            | Token::Question
            | Token::Dollar
    )
}

fn returning_expr_tail(token: &Token) -> bool {
    matches!(
        token,
        Token::LParen
            | Token::Plus
            | Token::Minus
            | Token::Star
            | Token::Slash
            | Token::Percent
            | Token::DoublePipe
            | Token::Pipe
            | Token::Eq
            | Token::Ne
            | Token::Lt
            | Token::Le
            | Token::Gt
            | Token::Ge
            | Token::Dot
            | Token::Colon
    )
}

fn validate_update_order_by(
    clauses: &[OrderByClause],
    position: crate::lexer::Position,
) -> Result<(), ParseError> {
    for clause in clauses {
        if clause.expr.is_some() {
            return Err(ParseError::new(
                "UPDATE ORDER BY only supports top-level fields",
                position,
            ));
        }
        match &clause.field {
            FieldRef::TableColumn { table, column }
                if table.is_empty() && !column.contains('.') => {}
            _ => {
                return Err(ParseError::new(
                    "UPDATE ORDER BY only supports top-level fields",
                    position,
                ));
            }
        }
    }
    Ok(())
}

fn update_order_by_mentions_rid(clauses: &[OrderByClause]) -> bool {
    clauses.iter().any(|clause| {
        matches!(
            &clause.field,
            FieldRef::TableColumn { table, column }
                if table.is_empty() && column.eq_ignore_ascii_case("rid")
        )
    })
}

fn returning_expr_not_supported(position: crate::lexer::Position) -> ParseError {
    ParseError::new(
        "NOT_YET_SUPPORTED: RETURNING expressions are not supported yet; use RETURNING * or named columns. Track a follow-up issue for RETURNING <expr>.",
        position,
    )
}

fn secret_ref_value(store: &str, collection: &str, key: &str) -> Value {
    let mut map = reddb_types::json::Map::new();
    map.insert(
        "type".to_string(),
        reddb_types::json::Value::String("secret_ref".to_string()),
    );
    map.insert(
        "store".to_string(),
        reddb_types::json::Value::String(store.to_string()),
    );
    map.insert(
        "collection".to_string(),
        reddb_types::json::Value::String(collection.to_string()),
    );
    map.insert(
        "key".to_string(),
        reddb_types::json::Value::String(key.to_string()),
    );
    Value::Json(
        reddb_types::json::to_vec(&reddb_types::json::Value::Object(map)).unwrap_or_default(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{InsertEntityType, ReturningItem, UpdateTarget};

    fn make_parser(input: &str) -> Parser<'_> {
        Parser::new(input).expect("lexer")
    }

    fn insert(input: &str) -> InsertQuery {
        let mut parser = make_parser(input);
        let QueryExpr::Insert(query) = parser.parse_insert_query().expect("insert") else {
            panic!("expected insert query");
        };
        query
    }

    fn update(input: &str) -> UpdateQuery {
        let mut parser = make_parser(input);
        let QueryExpr::Update(query) = parser.parse_update_query().expect("update") else {
            panic!("expected update query");
        };
        query
    }

    fn delete(input: &str) -> DeleteQuery {
        let mut parser = make_parser(input);
        let QueryExpr::Delete(query) = parser.parse_delete_query().expect("delete") else {
            panic!("expected delete query");
        };
        query
    }

    fn ask(input: &str) -> AskQuery {
        let mut parser = make_parser(input);
        let QueryExpr::Ask(query) = parser.parse_ask_query().expect("ask") else {
            panic!("expected ask query");
        };
        query
    }

    #[test]
    fn insert_entity_types_with_options_returning_and_suppress_events() {
        let cases = [
            (
                "INSERT INTO items NODE (id) VALUES (1)",
                InsertEntityType::Node,
            ),
            (
                "INSERT INTO items EDGE (id) VALUES (1)",
                InsertEntityType::Edge,
            ),
            (
                "INSERT INTO items VECTOR (id) VALUES (1)",
                InsertEntityType::Vector,
            ),
            (
                "INSERT INTO items DOCUMENT VALUES ({\"id\": 1})",
                InsertEntityType::Document,
            ),
            ("INSERT INTO items KV (id) VALUES (1)", InsertEntityType::Kv),
        ];
        for (input, expected) in cases {
            assert_eq!(insert(input).entity_type, expected, "{input}");
        }

        let query = insert(
            "INSERT INTO docs (id, body) VALUES (1, 'red'), (2, ?) \
             WITH TTL 2 h WITH EXPIRES AT 999 \
             WITH METADATA (source = 'test', score = 3) \
             WITH AUTO EMBED (body, title) USING openai MODEL 'text-embedding-3-small' \
             RETURNING * SUPPRESS EVENTS",
        );
        assert_eq!(query.table, "docs");
        assert_eq!(query.entity_type, InsertEntityType::Row);
        assert_eq!(query.columns, vec!["id", "body"]);
        assert_eq!(query.values.len(), 2);
        assert_eq!(query.value_exprs.len(), 2);
        assert_eq!(query.ttl_ms, Some(7_200_000));
        assert_eq!(query.expires_at_ms, Some(999));
        assert_eq!(query.with_metadata.len(), 2);
        assert_eq!(
            query.returning.as_deref(),
            Some([ReturningItem::All].as_slice())
        );
        let auto_embed = query.auto_embed.expect("auto embed");
        assert_eq!(auto_embed.fields, vec!["body", "title"]);
        assert_eq!(auto_embed.provider, "openai");
        assert_eq!(auto_embed.model.as_deref(), Some("text-embedding-3-small"));
        assert!(query.suppress_events);
    }

    #[test]
    fn insert_rejects_metric_and_bad_with_clause() {
        let mut parser = make_parser("INSERT INTO METRIC cpu.usage (value) VALUES (1)");
        let err = parser
            .parse_insert_query()
            .expect_err("metric insert should fail");
        assert!(err.to_string().contains("INSERT INTO METRIC"));

        let mut parser = make_parser("INSERT INTO docs (id) VALUES (1) WITH TTL 1 fortnight");
        let err = parser
            .parse_insert_query()
            .expect_err("bad ttl unit should fail");
        assert!(err.to_string().contains("unsupported TTL unit"));

        let mut parser = make_parser("INSERT INTO docs (id) VALUES (1) WITH UNKNOWN");
        let err = parser
            .parse_insert_query()
            .expect_err("bad WITH should fail");
        assert!(err.to_string().contains("expected"));
    }

    #[test]
    fn document_insert_canonical_bare_values_form() {
        // Bare inline JSON literal, no column list — the ADR 0067 form.
        let query = insert("INSERT INTO events DOCUMENT VALUES ({\"level\": \"info\"})");
        assert_eq!(query.table, "events");
        assert_eq!(query.entity_type, InsertEntityType::Document);
        assert_eq!(query.columns, vec!["body"]);
        assert_eq!(query.values.len(), 1);
        assert!(matches!(query.values[0][0], Value::Json(_)));

        // Multi-row plus WITH clauses keep working.
        let query = insert(
            "INSERT INTO events DOCUMENT VALUES ({\"a\": 1}), ({\"b\": 2}) \
             WITH TTL 30 s WITH METADATA (source = 'test') RETURNING *",
        );
        assert_eq!(query.columns, vec!["body"]);
        assert_eq!(query.values.len(), 2);
        assert_eq!(query.ttl_ms, Some(30_000));
        assert_eq!(query.with_metadata.len(), 1);
        assert_eq!(
            query.returning.as_deref(),
            Some([ReturningItem::All].as_slice())
        );
    }

    #[test]
    fn unmarked_bare_values_insert_parses_with_empty_columns() {
        // ADR 0067 (#1710): `INSERT INTO c VALUES ({…})` with no column
        // list and no marker parses to a Row insert with an empty column
        // list; the runtime infers the model from the catalog.
        let query = insert("INSERT INTO events VALUES ({\"level\": \"info\"})");
        assert_eq!(query.table, "events");
        assert_eq!(query.entity_type, InsertEntityType::Row);
        assert!(query.columns.is_empty());
        assert_eq!(query.values.len(), 1);
        assert!(matches!(query.values[0][0], Value::Json(_)));

        // Multi-row bare VALUES keeps working.
        let query = insert("INSERT INTO events VALUES ({\"a\": 1}), ({\"b\": 2})");
        assert!(query.columns.is_empty());
        assert_eq!(query.entity_type, InsertEntityType::Row);
        assert_eq!(query.values.len(), 2);

        // An explicit column list is still parsed positionally.
        let query = insert("INSERT INTO t (id) VALUES (1)");
        assert_eq!(query.columns, vec!["id"]);
    }

    #[test]
    fn document_insert_rejects_removed_forms() {
        // `(body)` column list -> didactic bare-VALUES rewrite.
        let mut parser =
            make_parser("INSERT INTO events DOCUMENT (body) VALUES ({\"level\": \"info\"})");
        let err = parser
            .parse_insert_query()
            .expect_err("document column list should fail");
        assert!(err.to_string().contains("column list is removed"), "{err}");

        // Legacy `_ttl` column -> point at WITH TTL.
        let mut parser =
            make_parser("INSERT INTO events DOCUMENT (body, _ttl) VALUES ({\"a\": 1}, 30)");
        let err = parser
            .parse_insert_query()
            .expect_err("legacy _ttl column should fail");
        assert!(err.to_string().contains("WITH TTL"), "{err}");

        // Quoted-string body coercion -> inline literal + JSON_PARSE.
        let mut parser =
            make_parser("INSERT INTO events DOCUMENT VALUES ('{\"level\": \"info\"}')");
        let err = parser
            .parse_insert_query()
            .expect_err("quoted-string body should fail");
        let msg = err.to_string();
        assert!(msg.contains("inline JSON literal"), "{msg}");
        assert!(msg.contains("JSON_PARSE"), "{msg}");
    }

    #[test]
    fn update_targets_compound_assignments_order_limit_returning() {
        // Only NODES/EDGES survive as parse-time markers (ADR 0067, #1711);
        // an unmarked UPDATE parses to the default Rows target and the runtime
        // resolves document/KV semantics from the catalog.
        let cases = [
            ("UPDATE docs SET count = 1", UpdateTarget::Rows),
            ("UPDATE docs NODES SET count = 1", UpdateTarget::Nodes),
            ("UPDATE docs EDGES SET count = 1", UpdateTarget::Edges),
        ];
        for (input, expected) in cases {
            assert_eq!(update(input).target, expected, "{input}");
        }

        // The removed markers are rejected with a didactic error.
        for marker in ["DOCUMENTS", "ROWS", "KV"] {
            let input = format!("UPDATE docs {marker} SET count = 1");
            let mut parser = make_parser(&input);
            let err = parser
                .parse_update_query()
                .expect_err("removed update marker should fail");
            let msg = err.to_string();
            assert!(msg.contains(marker), "{msg}");
            assert!(msg.contains("has been removed"), "{msg}");
            assert!(msg.contains("with no marker"), "{msg}");
        }

        let query = update(
            "UPDATE docs SET count += 2, title = UPPER(title) \
             WHERE id = 1 WITH TTL 30 s WITH METADATA (source = 'update') \
             ORDER BY updated_at DESC LIMIT 5 RETURNING weight, title SUPPRESS EVENTS",
        );
        assert_eq!(query.table, "docs");
        assert_eq!(query.target, UpdateTarget::Rows);
        assert_eq!(query.assignment_exprs.len(), 2);
        assert_eq!(query.compound_assignment_ops, vec![Some(BinOp::Add), None]);
        assert_eq!(query.assignments.len(), 0);
        assert!(query.filter.is_some());
        assert!(query.where_expr.is_some());
        assert_eq!(query.ttl_ms, Some(30_000));
        assert_eq!(query.with_metadata.len(), 1);
        assert_eq!(query.claim_limit, None);
        assert!(!query.claim_exact);
        assert_eq!(query.limit, Some(5));
        assert_eq!(query.order_by.len(), 2);
        assert!(matches!(
            &query.order_by[1].field,
            FieldRef::TableColumn { column, .. } if column == "rid"
        ));
        assert_eq!(
            query.returning.as_deref(),
            Some(
                [
                    ReturningItem::Column("weight".to_string()),
                    ReturningItem::Column("title".to_string())
                ]
                .as_slice()
            )
        );
        assert!(query.suppress_events);
    }

    #[test]
    fn update_dotted_targets_parse_unmarked_for_every_target() {
        // Dotted assignment targets parse for the unmarked (Rows) target and
        // for graph targets alike (ADR 0067, #1711); legality is an analyzer
        // concern, not a parser one.
        let unmarked = update("UPDATE docs SET profile.address.city = 'Lisbon' WHERE name = 'ada'");
        assert_eq!(unmarked.target, UpdateTarget::Rows);
        assert_eq!(unmarked.assignment_exprs[0].0, "profile.address.city");
        assert_eq!(unmarked.assignments[0].0, "profile.address.city");

        let graph = update("UPDATE social NODES SET meta.seen_at = 1");
        assert_eq!(graph.target, UpdateTarget::Nodes);
        assert_eq!(graph.assignment_exprs[0].0, "meta.seen_at");
    }

    #[test]
    fn update_claim_limit_requires_order_by() {
        let query = update(
            "UPDATE docs SET status = 'reserved' WHERE status = 'available' \
             CLAIM LIMIT 2 ORDER BY priority ASC RETURNING id",
        );
        assert_eq!(query.claim_limit, Some(2));
        assert!(!query.claim_exact);
        assert_eq!(query.limit, None);
        assert_eq!(query.order_by.len(), 2);
        assert!(matches!(
            &query.order_by[1].field,
            FieldRef::TableColumn { column, .. } if column == "rid"
        ));

        let query = update(
            "UPDATE docs SET status = 'reserved' WHERE status = 'available' \
             CLAIM EXACT 2 ORDER BY priority ASC RETURNING id",
        );
        assert_eq!(query.claim_limit, Some(2));
        assert!(query.claim_exact);
        assert_eq!(query.limit, None);
        assert_eq!(query.order_by.len(), 2);
        assert!(matches!(
            &query.order_by[1].field,
            FieldRef::TableColumn { column, .. } if column == "rid"
        ));

        let mut parser = make_parser("UPDATE docs SET status = 'reserved' CLAIM LIMIT 1");
        let err = parser
            .parse_update_query()
            .expect_err("claim without order should fail");
        assert!(err.to_string().contains("CLAIM requires ORDER BY"));
    }

    #[test]
    fn update_rejects_invalid_assignment_and_order_by_forms() {
        let mut parser = make_parser("UPDATE docs SET count ^= 1");
        let err = parser
            .parse_update_query()
            .expect_err("unknown compound assignment should fail");
        assert!(err.to_string().contains("expected"));

        let mut parser = make_parser("UPDATE docs SET count = 1 ORDER BY updated_at");
        let err = parser
            .parse_update_query()
            .expect_err("ORDER BY without LIMIT should fail");
        assert!(err.to_string().contains("requires LIMIT"));

        let mut parser = make_parser("UPDATE docs SET count = 1 ORDER BY updated_at + 1 LIMIT 1");
        let err = parser
            .parse_update_query()
            .expect_err("ORDER BY expression should fail");
        assert!(err.to_string().contains("top-level fields"));
    }

    #[test]
    fn delete_returning_and_suppress_events() {
        let query = delete("DELETE FROM docs WHERE id = 1 RETURNING id, title SUPPRESS EVENTS");
        assert_eq!(query.table, "docs");
        assert!(query.filter.is_some());
        assert!(query.where_expr.is_some());
        assert_eq!(
            query.returning.as_deref(),
            Some(
                [
                    ReturningItem::Column("id".to_string()),
                    ReturningItem::Column("title".to_string())
                ]
                .as_slice()
            )
        );
        assert!(query.suppress_events);

        let query = delete("DELETE FROM docs RETURNING *");
        assert_eq!(
            query.returning.as_deref(),
            Some([ReturningItem::All].as_slice())
        );
    }

    #[test]
    fn returning_rejects_expression_forms() {
        for input in [
            "DELETE FROM docs RETURNING 1",
            "DELETE FROM docs RETURNING UPPER(title)",
            "DELETE FROM docs RETURNING title || body",
        ] {
            let mut parser = make_parser(input);
            let err = parser
                .parse_delete_query()
                .expect_err("RETURNING expression should fail");
            assert!(err.to_string().contains("RETURNING expressions"));
        }
    }

    #[test]
    fn ask_parses_all_optional_clauses_and_cache_modes() {
        let query = ask(
            "ASK 'what changed?' USING 'openai' MODEL 'gpt' DEPTH 3 LIMIT 4 \
             MIN_SCORE 0.7 COLLECTION docs TEMPERATURE 0.2 SEED 42 STRICT OFF \
             STREAM CACHE TTL '10m'",
        );
        assert_eq!(query.question, "what changed?");
        assert_eq!(query.provider.as_deref(), Some("openai"));
        assert_eq!(query.model.as_deref(), Some("gpt"));
        assert_eq!(query.depth, Some(3));
        assert_eq!(query.limit, Some(4));
        assert_eq!(query.min_score, Some(0.7));
        assert_eq!(query.collection.as_deref(), Some("docs"));
        assert_eq!(query.temperature, Some(0.2));
        assert_eq!(query.seed, Some(42));
        assert!(!query.strict);
        assert!(query.stream);
        assert_eq!(query.cache, AskCacheClause::CacheTtl("10m".to_string()));

        let query = ask("ASK ? NOCACHE");
        assert_eq!(query.question, "");
        assert_eq!(query.question_param, Some(0));
        assert_eq!(query.cache, AskCacheClause::NoCache);
    }

    #[test]
    fn ask_parses_steps_budget_clause() {
        // Absent by default — the runtime falls back to the config cap.
        assert_eq!(ask("ASK 'q'").steps, None);

        // A positive STEPS N is captured verbatim (clamping to the config
        // cap happens later, in the planner).
        let query = ask("ASK 'q' STEPS 2 STRICT OFF");
        assert_eq!(query.steps, Some(2));

        // STEPS composes with the other clauses in any order.
        let query = ask("ASK 'q' DEPTH 3 STEPS 5 LIMIT 4");
        assert_eq!(query.steps, Some(5));
        assert_eq!(query.depth, Some(3));
        assert_eq!(query.limit, Some(4));
    }

    #[test]
    fn ask_steps_clause_error_paths() {
        let mut parser = make_parser("ASK 'q' STEPS 0");
        let err = parser.parse_ask_query().expect_err("STEPS 0 should fail");
        assert!(
            err.to_string()
                .contains("ASK STEPS must be a positive integer"),
            "got: {err}"
        );

        let mut parser = make_parser("ASK 'q' STEPS 2 STEPS 3");
        let err = parser
            .parse_ask_query()
            .expect_err("duplicate STEPS should fail");
        assert!(
            err.to_string()
                .contains("ASK STEPS specified more than once"),
            "got: {err}"
        );

        let mut parser = make_parser("ASK 'q' STEPS abc");
        let err = parser
            .parse_ask_query()
            .expect_err("non-integer STEPS should fail");
        assert!(
            err.to_string().to_ascii_lowercase().contains("integer"),
            "got: {err}"
        );
    }

    #[test]
    fn explain_ask_and_ask_error_paths() {
        let mut parser = make_parser("EXPLAIN ASK $2 STRICT ON");
        let QueryExpr::Ask(query) = parser.parse_explain_ask_query().expect("explain ask") else {
            panic!("expected ask query");
        };
        assert!(query.explain);
        assert_eq!(query.question_param, Some(1));
        assert!(query.strict);

        let mut parser = make_parser("EXPLAIN SELECT 1");
        let err = parser
            .parse_explain_ask_query()
            .expect_err("missing ASK should fail");
        assert!(err.to_string().contains("expected"));

        let mut parser = make_parser("ASK 'q' STRICT MAYBE");
        let err = parser
            .parse_ask_query()
            .expect_err("bad strict should fail");
        assert!(err.to_string().contains("Expected ON or OFF"));

        let mut parser = make_parser("ASK 'q' CACHE TTL '10m' NOCACHE");
        let err = parser
            .parse_ask_query()
            .expect_err("duplicate cache should fail");
        assert!(err.to_string().contains("cache clause"));

        let mut parser = make_parser("ASK 'q' CACHE FOREVER '10m'");
        let err = parser
            .parse_ask_query()
            .expect_err("bad cache ttl keyword should fail");
        assert!(err.to_string().contains("Expected TTL"));
    }

    #[test]
    fn literal_value_special_constructors_arrays_and_objects() {
        let mut parser = make_parser("PASSWORD('pw')");
        assert!(matches!(
            parser.parse_literal_value().expect("password"),
            Value::Password(secret) if secret == "@@plain@@pw"
        ));

        let mut parser = make_parser("SECRET('pw')");
        assert!(matches!(
            parser.parse_literal_value().expect("secret"),
            Value::Secret(bytes) if bytes == b"@@plain@@pw"
        ));

        let mut parser = make_parser("SECRET_REF(vault, red.vault.api_key)");
        let value = parser.parse_literal_value().expect("secret ref");
        assert!(matches!(value, Value::Json(_)));

        // Array literals parse losslessly into `Value::Array`, preserving each
        // element's integer/float identity (issue #1708). Vector-vs-JSON typing
        // is resolved downstream from the target, not guessed at parse time.
        let mut parser = make_parser("[1, 2.5]");
        assert!(matches!(
            parser.parse_literal_value().expect("array"),
            Value::Array(items)
                if items == vec![Value::Integer(1), Value::Float(2.5)]
        ));

        let mut parser = make_parser("['a', 2]");
        assert!(matches!(
            parser.parse_literal_value().expect("mixed array"),
            Value::Array(items)
                if items == vec![Value::Text("a".into()), Value::Integer(2)]
        ));

        // A large integer that cannot survive an f32 (or even f64) round-trip
        // keeps its exact `Value::Integer` identity through the parser.
        let mut parser = make_parser("[1, 2, 9007199254740993]");
        assert!(matches!(
            parser.parse_literal_value().expect("lossless big-int array"),
            Value::Array(items)
                if items
                    == vec![
                        Value::Integer(1),
                        Value::Integer(2),
                        Value::Integer(9007199254740993),
                    ]
        ));

        let mut parser = make_parser("{level = 'info', count: 2}");
        assert!(matches!(
            parser.parse_literal_value().expect("json object"),
            Value::Json(_)
        ));
    }

    #[test]
    fn literal_value_rejects_invalid_secret_ref_and_scalar_start() {
        let mut parser = make_parser("SECRET_REF(config, red.vault.api_key)");
        let err = parser
            .parse_literal_value()
            .expect_err("non-vault secret ref should fail");
        assert!(err.to_string().contains("expected"));

        let mut parser = make_parser("ORDER");
        let err = parser
            .parse_literal_value()
            .expect_err("non literal should fail");
        assert!(err.to_string().contains("expected"));
    }

    #[test]
    fn json_depth_check_rejects_deep_literals() {
        let mut deep = reddb_types::utils::json::JsonValue::Array(vec![]);
        for _ in 0..JSON_LITERAL_MAX_DEPTH {
            deep = reddb_types::utils::json::JsonValue::Array(vec![deep]);
        }
        let err = json_literal_depth_check(&deep).expect_err("depth should fail");
        assert!(err.contains("JSON_LITERAL_MAX_DEPTH"));
    }
}
