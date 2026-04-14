//! Table query parsing (SELECT ... FROM ...)

use super::super::ast::{
    BinOp, CompareOp, Expr, FieldRef, Filter, OrderByClause, Projection, QueryExpr, SelectItem,
    Span, TableQuery, UnaryOp,
};
use super::super::lexer::Token;
use super::error::ParseError;
use crate::storage::query::sql_lowering::{expr_to_projection, filter_to_expr};
use crate::storage::schema::Value;

fn is_scalar_function(name: &str) -> bool {
    matches!(
        name,
        "GEO_DISTANCE"
            | "GEO_DISTANCE_VINCENTY"
            | "GEO_BEARING"
            | "GEO_MIDPOINT"
            | "HAVERSINE"
            | "VINCENTY"
            | "TIME_BUCKET"
            | "UPPER"
            | "LOWER"
            | "LENGTH"
            | "CHAR_LENGTH"
            | "CHARACTER_LENGTH"
            | "OCTET_LENGTH"
            | "BIT_LENGTH"
            | "SUBSTRING"
            | "SUBSTR"
            | "POSITION"
            | "TRIM"
            | "LTRIM"
            | "RTRIM"
            | "BTRIM"
            | "CONCAT"
            | "CONCAT_WS"
            | "REVERSE"
            | "LEFT"
            | "RIGHT"
            | "QUOTE_LITERAL"
            | "ABS"
            | "ROUND"
            | "COALESCE"
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
            | "VERIFY_PASSWORD"
            | "CAST"
            | "CASE"
    )
}

fn is_aggregate_function(name: &str) -> bool {
    matches!(
        name,
        "COUNT"
            | "AVG"
            | "SUM"
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

fn aggregate_token_name(token: &Token) -> Option<&'static str> {
    match token {
        Token::Count => Some("COUNT"),
        Token::Sum => Some("SUM"),
        Token::Avg => Some("AVG"),
        Token::Min => Some("MIN"),
        Token::Max => Some("MAX"),
        Token::First => Some("FIRST"),
        Token::Last => Some("LAST"),
        _ => None,
    }
}

fn scalar_token_name(token: &Token) -> Option<&'static str> {
    match token {
        Token::Left => Some("LEFT"),
        Token::Right => Some("RIGHT"),
        _ => None,
    }
}
use super::Parser;

impl<'a> Parser<'a> {
    /// Parse SELECT ... FROM ... query
    pub fn parse_select_query(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::Select)?;

        // Parse column list
        let (select_items, columns) = self.parse_select_items_and_projections()?;

        // Parse optional table source. If omitted, default to `ANY` so the query
        // can return mixed entities (table, document, graph, and vector) by default.
        let has_from = self.consume(&Token::From)?;
        let table = if has_from {
            if self.consume(&Token::Star)? {
                "*".to_string()
            } else if self.consume(&Token::All)? {
                "all".to_string()
            } else {
                self.expect_ident()?
            }
        } else {
            "any".to_string()
        };

        // Parse optional alias (only when a FROM clause exists).
        let alias = if !has_from {
            None
        } else if self.consume(&Token::As)?
            || (self.check(&Token::Ident("".into())) && !self.is_clause_keyword())
        {
            Some(self.expect_ident()?)
        } else {
            None
        };

        let mut query = TableQuery {
            table,
            source: None,
            alias,
            select_items,
            columns,
            where_expr: None,
            filter: None,
            group_by_exprs: Vec::new(),
            group_by: Vec::new(),
            having_expr: None,
            having: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            expand: None,
        };

        // Parse optional clauses
        self.parse_table_clauses(&mut query)?;

        Ok(QueryExpr::Table(query))
    }
}

impl<'a> Parser<'a> {
    /// Check if current identifier is a clause keyword
    pub fn is_clause_keyword(&self) -> bool {
        matches!(
            self.peek(),
            Token::Where
                | Token::Order
                | Token::Limit
                | Token::Offset
                | Token::Join
                | Token::Inner
                | Token::Left
                | Token::Right
        )
    }

    /// Parse projection list (column selections)
    pub fn parse_projection_list(&mut self) -> Result<Vec<Projection>, ParseError> {
        Ok(self.parse_select_items_and_projections()?.1)
    }

