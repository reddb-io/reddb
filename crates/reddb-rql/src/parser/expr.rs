//! Pratt-style parser for the Fase 2 `Expr` AST.
//!
//! This module is the Week 2 deliverable of the parser v2 refactor
//! tracked in `/home/cyber/.claude/plans/squishy-mixing-honey.md`.
//! It produces `ast::Expr` trees with proper operator precedence,
//! `Span` tracking from the lexer, and support for the full set of
//! unary / binary / postfix operators the existing hand-rolled
//! projection climb covers in Fase 1.3 — plus the missing pieces
//! (CASE, CAST, parenthesised subexprs, IS NULL, IN, BETWEEN).
//!
//! # Design notes
//!
//! The parser is now the canonical entry point for SQL expression
//! parsing in the table-query flow:
//! - `SELECT` projections parse through `Parser::parse_expr`
//! - `WHERE` / `HAVING` operands parse through `Parser::parse_expr`
//! - `ORDER BY` expressions parse through `Parser::parse_expr`
//!
//! Some legacy AST slots are still adapter-based (`Projection`,
//! `Filter`, `GROUP BY` strings), so statement parsing still lowers
//! `Expr` trees into those older shapes at the boundary.
//!
//! # Precedence table (matches PG gram.y modulo features we don't have)
//!
//! ```text
//! prec  operators
//! ----  ----------------------------------
//!  10   OR
//!  20   AND
//!  25   NOT                      (prefix)
//!  30   = <> < <= > >=           (comparison)
//!  32   IS NULL / IS NOT NULL    (postfix)
//!  33   BETWEEN … AND …          (postfix)
//!  34   IN (…)                   (postfix)
//!  40   ||                       (string concat)
//!  50   + -                      (additive)
//!  60   * / %                    (multiplicative)
//!  70   -                        (unary negation)
//!  80   ::type  CAST(…AS type)   (explicit type coercion)
//! ```
//!
//! Higher precedence binds tighter. The climb uses the classic
//! "min-precedence" algorithm — `parse_expr_prec(min)` loops consuming
//! any infix operator whose precedence is ≥ `min`, recursing with
//! `prec + 1` on the right-hand side for left-associativity.

use super::error::ParseError;
use super::Parser;
use super::PlaceholderMode;
use crate::ast::{BinOp, Expr, ExprSubquery, FieldRef, Span, UnaryOp};
use crate::lexer::Token;
use reddb_types::types::{DataType, Value};

fn is_duration_unit(unit: &str) -> bool {
    matches!(
        unit.to_ascii_lowercase().as_str(),
        "ms" | "msec"
            | "millisecond"
            | "milliseconds"
            | "s"
            | "sec"
            | "secs"
            | "second"
            | "seconds"
            | "m"
            | "min"
            | "mins"
            | "minute"
            | "minutes"
            | "h"
            | "hr"
            | "hrs"
            | "hour"
            | "hours"
            | "d"
            | "day"
            | "days"
    )
}

fn keyword_function_name(token: &Token) -> Option<&'static str> {
    match token {
        Token::Count => Some("COUNT"),
        Token::Sum => Some("SUM"),
        Token::Avg => Some("AVG"),
        Token::Min => Some("MIN"),
        Token::Max => Some("MAX"),
        Token::First => Some("FIRST"),
        Token::Last => Some("LAST"),
        Token::Left => Some("LEFT"),
        Token::Right => Some("RIGHT"),
        Token::Contains => Some("CONTAINS"),
        Token::Kv => Some("KV"),
        _ => None,
    }
}

/// Whether `name` may appear as the function in `fn(...) OVER (...)`.
/// Window-only functions plus the standard aggregates (which behave as
/// window aggregates when an OVER clause is attached). Mirrored loosely
/// from PG's pg_proc catalog — slice 7a only validates lexical eligibility,
/// runtime support arrives with the analytics executor.
fn is_window_eligible_function(name: &str) -> bool {
    matches!(
        name.to_ascii_uppercase().as_str(),
        // Window-only.
        "LAG"
            | "LEAD"
            | "ROW_NUMBER"
            | "RANK"
            | "DENSE_RANK"
            | "PERCENT_RANK"
            | "CUME_DIST"
            | "NTILE"
            | "FIRST_VALUE"
            | "LAST_VALUE"
            | "NTH_VALUE"
            // Aggregates valid in window position.
            | "COUNT"
            | "SUM"
            | "AVG"
            | "MIN"
            | "MAX"
            | "STDDEV"
            | "VARIANCE"
            | "MEDIAN"
            | "PERCENTILE"
            | "GROUP_CONCAT"
            | "STRING_AGG"
            | "FIRST"
            | "LAST"
            | "ARRAY_AGG"
            | "COUNT_DISTINCT"
    )
}

fn bare_zero_arg_function_name(name: &str) -> Option<&'static str> {
    match name.to_ascii_uppercase().as_str() {
        "CURRENT_TIMESTAMP" => Some("CURRENT_TIMESTAMP"),
        "CURRENT_DATE" => Some("CURRENT_DATE"),
        "CURRENT_TIME" => Some("CURRENT_TIME"),
        _ => None,
    }
}

