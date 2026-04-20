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
            | "MONEY"
            | "MONEY_ASSET"
            | "MONEY_MINOR"
            | "MONEY_SCALE"
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

    pub(crate) fn parse_select_items_and_projections(
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

fn attach_projection_alias(proj: Projection, alias: Option<String>) -> Projection {
    let Some(alias) = alias else { return proj };
    match proj {
        Projection::Field(field, _) => Projection::Field(field, Some(alias)),
        Projection::Expression(filter, _) => Projection::Expression(filter, Some(alias)),
        Projection::Function(name, args) => {
            if name.contains(':') {
                Projection::Function(name, args)
            } else {
                Projection::Function(format!("{name}:{alias}"), args)
            }
        }
        Projection::Column(column) => Projection::Alias(column, alias),
        other => other,
    }
}

impl<'a> Parser<'a> {
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
        if !query.group_by_exprs.is_empty() && self.consume_ident_ci("HAVING")? {
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
            Value::Text(text) => text.to_string(),
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