    fn parse_select_items_and_projections(
        &mut self,
    ) -> Result<(Vec<SelectItem>, Vec<Projection>), ParseError> {
        // Handle SELECT *
        if self.consume(&Token::Star)? {
            return Ok((vec![SelectItem::Wildcard], Vec::new())); // Empty legacy vec means all columns
        }

        let mut select_items = Vec::new();
        let mut projections = Vec::new();
        loop {
            let (item, proj) = self.parse_projection()?;
            select_items.push(item);
            projections.push(proj);

            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        Ok((select_items, projections))
    }

    /// Parse a single projection — supports columns, aggregate functions, and scalar functions
    fn parse_projection(&mut self) -> Result<(SelectItem, Projection), ParseError> {
        let expr = self.parse_expr()?;
        if contains_nested_aggregate(&expr) && !is_plain_aggregate_expr(&expr) {
            return Err(ParseError::new(
                "aggregate function is not valid inside another expression".to_string(),
                self.position(),
            ));
        }
        let alias = if self.consume(&Token::As)? {
            Some(self.expect_ident()?)
        } else {
            None
        };
        let select_item = SelectItem::Expr {
            expr: expr.clone(),
            alias: alias.clone(),
        };
        let projection = attach_projection_alias(
            expr_to_projection(&expr).ok_or_else(|| {
                ParseError::new(
                    "projection cannot yet be lowered to legacy runtime representation".to_string(),
                    self.position(),
                )
            })?,
            alias,
        );
        Ok((select_item, projection))
    }

    /// Pratt-style climb for `+ - * / % ||` infix operators on top of an
    /// already-parsed LHS projection. Emits the operation as a nested
    /// `Projection::Function` so the executor plumbing in
    /// `evaluate_scalar_function` handles it without a new AST variant.
    /// Left-associative. Precedence table:
    ///   10  || (concat)
    ///   20  + -
    ///   30  * / %
    fn parse_projection_binop_tail(
        &mut self,
        mut left: Projection,
        min_prec: u8,
    ) -> Result<Projection, ParseError> {
        loop {
            let (op_name, prec) = match self.peek() {
                Token::Plus => ("ADD", 20u8),
                Token::Dash => ("SUB", 20u8),
                Token::Star => ("MUL", 30u8),
                Token::Slash => ("DIV", 30u8),
                Token::Percent => ("MOD", 30u8),
                Token::DoublePipe => ("CONCAT", 10u8),
                _ => break,
            };
            if prec < min_prec {
                break;
            }
            self.advance()?; // consume operator
                             // Parse RHS atom then climb higher-precedence tail (left-assoc).
            let rhs_atom = self.parse_projection_factor()?;
            let rhs = self.parse_projection_binop_tail(rhs_atom, prec + 1)?;
            left = Projection::Function(op_name.to_string(), vec![left, rhs]);
        }
        Ok(left)
    }

    /// Parse a single projection factor — an atom usable as the LHS or
    /// RHS of an arithmetic operator. Accepts literals, field refs,
    /// scalar-function calls, CASE, CAST, and parenthesised
    /// sub-expressions. Aggregate functions still stay at top level.
    fn parse_projection_factor(&mut self) -> Result<Projection, ParseError> {
        // Parenthesised sub-expression: ( expr )
        if self.consume(&Token::LParen)? {
            let inner = self.parse_projection_factor()?;
            let climbed = self.parse_projection_binop_tail(inner, 0)?;
            self.expect(Token::RParen)?;
            return Ok(climbed);
        }
        if let Some(func_name) = aggregate_token_name(self.peek()) {
            if matches!(self.peek_next()?, Token::LParen) {
                return Err(ParseError::new(
                    format!(
                        "aggregate function `{func_name}` is not valid inside another expression"
                    ),
                    self.position(),
                ));
            }
        }
        // Nested CAST inside an arithmetic expression.
        if let Some(name) = match self.peek() {
            Token::Ident(name) => Some(name.clone()),
            _ => None,
        } {
            if name.eq_ignore_ascii_case("CAST") && matches!(self.peek_next()?, Token::LParen) {
                self.advance()?;
                self.expect(Token::LParen)?;
                let inner = self.parse_projection_factor()?;
                let inner = self.parse_projection_binop_tail(inner, 0)?;
                self.expect(Token::As)?;
                let type_name = self.expect_ident_or_keyword()?;
                self.expect(Token::RParen)?;
                let args = vec![
                    inner,
                    Projection::Column(format!("TYPE:{}", type_name.to_uppercase())),
                ];
                return Ok(Projection::Function("CAST".to_string(), args));
            }
        }
        if let Some(name) = match self.peek() {
            Token::Ident(name) => Some(name.clone()),
            _ => None,
        } {
            if name.eq_ignore_ascii_case("CASE") {
                return self.parse_case_projection();
            }
            if matches!(self.peek_next()?, Token::LParen) {
                if name.eq_ignore_ascii_case("POSITION") {
                    return self.parse_position_projection();
                }
                if name.eq_ignore_ascii_case("TRIM") {
                    return self.parse_trim_projection();
                }
                if name.eq_ignore_ascii_case("SUBSTRING") {
                    return self.parse_substring_projection();
                }
                let upper = name.to_uppercase();
                if is_aggregate_function(&upper) {
                    return Err(ParseError::new(
                        format!(
                            "aggregate function `{upper}` is not valid inside another expression"
                        ),
                        self.position(),
                    ));
                }
                if is_scalar_function(&upper) {
                    self.advance()?; // consume function name
                    self.expect(Token::LParen)?;
                    let args = self.parse_function_args()?;
                    self.expect(Token::RParen)?;
                    return Ok(Projection::Function(upper, args));
                }
            }
        }

        if let Some(func_name) = scalar_token_name(self.peek()) {
            if matches!(self.peek_next()?, Token::LParen) && is_scalar_function(func_name) {
                self.advance()?;
                self.expect(Token::LParen)?;
                let args = self.parse_function_args()?;
                self.expect(Token::RParen)?;
                return Ok(Projection::Function(func_name.to_string(), args));
            }
        }
        // Numeric / string / null literal.
        match self.peek().clone() {
            Token::Integer(_) | Token::Float(_) => {
                let val = self.parse_function_literal_arg()?;
                return Ok(Projection::Column(format!("LIT:{}", val)));
            }
            Token::Dash => {
                self.advance()?;
                let val = self.parse_function_literal_arg()?;
                return Ok(Projection::Column(format!("LIT:-{}", val)));
            }
            Token::String(s) => {
                self.advance()?;
                return Ok(Projection::Column(format!("LIT:{}", s)));
            }
            Token::Null => {
                self.advance()?;
                return Ok(Projection::Column("LIT:".to_string()));
            }
            _ => {}
        }
        // Bare column / qualified field reference.
        let field = self.parse_field_ref()?;
        Ok(Projection::Field(field, None))
    }
}

/// Attach an optional alias to a projection by re-wrapping. Field and
/// Function projections store alias natively; anything else (including
/// a nested arithmetic tree rooted in Function) gets a `:alias` suffix
/// on the function name, matching the existing CAST/CASE convention.
fn attach_projection_alias(proj: Projection, alias: Option<String>) -> Projection {
    let Some(alias) = alias else { return proj };
    match proj {
        Projection::Field(f, _) => Projection::Field(f, Some(alias)),
        Projection::Expression(filter, _) => Projection::Expression(filter, Some(alias)),
        Projection::Function(name, args) => {
            // Don't double-suffix if name already carries an alias.
            if name.contains(':') {
                Projection::Function(name, args)
            } else {
                Projection::Function(format!("{}:{}", name, alias), args)
            }
        }
        Projection::Column(c) => Projection::Alias(c, alias),
        other => other,
    }
}

fn contains_nested_aggregate(expr: &Expr) -> bool {
    match expr {
        Expr::FunctionCall { name, args, .. } => {
            is_aggregate_function(&name.to_uppercase())
                || args.iter().any(contains_nested_aggregate)
        }
        Expr::BinaryOp { lhs, rhs, .. } => {
            contains_nested_aggregate(lhs) || contains_nested_aggregate(rhs)
        }
        Expr::UnaryOp { operand, .. } | Expr::IsNull { operand, .. } => {
            contains_nested_aggregate(operand)
        }
        Expr::Cast { inner, .. } => contains_nested_aggregate(inner),
        Expr::Case {
            branches, else_, ..
        } => {
            branches.iter().any(|(cond, value)| {
                contains_nested_aggregate(cond) || contains_nested_aggregate(value)
            }) || else_.as_deref().is_some_and(contains_nested_aggregate)
        }
        Expr::InList { target, values, .. } => {
            contains_nested_aggregate(target) || values.iter().any(contains_nested_aggregate)
        }
        Expr::Between {
            target, low, high, ..
        } => {
            contains_nested_aggregate(target)
                || contains_nested_aggregate(low)
                || contains_nested_aggregate(high)
        }
        Expr::Literal { .. } | Expr::Column { .. } | Expr::Parameter { .. } => false,
    }
}

fn is_plain_aggregate_expr(expr: &Expr) -> bool {
    match expr {
        Expr::FunctionCall { name, args, .. } if is_aggregate_function(&name.to_uppercase()) => {
            !args.iter().any(contains_nested_aggregate)
        }
        _ => false,
    }
}

fn projection_from_expr(expr: Expr) -> Result<Projection, ParseError> {
    match expr {
        Expr::Literal { value, .. } => Ok(projection_from_literal(value)),
        Expr::Column { field, .. } => Ok(
            if matches!(
                field,
                FieldRef::TableColumn { ref table, ref column } if table.is_empty() && column == "*"
            ) {
                Projection::All
            } else {
                Projection::Field(field, None)
            },
        ),
        Expr::Parameter { .. } => Err(ParseError::new(
            "query parameters are not supported in SELECT projections yet".to_string(),
            crate::storage::query::lexer::Position::default(),
        )),
        Expr::BinaryOp { op, lhs, rhs, .. } => match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Mod | BinOp::Concat => {
                Ok(Projection::Function(
                    projection_binop_name(op).to_string(),
                    vec![projection_from_expr(*lhs)?, projection_from_expr(*rhs)?],
                ))
            }
            _ => Ok(boolean_expr_projection(Expr::BinaryOp {
                op,
                lhs,
                rhs,
                span: Span::synthetic(),
            })),
        },
        Expr::UnaryOp { op, operand, .. } => match op {
            UnaryOp::Neg => Ok(Projection::Function(
                "SUB".to_string(),
                vec![
                    Projection::Column("LIT:0".to_string()),
                    projection_from_expr(*operand)?,
                ],
            )),
            UnaryOp::Not => Ok(boolean_expr_projection(Expr::UnaryOp {
                op,
                operand,
                span: Span::synthetic(),
            })),
        },
        Expr::Cast { inner, target, .. } => Ok(Projection::Function(
            "CAST".to_string(),
            vec![
                projection_from_expr(*inner)?,
                Projection::Column(format!("TYPE:{target}")),
            ],
        )),
        Expr::FunctionCall { name, args, .. } => Ok(Projection::Function(
            name.to_uppercase(),
            args.into_iter()
                .map(projection_from_expr)
                .collect::<Result<Vec<_>, _>>()?,
        )),
        Expr::Case {
            branches, else_, ..
        } => {
            let mut args = Vec::with_capacity(branches.len() * 2 + usize::from(else_.is_some()));
            for (cond, value) in branches {
                args.push(case_condition_projection(cond));
                args.push(projection_from_expr(value)?);
            }
            if let Some(else_expr) = else_ {
                args.push(projection_from_expr(*else_expr)?);
            }
            Ok(Projection::Function("CASE".to_string(), args))
        }
        Expr::IsNull { .. } | Expr::InList { .. } | Expr::Between { .. } => {
            Ok(boolean_expr_projection(expr))
        }
    }
}

