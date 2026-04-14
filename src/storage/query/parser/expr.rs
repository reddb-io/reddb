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
//! The parser is **not** wired into the main `parse_*_query` flow yet.
//! That migration lands in Week 3, where `OrderByClause`, the RHS of
//! `Filter::Compare`, and `TableQuery.table` all grow `Expr` slots.
//! Until then, this module exposes `Parser::parse_expr` as a standalone
//! entry point that tests and shim paths can call explicitly.
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

use super::super::ast::{BinOp, Expr, FieldRef, Span, UnaryOp};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;
use crate::storage::schema::{DataType, Value};

impl<'a> Parser<'a> {
    /// Parse a complete expression at the lowest precedence level.
    /// Entry point for every caller that wants an `Expr` tree.
    pub fn parse_expr(&mut self) -> Result<Expr, ParseError> {
        self.parse_expr_prec(0)
    }

    /// Pratt climb: parse a unary atom then consume any infix operators
    /// whose precedence meets or exceeds `min_prec`.
    fn parse_expr_prec(&mut self, min_prec: u8) -> Result<Expr, ParseError> {
        let mut left = self.parse_expr_unary()?;
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

        // Numeric literals
        if let Token::Integer(n) = *self.peek() {
            self.advance()?;
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
                value: Value::Text(text),
                span: Span::new(start, self.position()),
            });
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
                self.advance()?; // consume `(`
                                 // CAST is sugar — args are `expr AS type_name`.
                if saved_name.eq_ignore_ascii_case("CAST") {
                    let inner = self.parse_expr_prec(0)?;
                    self.expect(Token::As)?;
                    let type_name = self.expect_ident_or_keyword()?;
                    self.expect(Token::RParen)?;
                    let end = self.position();
                    let Some(target) = DataType::from_sql_name(&type_name) else {
                        return Err(ParseError::new(
                            format!("unknown type name `{type_name}` in CAST"),
                            self.position(),
                        ));
                    };
                    return Ok(Expr::Cast {
                        inner: Box::new(inner),
                        target,
                        span: Span::new(start, end),
                    });
                }
                // Generic function call.
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
                return Ok(Expr::FunctionCall {
                    name: saved_name,
                    args,
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

    /// Parse `CASE WHEN cond THEN val [WHEN …] [ELSE val] END`.
    /// Assumes the caller has already peeked `CASE`.
    fn parse_case_expr(
        &mut self,
        start: crate::storage::query::lexer::Position,
    ) -> Result<Expr, ParseError> {
        self.advance()?; // consume CASE
        let mut branches: Vec<(Expr, Expr)> = Vec::new();
        loop {
            if !self.consume_ident_ci("WHEN")? {
                break;
            }
            let cond = self.parse_expr_prec(0)?;
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
            if !self.check(&Token::RParen) {
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
    fn span_start_of(&self, expr: &Expr) -> crate::storage::query::lexer::Position {
        let s = expr.span();
        if s.is_synthetic() {
            self.position()
        } else {
            s.start
        }
    }

    /// Return the end position of an expression's span — same
    /// synthetic fallback as `span_start_of`.
    fn span_end_of(&self, expr: &Expr) -> crate::storage::query::lexer::Position {
        let s = expr.span();
        if s.is_synthetic() {
            self.position()
        } else {
            s.end
        }
    }
}

// Avoid `unused` lints if no integration yet references this type —
// Week 3 wires it into the analyze pass and removes this shim.
#[allow(dead_code)]
fn _expr_module_used(_: Expr) {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::query::ast::FieldRef;

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
            } if s == "hello" => {}
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
    fn span_tracks_token_range() {
        // A literal's span must cover the exact tokens consumed.
        let mut parser = Parser::new("123 + 456").expect("lexer");
        let e = parser.parse_expr().expect("parse_expr");
        let span = e.span();
        assert!(!span.is_synthetic(), "root span must be real");
        assert!(span.start.offset < span.end.offset);
    }
}