impl<'a> Parser<'a> {
    /// Parse a complete expression at the lowest precedence level.
    /// Entry point for every caller that wants an `Expr` tree.
    pub fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_expr_prec(0)
    }

    pub(crate) fn parse_expr_with_min_precedence(
        &mut self,
        min_prec: u8,
    ) -> Result<Expr, ParseError> {
        self.parse_expr_prec(min_prec)
    }

    /// Continue parsing an expression after the caller has already
    /// materialized the left-hand side atom.
    pub(crate) fn continue_expr(&mut self, left: Expr, min_prec: u8) -> Result<Expr, ParseError> {
        self.parse_expr_suffix(left, min_prec)
    }

    /// Pratt climb: parse a unary atom then consume any infix operators
    /// whose precedence meets or exceeds `min_prec`.
    fn parse_expr_prec(&mut self, min_prec: u8) -> Result<Expr, ParseError> {
        // Depth guard: every recursive descent point in the expr
        // grammar bottoms out here, so checking once is enough to
        // catch deeply nested literals like `((((((1))))))` and
        // boolean chains like `NOT NOT NOT NOT … x`.
        self.enter_depth()?;
        let result = (|| {
            let left = self.parse_expr_unary()?;
            self.parse_expr_suffix(left, min_prec)
        })();
        self.exit_depth();
        result
    }

    fn parse_expr_suffix(&mut self, mut left: Expr, min_prec: u8) -> Result<Expr, ParseError> {
        loop {
            let Some((op, prec)) = self.peek_binop() else {
                // Not a standard infix op — check for postfix forms.
                if min_prec <= 32 {
                    if let Some(node) = self.try_parse_postfix(&left)? {
                        left = node;
                        continue;
                    }
                }
                break;
            };
            if prec < min_prec {
                break;
            }
            self.advance()?; // consume the operator token
            let start_span = self.span_start_of(&left);
            let rhs = self.parse_expr_prec(prec + 1)?;
            let end_span = self.span_end_of(&rhs);
            left = Expr::BinaryOp {
                op,
                lhs: Box::new(left),
                rhs: Box::new(rhs),
                span: Span::new(start_span, end_span),
            };
        }
        Ok(left)
    }

    /// Parse a unary-prefix expression or drop through to the atomic
    /// factor. Handles `NOT`, unary `-`, and `+` (no-op sign).
    fn parse_expr_unary(&mut self) -> Result<Expr, ParseError> {
        match self.peek() {
            Token::Not => {
                let start = self.position();
                self.advance()?;
                let operand = self.parse_expr_prec(25)?;
                let end = self.span_end_of(&operand);
                Ok(Expr::UnaryOp {
                    op: UnaryOp::Not,
                    operand: Box::new(operand),
                    span: Span::new(start, end),
                })
            }
            Token::Dash => {
                let start = self.position();
                self.advance()?;
                let operand = self.parse_expr_prec(70)?;
                let end = self.span_end_of(&operand);
                Ok(Expr::UnaryOp {
                    op: UnaryOp::Neg,
                    operand: Box::new(operand),
                    span: Span::new(start, end),
                })
            }
            Token::Plus => {
                // Unary plus is a no-op. Consume and recurse.
                self.advance()?;
                self.parse_expr_prec(70)
            }
            _ => self.parse_expr_factor(),
        }
    }

    /// Parse a single atomic expression factor: literal, column ref,
    /// parenthesised subexpression, CAST, CASE, or function call.
    fn parse_expr_factor(&mut self) -> Result<Expr, ParseError> {
        let start = self.position();

        // Parenthesised subexpression: `( expr )`
        if self.consume(&Token::LParen)? {
            if self.check(&Token::Select) {
                let query = self.parse_select_query()?;
                self.expect(Token::RParen)?;
                return Ok(Expr::Subquery {
                    query: ExprSubquery {
                        query: Box::new(query),
                    },
                    span: Span::new(start, self.position()),
                });
            }
            let inner = self.parse_expr_prec(0)?;
            self.expect(Token::RParen)?;
            return Ok(inner);
        }

        // Literal: true / false / null
        if self.consume(&Token::True)? {
            return Ok(Expr::Literal {
                value: Value::Boolean(true),
                span: Span::new(start, self.position()),
            });
        }
        if self.consume(&Token::False)? {
            return Ok(Expr::Literal {
                value: Value::Boolean(false),
                span: Span::new(start, self.position()),
            });
        }
        if self.consume(&Token::Null)? {
            return Ok(Expr::Literal {
                value: Value::Null,
                span: Span::new(start, self.position()),
            });
        }

        // Numeric literals — with optional duration-unit suffix (e.g. `5m`, `10s`, `2h`).
        // Duration literals are emitted as Value::Text so downstream code sees "5m" verbatim
        // (matching the legacy Projection::Column("LIT:5m") path used by time_bucket).
        if let Token::Integer(n) = *self.peek() {
            self.advance()?;
            if let Token::Ident(ref unit) = *self.peek() {
                if is_duration_unit(unit) {
                    let duration = format!("{n}{}", unit.to_ascii_lowercase());
                    self.advance()?;
                    return Ok(Expr::Literal {
                        value: Value::text(duration),
                        span: Span::new(start, self.position()),
                    });
                }
            }
            return Ok(Expr::Literal {
                value: Value::Integer(n),
                span: Span::new(start, self.position()),
            });
        }
        if let Token::Float(n) = *self.peek() {
            self.advance()?;
            return Ok(Expr::Literal {
                value: Value::Float(n),
                span: Span::new(start, self.position()),
            });
        }
        if let Token::String(ref s) = *self.peek() {
            let text = s.clone();
            self.advance()?;
            return Ok(Expr::Literal {
                value: Value::text(text),
                span: Span::new(start, self.position()),
            });
        }

        // JSON object `{…}` and array `[…]` literals — delegate to the DML literal parser
        // which already handles the full JSON value grammar including nested objects.
        // `JsonLiteral` is the strict-JSON variant emitted by the lexer's sub-mode
        // when `{` is followed by `"`; both shapes route through `parse_literal_value`.
        if matches!(
            self.peek(),
            Token::LBrace | Token::LBracket | Token::JsonLiteral(_)
        ) {
            let value = self
                .parse_literal_value()
                .map_err(|e| ParseError::new(e.message, self.position()))?;
            return Ok(Expr::Literal {
                value,
                span: Span::new(start, self.position()),
            });
        }

        // `?` positional placeholder — auto-numbered left-to-right.
        // Immediate `?N` uses an explicit 1-based index. Mixing with
        // `$N` in one statement is rejected.
        if self.check(&Token::Question) {
            let (index, span) = self.parse_question_param_index()?;
            return Ok(Expr::Parameter { index, span });
        }

        if self.consume(&Token::Dollar)? {
            // `$N` positional parameter placeholder (1-based in source,
            // 0-based in the AST so it matches `Vec<Value>` indexing).
            // Rejected at parse time when N < 1; gaps and arity are
            // validated by the binder once the full statement is parsed.
            if let Token::Integer(n) = *self.peek() {
                if n < 1 {
                    return Err(ParseError::new(
                        "placeholder index must be >= 1".to_string(),
                        self.position(),
                    ));
                }
                if self.placeholder_mode == PlaceholderMode::Question {
                    return Err(ParseError::new(
                        "cannot mix `?` and `$N` placeholders in one statement".to_string(),
                        self.position(),
                    ));
                }
                self.placeholder_mode = PlaceholderMode::Dollar;
                self.advance()?;
                return Ok(Expr::Parameter {
                    index: (n - 1) as usize,
                    span: Span::new(start, self.position()),
                });
            }
            let path = self.parse_dollar_ref_path()?;
            let path_lc = path.to_ascii_lowercase();
            let (name, key) = if let Some(rest) = path_lc.strip_prefix("secret.") {
                ("__SECRET_REF", format!("red.vault/{rest}"))
            } else if path_lc.starts_with("red.secret.") {
                let rest = path_lc.trim_start_matches("red.secret.");
                ("__SECRET_REF", format!("red.vault/{rest}"))
            } else if let Some(rest) = path_lc.strip_prefix("config.") {
                ("CONFIG", format!("red.config/{rest}"))
            } else if path_lc.starts_with("red.config.") {
                let rest = path_lc.trim_start_matches("red.config.");
                ("CONFIG", format!("red.config/{rest}"))
            } else {
                return Err(ParseError::new(
                    format!(
                        "unknown $ reference `${path}`; expected $secret.*, $red.secret.*, $config.*, or $red.config.*"
                    ),
                    self.position(),
                ));
            };
            return Ok(Expr::FunctionCall {
                name: name.to_string(),
                args: vec![Expr::Literal {
                    value: Value::text(key),
                    span: Span::new(start, self.position()),
                }],
                span: Span::new(start, self.position()),
            });
        }

        if let Some(name) = keyword_function_name(self.peek()) {
            if matches!(self.peek_next()?, Token::LParen) {
                self.advance()?; // consume the keyword token
                return self.parse_function_call_expr_with_name(start, name.to_string());
            }
        }

        // Identifier-led constructs: function call, CAST, CASE, column.
        //
        // We commit to consuming the identifier immediately and then
        // inspect the NEXT token to decide shape. This avoids needing
        // two-token lookahead on the parser. If the next token is `(`
        // it's a function call; if `.` it's a qualified column ref;
        // otherwise it's a bare column ref.
        if let Token::Ident(ref name) = *self.peek() {
            let name_upper = name.to_uppercase();

            // CAST(expr AS type) — must test before consuming because
            // CAST is not a reserved keyword; users could legitimately
            // have a column literally named `cast`. Distinguish by
            // looking at whether the identifier equals CAST AND is
            // immediately followed by `(`. Since we can't two-step
            // lookahead, handle CAST by parsing the ident, then if the
            // uppercased name is CAST and the next token is `(`,
            // switch to the CAST form; otherwise the saved name
            // becomes the first segment of a column ref.
            if name_upper == "CASE" {
                return self.parse_case_expr(start);
            }

            let saved_name = name.clone();
            self.advance()?; // consume the identifier unconditionally

            // Function call / CAST: IDENT (
            if matches!(self.peek(), Token::LParen) {
                return self.parse_function_call_expr_with_name(start, saved_name);
            }

            if let Some(function_name) = bare_zero_arg_function_name(&saved_name) {
                let end = self.position();
                return Ok(Expr::FunctionCall {
                    name: function_name.to_string(),
                    args: Vec::new(),
                    span: Span::new(start, end),
                });
            }

            // Qualified column: IDENT.IDENT[.IDENT …]
            if matches!(self.peek(), Token::Dot) {
                let mut segments = vec![saved_name];
                while self.consume(&Token::Dot)? {
                    segments.push(self.expect_ident_or_keyword()?);
                }
                let field = FieldRef::TableColumn {
                    table: segments.remove(0),
                    column: segments.join("."),
                };
                let end = self.position();
                return Ok(Expr::Column {
                    field,
                    span: Span::new(start, end),
                });
            }

            // Bare column reference with empty table name.
            let field = FieldRef::TableColumn {
                table: String::new(),
                column: saved_name,
            };
            let end = self.position();
            return Ok(Expr::Column {
                field,
                span: Span::new(start, end),
            });
        }

        // Default: column reference (optionally qualified: table.column).
        // Reached only when the leading token is not an Ident. Falls
        // through to parse_field_ref which handles keyword-shaped
        // column names.
        let field = self.parse_field_ref()?;
        let end = self.position();
        Ok(Expr::Column {
            field,
            span: Span::new(start, end),
        })
    }

    fn parse_dollar_ref_path(&mut self) -> Result<String, ParseError> {
        let mut path = self.expect_ident_or_keyword()?;
        while self.consume(&Token::Dot)? {
            let next = self.expect_ident_or_keyword()?;
            path = format!("{path}.{next}");
        }
        Ok(path)
    }

    fn parse_function_call_expr_with_name(
        &mut self,
        start: crate::lexer::Position,
        function_name: String,
    ) -> Result<Expr, ParseError> {
        let call = self.parse_function_call_expr_with_name_inner(start, function_name)?;
        // Issue #589 slice 7a: `fn(args) OVER (...)` lifts a plain
        // FunctionCall into a WindowFunctionCall carrying the OVER
        // clause. CAST and other shapes that don't return a
        // FunctionCall are rejected by `parse_over_clause_for` so the
        // user gets a clear error rather than silent acceptance.
        if matches!(self.peek(), Token::Over) {
            return self.lift_to_window_call(start, call);
        }
        Ok(call)
    }

    fn parse_function_call_expr_with_name_inner(
        &mut self,
        start: crate::lexer::Position,
        function_name: String,
    ) -> Result<Expr, ParseError> {
        self.expect(Token::LParen)?;

        if function_name.eq_ignore_ascii_case("CAST") {
            let inner = self.parse_expr_prec(0)?;
            self.expect(Token::As)?;
            let type_name = self.expect_ident_or_keyword()?;
            self.expect(Token::RParen)?;
            let end = self.position();
            let Some(target) = DataType::from_sql_name(&type_name) else {
                return Err(ParseError::new(
                    // F-05: `type_name` is caller-controlled identifier text.
                    // Render via `{:?}` so embedded CR/LF/NUL/quotes are
                    // escaped before reaching downstream serialization sinks.
                    format!("unknown type name {type_name:?} in CAST"),
                    self.position(),
                ));
            };
            return Ok(Expr::Cast {
                inner: Box::new(inner),
                target,
                span: Span::new(start, end),
            });
        }

        if function_name.eq_ignore_ascii_case("TRIM") {
            let (name, args) = self.parse_trim_expr_args()?;
            self.expect(Token::RParen)?;
            let end = self.position();
            return Ok(Expr::FunctionCall {
                name,
                args,
                span: Span::new(start, end),
            });
        }

        if function_name.eq_ignore_ascii_case("POSITION") {
            let args = self.parse_position_expr_args()?;
            self.expect(Token::RParen)?;
            let end = self.position();
            return Ok(Expr::FunctionCall {
                name: function_name,
                args,
                span: Span::new(start, end),
            });
        }

        if function_name.eq_ignore_ascii_case("SUBSTRING") {
            let args = self.parse_substring_expr_args()?;
            self.expect(Token::RParen)?;
            let end = self.position();
            return Ok(Expr::FunctionCall {
                name: function_name,
                args,
                span: Span::new(start, end),
            });
        }

        if function_name.eq_ignore_ascii_case("COUNT") {
            if self.consume(&Token::Distinct)? {
                let arg = self.parse_expr_prec(0)?;
                self.expect(Token::RParen)?;
                let end = self.position();
                return Ok(Expr::FunctionCall {
                    name: "COUNT_DISTINCT".to_string(),
                    args: vec![arg],
                    span: Span::new(start, end),
                });
            }

            if self.consume(&Token::Star)? {
                self.expect(Token::RParen)?;
                let end = self.position();
                return Ok(Expr::FunctionCall {
                    name: function_name,
                    args: vec![Expr::Column {
                        field: FieldRef::TableColumn {
                            table: String::new(),
                            column: "*".to_string(),
                        },
                        span: Span::synthetic(),
                    }],
                    span: Span::new(start, end),
                });
            }
        }

        // CONFIG()/KV() take bare dotted config paths as arguments
        // (e.g. `CONFIG(red.ai.default.provider, openai)`,
        // `KV(cfg, default.role, guest)`). Parsed through the generic
        // expression grammar these become column references — and a
        // keyword segment like `default` would be folded to `DEFAULT`,
        // breaking the case-sensitive config-key lookup, while a
        // source-free `SELECT CONFIG(...)` would fail with "unknown
        // column". Capture each path-shaped argument as a lowercased
        // string literal instead so it matches stored keys (which
        // `SET CONFIG` also lowercases) and never resolves as a column.
        if function_name.eq_ignore_ascii_case("CONFIG") || function_name.eq_ignore_ascii_case("KV")
        {
            let mut args = Vec::new();
            if !self.check(&Token::RParen) {
                loop {
                    args.push(self.parse_config_kv_arg(start)?);
                    if !self.consume(&Token::Comma)? {
                        break;
                    }
                }
            }
            self.expect(Token::RParen)?;
            let end = self.position();
            return Ok(Expr::FunctionCall {
                name: function_name,
                args,
                span: Span::new(start, end),
            });
        }

        let mut args = Vec::new();
        if !self.check(&Token::RParen) {
            loop {
                args.push(self.parse_expr_prec(0)?);
                if !self.consume(&Token::Comma)? {
                    break;
                }
            }
        }
        self.expect(Token::RParen)?;
        let end = self.position();
        Ok(Expr::FunctionCall {
            name: function_name,
            args,
            span: Span::new(start, end),
        })
    }

    /// Parse a single CONFIG()/KV() argument. A bare identifier or
    /// dotted path (including keyword-shaped segments) becomes a
    /// lowercased string literal — the config-key form. Anything else
    /// (quoted string, number, `?`/`$N` placeholder, parenthesised
    /// expression) falls through to the normal expression grammar so
    /// dynamic defaults still work.
    fn parse_config_kv_arg(&mut self, start: crate::lexer::Position) -> Result<Expr, ParseError> {
        // Literals, placeholders and parenthesised sub-expressions are
        // real expressions (dynamic defaults); everything else that can
        // open an argument here is an identifier or keyword that forms a
        // bare config path.
        let mut is_expression_start = matches!(
            self.peek(),
            Token::String(_)
                | Token::Integer(_)
                | Token::Float(_)
                | Token::Dollar
                | Token::Question
                | Token::LParen
        );
        // A bare identifier immediately followed by `(` is a nested
        // function call (e.g. a dynamic default), not a config path.
        if matches!(self.peek(), Token::Ident(_)) && matches!(self.peek_next()?, Token::LParen) {
            is_expression_start = true;
        }
        if !is_expression_start && !self.check(&Token::RParen) {
            let mut path = self.expect_ident_or_keyword()?;
            while self.consume(&Token::Dot)? {
                let next = self.expect_ident_or_keyword()?;
                path = format!("{path}.{next}");
            }
            let end = self.position();
            return Ok(Expr::Literal {
                value: Value::text(path.to_ascii_lowercase()),
                span: Span::new(start, end),
            });
        }
        self.parse_expr_prec(0)
    }

    /// Wrap a freshly-parsed `Expr::FunctionCall` in
    /// `Expr::WindowFunctionCall` by consuming the trailing `OVER (...)`
    /// clause. The caller has already confirmed the next token is
    /// `OVER`. Rejects:
    /// - CAST(...) OVER (...) and other non-FunctionCall shapes.
    /// - Function names that are neither window-only nor aggregates.
    fn lift_to_window_call(
        &mut self,
        start: crate::lexer::Position,
        call: Expr,
    ) -> Result<Expr, ParseError> {
        let (name, args) = match call {
            Expr::FunctionCall { name, args, .. } => (name, args),
            other => {
                return Err(ParseError::new(
                    format!(
                        "OVER may only follow a function call, got {:?}",
                        std::mem::discriminant(&other)
                    ),
                    self.position(),
                ));
            }
        };
        if !is_window_eligible_function(&name) {
            return Err(ParseError::new(
                format!(
                    "function `{}` cannot be used with an OVER clause; \
                     expected a window function (LAG, LEAD, ROW_NUMBER, \
                     RANK, DENSE_RANK) or an aggregate",
                    name.to_uppercase()
                ),
                self.position(),
            ));
        }
        let window = self.parse_over_clause()?;
        let end = self.position();
        Ok(Expr::WindowFunctionCall {
            name,
            args,
            window,
            span: Span::new(start, end),
        })
    }

    /// Parse the `OVER ( [PARTITION BY ...] [ORDER BY ...] [frame] )`
    /// clause. The leading `OVER` keyword is consumed here.
    fn parse_over_clause(&mut self) -> Result<crate::ast::WindowSpec, ParseError> {
        self.expect(Token::Over)?;
        self.expect(Token::LParen)?;

        let mut spec = crate::ast::WindowSpec::default();

        if self.consume(&Token::Partition)? {
            self.expect(Token::By)?;
            loop {
                spec.partition_by.push(self.parse_expr_prec(0)?);
                if !self.consume(&Token::Comma)? {
                    break;
                }
            }
        }

        if self.consume(&Token::Order)? {
            self.expect(Token::By)?;
            loop {
                let expr = self.parse_expr_prec(0)?;
                let ascending = if self.consume(&Token::Desc)? {
                    false
                } else {
                    self.consume(&Token::Asc)?;
                    true
                };
                // NULLS FIRST / LAST defaults mirror PG: nulls last for
                // ASC, nulls first for DESC. Explicit clause overrides.
                let mut nulls_first = !ascending;
                if self.consume(&Token::Nulls)? {
                    if self.consume(&Token::First)? {
                        nulls_first = true;
                    } else if self.consume(&Token::Last)? {
                        nulls_first = false;
                    } else {
                        return Err(ParseError::new(
                            "expected FIRST or LAST after NULLS".to_string(),
                            self.position(),
                        ));
                    }
                }
                spec.order_by.push(crate::ast::WindowOrderItem {
                    expr,
                    ascending,
                    nulls_first,
                });
                if !self.consume(&Token::Comma)? {
                    break;
                }
            }
        }

        if matches!(self.peek(), Token::Rows | Token::Range) {
            spec.frame = Some(self.parse_window_frame()?);
        }

        self.expect(Token::RParen)?;
        Ok(spec)
    }

    fn parse_window_frame(&mut self) -> Result<crate::ast::WindowFrame, ParseError> {
        let unit = if self.consume(&Token::Rows)? {
            crate::ast::WindowFrameUnit::Rows
        } else if self.consume(&Token::Range)? {
            crate::ast::WindowFrameUnit::Range
        } else {
            return Err(ParseError::new(
                "expected ROWS or RANGE in window frame".to_string(),
                self.position(),
            ));
        };

        if self.consume(&Token::Between)? {
            let start = self.parse_window_frame_bound()?;
            self.expect(Token::And)?;
            let end = self.parse_window_frame_bound()?;
            Ok(crate::ast::WindowFrame {
                unit,
                start,
                end: Some(end),
            })
        } else {
            let start = self.parse_window_frame_bound()?;
            Ok(crate::ast::WindowFrame {
                unit,
                start,
                end: None,
            })
        }
    }

    fn parse_window_frame_bound(&mut self) -> Result<crate::ast::WindowFrameBound, ParseError> {
        use crate::ast::WindowFrameBound;
        if self.consume(&Token::Unbounded)? {
            if self.consume(&Token::Preceding)? {
                return Ok(WindowFrameBound::UnboundedPreceding);
            }
            if self.consume(&Token::Following)? {
                return Ok(WindowFrameBound::UnboundedFollowing);
            }
            return Err(ParseError::new(
                "expected PRECEDING or FOLLOWING after UNBOUNDED".to_string(),
                self.position(),
            ));
        }
        if self.consume(&Token::Current)? {
            self.expect(Token::Row)?;
            return Ok(WindowFrameBound::CurrentRow);
        }
        // Numeric / expression offset: `N PRECEDING` / `N FOLLOWING`.
        let offset = self.parse_expr_prec(0)?;
        if self.consume(&Token::Preceding)? {
            return Ok(WindowFrameBound::Preceding(Box::new(offset)));
        }
        if self.consume(&Token::Following)? {
            return Ok(WindowFrameBound::Following(Box::new(offset)));
        }
        Err(ParseError::new(
            "expected PRECEDING or FOLLOWING after frame offset".to_string(),
            self.position(),
        ))
    }

    /// Parse both CASE forms:
    /// - searched: `CASE WHEN cond THEN val [WHEN …] [ELSE val] END`
    /// - simple:   `CASE expr WHEN val THEN val [WHEN …] [ELSE val] END`
    ///
    /// The simple form is desugared into the searched form: each
    /// `WHEN <value>` becomes the equality condition `<selector> = <value>`,
    /// which preserves SQL's three-valued comparison semantics (a NULL
    /// selector never matches a WHEN value) without growing the `Expr::Case`
    /// AST or the executor.
    ///
    /// Assumes the caller has already peeked `CASE`.
    fn parse_case_expr(&mut self, start: crate::lexer::Position) -> Result<Expr, ParseError> {
        self.advance()?; // consume CASE
                         // Simple CASE: a selector expression precedes the first WHEN.
        let selector = if matches!(self.peek(), Token::Ident(id) if id.eq_ignore_ascii_case("WHEN"))
        {
            None
        } else {
            Some(self.parse_expr_prec(0)?)
        };
        let mut branches: Vec<(Expr, Expr)> = Vec::new();
        loop {
            if !self.consume_ident_ci("WHEN")? {
                break;
            }
            let when_val = self.parse_expr_prec(0)?;
            // Searched form keeps the WHEN expression as the condition;
            // simple form rewrites it to `selector = when_val`.
            let cond = match &selector {
                None => when_val,
                Some(sel) => {
                    let span = Span::new(sel.span().start, when_val.span().end);
                    Expr::BinaryOp {
                        op: BinOp::Eq,
                        lhs: Box::new(sel.clone()),
                        rhs: Box::new(when_val),
                        span,
                    }
                }
            };
            if !self.consume_ident_ci("THEN")? {
                return Err(ParseError::new(
                    "expected THEN after CASE WHEN condition".to_string(),
                    self.position(),
                ));
            }
            let then_val = self.parse_expr_prec(0)?;
            branches.push((cond, then_val));
        }
        if branches.is_empty() {
            return Err(ParseError::new(
                "CASE must have at least one WHEN branch".to_string(),
                self.position(),
            ));
        }
        let else_ = if self.consume_ident_ci("ELSE")? {
            Some(Box::new(self.parse_expr_prec(0)?))
        } else {
            None
        };
        if !self.consume_ident_ci("END")? {
            return Err(ParseError::new(
                "expected END to close CASE expression".to_string(),
                self.position(),
            ));
        }
        let end = self.position();
        Ok(Expr::Case {
            branches,
            else_,
            span: Span::new(start, end),
        })
    }

    fn parse_trim_expr_args(&mut self) -> Result<(String, Vec<Expr>), ParseError> {
        let mut function_name = "TRIM".to_string();

        if self.consume_ident_ci("LEADING")? {
            function_name = "LTRIM".to_string();
        } else if self.consume_ident_ci("TRAILING")? {
            function_name = "RTRIM".to_string();
        } else if self.consume_ident_ci("BOTH")? {
            function_name = "TRIM".to_string();
        }

        if self.consume(&Token::From)? {
            let source = self.parse_expr_prec(0)?;
            return Ok((function_name, vec![source]));
        }

        let first = self.parse_expr_prec(0)?;

        if self.consume(&Token::Comma)? {
            let second = self.parse_expr_prec(0)?;
            return Ok((function_name, vec![first, second]));
        }

        if self.consume(&Token::From)? {
            let source = self.parse_expr_prec(0)?;
            return Ok((function_name, vec![source, first]));
        }

        Ok((function_name, vec![first]))
    }

    /// PostgreSQL-style `POSITION(substr IN string)` or plain
    /// `POSITION(substr, string)` lowered to the ordinary two-argument
    /// function form.
    fn parse_position_expr_args(&mut self) -> Result<Vec<Expr>, ParseError> {
        // `IN` is also a postfix operator in the main expression grammar, so
        // parse the first operand above postfix-IN precedence and then consume
        // the function's `IN` keyword explicitly.
        let needle = self.parse_expr_prec(35)?;
        if !self.consume(&Token::Comma)? {
            self.expect(Token::In)?;
        }
        let haystack = self.parse_expr_prec(0)?;
        Ok(vec![needle, haystack])
    }

    /// PostgreSQL-style `SUBSTRING` syntax:
    /// - `SUBSTRING(expr FROM start [FOR count])`
    /// - `SUBSTRING(expr FOR count [FROM start])`
    /// - plain function-call form `SUBSTRING(expr, start[, count])`
    ///
    /// The SQL-syntax variants are desugared to the comma-arg form so the
    /// rest of the stack sees the same `Expr::FunctionCall` shape.
    fn parse_substring_expr_args(&mut self) -> Result<Vec<Expr>, ParseError> {
        let source = self.parse_expr_prec(0)?;

        if self.consume(&Token::Comma)? {
            let mut args = vec![source];
            loop {
                args.push(self.parse_expr_prec(0)?);
                if !self.consume(&Token::Comma)? {
                    break;
                }
            }
            return Ok(args);
        }

        if self.consume(&Token::From)? {
            let start = self.parse_expr_prec(0)?;
            if self.consume(&Token::For)? {
                let count = self.parse_expr_prec(0)?;
                return Ok(vec![source, start, count]);
            }
            return Ok(vec![source, start]);
        }

        if self.consume(&Token::For)? {
            let count = self.parse_expr_prec(0)?;
            if self.consume(&Token::From)? {
                let start = self.parse_expr_prec(0)?;
                return Ok(vec![source, start, count]);
            }
            return Ok(vec![source, Expr::lit(Value::Integer(1)), count]);
        }

        Ok(vec![source])
    }

    /// Try to consume a postfix operator on top of the already-parsed
    /// `left` expression: `IS [NOT] NULL`, `[NOT] BETWEEN … AND …`,
    /// `[NOT] IN (…)`. Returns `Ok(None)` if no postfix follows.
    ///
    /// NOT at this position is unambiguous — prefix `NOT` is always
    /// consumed at `parse_expr_unary` level before reaching postfix.
    /// So seeing `NOT` here means the user wrote `x NOT BETWEEN …`
    /// or `x NOT IN …`; we consume it eagerly and require BETWEEN
    /// or IN to follow.
    fn try_parse_postfix(&mut self, left: &Expr) -> Result<Option<Expr>, ParseError> {
        let start = self.span_start_of(left);

        // IS [NOT] NULL
        if self.consume(&Token::Is)? {
            let negated = self.consume(&Token::Not)?;
            self.expect(Token::Null)?;
            let end = self.position();
            return Ok(Some(Expr::IsNull {
                operand: Box::new(left.clone()),
                negated,
                span: Span::new(start, end),
            }));
        }

        // Detect NOT BETWEEN / NOT IN. NOT is consumed eagerly — we
        // don't have two-token lookahead and the grammar guarantees
        // no other valid postfix starts with NOT.
        let negated = if matches!(self.peek(), Token::Not) {
            self.advance()?;
            if !matches!(self.peek(), Token::Between | Token::In) {
                return Err(ParseError::new(
                    "expected BETWEEN or IN after postfix NOT".to_string(),
                    self.position(),
                ));
            }
            true
        } else {
            false
        };

        // BETWEEN low AND high
        if self.consume(&Token::Between)? {
            let low = self.parse_expr_prec(34)?;
            self.expect(Token::And)?;
            let high = self.parse_expr_prec(34)?;
            let end = self.position();
            return Ok(Some(Expr::Between {
                target: Box::new(left.clone()),
                low: Box::new(low),
                high: Box::new(high),
                negated,
                span: Span::new(start, end),
            }));
        }

        // IN (v1, v2, …)
        if self.consume(&Token::In)? {
            self.expect(Token::LParen)?;
            let mut values = Vec::new();
            if self.check(&Token::Select) {
                let query = self.parse_select_query()?;
                values.push(Expr::Subquery {
                    query: ExprSubquery {
                        query: Box::new(query),
                    },
                    span: Span::new(self.span_start_of(left), self.position()),
                });
            } else if !self.check(&Token::RParen) {
                loop {
                    values.push(self.parse_expr_prec(0)?);
                    if !self.consume(&Token::Comma)? {
                        break;
                    }
                }
            }
            self.expect(Token::RParen)?;
            let end = self.position();
            return Ok(Some(Expr::InList {
                target: Box::new(left.clone()),
                values,
                negated,
                span: Span::new(start, end),
            }));
        }

        if negated {
            // Unreachable because the early-return above already
            // validated NOT is followed by BETWEEN or IN. Guarded
            // to keep callers loud if the grammar grows later.
            return Err(ParseError::new(
                "internal: NOT consumed without BETWEEN/IN follow".to_string(),
                self.position(),
            ));
        }
        Ok(None)
    }

    /// Peek the current token and translate it into a `BinOp` plus
    /// its precedence. Returns `None` if the token is not a recognised
    /// infix operator — the caller then tries postfix handling.
    fn peek_binop(&self) -> Option<(BinOp, u8)> {
        let op = match self.peek() {
            Token::Or => BinOp::Or,
            Token::And => BinOp::And,
            Token::Eq => BinOp::Eq,
            Token::Ne => BinOp::Ne,
            Token::Lt => BinOp::Lt,
            Token::Le => BinOp::Le,
            Token::Gt => BinOp::Gt,
            Token::Ge => BinOp::Ge,
            Token::DoublePipe => BinOp::Concat,
            Token::Plus => BinOp::Add,
            Token::Dash => BinOp::Sub,
            Token::Star => BinOp::Mul,
            Token::Slash => BinOp::Div,
            Token::Percent => BinOp::Mod,
            _ => return None,
        };
        Some((op, op.precedence()))
    }

    /// Return the start position of an expression's span. Handles the
    /// synthetic case by falling back to the current parser cursor,
    /// which is good enough for the Pratt climb since the caller just
    /// parsed the atom.
    fn span_start_of(&self, expr: &Expr) -> crate::lexer::Position {
        let s = expr.span();
        if s.is_synthetic() {
            self.position()
        } else {
            s.start
        }
    }

    /// Return the end position of an expression's span — same
    /// synthetic fallback as `span_start_of`.
    fn span_end_of(&self, expr: &Expr) -> crate::lexer::Position {
        let s = expr.span();
        if s.is_synthetic() {
            self.position()
        } else {
            s.end
        }
    }
}