fn projection_from_literal(value: Value) -> Projection {
    match value {
        Value::Boolean(_) => boolean_expr_projection(Expr::Literal {
            value,
            span: Span::synthetic(),
        }),
        other => Projection::Column(format!("LIT:{}", render_projection_literal(&other))),
    }
}

fn boolean_expr_projection(expr: Expr) -> Projection {
    Projection::Expression(
        Box::new(Filter::CompareExpr {
            lhs: expr,
            op: CompareOp::Eq,
            rhs: Expr::Literal {
                value: Value::Boolean(true),
                span: Span::synthetic(),
            },
        }),
        None,
    )
}

fn case_condition_projection(condition: Expr) -> Projection {
    Projection::Expression(
        Box::new(Filter::CompareExpr {
            lhs: condition,
            op: CompareOp::Eq,
            rhs: Expr::Literal {
                value: Value::Boolean(true),
                span: Span::synthetic(),
            },
        }),
        None,
    )
}

fn projection_binop_name(op: BinOp) -> &'static str {
    match op {
        BinOp::Add => "ADD",
        BinOp::Sub => "SUB",
        BinOp::Mul => "MUL",
        BinOp::Div => "DIV",
        BinOp::Mod => "MOD",
        BinOp::Concat => "CONCAT",
        BinOp::Eq
        | BinOp::Ne
        | BinOp::Lt
        | BinOp::Le
        | BinOp::Gt
        | BinOp::Ge
        | BinOp::And
        | BinOp::Or => {
            unreachable!("boolean operators are lowered through Projection::Expression")
        }
    }
}

