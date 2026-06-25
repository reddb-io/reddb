//! Filter parsing for WHERE clauses

use super::error::ParseError;
use super::Parser;
use crate::ast::{BinOp, CompareOp, Expr, FieldRef, Filter, Span, UnaryOp};
use crate::lexer::Token;
use reddb_types::types::Value;

fn token_can_start_field_ref(token: &Token) -> bool {
    !matches!(
        token,
        Token::Eof
            | Token::LParen
            | Token::RParen
            | Token::LBracket
            | Token::RBracket
            | Token::Integer(_)
            | Token::Float(_)
            | Token::String(_)
            | Token::True
            | Token::False
            | Token::Null
            | Token::Comma
            | Token::Dot
            | Token::Eq
            | Token::Lt
            | Token::Gt
            | Token::Le
            | Token::Ge
            | Token::Arrow
            | Token::ArrowLeft
            | Token::Dash
            | Token::Colon
            | Token::Semi
            | Token::Star
            | Token::Plus
            | Token::Slash
            | Token::Question
    )
}

fn token_is_bare_zero_arg_function(token: &Token) -> bool {
    match token {
        Token::Ident(name) => matches!(
            name.to_ascii_uppercase().as_str(),
            "CURRENT_TIMESTAMP" | "CURRENT_DATE" | "CURRENT_TIME"
        ),
        _ => false,
    }
}

impl<'a> Parser<'a> {
    /// Parse a filter expression (WHERE condition)
    pub fn parse_filter(&mut self) -> Result<Filter, ParseError> {
        self.parse_or_expr()
    }

    /// Parse OR expression
    fn parse_or_expr(&mut self) -> Result<Filter, ParseError> {
        let mut left = self.parse_and_expr()?;

        while self.consume(&Token::Or)? {
            let right = self.parse_and_expr()?;
            left = Filter::Or(Box::new(left), Box::new(right));
        }

        Ok(left)
    }

    /// Parse AND expression
    fn parse_and_expr(&mut self) -> Result<Filter, ParseError> {
        let mut left = self.parse_not_expr()?;

        while self.consume(&Token::And)? {
            let right = self.parse_not_expr()?;
            left = Filter::And(Box::new(left), Box::new(right));
        }

        Ok(left)
    }

    /// Parse NOT expression
    ///
    /// `NOT` recurses into itself for chained negations (`NOT NOT NOT x`).
    /// Each frame must enter the depth counter — otherwise an
    /// adversarial payload like `NOT NOT NOT … (10k×) … x` overflows
    /// the Rust stack BEFORE `ParserLimits::max_depth` fires (the leaf
    /// only reaches `parse_expr_prec`, which tracks depth, after the
    /// stack has already blown). Bracket the recursion explicitly.
    fn parse_not_expr(&mut self) -> Result<Filter, ParseError> {
        if self.consume(&Token::Not)? {
            self.enter_depth()?;
            let result = self
                .parse_not_expr()
                .map(|expr| Filter::Not(Box::new(expr)));
            self.exit_depth();
            result
        } else {
            self.parse_primary_filter()
        }
    }