// Avoid `unused` lints in partial-migration builds where the analyzer
// still does not consume every expression shape directly.
#[allow(dead_code)]
fn _expr_module_used(_: Expr) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::FieldRef;

    fn parse(input: &str) -> Expr {
        let mut parser = Parser::new(input).expect("lexer init");
        let expr = parser.parse_expr().expect("parse_expr");
        expr
    }

    #[test]
    fn literal_integer() {
        let e = parse("42");
        match e {
            Expr::Literal {
                value: Value::Integer(42),
                ..
            } => {}
            other => panic!("expected Integer(42), got {other:?}"),
        }
    }

    #[test]
    fn literal_float() {
        let e = parse("3.14");
        match e {
            Expr::Literal {
                value: Value::Float(f),
                ..
            } => assert!((f - 3.14).abs() < 1e-9),
            other => panic!("expected float literal, got {other:?}"),
        }
    }

    #[test]
    fn literal_string() {
        let e = parse("'hello'");
        match e {
            Expr::Literal {
                value: Value::Text(ref s),
                ..
            } if s.as_ref() == "hello" => {}
            other => panic!("expected Text(hello), got {other:?}"),
        }
    }

    #[test]
    fn literal_booleans_and_null() {
        assert!(matches!(
            parse("TRUE"),
            Expr::Literal {
                value: Value::Boolean(true),
                ..
            }
        ));
        assert!(matches!(
            parse("FALSE"),
            Expr::Literal {
                value: Value::Boolean(false),
                ..
            }
        ));
        assert!(matches!(
            parse("NULL"),
            Expr::Literal {
                value: Value::Null,
                ..
            }
        ));
    }

    #[test]
    fn bare_column() {
        let e = parse("user_id");
        match e {
            Expr::Column {
                field: FieldRef::TableColumn { column, .. },
                ..
            } => {
                assert_eq!(column, "user_id");
            }
            other => panic!("expected column, got {other:?}"),
        }
    }

    #[test]
    fn arithmetic_precedence_mul_over_add() {
        // a + b * c  →  Add(a, Mul(b, c))
        let e = parse("a + b * c");
        let Expr::BinaryOp {
            op: BinOp::Add,
            rhs,
            ..
        } = e
        else {
            panic!("root must be Add");
        };
        let Expr::BinaryOp { op: BinOp::Mul, .. } = *rhs else {
            panic!("rhs must be Mul");
        };
    }

    #[test]
    fn arithmetic_left_associativity() {
        // a - b - c  →  Sub(Sub(a, b), c)
        let e = parse("a - b - c");
        let Expr::BinaryOp {
            op: BinOp::Sub,
            lhs,
            ..
        } = e
        else {
            panic!("root must be Sub");
        };
        let Expr::BinaryOp { op: BinOp::Sub, .. } = *lhs else {
            panic!("lhs must be Sub (left-assoc)");
        };
    }

    #[test]
    fn parenthesised_override() {
        // (a + b) * c  →  Mul(Add(a, b), c)
        let e = parse("(a + b) * c");
        let Expr::BinaryOp {
            op: BinOp::Mul,
            lhs,
            ..
        } = e
        else {
            panic!("root must be Mul");
        };
        let Expr::BinaryOp { op: BinOp::Add, .. } = *lhs else {
            panic!("lhs must be Add");
        };
    }

    #[test]
    fn comparison_binds_weaker_than_arith() {
        // a + 1 = b - 2
        //   →  Eq(Add(a, 1), Sub(b, 2))
        let e = parse("a + 1 = b - 2");
        let Expr::BinaryOp {
            op: BinOp::Eq,
            lhs,
            rhs,
            ..
        } = e
        else {
            panic!("root must be Eq");
        };
        assert!(matches!(*lhs, Expr::BinaryOp { op: BinOp::Add, .. }));
        assert!(matches!(*rhs, Expr::BinaryOp { op: BinOp::Sub, .. }));
    }

    #[test]
    fn and_binds_tighter_than_or() {
        // a OR b AND c  →  Or(a, And(b, c))
        let e = parse("a OR b AND c");
        let Expr::BinaryOp {
            op: BinOp::Or, rhs, ..
        } = e
        else {
            panic!("root must be Or");
        };
        assert!(matches!(*rhs, Expr::BinaryOp { op: BinOp::And, .. }));
    }

    #[test]
    fn unary_negation() {
        let e = parse("-a");
        let Expr::UnaryOp {
            op: UnaryOp::Neg, ..
        } = e
        else {
            panic!("expected unary Neg");
        };
    }

    #[test]
    fn unary_not() {
        let e = parse("NOT a");
        let Expr::UnaryOp {
            op: UnaryOp::Not, ..
        } = e
        else {
            panic!("expected unary Not");
        };
    }

    #[test]
    fn concat_operator() {
        let e = parse("'hello' || name");
        let Expr::BinaryOp {
            op: BinOp::Concat, ..
        } = e
        else {
            panic!("expected Concat");
        };
    }

    #[test]
    fn cast_expr() {
        let e = parse("CAST(age AS TEXT)");
        let Expr::Cast { target, .. } = e else {
            panic!("expected Cast");
        };
        assert_eq!(target, DataType::Text);
    }

    #[test]
    fn case_expr() {
        let e = parse("CASE WHEN a = 1 THEN 'one' WHEN a = 2 THEN 'two' ELSE 'other' END");
        let Expr::Case {
            branches, else_, ..
        } = e
        else {
            panic!("expected Case");
        };
        assert_eq!(branches.len(), 2);
        assert!(else_.is_some());
    }

    #[test]
    fn simple_case_desugars_to_equality() {
        let e = parse("CASE id WHEN 1 THEN 'one' WHEN 2 THEN 'two' ELSE 'many' END");
        let Expr::Case {
            branches, else_, ..
        } = e
        else {
            panic!("expected Case");
        };
        assert_eq!(branches.len(), 2);
        assert!(else_.is_some());
        // Each WHEN value is rewritten to `selector = value`.
        for (cond, _) in &branches {
            let Expr::BinaryOp { op, lhs, .. } = cond else {
                panic!("expected desugared equality condition");
            };
            assert_eq!(*op, BinOp::Eq);
            assert!(matches!(**lhs, Expr::Column { .. }));
        }
    }

    #[test]
    fn is_null_postfix() {
        let e = parse("name IS NULL");
        assert!(matches!(e, Expr::IsNull { negated: false, .. }));
    }

    #[test]
    fn is_not_null_postfix() {
        let e = parse("name IS NOT NULL");
        assert!(matches!(e, Expr::IsNull { negated: true, .. }));
    }

    #[test]
    fn between_with_columns() {
        let e = parse("temp BETWEEN min_t AND max_t");
        let Expr::Between {
            target,
            low,
            high,
            negated,
            ..
        } = e
        else {
            panic!("expected Between");
        };
        assert!(!negated);
        assert!(matches!(*target, Expr::Column { .. }));
        assert!(matches!(*low, Expr::Column { .. }));
        assert!(matches!(*high, Expr::Column { .. }));
    }

    #[test]
    fn not_between_negates() {
        let e = parse("temp NOT BETWEEN 0 AND 100");
        let Expr::Between { negated: true, .. } = e else {
            panic!("expected negated Between");
        };
    }

    #[test]
    fn in_list_literal() {
        let e = parse("status IN (1, 2, 3)");
        let Expr::InList {
            values, negated, ..
        } = e
        else {
            panic!("expected InList");
        };
        assert!(!negated);
        assert_eq!(values.len(), 3);
    }

    #[test]
    fn not_in_list() {
        let e = parse("status NOT IN (1, 2)");
        let Expr::InList { negated: true, .. } = e else {
            panic!("expected negated InList");
        };
    }

    #[test]
    fn function_call_with_args() {
        let e = parse("UPPER(name)");
        let Expr::FunctionCall { name, args, .. } = e else {
            panic!("expected FunctionCall");
        };
        assert_eq!(name, "UPPER");
        assert_eq!(args.len(), 1);
    }

    #[test]
    fn nested_function_call() {
        let e = parse("COALESCE(a, UPPER(b))");
        let Expr::FunctionCall { name, args, .. } = e else {
            panic!("expected FunctionCall");
        };
        assert_eq!(name, "COALESCE");
        assert_eq!(args.len(), 2);
        assert!(matches!(&args[1], Expr::FunctionCall { .. }));
    }

    #[test]
    fn duration_literal_parses_as_text() {
        let e = parse("time_bucket(5m)");
        let Expr::FunctionCall { name, args, .. } = e else {
            panic!("expected FunctionCall, got {e:?}");
        };
        assert_eq!(name.to_uppercase(), "TIME_BUCKET");
        assert_eq!(args.len(), 1);
        assert!(
            matches!(&args[0], Expr::Literal { value: Value::Text(s), .. } if s.as_ref() == "5m"),
            "expected Text(\"5m\"), got {:?}",
            args[0]
        );
    }

    #[test]
    fn placeholder_dollar_one() {
        let e = parse("$1");
        match e {
            Expr::Parameter { index: 0, .. } => {}
            other => panic!("expected Parameter(0), got {other:?}"),
        }
    }

    #[test]
    fn placeholder_dollar_n() {
        let e = parse("$7");
        match e {
            Expr::Parameter { index: 6, .. } => {}
            other => panic!("expected Parameter(6), got {other:?}"),
        }
    }

    #[test]
    fn placeholder_in_string_literal_is_text() {
        // `$1` inside a string literal must NOT parse as a placeholder.
        let e = parse("'$1'");
        match e {
            Expr::Literal {
                value: Value::Text(s),
                ..
            } if s.as_ref() == "$1" => {}
            other => panic!("expected text literal '$1', got {other:?}"),
        }
    }

    #[test]
    fn placeholder_in_comparison() {
        // SELECT-WHERE shape: `id = $1`
        let e = parse("id = $1");
        let Expr::BinaryOp {
            op: BinOp::Eq, rhs, ..
        } = e
        else {
            panic!("root must be Eq");
        };
        assert!(matches!(*rhs, Expr::Parameter { index: 0, .. }));
    }

    #[test]
    fn placeholder_zero_rejected() {
        let mut parser = Parser::new("$0").expect("lexer");
        let err = parser.parse_expr().unwrap_err();
        assert!(err.to_string().contains("placeholder"));
    }

    #[test]
    fn placeholder_question_single() {
        // Lone `?` numbered as parameter 1 (index 0).
        let e = parse("?");
        match e {
            Expr::Parameter { index: 0, .. } => {}
            other => panic!("expected Parameter(0), got {other:?}"),
        }
    }

    #[test]
    fn placeholder_question_numbered() {
        let e = parse("?7");
        match e {
            Expr::Parameter { index: 6, .. } => {}
            other => panic!("expected Parameter(6), got {other:?}"),
        }
    }

    #[test]
    fn placeholder_question_numbered_zero_rejected() {
        let mut parser = Parser::new("?0").expect("lexer");
        let err = parser.parse_expr().unwrap_err();
        assert!(err.to_string().contains("placeholder"));
    }

    #[test]
    fn placeholder_question_left_to_right() {
        // `id = ? AND name = ?` → params 0 and 1
        let e = parse("id = ? AND name = ?");
        let Expr::BinaryOp {
            op: BinOp::And,
            lhs,
            rhs,
            ..
        } = e
        else {
            panic!("root must be And");
        };
        let Expr::BinaryOp {
            op: BinOp::Eq,
            rhs: r1,
            ..
        } = *lhs
        else {
            panic!("lhs must be Eq");
        };
        assert!(matches!(*r1, Expr::Parameter { index: 0, .. }));
        let Expr::BinaryOp {
            op: BinOp::Eq,
            rhs: r2,
            ..
        } = *rhs
        else {
            panic!("rhs must be Eq");
        };
        assert!(matches!(*r2, Expr::Parameter { index: 1, .. }));
    }

    #[test]
    fn placeholder_question_in_string_literal_is_text() {
        let e = parse("'?'");
        match e {
            Expr::Literal {
                value: Value::Text(s),
                ..
            } if s.as_ref() == "?" => {}
            other => panic!("expected text literal '?', got {other:?}"),
        }
    }

    #[test]
    fn placeholder_mixing_question_then_dollar_rejected() {
        let mut parser = Parser::new("id = ? AND x = $2").expect("lexer");
        let err = parser.parse_expr().err().expect("should fail");
        assert!(
            err.to_string().contains("mix"),
            "expected mixing error, got: {err}"
        );
    }

    #[test]
    fn placeholder_mixing_dollar_then_question_rejected() {
        let mut parser = Parser::new("id = $1 AND x = ?").expect("lexer");
        let err = parser.parse_expr().err().expect("should fail");
        assert!(
            err.to_string().contains("mix"),
            "expected mixing error, got: {err}"
        );
    }

    #[test]
    fn placeholder_question_in_comment_ignored() {
        // `?` inside an SQL line comment must not bump the counter.
        // The expression after the comment is the only param.
        let mut parser = Parser::new("-- ? ignored\n  ?").expect("lexer");
        let e = parser.parse_expr().expect("parse_expr");
        match e {
            Expr::Parameter { index: 0, .. } => {}
            other => panic!("expected Parameter(0), got {other:?}"),
        }
    }

    #[test]
    fn unary_plus_is_noop() {
        let e = parse("+42");
        assert!(matches!(
            e,
            Expr::Literal {
                value: Value::Integer(42),
                ..
            }
        ));
    }

    #[test]
    fn parenthesised_select_becomes_subquery_expr() {
        let e = parse("(SELECT 1)");
        assert!(matches!(e, Expr::Subquery { .. }));
    }

    #[test]
    fn bare_zero_arg_current_functions_parse_as_calls() {
        for (input, expected) in [
            ("CURRENT_TIMESTAMP", "CURRENT_TIMESTAMP"),
            ("CURRENT_DATE", "CURRENT_DATE"),
            ("CURRENT_TIME", "CURRENT_TIME"),
        ] {
            let e = parse(input);
            let Expr::FunctionCall { name, args, .. } = e else {
                panic!("expected FunctionCall for {input}");
            };
            assert_eq!(name, expected);
            assert!(args.is_empty());
        }
    }

    #[test]
    fn keyword_function_names_parse_as_calls() {
        for (input, expected_len) in [
            ("COUNT(*)", 1),
            ("SUM(amount)", 1),
            ("LEFT(name, 2)", 2),
            ("RIGHT(name, 2)", 2),
            ("CONTAINS(body, 'red')", 2),
            ("KV(cfg, path)", 2),
        ] {
            let e = parse(input);
            let Expr::FunctionCall { args, .. } = e else {
                panic!("expected FunctionCall for {input}");
            };
            assert_eq!(args.len(), expected_len, "{input}");
        }
    }

    #[test]
    fn count_distinct_lowers_to_count_distinct_function() {
        let e = parse("COUNT(DISTINCT user_id)");
        let Expr::FunctionCall { name, args, .. } = e else {
            panic!("expected FunctionCall");
        };
        assert_eq!(name, "COUNT_DISTINCT");
        assert_eq!(args.len(), 1);
    }

    #[test]
    fn dollar_secret_and_config_refs_become_function_calls() {
        for (input, expected_name, expected_key) in [
            ("$secret.api_key", "__SECRET_REF", "red.vault/api_key"),
            ("$red.secret.api_key", "__SECRET_REF", "red.vault/api_key"),
            ("$config.ai.provider", "CONFIG", "red.config/ai.provider"),
            (
                "$red.config.ai.provider",
                "CONFIG",
                "red.config/ai.provider",
            ),
        ] {
            let e = parse(input);
            let Expr::FunctionCall { name, args, .. } = e else {
                panic!("expected FunctionCall for {input}");
            };
            assert_eq!(name, expected_name);
            assert!(matches!(
                &args[..],
                [Expr::Literal { value: Value::Text(key), .. }] if key.as_ref() == expected_key
            ));
        }
    }

    #[test]
    fn dollar_ref_rejects_unknown_namespace() {
        let mut parser = Parser::new("$tenant.id").expect("lexer");
        let err = parser
            .parse_expr()
            .expect_err("unknown namespace should fail");
        assert!(err.to_string().contains("unknown $ reference"));
    }

    #[test]
    fn config_and_kv_bare_path_args_lowercase_to_text() {
        let e = parse("CONFIG(Red.AI.Default.Provider, 'openai')");
        let Expr::FunctionCall { name, args, .. } = e else {
            panic!("expected FunctionCall");
        };
        assert_eq!(name, "CONFIG");
        assert_eq!(args.len(), 2);
        assert!(matches!(
            &args[0],
            Expr::Literal { value: Value::Text(path), .. }
                if path.as_ref() == "red.ai.default.provider"
        ));
        assert!(matches!(
            &args[1],
            Expr::Literal { value: Value::Text(provider), .. } if provider.as_ref() == "openai"
        ));

        let e = parse("KV(cfg, default.role, LOWER(name))");
        let Expr::FunctionCall { name, args, .. } = e else {
            panic!("expected FunctionCall");
        };
        assert_eq!(name, "KV");
        assert!(matches!(
            &args[0],
            Expr::Literal { value: Value::Text(path), .. } if path.as_ref() == "cfg"
        ));
        assert!(matches!(
            &args[1],
            Expr::Literal { value: Value::Text(path), .. } if path.as_ref() == "default.role"
        ));
        assert!(matches!(&args[2], Expr::FunctionCall { name, .. } if name == "LOWER"));
    }

    #[test]
    fn cast_rejects_unknown_type_name() {
        let mut parser = Parser::new("CAST(age AS BOGUS_TYPE)").expect("lexer");
        let err = parser
            .parse_expr()
            .expect_err("unknown cast target should fail");
        assert!(err.to_string().contains("unknown type name"));
    }

    #[test]
    fn trim_position_and_substring_sql_forms_lower_to_function_args() {
        let e = parse("TRIM(LEADING 'x' FROM name)");
        let Expr::FunctionCall { name, args, .. } = e else {
            panic!("expected trim function");
        };
        assert_eq!(name, "LTRIM");
        assert_eq!(args.len(), 2);

        let e = parse("TRIM(TRAILING FROM name)");
        let Expr::FunctionCall { name, args, .. } = e else {
            panic!("expected trim function");
        };
        assert_eq!(name, "RTRIM");
        assert_eq!(args.len(), 1);

        let e = parse("POSITION('x' IN name)");
        let Expr::FunctionCall { name, args, .. } = e else {
            panic!("expected position function");
        };
        assert_eq!(name, "POSITION");
        assert_eq!(args.len(), 2);

        let e = parse("POSITION('x', name)");
        let Expr::FunctionCall { args, .. } = e else {
            panic!("expected position function");
        };
        assert_eq!(args.len(), 2);

        let e = parse("SUBSTRING(name FROM 2 FOR 3)");
        let Expr::FunctionCall { name, args, .. } = e else {
            panic!("expected substring function");
        };
        assert_eq!(name, "SUBSTRING");
        assert_eq!(args.len(), 3);

        let e = parse("SUBSTRING(name FOR 3)");
        let Expr::FunctionCall { args, .. } = e else {
            panic!("expected substring function");
        };
        assert_eq!(args.len(), 3);
        assert!(matches!(
            args[1],
            Expr::Literal {
                value: Value::Integer(1),
                ..
            }
        ));
    }

    #[test]
    fn postfix_in_accepts_subquery_and_empty_list() {
        let e = parse("id IN (SELECT user_id FROM users)");
        let Expr::InList { values, .. } = e else {
            panic!("expected InList");
        };
        assert!(matches!(&values[..], [Expr::Subquery { .. }]));

        let e = parse("id IN ()");
        let Expr::InList { values, .. } = e else {
            panic!("expected InList");
        };
        assert!(values.is_empty());
    }

    #[test]
    fn postfix_not_requires_between_or_in() {
        let mut parser = Parser::new("status NOT NULL").expect("lexer");
        let err = parser.parse_expr().expect_err("postfix NOT should fail");
        assert!(err.to_string().contains("BETWEEN or IN"));
    }

    #[test]
    fn case_reports_missing_then_end_and_empty_branch() {
        for input in [
            "CASE END",
            "CASE WHEN a = 1 'one' END",
            "CASE WHEN a = 1 THEN 'one'",
        ] {
            let mut parser = Parser::new(input).expect("lexer");
            assert!(
                parser.parse_expr().is_err(),
                "expected CASE parse failure for {input}"
            );
        }
    }

    #[test]
    fn span_tracks_token_range() {
        // A literal's span must cover the exact tokens consumed.
        let mut parser = Parser::new("123 + 456").expect("lexer");
        let e = parser.parse_expr().expect("parse_expr");
        let span = e.span();
        assert!(!span.is_synthetic(), "root span must be real");
        assert!(span.start.offset < span.end.offset);
    }

    // ====================================================================
    // Window OVER clause — issue #589 slice 7a
    // ====================================================================

    fn try_parse(input: &str) -> Result<Expr, ParseError> {
        let mut parser = Parser::new(input).expect("lexer init");
        parser.parse_expr()
    }

    #[test]
    fn window_lag_partition_and_order() {
        let e = parse("LAG(ts) OVER (PARTITION BY user_id ORDER BY ts)");
        let Expr::WindowFunctionCall {
            name, args, window, ..
        } = e
        else {
            panic!("expected WindowFunctionCall");
        };
        assert_eq!(name.to_uppercase(), "LAG");
        assert_eq!(args.len(), 1);
        assert_eq!(window.partition_by.len(), 1);
        assert_eq!(window.order_by.len(), 1);
        assert!(window.order_by[0].ascending);
        assert!(window.frame.is_none());
    }

    #[test]
    fn window_row_number_empty_over() {
        let e = parse("ROW_NUMBER() OVER ()");
        let Expr::WindowFunctionCall {
            name, args, window, ..
        } = e
        else {
            panic!("expected WindowFunctionCall");
        };
        assert_eq!(name.to_uppercase(), "ROW_NUMBER");
        assert!(args.is_empty());
        assert!(window.partition_by.is_empty());
        assert!(window.order_by.is_empty());
        assert!(window.frame.is_none());
    }

    #[test]
    fn window_sum_with_frame_rows_between() {
        let e = parse(
            "SUM(amount) OVER (PARTITION BY user_id ORDER BY ts \
             ROWS BETWEEN 2 PRECEDING AND CURRENT ROW)",
        );
        let Expr::WindowFunctionCall { name, window, .. } = e else {
            panic!("expected WindowFunctionCall");
        };
        assert_eq!(name.to_uppercase(), "SUM");
        let frame = window.frame.expect("frame present");
        assert!(matches!(frame.unit, crate::ast::WindowFrameUnit::Rows));
        assert!(matches!(
            frame.start,
            crate::ast::WindowFrameBound::Preceding(_)
        ));
        assert!(matches!(
            frame.end,
            Some(crate::ast::WindowFrameBound::CurrentRow)
        ));
    }

    #[test]
    fn window_rank_order_desc_multiple_keys() {
        let e = parse("RANK() OVER (ORDER BY score DESC, ts)");
        let Expr::WindowFunctionCall { window, .. } = e else {
            panic!("expected WindowFunctionCall");
        };
        assert_eq!(window.order_by.len(), 2);
        assert!(!window.order_by[0].ascending);
        assert!(window.order_by[1].ascending);
    }

    #[test]
    fn window_unbounded_preceding_following_frame() {
        let e = parse(
            "AVG(x) OVER (ORDER BY t \
             RANGE BETWEEN UNBOUNDED PRECEDING AND UNBOUNDED FOLLOWING)",
        );
        let Expr::WindowFunctionCall { window, .. } = e else {
            panic!("expected WindowFunctionCall");
        };
        let frame = window.frame.expect("frame present");
        assert!(matches!(frame.unit, crate::ast::WindowFrameUnit::Range));
        assert!(matches!(
            frame.start,
            crate::ast::WindowFrameBound::UnboundedPreceding
        ));
        assert!(matches!(
            frame.end,
            Some(crate::ast::WindowFrameBound::UnboundedFollowing)
        ));
    }

    #[test]
    fn window_rejects_non_window_function() {
        // UPPER is a scalar function, not eligible for OVER.
        let err = try_parse("UPPER(name) OVER (PARTITION BY id)")
            .err()
            .expect("should reject scalar OVER");
        let msg = err.to_string();
        assert!(
            msg.contains("UPPER") || msg.contains("upper"),
            "error should mention function name, got: {msg}"
        );
        assert!(msg.to_ascii_uppercase().contains("OVER") || msg.contains("window"));
    }

    #[test]
    fn window_rejects_missing_open_paren() {
        let err = try_parse("LAG(ts) OVER PARTITION BY user_id")
            .err()
            .expect("should reject");
        let msg = err.to_string();
        assert!(
            msg.contains("(") || msg.to_ascii_uppercase().contains("EXPECTED"),
            "got: {msg}"
        );
    }

    #[test]
    fn window_rejects_invalid_frame_syntax() {
        // CURRENT without ROW is malformed.
        let err = try_parse("LAG(ts) OVER (ORDER BY ts ROWS CURRENT)")
            .err()
            .expect("should reject");
        let msg = err.to_string();
        assert!(
            !msg.is_empty(),
            "expected non-empty error for malformed frame"
        );
    }

    #[test]
    fn window_first_value_with_partition_only() {
        let e = parse("FIRST_VALUE(price) OVER (PARTITION BY symbol)");
        let Expr::WindowFunctionCall {
            name, window, args, ..
        } = e
        else {
            panic!("expected WindowFunctionCall");
        };
        assert_eq!(name.to_uppercase(), "FIRST_VALUE");
        assert_eq!(args.len(), 1);
        assert_eq!(window.partition_by.len(), 1);
        assert!(window.order_by.is_empty());
    }

    #[test]
    fn window_order_nulls_first_and_last() {
        let e = parse("SUM(x) OVER (ORDER BY score ASC NULLS FIRST, ts DESC NULLS LAST)");
        let Expr::WindowFunctionCall { window, .. } = e else {
            panic!("expected WindowFunctionCall");
        };
        assert_eq!(window.order_by.len(), 2);
        assert!(window.order_by[0].ascending);
        assert!(window.order_by[0].nulls_first);
        assert!(!window.order_by[1].ascending);
        assert!(!window.order_by[1].nulls_first);
    }

    #[test]
    fn window_single_bound_frames() {
        let e = parse("SUM(x) OVER (ORDER BY ts ROWS 3 PRECEDING)");
        let Expr::WindowFunctionCall { window, .. } = e else {
            panic!("expected WindowFunctionCall");
        };
        let frame = window.frame.expect("frame");
        assert!(matches!(
            frame.start,
            crate::ast::WindowFrameBound::Preceding(_)
        ));
        assert!(frame.end.is_none());

        let e = parse("SUM(x) OVER (ORDER BY ts RANGE 1 FOLLOWING)");
        let Expr::WindowFunctionCall { window, .. } = e else {
            panic!("expected WindowFunctionCall");
        };
        let frame = window.frame.expect("frame");
        assert!(matches!(
            frame.start,
            crate::ast::WindowFrameBound::Following(_)
        ));
        assert!(frame.end.is_none());
    }

    #[test]
    fn window_reports_nulls_and_frame_bound_errors() {
        for input in [
            "SUM(x) OVER (ORDER BY score NULLS MIDDLE)",
            "SUM(x) OVER (ORDER BY score ROWS UNBOUNDED)",
            "SUM(x) OVER (ORDER BY score ROWS 3)",
        ] {
            let err = try_parse(input).expect_err("window syntax should fail");
            assert!(!err.to_string().is_empty(), "{input}");
        }
    }
}