fn render_projection_literal(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Integer(v) => v.to_string(),
        Value::Float(v) => {
            if v.fract().abs() < f64::EPSILON {
                (*v as i64).to_string()
            } else {
                v.to_string()
            }
        }
        Value::Text(v) => v.clone(),
        Value::Boolean(true) => "true".to_string(),
        Value::Boolean(false) => "false".to_string(),
        Value::Blob(bytes) => String::from_utf8_lossy(bytes).into_owned(),
        other => other.to_string(),
    }
}

impl<'a> Parser<'a> {
    /// Parse a single atomic projection — a column reference, numeric
    /// literal, or quoted string — used inside CAST / CASE / future
    /// Fase 1.3 arithmetic expressions. Wider forms (function calls,
    /// arithmetic) are deferred until the projection-level Pratt parser
    /// lands in Fase 1.3.
    fn parse_projection_atom(&mut self) -> Result<Projection, ParseError> {
        match self.peek().clone() {
            Token::Integer(_) | Token::Float(_) | Token::Dash => {
                let val = self.parse_function_literal_arg()?;
                Ok(Projection::Column(format!("LIT:{}", val)))
            }
            Token::String(s) => {
                self.advance()?;
                Ok(Projection::Column(format!("LIT:{}", s)))
            }
            Token::Null => {
                self.advance()?;
                Ok(Projection::Column("LIT:".to_string()))
            }
            _ => {
                let col = self.expect_ident_or_keyword()?;
                Ok(Projection::Column(col))
            }
        }
    }