    /// Parse primary filter (comparison, parenthesized, etc.)
    fn parse_primary_filter(&mut self) -> Result<Filter, ParseError> {
        let lhs = self.parse_filter_operand_expr()?;
        let lhs_field = expr_as_field_ref(&lhs);

        // IS NULL / IS NOT NULL
        if self.consume(&Token::Is)? {
            let negated = self.consume(&Token::Not)?;
            self.expect(Token::Null)?;
            return Ok(if let Some(field) = lhs_field.clone() {
                if negated {
                    Filter::IsNotNull(field)
                } else {
                    Filter::IsNull(field)
                }
            } else {
                Filter::CompareExpr {
                    lhs: Expr::IsNull {
                        operand: Box::new(lhs),
                        negated,
                        span: Span::synthetic(),
                    },
                    op: CompareOp::Eq,
                    rhs: Expr::Literal {
                        value: Value::Boolean(true),
                        span: Span::synthetic(),
                    },
                }
            });
        }

        // Infix `NOT IN` / `NOT LIKE`. A prefix `NOT` is consumed earlier at
        // `parse_not_expr`, so reaching this point with `NOT` means the user
        // wrote the infix negated form `lhs NOT IN (...)` / `lhs NOT LIKE '…'`.
        // Only `IN` and `LIKE` follow an infix NOT here; anything else stays an
        // error so we don't silently swallow malformed input.
        let negated = matches!(self.peek(), Token::Not)
            && matches!(self.peek_next()?, Token::In | Token::Like);
        if negated {
            self.advance()?;
        }

        if self.consume(&Token::Between)? {
            if let Some(field) = lhs_field.clone() {
                let low = self.parse_value_or_field()?;
                self.expect(Token::And)?;
                let high = self.parse_value_or_field()?;
                return Ok(match (low, high) {
                    (ValueOrField::Value(low), ValueOrField::Value(high)) => {
                        Filter::Between { field, low, high }
                    }
                    (ValueOrField::Field(low_field), ValueOrField::Field(high_field)) => {
                        Filter::And(
                            Box::new(Filter::CompareFields {
                                left: field.clone(),
                                op: CompareOp::Ge,
                                right: low_field,
                            }),
                            Box::new(Filter::CompareFields {
                                left: field,
                                op: CompareOp::Le,
                                right: high_field,
                            }),
                        )
                    }
                    (ValueOrField::Value(low), ValueOrField::Field(high_field)) => Filter::And(
                        Box::new(Filter::Compare {
                            field: field.clone(),
                            op: CompareOp::Ge,
                            value: low,
                        }),
                        Box::new(Filter::CompareFields {
                            left: field,
                            op: CompareOp::Le,
                            right: high_field,
                        }),
                    ),
                    (ValueOrField::Field(low_field), ValueOrField::Value(high)) => Filter::And(
                        Box::new(Filter::CompareFields {
                            left: field.clone(),
                            op: CompareOp::Ge,
                            right: low_field,
                        }),
                        Box::new(Filter::Compare {
                            field,
                            op: CompareOp::Le,
                            value: high,
                        }),
                    ),
                });
            }

            let low = self.parse_filter_operand_expr()?;
            self.expect(Token::And)?;
            let high = self.parse_filter_operand_expr()?;
            return Ok(Filter::CompareExpr {
                lhs: Expr::Between {
                    target: Box::new(lhs),
                    low: Box::new(low),
                    high: Box::new(high),
                    negated: false,
                    span: Span::synthetic(),
                },
                op: CompareOp::Eq,
                rhs: Expr::Literal {
                    value: Value::Boolean(true),
                    span: Span::synthetic(),
                },
            });
        }

        // IN
        if self.consume(&Token::In)? {
            if let Some(field) = lhs_field.clone() {
                self.expect(Token::LParen)?;
                if self.check(&Token::Select) {
                    // Bracket the subquery recursion in the depth counter so a
                    // deeply nested `x IN (SELECT … WHERE x IN (…))` payload
                    // hits `max_depth` and errors instead of overflowing the
                    // Rust stack before the limit can fire (mirrors
                    // `parse_not_expr`).
                    self.enter_depth()?;
                    let query = self.parse_select_query();
                    self.exit_depth();
                    let query = query?;
                    self.expect(Token::RParen)?;
                    return Ok(negate_if(
                        negated,
                        Filter::CompareExpr {
                            lhs: Expr::InList {
                                target: Box::new(lhs),
                                values: vec![Expr::Subquery {
                                    query: crate::ast::ExprSubquery {
                                        query: Box::new(query),
                                    },
                                    span: Span::synthetic(),
                                }],
                                negated: false,
                                span: Span::synthetic(),
                            },
                            op: CompareOp::Eq,
                            rhs: Expr::Literal {
                                value: Value::Boolean(true),
                                span: Span::synthetic(),
                            },
                        },
                    ));
                }
                let values = self.parse_value_list()?;
                self.expect(Token::RParen)?;
                return Ok(negate_if(negated, Filter::In { field, values }));
            }

            self.expect(Token::LParen)?;
            let values = self.parse_filter_expr_list()?;
            self.expect(Token::RParen)?;
            return Ok(negate_if(
                negated,
                Filter::CompareExpr {
                    lhs: Expr::InList {
                        target: Box::new(lhs),
                        values,
                        negated: false,
                        span: Span::synthetic(),
                    },
                    op: CompareOp::Eq,
                    rhs: Expr::Literal {
                        value: Value::Boolean(true),
                        span: Span::synthetic(),
                    },
                },
            ));
        }

        // LIKE
        if self.consume(&Token::Like)? {
            let Some(field) = lhs_field.clone() else {
                return Err(ParseError::new(
                    "LIKE requires a column reference on the left-hand side".to_string(),
                    self.position(),
                ));
            };
            let pattern = self.parse_string()?;
            return Ok(negate_if(negated, Filter::Like { field, pattern }));
        }

        // STARTS WITH
        if self.consume(&Token::Starts)? {
            let Some(field) = lhs_field.clone() else {
                return Err(ParseError::new(
                    "STARTS WITH requires a column reference on the left-hand side".to_string(),
                    self.position(),
                ));
            };
            self.expect(Token::With)?;
            let prefix = self.parse_string()?;
            return Ok(Filter::StartsWith { field, prefix });
        }

        // ENDS WITH
        if self.consume(&Token::Ends)? {
            let Some(field) = lhs_field.clone() else {
                return Err(ParseError::new(
                    "ENDS WITH requires a column reference on the left-hand side".to_string(),
                    self.position(),
                ));
            };
            self.expect(Token::With)?;
            let suffix = self.parse_string()?;
            return Ok(Filter::EndsWith { field, suffix });
        }

        // CONTAINS
        if self.consume(&Token::Contains)? {
            let Some(field) = lhs_field.clone() else {
                return Err(ParseError::new(
                    "CONTAINS requires a column reference on the left-hand side".to_string(),
                    self.position(),
                ));
            };
            let substring = self.parse_string()?;
            return Ok(Filter::Contains { field, substring });
        }

        if matches!(
            self.peek(),
            Token::And | Token::Or | Token::RParen | Token::Eof
        ) && expr_is_booleanish(&lhs)
        {
            return Ok(Filter::CompareExpr {
                lhs,
                op: CompareOp::Eq,
                rhs: Expr::Literal {
                    value: Value::Boolean(true),
                    span: Span::synthetic(),
                },
            });
        }

        // Comparison operator — now accepts an `Expr` RHS so users
        // can write `WHERE age = price + 1`, `WHERE status = CAST(flag AS TEXT)`,
        // or `WHERE name = UPPER(alias)`. The parser uses a cheap
        // token lookahead to pick the fast `Filter::Compare` form
        // when the RHS is a bare literal, and falls back to
        // `Filter::CompareExpr` when it sees anything expression-shaped.
        let op = self.parse_compare_op()?;
        if let Some(field) = lhs_field {
            if self.rhs_looks_like_bare_field_ref()? {
                let start = self.position();
                let right = self.parse_field_ref()?;
                if !self.rhs_field_ref_extends_to_expression() {
                    return Ok(Filter::CompareFields {
                        left: field.clone(),
                        op,
                        right,
                    });
                }
                let rhs = self.continue_expr(
                    Expr::Column {
                        field: right,
                        span: Span::new(start, self.position()),
                    },
                    0,
                )?;
                return Ok(Filter::CompareExpr { lhs, op, rhs });
            }
            if self.rhs_looks_like_expression() {
                let rhs = self.parse_filter_operand_expr()?;
                return Ok(Filter::CompareExpr { lhs, op, rhs });
            }
            let value = self.parse_value()?;
            return Ok(Filter::Compare { field, op, value });
        }

        let rhs = if self.rhs_looks_like_bare_field_ref()? || self.rhs_looks_like_expression() {
            self.parse_filter_operand_expr()?
        } else {
            Expr::Literal {
                value: self.parse_value()?,
                span: Span::synthetic(),
            }
        };
        Ok(Filter::CompareExpr { lhs, op, rhs })
    }

