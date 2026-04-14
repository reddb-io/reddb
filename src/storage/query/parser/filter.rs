//! Filter parsing for WHERE clauses

use super::super::ast::{CompareOp, Expr, FieldRef, Filter, Span};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;
use crate::storage::schema::Value;

fn token_can_start_field_ref(token: &Token) -> bool {
    !matches!(
        token,
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
            | Token::Slash
    )
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
    fn parse_not_expr(&mut self) -> Result<Filter, ParseError> {
        if self.consume(&Token::Not)? {
            let expr = self.parse_not_expr()?;
            Ok(Filter::Not(Box::new(expr)))
        } else {
            self.parse_primary_filter()
        }
    }

    /// Parse primary filter (comparison, parenthesized, etc.)
    fn parse_primary_filter(&mut self) -> Result<Filter, ParseError> {
        // Parenthesized expression
        if self.consume(&Token::LParen)? {
            let expr = self.parse_filter()?;
            self.expect(Token::RParen)?;
            return Ok(expr);
        }

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
                let values = self.parse_value_list()?;
                self.expect(Token::RParen)?;
                return Ok(Filter::In { field, values });
            }

            self.expect(Token::LParen)?;
            let values = self.parse_filter_expr_list()?;
            self.expect(Token::RParen)?;
            return Ok(Filter::CompareExpr {
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
            });
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
            return Ok(Filter::Like { field, pattern });
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

fn expr_as_field_ref(expr: &Expr) -> Option<FieldRef> {
    match expr {
        Expr::Column { field, .. } => Some(field.clone()),
        _ => None,
    }
}