    /// Parse `CASE WHEN <filter> THEN <expr> [WHEN ... THEN ...]
    /// [ELSE <expr>] END`. The caller has already peeked `CASE`.
    fn parse_case_projection(&mut self) -> Result<Projection, ParseError> {
        // Consume CASE (it's an Ident token).
        self.advance()?;
        let mut args: Vec<Projection> = Vec::new();
        loop {
            if !self.consume_ident_ci("WHEN")? {
                break;
            }
            let cond = self.parse_filter()?;
            if !self.consume_ident_ci("THEN")? {
                return Err(ParseError::new(
                    "expected THEN after CASE WHEN condition".to_string(),
                    self.position(),
                ));
            }
            let then_val = self.parse_case_projection_value()?;
            args.push(Projection::Expression(Box::new(cond), None));
            args.push(then_val);
        }
        if args.is_empty() {
            return Err(ParseError::new(
                "CASE must have at least one WHEN branch".to_string(),
                self.position(),
            ));
        }
        if self.consume_ident_ci("ELSE")? {
            let else_val = self.parse_case_projection_value()?;
            args.push(else_val);
        }
        if !self.consume_ident_ci("END")? {
            return Err(ParseError::new(
                "expected END to close CASE expression".to_string(),
                self.position(),
            ));
        }
        let alias = if self.consume(&Token::As)? {
            Some(self.expect_ident()?)
        } else {
            None
        };
        Ok(if let Some(a) = alias {
            Projection::Function(format!("CASE:{}", a), args)
        } else {
            Projection::Function("CASE".to_string(), args)
        })
    }

    fn parse_case_projection_value(&mut self) -> Result<Projection, ParseError> {
        let atom = self.parse_projection_factor()?;
        self.parse_projection_binop_tail(atom, 0)
    }

    fn parse_trim_projection(&mut self) -> Result<Projection, ParseError> {
        self.advance()?; // consume TRIM
        self.expect(Token::LParen)?;
        let (name, args) = self.parse_trim_projection_args()?;
        self.expect(Token::RParen)?;
        let alias = if self.consume(&Token::As)? {
            Some(self.expect_ident()?)
        } else {
            None
        };
        Ok(if let Some(alias) = alias {
            Projection::Function(format!("{name}:{alias}"), args)
        } else {
            Projection::Function(name, args)
        })
    }

    fn parse_trim_projection_args(&mut self) -> Result<(String, Vec<Projection>), ParseError> {
        let mut function_name = "TRIM".to_string();

        if self.consume_ident_ci("LEADING")? {
            function_name = "LTRIM".to_string();
        } else if self.consume_ident_ci("TRAILING")? {
            function_name = "RTRIM".to_string();
        } else if self.consume_ident_ci("BOTH")? {
            function_name = "TRIM".to_string();
        }

        if self.consume(&Token::From)? {
            let source = self.parse_trim_projection_value()?;
            return Ok((function_name, vec![source]));
        }

        let first = self.parse_trim_projection_value()?;

        if self.consume(&Token::Comma)? {
            let second = self.parse_trim_projection_value()?;
            return Ok((function_name, vec![first, second]));
        }

        if self.consume(&Token::From)? {
            let source = self.parse_trim_projection_value()?;
            return Ok((function_name, vec![source, first]));
        }

        Ok((function_name, vec![first]))
    }

    fn parse_trim_projection_value(&mut self) -> Result<Projection, ParseError> {
        let atom = self.parse_projection_factor()?;
        self.parse_projection_binop_tail(atom, 0)
    }