    fn parse_filter_operand_expr(&mut self) -> Result<Expr, ParseError> {
        // Comparison and postfix predicate operators stay at the Filter layer.
        self.parse_expr_with_min_precedence(35)
    }

    fn parse_filter_expr_list(&mut self) -> Result<Vec<Expr>, ParseError> {
        let mut values = Vec::new();
        if self.peek() == &Token::RParen {
            return Ok(values);
        }
        loop {
            values.push(self.parse_filter_operand_expr()?);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        Ok(values)
    }

    /// Returns true when the next token starts an expression-shaped
    /// RHS (function call, CAST, CASE, parenthesised, unary sign, or
    /// any identifier that isn't already in the literal-value set).
    /// Used by `parse_primary_filter` to decide between the fast
    /// `Compare` form and the general `CompareExpr` form.
    fn rhs_looks_like_expression(&self) -> bool {
        match self.peek() {
            // Literal tokens stay on the fast path.
            Token::Integer(_)
            | Token::Float(_)
            | Token::String(_)
            | Token::True
            | Token::False
            | Token::Null => false,
            // Minus is ambiguous — `-5` is a literal, `-col` is an
            // expression. The legacy `parse_value` already handles
            // the literal case, so err on the literal side.
            Token::Dash => false,
            // Anything else that can start a primary expression goes
            // through the Expr path.
            Token::Dollar | Token::Question => true,
            Token::LParen => true,
            token if token_can_start_field_ref(token) => true,
            _ => false,
        }
    }

    /// Bare RHS identifiers should stay on the `CompareFields` fast
    /// path unless they are immediately followed by `(`, which makes
    /// them a function call and therefore a general expression.
    fn rhs_looks_like_bare_field_ref(&mut self) -> Result<bool, ParseError> {
        match self.peek() {
            Token::Dollar | Token::Question => Ok(false),
            token if token_is_bare_zero_arg_function(token) => Ok(false),
            token if token_can_start_field_ref(token) => {
                Ok(!matches!(self.peek_next()?, Token::LParen))
            }
            _ => Ok(false),
        }
    }

    fn rhs_field_ref_extends_to_expression(&self) -> bool {
        matches!(
            self.peek(),
            Token::Eq
                | Token::Ne
                | Token::Lt
                | Token::Le
                | Token::Gt
                | Token::Ge
                | Token::Plus
                | Token::Dash
                | Token::Star
                | Token::Slash
                | Token::Percent
                | Token::DoublePipe
                | Token::Is
                | Token::Between
                | Token::In
        )
    }

    /// Parse either a literal Value or a FieldRef. Used by BETWEEN
    /// and other RHS positions that tolerate column-to-column
    /// predicates. Decides based on the next token — literals
    /// (Integer / Float / String / TRUE / FALSE / NULL / minus)
    /// go through parse_value; anything else is treated as an
    /// identifier / qualified column reference.
    pub(super) fn parse_value_or_field(&mut self) -> Result<ValueOrField, ParseError> {
        match self.peek() {
            Token::Integer(_)
            | Token::Float(_)
            | Token::String(_)
            | Token::True
            | Token::False
            | Token::Null
            | Token::Dash => Ok(ValueOrField::Value(self.parse_value()?)),
            _ => Ok(ValueOrField::Field(self.parse_field_ref()?)),
        }
    }

    /// Parse comparison operator
    fn parse_compare_op(&mut self) -> Result<CompareOp, ParseError> {
        let op = match self.peek() {
            Token::Eq => CompareOp::Eq,
            Token::Ne => CompareOp::Ne,
            Token::Lt => CompareOp::Lt,
            Token::Le => CompareOp::Le,
            Token::Gt => CompareOp::Gt,
            Token::Ge => CompareOp::Ge,
            other => {
                return Err(ParseError::expected(
                    vec!["=", "<>", "<", "<=", ">", ">="],
                    other,
                    self.position(),
                ))
            }
        };
        self.advance()?;
        Ok(op)
    }

    /// Parse field reference (table.column or just column)
    pub fn parse_field_ref(&mut self) -> Result<FieldRef, ParseError> {
        let mut segments = vec![self.parse_field_ref_segment()?];
        while self.consume(&Token::Dot)? {
            segments.push(self.parse_field_ref_segment()?);
        }

        match segments.len() {
            0 => unreachable!("field reference must have at least one segment"),
            1 => Ok(FieldRef::TableColumn {
                table: String::new(),
                column: segments.pop().unwrap(),
            }),
            _ => Ok(FieldRef::TableColumn {
                table: segments.remove(0),
                column: segments.join("."),
            }),
        }
    }

    fn parse_field_ref_segment(&mut self) -> Result<String, ParseError> {
        match &self.current.token {
            Token::Ident(name) => {
                let name = name.clone();
                self.advance()?;
                Ok(name)
            }
            Token::Eof
            | Token::LParen
            | Token::RParen
            | Token::LBracket
            | Token::RBracket
            | Token::Comma
            | Token::Dot
            | Token::Eq
            | Token::Lt
            | Token::Gt
            | Token::Le
            | Token::Ge
            | Token::Arrow
            | Token::ArrowLeft
            | Token::Dash
            | Token::Colon
            | Token::Semi
            | Token::Star
            | Token::Plus
            | Token::Slash => Err(ParseError::expected(
                vec!["identifier or field name"],
                &self.current.token,
                self.position(),
            )),
            other => {
                let name = other.to_string().to_ascii_lowercase();
                self.advance()?;
                Ok(name)
            }
        }
    }
}

/// Result of parsing an RHS that accepts either a literal value or a
/// column reference. Temporary shim until Fase 2 introduces a proper
/// `Expr` AST that can unify the two under one walker.
pub(super) enum ValueOrField {
    Value(Value),
    Field(FieldRef),
}

/// Wrap `filter` in `Filter::Not` when `negated` is set. Used by the infix
/// `NOT IN` / `NOT LIKE` forms, which reuse the positive predicate construction
/// and negate the whole result.
fn negate_if(negated: bool, filter: Filter) -> Filter {
    if negated {
        Filter::Not(Box::new(filter))
    } else {
        filter
    }
}

fn expr_as_field_ref(expr: &Expr) -> Option<FieldRef> {
    match expr {
        Expr::Column { field, .. } => Some(field.clone()),
        _ => None,
    }
}

fn expr_is_booleanish(expr: &Expr) -> bool {
    match expr {
        Expr::Literal {
            value: Value::Boolean(_),
            ..
        } => true,
        Expr::BinaryOp { op, .. } => matches!(
            op,
            BinOp::Eq
                | BinOp::Ne
                | BinOp::Lt
                | BinOp::Le
                | BinOp::Gt
                | BinOp::Ge
                | BinOp::And
                | BinOp::Or
        ),
        Expr::UnaryOp {
            op: UnaryOp::Not, ..
        }
        | Expr::IsNull { .. }
        | Expr::InList { .. }
        | Expr::Between { .. } => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(input: &str) -> Filter {
        let mut parser =
            Parser::new(input).unwrap_or_else(|err| panic!("failed to lex {input:?}: {err:?}"));
        let filter = parser
            .parse_filter()
            .unwrap_or_else(|err| panic!("failed to parse filter {input:?}: {err:?}"));
        assert_eq!(
            parser.peek(),
            &Token::Eof,
            "filter did not consume all input: {input:?}"
        );
        filter
    }

    fn parse_err(input: &str) -> ParseError {
        let mut parser =
            Parser::new(input).unwrap_or_else(|err| panic!("failed to lex {input:?}: {err:?}"));
        parser.parse_filter().unwrap_err()
    }

    fn assert_table_column(field: &FieldRef, table: &str, column: &str) {
        let FieldRef::TableColumn {
            table: actual_table,
            column: actual_column,
        } = field
        else {
            panic!("expected table column, got {field:?}");
        };
        assert_eq!(actual_table, table);
        assert_eq!(actual_column, column);
    }

    fn assert_compare_integer(filter: &Filter, column: &str, op: CompareOp, expected: i64) {
        let Filter::Compare {
            field,
            op: actual_op,
            value,
        } = filter
        else {
            panic!("expected integer Compare filter, got {filter:?}");
        };
        assert_table_column(field, "", column);
        assert_eq!(*actual_op, op);
        assert_eq!(value, &Value::Integer(expected));
    }

    fn assert_true_literal(expr: &Expr) {
        assert!(
            matches!(
                expr,
                Expr::Literal {
                    value: Value::Boolean(true),
                    ..
                }
            ),
            "expected literal true, got {expr:?}"
        );
    }

    fn assert_text_value(value: &Value, expected: &str) {
        let Value::Text(actual) = value else {
            panic!("expected text value {expected:?}, got {value:?}");
        };
        assert_eq!(actual.as_ref(), expected);
    }

    #[test]
    fn parse_or_expression_chains_left_associatively() {
        let filter = parse("a = 1 OR b = 2 OR c = 3");
        let Filter::Or(left, right) = filter else {
            panic!("expected OR filter");
        };

        assert!(matches!(*left, Filter::Or(_, _)));
        assert_compare_integer(right.as_ref(), "c", CompareOp::Eq, 3);
    }

    #[test]
    fn parse_all_compare_operators_on_literal_rhs() {
        for (input, expected_op) in [
            ("a = 1", CompareOp::Eq),
            ("a <> 1", CompareOp::Ne),
            ("a < 1", CompareOp::Lt),
            ("a <= 1", CompareOp::Le),
            ("a > 1", CompareOp::Gt),
            ("a >= 1", CompareOp::Ge),
        ] {
            assert_compare_integer(&parse(input), "a", expected_op, 1);
        }
    }

    #[test]
    fn parse_compare_rhs_field_value_and_expression_paths() {
        let filter = parse("temp = max_temp");
        let Filter::CompareFields { left, op, right } = filter else {
            panic!("expected CompareFields");
        };
        assert_table_column(&left, "", "temp");
        assert_eq!(op, CompareOp::Eq);
        assert_table_column(&right, "", "max_temp");

        let filter = parse("temp = max_temp + 1");
        let Filter::CompareExpr { rhs, .. } = filter else {
            panic!("expected CompareExpr for arithmetic rhs");
        };
        assert!(matches!(rhs, Expr::BinaryOp { op: BinOp::Add, .. }));

        let filter = parse("created_at >= CURRENT_TIMESTAMP");
        let Filter::CompareExpr { rhs, .. } = filter else {
            panic!("expected CompareExpr for bare zero-arg function rhs");
        };
        assert!(matches!(
            rhs,
            Expr::FunctionCall { ref name, ref args, .. }
                if name == "CURRENT_TIMESTAMP" && args.is_empty()
        ));

        let filter = parse("LOWER(name) = 'ada'");
        let Filter::CompareExpr { lhs, rhs, .. } = filter else {
            panic!("expected CompareExpr for expression lhs");
        };
        assert!(matches!(lhs, Expr::FunctionCall { ref name, .. } if name == "LOWER"));
        assert!(matches!(
            rhs,
            Expr::Literal {
                value: Value::Text(ref text),
                ..
            } if text.as_ref() == "ada"
        ));
    }

    #[test]
    fn parse_in_value_expression_and_subquery_lists() {
        let filter = parse("status IN ('open', 'closed')");
        let Filter::In { field, values } = filter else {
            panic!("expected value-list IN filter");
        };
        assert_table_column(&field, "", "status");
        assert_eq!(values.len(), 2);
        assert_text_value(&values[0], "open");
        assert_text_value(&values[1], "closed");

        let filter = parse("LOWER(name) IN ('ada', UPPER(alias), 1 + 2)");
        let Filter::CompareExpr { lhs, op, rhs } = filter else {
            panic!("expected expression-list IN as CompareExpr");
        };
        assert_eq!(op, CompareOp::Eq);
        assert_true_literal(&rhs);
        let Expr::InList { target, values, .. } = lhs else {
            panic!("expected InList lhs");
        };
        assert!(matches!(*target, Expr::FunctionCall { ref name, .. } if name == "LOWER"));
        assert_eq!(values.len(), 3);
        assert!(matches!(values[0], Expr::Literal { .. }));
        assert!(matches!(values[1], Expr::FunctionCall { ref name, .. } if name == "UPPER"));
        assert!(matches!(values[2], Expr::BinaryOp { op: BinOp::Add, .. }));

        let filter = parse("LOWER(name) IN ()");
        let Filter::CompareExpr { lhs, .. } = filter else {
            panic!("expected empty expression-list IN as CompareExpr");
        };
        let Expr::InList { values, .. } = lhs else {
            panic!("expected InList lhs");
        };
        assert!(values.is_empty());

        let filter = parse("id IN (SELECT user_id FROM users)");
        let Filter::CompareExpr { lhs, rhs, .. } = filter else {
            panic!("expected subquery IN as CompareExpr");
        };
        assert_true_literal(&rhs);
        let Expr::InList { values, .. } = lhs else {
            panic!("expected InList lhs");
        };
        assert!(matches!(&values[..], [Expr::Subquery { .. }]));
    }

    #[test]
    fn parse_between_literal_mixed_and_expression_variants() {
        let filter = parse("temp BETWEEN 10 AND 20");
        let Filter::Between { field, low, high } = filter else {
            panic!("expected literal BETWEEN");
        };
        assert_table_column(&field, "", "temp");
        assert_eq!(low, Value::Integer(10));
        assert_eq!(high, Value::Integer(20));

        let filter = parse("temp BETWEEN 0 AND max_temp");
        let Filter::And(left, right) = filter else {
            panic!("expected mixed value/field BETWEEN to lower to AND");
        };
        assert_compare_integer(left.as_ref(), "temp", CompareOp::Ge, 0);
        let Filter::CompareFields { left, op, right } = right.as_ref() else {
            panic!("expected upper field bound CompareFields");
        };
        assert_table_column(left, "", "temp");
        assert_eq!(*op, CompareOp::Le);
        assert_table_column(right, "", "max_temp");

        let filter = parse("temp BETWEEN min_temp AND 100");
        let Filter::And(left, right) = filter else {
            panic!("expected mixed field/value BETWEEN to lower to AND");
        };
        let Filter::CompareFields {
            left: lower_left,
            op,
            right: lower_right,
        } = left.as_ref()
        else {
            panic!("expected lower field bound CompareFields");
        };
        assert_table_column(lower_left, "", "temp");
        assert_eq!(*op, CompareOp::Ge);
        assert_table_column(lower_right, "", "min_temp");
        assert_compare_integer(right.as_ref(), "temp", CompareOp::Le, 100);

        let filter = parse("LOWER(name) BETWEEN 'a' AND 'm'");
        let Filter::CompareExpr { lhs, op, rhs } = filter else {
            panic!("expected expression BETWEEN to become CompareExpr");
        };
        assert_eq!(op, CompareOp::Eq);
        assert_true_literal(&rhs);
        let Expr::Between {
            target,
            low,
            high,
            negated,
            ..
        } = lhs
        else {
            panic!("expected Expr::Between lhs");
        };
        assert!(!negated);
        assert!(matches!(*target, Expr::FunctionCall { ref name, .. } if name == "LOWER"));
        assert!(matches!(
            *low,
            Expr::Literal {
                value: Value::Text(ref text),
                ..
            } if text.as_ref() == "a"
        ));
        assert!(matches!(
            *high,
            Expr::Literal {
                value: Value::Text(ref text),
                ..
            } if text.as_ref() == "m"
        ));
    }

    #[test]
    fn parse_string_predicates_and_infix_not() {
        let filter = parse("name LIKE 'A%'");
        let Filter::Like { field, pattern } = filter else {
            panic!("expected LIKE filter");
        };
        assert_table_column(&field, "", "name");
        assert_eq!(pattern, "A%");

        let filter = parse("name NOT LIKE 'A%'");
        let Filter::Not(inner) = filter else {
            panic!("expected NOT LIKE filter");
        };
        assert!(matches!(*inner, Filter::Like { .. }));

        let filter = parse("name STARTS WITH 'A'");
        let Filter::StartsWith { field, prefix } = filter else {
            panic!("expected STARTS WITH filter");
        };
        assert_table_column(&field, "", "name");
        assert_eq!(prefix, "A");

        let filter = parse("name ENDS WITH 'z'");
        let Filter::EndsWith { field, suffix } = filter else {
            panic!("expected ENDS WITH filter");
        };
        assert_table_column(&field, "", "name");
        assert_eq!(suffix, "z");

        let filter = parse("name CONTAINS 'mid'");
        let Filter::Contains { field, substring } = filter else {
            panic!("expected CONTAINS filter");
        };
        assert_table_column(&field, "", "name");
        assert_eq!(substring, "mid");
    }

    #[test]
    fn string_predicates_reject_expression_lhs() {
        for (input, message) in [
            (
                "LOWER(name) LIKE 'a%'",
                "LIKE requires a column reference on the left-hand side",
            ),
            (
                "LOWER(name) STARTS WITH 'a'",
                "STARTS WITH requires a column reference on the left-hand side",
            ),
            (
                "LOWER(name) ENDS WITH 'z'",
                "ENDS WITH requires a column reference on the left-hand side",
            ),
            (
                "LOWER(name) CONTAINS 'm'",
                "CONTAINS requires a column reference on the left-hand side",
            ),
        ] {
            let err = parse_err(input);
            assert!(
                err.to_string().contains(message),
                "unexpected error for {input:?}: {err}"
            );
        }
    }

    #[test]
    fn booleanish_lhs_becomes_compare_expr_against_true() {
        let filter = parse("TRUE");
        let Filter::CompareExpr { lhs, op, rhs } = filter else {
            panic!("expected boolean literal to become CompareExpr");
        };
        assert_eq!(op, CompareOp::Eq);
        assert_true_literal(&lhs);
        assert_true_literal(&rhs);
    }

    #[test]
    fn compare_operator_error_reports_expected_ops() {
        let err = parse_err("age AND active = true");
        let message = err.to_string();
        assert!(message.contains("="), "{message}");
        assert!(message.contains("<="), "{message}");
        assert!(message.contains(">="), "{message}");
    }

    #[test]
    fn field_ref_segments_accept_keywords_and_reject_empty_segments() {
        let mut parser = Parser::new("table.ORDER.by").expect("lexer init");
        let field = parser.parse_field_ref().expect("parse field ref");
        assert_table_column(&field, "table", "order.by");
        assert_eq!(parser.peek(), &Token::Eof);

        for input in ["table.", "*"] {
            let mut parser = Parser::new(input).expect("lexer init");
            let err = parser.parse_field_ref().unwrap_err();
            assert!(
                err.to_string().contains("identifier or field name"),
                "unexpected error for {input:?}: {err}"
            );
        }
    }
}
