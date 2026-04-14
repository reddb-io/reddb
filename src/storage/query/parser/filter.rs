//! Filter parsing for WHERE clauses

use super::super::ast::{CompareOp, FieldRef, Filter};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;
use crate::storage::schema::Value;

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

        // Field-based filter
        let field = self.parse_field_ref()?;

        // IS NULL / IS NOT NULL
        if self.consume(&Token::Is)? {
            let negated = self.consume(&Token::Not)?;
            self.expect(Token::Null)?;
            return Ok(if negated {
                Filter::IsNotNull(field)
            } else {
                Filter::IsNull(field)
            });
        }

        // BETWEEN — accept either literal-low/literal-high (emits
        // Filter::Between for backwards compat) or column-low/column-
        // high (decomposes to `field >= low AND field <= high` using
        // the new Filter::CompareFields variant). Mixed forms are
        // deferred until Fase 2's Expr AST rewrite.
        if self.consume(&Token::Between)? {
            let low = self.parse_value_or_field()?;
            self.expect(Token::And)?;
            let high = self.parse_value_or_field()?;
            return Ok(match (low, high) {
                (ValueOrField::Value(low), ValueOrField::Value(high)) => {
                    Filter::Between { field, low, high }
                }
                (ValueOrField::Field(low_field), ValueOrField::Field(high_field)) => Filter::And(
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
                ),
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

        // IN
        if self.consume(&Token::In)? {
            self.expect(Token::LParen)?;
            let values = self.parse_value_list()?;
            self.expect(Token::RParen)?;
            return Ok(Filter::In { field, values });
        }

        // LIKE
        if self.consume(&Token::Like)? {
            let pattern = self.parse_string()?;
            return Ok(Filter::Like { field, pattern });
        }

        // STARTS WITH
        if self.consume(&Token::Starts)? {
            self.expect(Token::With)?;
            let prefix = self.parse_string()?;
            return Ok(Filter::StartsWith { field, prefix });
        }

        // ENDS WITH
        if self.consume(&Token::Ends)? {
            self.expect(Token::With)?;
            let suffix = self.parse_string()?;
            return Ok(Filter::EndsWith { field, suffix });
        }

        // CONTAINS
        if self.consume(&Token::Contains)? {
            let substring = self.parse_string()?;
            return Ok(Filter::Contains { field, substring });
        }

        // Comparison operator
        let op = self.parse_compare_op()?;
        let value = self.parse_value()?;

        Ok(Filter::Compare { field, op, value })
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