    fn parse_position_projection(&mut self) -> Result<Projection, ParseError> {
        self.advance()?; // consume POSITION
        self.expect(Token::LParen)?;
        let args = self.parse_position_projection_args()?;
        self.expect(Token::RParen)?;
        let alias = if self.consume(&Token::As)? {
            Some(self.expect_ident()?)
        } else {
            None
        };
        Ok(if let Some(alias) = alias {
            Projection::Function(format!("POSITION:{alias}"), args)
        } else {
            Projection::Function("POSITION".to_string(), args)
        })
    }

    fn parse_position_projection_args(&mut self) -> Result<Vec<Projection>, ParseError> {
        let needle = self.parse_projection_factor()?;
        let needle = self.parse_projection_binop_tail(needle, 0)?;
        if !self.consume(&Token::Comma)? {
            self.expect(Token::In)?;
        }
        let haystack = self.parse_projection_factor()?;
        let haystack = self.parse_projection_binop_tail(haystack, 0)?;
        Ok(vec![needle, haystack])
    }

    fn parse_substring_projection(&mut self) -> Result<Projection, ParseError> {
        self.advance()?; // consume SUBSTRING
        self.expect(Token::LParen)?;
        let args = self.parse_substring_projection_args()?;
        self.expect(Token::RParen)?;
        let alias = if self.consume(&Token::As)? {
            Some(self.expect_ident()?)
        } else {
            None
        };
        Ok(if let Some(alias) = alias {
            Projection::Function(format!("SUBSTRING:{alias}"), args)
        } else {
            Projection::Function("SUBSTRING".to_string(), args)
        })
    }

    /// PostgreSQL-style `SUBSTRING` syntax lowered to the plain variadic
    /// `Projection::Function("SUBSTRING", args)` form used by the legacy
    /// executor.
    fn parse_substring_projection_args(&mut self) -> Result<Vec<Projection>, ParseError> {
        let source = self.parse_substring_projection_value()?;

        if self.consume(&Token::Comma)? {
            let mut args = vec![source];
            loop {
                args.push(self.parse_substring_projection_value()?);
                if !self.consume(&Token::Comma)? {
                    break;
                }
            }
            return Ok(args);
        }

        if self.consume(&Token::From)? {
            let start = self.parse_substring_projection_value()?;
            if self.consume(&Token::For)? {
                let count = self.parse_substring_projection_value()?;
                return Ok(vec![source, start, count]);
            }
            return Ok(vec![source, start]);
        }

        if self.consume(&Token::For)? {
            let count = self.parse_substring_projection_value()?;
            if self.consume(&Token::From)? {
                let start = self.parse_substring_projection_value()?;
                return Ok(vec![source, start, count]);
            }
            return Ok(vec![source, Projection::Column("LIT:1".to_string()), count]);
        }

        Ok(vec![source])
    }

    fn parse_substring_projection_value(&mut self) -> Result<Projection, ParseError> {
        let atom = self.parse_projection_factor()?;
        self.parse_projection_binop_tail(atom, 0)
    }

    /// Parse comma-separated function arguments (columns, literals, POINT())
    fn parse_function_args(&mut self) -> Result<Vec<Projection>, ParseError> {
        let mut args = Vec::new();
        if self.peek() == &Token::RParen {
            return Ok(args);
        }
        loop {
            // POINT(lat, lon) → encoded as Column("POINT:lat:lon")
            if let Token::Ident(ref name) = self.peek() {
                if name.eq_ignore_ascii_case("POINT") {
                    self.advance()?; // consume POINT
                    self.expect(Token::LParen)?;
                    let lat = self.parse_numeric_literal()?;
                    self.expect(Token::Comma)?;
                    let lon = self.parse_numeric_literal()?;
                    self.expect(Token::RParen)?;
                    args.push(Projection::Column(format!("POINT:{}:{}", lat, lon)));
                    if !self.consume(&Token::Comma)? {
                        break;
                    }
                    continue;
                }
            }

            let atom = self.parse_projection_factor()?;
            let arg = self.parse_projection_binop_tail(atom, 0)?;
            args.push(arg);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        Ok(args)
    }

    /// Parse a numeric literal (float, positive or negative)
    fn parse_numeric_literal(&mut self) -> Result<f64, ParseError> {
        let negative = self.consume(&Token::Dash)?;
        match self.advance()? {
            Token::Integer(n) => Ok(if negative { -(n as f64) } else { n as f64 }),
            Token::Float(n) => Ok(if negative { -n } else { n }),
            other => Err(ParseError::new(
                format!("expected number, got {}", other),
                self.position(),
            )),
        }
    }

    /// Parse table query clauses (WHERE, GROUP BY, HAVING, ORDER BY, LIMIT, OFFSET)
    pub fn parse_table_clauses(&mut self, query: &mut TableQuery) -> Result<(), ParseError> {
        // WHERE clause
        if self.consume(&Token::Where)? {
            let filter = self.parse_filter()?;
            query.where_expr = Some(filter_to_expr(&filter));
            query.filter = Some(filter);
        }

        // GROUP BY clause
        if self.consume(&Token::Group)? {
            self.expect(Token::By)?;
            let (group_by_exprs, group_by) = self.parse_group_by_items()?;
            query.group_by_exprs = group_by_exprs;
            query.group_by = group_by;
        }

        // HAVING clause (only valid after GROUP BY)
        if !query.group_by.is_empty() && self.consume_ident_ci("HAVING")? {
            let having = self.parse_filter()?;
            query.having_expr = Some(filter_to_expr(&having));
            query.having = Some(having);
        }

        // ORDER BY clause
        if self.consume(&Token::Order)? {
            self.expect(Token::By)?;
            query.order_by = self.parse_order_by_list()?;
        }

        // LIMIT clause
        if self.consume(&Token::Limit)? {
            query.limit = Some(self.parse_integer()? as u64);
        }

        // OFFSET clause
        if self.consume(&Token::Offset)? {
            query.offset = Some(self.parse_integer()? as u64);
        }

        // WITH EXPAND clause
        if self.consume(&Token::With)? && self.consume_ident_ci("EXPAND")? {
            query.expand = Some(self.parse_expand_options()?);
        }

        Ok(())
    }

    /// Parse EXPAND options: GRAPH [DEPTH n], CROSS_REFS, ALL
    fn parse_expand_options(
        &mut self,
    ) -> Result<crate::storage::query::ast::ExpandOptions, ParseError> {
        use crate::storage::query::ast::ExpandOptions;
        let mut opts = ExpandOptions::default();

        loop {
            if self.consume(&Token::Graph)? || self.consume_ident_ci("GRAPH")? {
                opts.graph = true;
                opts.graph_depth = if self.consume(&Token::Depth)? {
                    self.parse_integer()? as usize
                } else {
                    1
                };
            } else if self.consume_ident_ci("CROSS_REFS")?
                || self.consume_ident_ci("CROSSREFS")?
                || self.consume_ident_ci("REFS")?
            {
                opts.cross_refs = true;
            } else if self.consume(&Token::All)? || self.consume_ident_ci("ALL")? {
                opts.graph = true;
                opts.cross_refs = true;
                opts.graph_depth = 1;
            } else {
                break;
            }
            if !self.consume(&Token::Comma)? {
                break;
            }
        }

        if !opts.graph && !opts.cross_refs {
            opts.graph = true;
            opts.cross_refs = true;
            opts.graph_depth = 1;
        }

        Ok(opts)
    }

    /// Parse GROUP BY field list
    pub fn parse_group_by_list(&mut self) -> Result<Vec<String>, ParseError> {
        Ok(self.parse_group_by_items()?.1)
    }

    fn parse_group_by_items(&mut self) -> Result<(Vec<Expr>, Vec<String>), ParseError> {
        let mut exprs = Vec::new();
        let mut fields = Vec::new();
        loop {
            let expr = self.parse_expr()?;
            let rendered = render_group_by_expr(&expr).ok_or_else(|| {
                ParseError::new(
                    "GROUP BY expression cannot yet be lowered to legacy runtime representation"
                        .to_string(),
                    self.position(),
                )
            })?;
            exprs.push(expr);
            fields.push(rendered);
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        Ok((exprs, fields))
    }

    /// Parse ORDER BY list.
    ///
    /// Fase 1.6 unlock: uses the new `Expr` Pratt parser so
    /// `ORDER BY CAST(age AS INT)`, `ORDER BY a + b * 2`,
    /// `ORDER BY last_seen - created_at` all parse cleanly. If the
    /// parsed expression is a bare `Column`, we store it in the
    /// legacy `field` slot and leave `expr` None so downstream
    /// consumers (planner cost, mode translators) keep using the
    /// fast path. Otherwise we stash the full tree in `expr` and
    /// populate `field` with a synthetic marker that runtime code
    /// never touches.
    pub fn parse_order_by_list(&mut self) -> Result<Vec<OrderByClause>, ParseError> {
        use super::super::ast::Expr as AstExpr;
        let mut clauses = Vec::new();
        loop {
            let parsed = self.parse_expr()?;
            let (field, expr_slot) = match parsed {
                AstExpr::Column { field, .. } => (field, None),
                other => (
                    // Synthetic placeholder so legacy pattern-matches
                    // on `OrderByClause.field` still destructure.
                    // Runtime comparators check `expr` first when set,
                    // so the sentinel never gets resolved against a
                    // real record.
                    FieldRef::TableColumn {
                        table: String::new(),
                        column: String::new(),
                    },
                    Some(other),
                ),
            };

            let ascending = if self.consume(&Token::Desc)? {
                false
            } else {
                self.consume(&Token::Asc)?;
                true
            };

            let nulls_first = if self.consume(&Token::Nulls)? {
                if self.consume(&Token::First)? {
                    true
                } else {
                    self.expect(Token::Last)?;
                    false
                }
            } else {
                !ascending // Default: nulls last for ASC, first for DESC
            };

            clauses.push(OrderByClause {
                field,
                expr: expr_slot,
                ascending,
                nulls_first,
            });

            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        Ok(clauses)
    }

    fn parse_function_literal_arg(&mut self) -> Result<String, ParseError> {
        let negative = self.consume(&Token::Dash)?;
        let mut literal = match self.advance()? {
            Token::Integer(n) => {
                if negative {
                    format!("-{n}")
                } else {
                    n.to_string()
                }
            }
            Token::Float(n) => {
                let value = if negative { -n } else { n };
                if value.fract().abs() < f64::EPSILON {
                    format!("{}", value as i64)
                } else {
                    value.to_string()
                }
            }
            other => {
                return Err(ParseError::new(
                    format!("expected number, got {}", other),
                    self.position(),
                ));
            }
        };

        if let Token::Ident(unit) = self.peek().clone() {
            if is_duration_unit(&unit) {
                self.advance()?;
                literal.push_str(&unit.to_ascii_lowercase());
            }
        }

        Ok(literal)
    }

    fn parse_group_by_entry(&mut self) -> Result<String, ParseError> {
        if let Token::Ident(name) = self.peek() {
            if name.eq_ignore_ascii_case("TIME_BUCKET") {
                return self.parse_group_by_time_bucket();
            }
        }
        self.expect_ident()
    }

    fn parse_group_by_time_bucket(&mut self) -> Result<String, ParseError> {
        self.advance()?; // TIME_BUCKET
        self.expect(Token::LParen)?;
        let args = self.parse_function_args()?;
        self.expect(Token::RParen)?;

        let rendered_args = args
            .iter()
            .map(render_group_by_function_arg)
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                ParseError::new(
                    "TIME_BUCKET arguments must be literals or column names".to_string(),
                    self.position(),
                )
            })?;

        Ok(format!("TIME_BUCKET({})", rendered_args.join(",")))
    }
}

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

fn render_group_by_expr(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Column { field, .. } => match field {
            FieldRef::TableColumn { table, column } if table.is_empty() => Some(column.clone()),
            FieldRef::TableColumn { table, column } => Some(format!("{table}.{column}")),
            other => Some(format!("{other:?}")),
        },
        Expr::FunctionCall { name, args, .. } if name.eq_ignore_ascii_case("TIME_BUCKET") => {
            let rendered = args
                .iter()
                .map(render_group_by_expr)
                .collect::<Option<Vec<_>>>()?;
            Some(format!("TIME_BUCKET({})", rendered.join(",")))
        }
        Expr::Literal { value, .. } => Some(match value {
            Value::Null => String::new(),
            Value::Text(text) => text.clone(),
            other => other.to_string(),
        }),
        _ => expr_to_projection(expr).map(|projection| match projection {
            Projection::Field(FieldRef::TableColumn { table, column }, _) if table.is_empty() => {
                column
            }
            Projection::Field(FieldRef::TableColumn { table, column }, _) => {
                format!("{table}.{column}")
            }
            Projection::Function(name, args) => {
                let rendered = args
                    .iter()
                    .map(render_group_by_function_arg)
                    .collect::<Option<Vec<_>>>()
                    .unwrap_or_default();
                format!(
                    "{}({})",
                    name.split(':').next().unwrap_or(&name),
                    rendered.join(",")
                )
            }
            Projection::Column(column) | Projection::Alias(column, _) => column,
            Projection::All => "*".to_string(),
            Projection::Expression(_, _) => "expr".to_string(),
            Projection::Field(other, _) => format!("{other:?}"),
        }),
    }
}

fn render_group_by_function_arg(arg: &Projection) -> Option<String> {
    match arg {
        Projection::Column(col) => Some(
            col.strip_prefix("LIT:")
                .map(str::to_string)
                .unwrap_or_else(|| col.clone()),
        ),
        Projection::All => Some("*".to_string()),
        _ => None,
    }
}
