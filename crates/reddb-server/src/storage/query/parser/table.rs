//! Table query parsing (SELECT ... FROM ...)

use super::super::ast::{
    BinOp, CompareOp, Expr, FieldRef, Filter, OrderByClause, Projection, QueryExpr,
    QueueSelectQuery, SelectItem, Span, TableQuery, UnaryOp,
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
        // Recursion guard: nested subqueries (UNION, derived tables,
        // EXISTS) re-enter through this point, so depth here bounds
        // the SELECT-shaped recursion in addition to the expr Pratt
        // climb guarded in `parse_expr_prec`.
        self.enter_depth()?;
        let result = self.parse_select_query_inner();
        self.exit_depth();
        result
    }

    fn parse_select_query_inner(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::Select)?;

        // Parse column list
        let (select_items, columns) = self.parse_select_items_and_projections()?;

        // Parse optional table source. If omitted, default to `ANY` so the query
        // can return mixed entities (table, document, graph, and vector) by default.
        let has_from = self.consume(&Token::From)?;
        let table = if has_from {
            if self.consume(&Token::Queue)? {
                let queue = self.expect_ident()?;
                let filter = if self.consume(&Token::Where)? {
                    Some(self.parse_filter()?)
                } else {
                    None
                };
                let limit = if self.consume(&Token::Limit)? {
                    Some(self.parse_integer()? as u64)
                } else {
                    None
                };
                return Ok(QueryExpr::QueueSelect(QueueSelectQuery {
                    queue,
                    columns: queue_projection_columns(&columns)?,
                    filter,
                    limit,
                }));
            } else if self.consume(&Token::Star)? {
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
        // `AS OF` is a clause — don't gobble the `AS` as an alias
        // marker when the following token is `OF`.
        let alias =
            if !has_from || (self.check(&Token::As) && matches!(self.peek_next()?, Token::Of)) {
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
            as_of: None,
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
                | Token::As
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

fn queue_projection_columns(columns: &[Projection]) -> Result<Vec<String>, ParseError> {
    let mut out = Vec::new();
    for column in columns {
        match column {
            Projection::Column(name) => out.push(name.clone()),
            Projection::Alias(name, _) => out.push(name.clone()),
            Projection::Field(FieldRef::TableColumn { table, column }, _) if table.is_empty() => {
                out.push(column.clone());
            }
            Projection::All => return Ok(Vec::new()),
            other => {
                return Err(ParseError::new(
                    format!("unsupported SELECT FROM QUEUE projection {other:?}"),
                    crate::storage::query::lexer::Position::default(),
                ));
            }
        }
    }
    Ok(out)
}

impl<'a> Parser<'a> {
    /// Parse table query clauses (AS OF, WHERE, GROUP BY, HAVING, ORDER BY, LIMIT, OFFSET)
    pub fn parse_table_clauses(&mut self, query: &mut TableQuery) -> Result<(), ParseError> {
        // AS OF clause — time-travel anchor. Must come before WHERE
        // so the executor can bind the snapshot before filter eval.
        if self.check(&Token::As) {
            let next_is_of = matches!(self.peek_next()?, Token::Of);
            if next_is_of {
                self.expect(Token::As)?;
                self.expect(Token::Of)?;
                query.as_of = Some(self.parse_as_of_spec()?);
            }
        }

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

    /// Parse an AS OF spec after `AS OF` has already been consumed.
    /// Grammar:
    ///   AS OF COMMIT   '<hex>'
    ///   AS OF BRANCH   '<name>'
    ///   AS OF TAG      '<name>'
    ///   AS OF TIMESTAMP <integer-ms>
    ///   AS OF SNAPSHOT  <xid>
    fn parse_as_of_spec(&mut self) -> Result<crate::storage::query::ast::AsOfClause, ParseError> {
        use crate::storage::query::ast::AsOfClause;

        // Keyword — accept both tokenized forms (e.g. Token::Commit
        // if present) and bare identifiers for flexibility.
        let keyword = match self.peek() {
            Token::Ident(s) => {
                let s = s.to_ascii_uppercase();
                self.advance()?;
                s
            }
            Token::Commit => {
                self.advance()?;
                "COMMIT".to_string()
            }
            other => {
                return Err(ParseError::expected(
                    vec!["COMMIT", "BRANCH", "TAG", "TIMESTAMP", "SNAPSHOT"],
                    other,
                    self.position(),
                ));
            }
        };

        match keyword.as_str() {
            "COMMIT" => {
                let value = self.parse_string()?;
                Ok(AsOfClause::Commit(value))
            }
            "BRANCH" => {
                let value = self.parse_string()?;
                Ok(AsOfClause::Branch(value))
            }
            "TAG" => {
                let value = self.parse_string()?;
                Ok(AsOfClause::Tag(value))
            }
            "TIMESTAMP" => {
                let value = self.parse_integer()?;
                Ok(AsOfClause::TimestampMs(value))
            }
            "SNAPSHOT" => {
                let value = self.parse_integer()?;
                if value < 0 {
                    return Err(ParseError::new(
                        "AS OF SNAPSHOT requires non-negative xid".to_string(),
                        self.position(),
                    ));
                }
                Ok(AsOfClause::Snapshot(value as u64))
            }
            other => Err(ParseError::expected(
                vec!["COMMIT", "BRANCH", "TAG", "TIMESTAMP", "SNAPSHOT"],
                &Token::Ident(other.into()),
                self.position(),
            )),
        }
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
                    // F-05: `other` is a `Token` whose Display arms emit raw
                    // user bytes for `Ident` / `String` / `JsonLiteral`.
                    // Render via `{:?}` so CR/LF/NUL/quotes are escaped
                    // before the message reaches downstream serialization
                    // sinks.
                    format!("expected number, got {:?}", other),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::query::ast::{AsOfClause, BinOp, CompareOp, ExpandOptions, TableSource};

    fn parse_table(sql: &str) -> TableQuery {
        let parsed = super::super::parse(sql).unwrap().query;
        let QueryExpr::Table(table) = parsed else {
            panic!("expected table query");
        };
        table
    }

    fn col(name: &str) -> Expr {
        Expr::Column {
            field: FieldRef::TableColumn {
                table: String::new(),
                column: name.to_string(),
            },
            span: Span::synthetic(),
        }
    }

    #[test]
    fn helper_function_catalogs_cover_all_names() {
        for name in [
            "GEO_DISTANCE",
            "GEO_DISTANCE_VINCENTY",
            "GEO_BEARING",
            "GEO_MIDPOINT",
            "HAVERSINE",
            "VINCENTY",
            "TIME_BUCKET",
            "UPPER",
            "LOWER",
            "LENGTH",
            "CHAR_LENGTH",
            "CHARACTER_LENGTH",
            "OCTET_LENGTH",
            "BIT_LENGTH",
            "SUBSTRING",
            "SUBSTR",
            "POSITION",
            "TRIM",
            "LTRIM",
            "RTRIM",
            "BTRIM",
            "CONCAT",
            "CONCAT_WS",
            "REVERSE",
            "LEFT",
            "RIGHT",
            "QUOTE_LITERAL",
            "ABS",
            "ROUND",
            "COALESCE",
            "STDDEV",
            "VARIANCE",
            "MEDIAN",
            "PERCENTILE",
            "GROUP_CONCAT",
            "STRING_AGG",
            "FIRST",
            "LAST",
            "ARRAY_AGG",
            "COUNT_DISTINCT",
            "MONEY",
            "MONEY_ASSET",
            "MONEY_MINOR",
            "MONEY_SCALE",
            "VERIFY_PASSWORD",
            "CAST",
            "CASE",
        ] {
            assert!(is_scalar_function(name), "{name}");
        }
        assert!(!is_scalar_function("NOT_A_FUNCTION"));

        for name in [
            "COUNT",
            "AVG",
            "SUM",
            "MIN",
            "MAX",
            "STDDEV",
            "VARIANCE",
            "MEDIAN",
            "PERCENTILE",
            "GROUP_CONCAT",
            "STRING_AGG",
            "FIRST",
            "LAST",
            "ARRAY_AGG",
            "COUNT_DISTINCT",
        ] {
            assert!(is_aggregate_function(name), "{name}");
        }
        assert!(!is_aggregate_function("LOWER"));

        assert_eq!(aggregate_token_name(&Token::Count), Some("COUNT"));
        assert_eq!(aggregate_token_name(&Token::Sum), Some("SUM"));
        assert_eq!(aggregate_token_name(&Token::Avg), Some("AVG"));
        assert_eq!(aggregate_token_name(&Token::Min), Some("MIN"));
        assert_eq!(aggregate_token_name(&Token::Max), Some("MAX"));
        assert_eq!(aggregate_token_name(&Token::First), Some("FIRST"));
        assert_eq!(aggregate_token_name(&Token::Last), Some("LAST"));
        assert_eq!(aggregate_token_name(&Token::Ident("COUNT".into())), None);

        assert_eq!(scalar_token_name(&Token::Left), Some("LEFT"));
        assert_eq!(scalar_token_name(&Token::Right), Some("RIGHT"));
        assert_eq!(scalar_token_name(&Token::Ident("LEFT".into())), None);

        for unit in [
            "ms",
            "msec",
            "millisecond",
            "milliseconds",
            "s",
            "sec",
            "secs",
            "second",
            "seconds",
            "m",
            "min",
            "mins",
            "minute",
            "minutes",
            "h",
            "hr",
            "hrs",
            "hour",
            "hours",
            "d",
            "day",
            "days",
        ] {
            assert!(is_duration_unit(unit), "{unit}");
        }
        assert!(!is_duration_unit("fortnight"));
    }

    #[test]
    fn projection_and_group_render_helpers_cover_aliases_and_exprs() {
        let field = FieldRef::TableColumn {
            table: String::new(),
            column: "name".into(),
        };
        let filter = Filter::Compare {
            field: field.clone(),
            op: CompareOp::Eq,
            value: Value::text("alice"),
        };

        assert_eq!(
            attach_projection_alias(Projection::Field(field.clone(), None), Some("n".into())),
            Projection::Field(field.clone(), Some("n".into()))
        );
        assert_eq!(
            attach_projection_alias(
                Projection::Expression(Box::new(filter.clone()), None),
                Some("ok".into())
            ),
            Projection::Expression(Box::new(filter), Some("ok".into()))
        );
        assert_eq!(
            attach_projection_alias(
                Projection::Function("LOWER".into(), vec![]),
                Some("l".into())
            ),
            Projection::Function("LOWER:l".into(), vec![])
        );
        assert_eq!(
            attach_projection_alias(
                Projection::Function("LOWER:l".into(), vec![]),
                Some("ignored".into())
            ),
            Projection::Function("LOWER:l".into(), vec![])
        );
        assert_eq!(
            attach_projection_alias(Projection::Column("name".into()), Some("n".into())),
            Projection::Alias("name".into(), "n".into())
        );
        assert_eq!(
            attach_projection_alias(Projection::All, Some("ignored".into())),
            Projection::All
        );

        assert_eq!(render_group_by_expr(&col("dept")).as_deref(), Some("dept"));
        assert_eq!(
            render_group_by_expr(&Expr::Column {
                field: FieldRef::TableColumn {
                    table: "employees".into(),
                    column: "dept".into()
                },
                span: Span::synthetic()
            })
            .as_deref(),
            Some("employees.dept")
        );
        assert_eq!(
            render_group_by_expr(&Expr::Column {
                field: FieldRef::NodeId { alias: "n".into() },
                span: Span::synthetic()
            }),
            Some("NodeId { alias: \"n\" }".into())
        );
        assert_eq!(
            render_group_by_expr(&Expr::Literal {
                value: Value::Null,
                span: Span::synthetic()
            })
            .as_deref(),
            Some("")
        );
        assert_eq!(
            render_group_by_expr(&Expr::Literal {
                value: Value::text("5m"),
                span: Span::synthetic()
            })
            .as_deref(),
            Some("5m")
        );
        assert_eq!(
            render_group_by_expr(&Expr::Literal {
                value: Value::Integer(7),
                span: Span::synthetic()
            })
            .as_deref(),
            Some("7")
        );
        assert_eq!(
            render_group_by_expr(&Expr::FunctionCall {
                name: "TIME_BUCKET".into(),
                args: vec![
                    col("ts"),
                    Expr::Literal {
                        value: Value::text("5m"),
                        span: Span::synthetic()
                    }
                ],
                span: Span::synthetic()
            })
            .as_deref(),
            Some("TIME_BUCKET(ts,5m)")
        );
        assert_eq!(
            render_group_by_expr(&Expr::FunctionCall {
                name: "LOWER".into(),
                args: vec![col("dept")],
                span: Span::synthetic()
            })
            .as_deref(),
            Some("LOWER()")
        );

        assert_eq!(
            render_group_by_function_arg(&Projection::Column("LIT:5m".into())),
            Some("5m".into())
        );
        assert_eq!(
            render_group_by_function_arg(&Projection::Column("dept".into())),
            Some("dept".into())
        );
        assert_eq!(
            render_group_by_function_arg(&Projection::All),
            Some("*".into())
        );
        assert_eq!(
            render_group_by_function_arg(&Projection::Function("LOWER".into(), vec![])),
            None
        );
    }

    #[test]
    fn expression_aggregate_detection_branches() {
        let count = Expr::FunctionCall {
            name: "COUNT".into(),
            args: vec![col("id")],
            span: Span::synthetic(),
        };
        assert!(contains_nested_aggregate(&count));
        assert!(is_plain_aggregate_expr(&count));

        let nested = Expr::FunctionCall {
            name: "SUM".into(),
            args: vec![count.clone()],
            span: Span::synthetic(),
        };
        assert!(contains_nested_aggregate(&nested));
        assert!(!is_plain_aggregate_expr(&nested));

        let binary = Expr::BinaryOp {
            op: BinOp::Add,
            lhs: Box::new(col("a")),
            rhs: Box::new(count.clone()),
            span: Span::synthetic(),
        };
        assert!(contains_nested_aggregate(&binary));

        let unary = Expr::UnaryOp {
            op: UnaryOp::Not,
            operand: Box::new(count.clone()),
            span: Span::synthetic(),
        };
        assert!(contains_nested_aggregate(&unary));

        let cast = Expr::Cast {
            inner: Box::new(count.clone()),
            target: crate::storage::schema::DataType::Integer,
            span: Span::synthetic(),
        };
        assert!(contains_nested_aggregate(&cast));

        let case = Expr::Case {
            branches: vec![(col("flag"), count.clone())],
            else_: Some(Box::new(col("fallback"))),
            span: Span::synthetic(),
        };
        assert!(contains_nested_aggregate(&case));

        let in_list = Expr::InList {
            target: Box::new(col("id")),
            values: vec![count.clone()],
            negated: false,
            span: Span::synthetic(),
        };
        assert!(contains_nested_aggregate(&in_list));

        let between = Expr::Between {
            target: Box::new(col("id")),
            low: Box::new(col("low")),
            high: Box::new(count),
            negated: false,
            span: Span::synthetic(),
        };
        assert!(contains_nested_aggregate(&between));
        assert!(!contains_nested_aggregate(&Expr::Parameter {
            index: 1,
            span: Span::synthetic()
        }));

        assert!(super::super::parse("SELECT SUM(COUNT(id)) FROM t").is_err());
    }

    #[test]
    fn table_clause_parsing_covers_as_of_order_offset_and_expand() {
        let table = parse_table(
            "SELECT name FROM users AS OF COMMIT 'abc123' \
             WHERE deleted_at IS NULL \
             ORDER BY LOWER(name) ASC NULLS FIRST, created_at DESC NULLS LAST \
             LIMIT 10 OFFSET 5 WITH EXPAND GRAPH DEPTH 3, CROSS_REFS",
        );
        assert!(matches!(table.as_of, Some(AsOfClause::Commit(ref v)) if v == "abc123"));
        assert!(table.filter.is_some());
        assert_eq!(table.order_by.len(), 2);
        assert!(table.order_by[0].expr.is_some());
        assert!(table.order_by[0].ascending);
        assert!(table.order_by[0].nulls_first);
        assert!(!table.order_by[1].ascending);
        assert!(!table.order_by[1].nulls_first);
        assert_eq!(table.limit, Some(10));
        assert_eq!(table.offset, Some(5));
        assert!(matches!(
            table.expand,
            Some(ExpandOptions {
                graph: true,
                graph_depth: 3,
                cross_refs: true,
                ..
            })
        ));

        let table = parse_table("SELECT * FROM users AS OF BRANCH 'main'");
        assert!(matches!(table.as_of, Some(AsOfClause::Branch(ref v)) if v == "main"));

        let table = parse_table("SELECT * FROM users AS OF TAG 'v1'");
        assert!(matches!(table.as_of, Some(AsOfClause::Tag(ref v)) if v == "v1"));

        let table = parse_table("SELECT * FROM users AS OF TIMESTAMP 1710000000000");
        assert!(matches!(
            table.as_of,
            Some(AsOfClause::TimestampMs(1_710_000_000_000))
        ));

        let table = parse_table("SELECT * FROM users AS OF SNAPSHOT 42");
        assert!(matches!(table.as_of, Some(AsOfClause::Snapshot(42))));

        let table = parse_table("SELECT * FROM users WITH EXPAND");
        assert!(matches!(
            table.expand,
            Some(ExpandOptions {
                graph: true,
                graph_depth: 1,
                cross_refs: true,
                ..
            })
        ));

        assert!(super::super::parse("SELECT * FROM users AS OF SNAPSHOT -1").is_err());
        assert!(super::super::parse("SELECT * FROM users AS OF UNKNOWN 'x'").is_err());
    }

    #[test]
    fn direct_parser_helpers_cover_projection_group_order_and_literals() {
        let mut parser = Parser::new("name, LOWER(email) AS email_l").unwrap();
        let projections = parser.parse_projection_list().unwrap();
        assert_eq!(projections.len(), 2);

        let mut parser = Parser::new("dept, TIME_BUCKET(5 m)").unwrap();
        let group_by = parser.parse_group_by_list().unwrap();
        assert_eq!(group_by, vec!["dept", "TIME_BUCKET(5m)"]);

        let mut parser = Parser::new("LOWER(name) DESC, created_at").unwrap();
        let order_by = parser.parse_order_by_list().unwrap();
        assert_eq!(order_by.len(), 2);
        assert!(order_by[0].expr.is_some());
        assert!(!order_by[0].ascending);
        assert!(order_by[0].nulls_first);
        assert!(order_by[1].ascending);
        assert!(!order_by[1].nulls_first);

        let mut parser = Parser::new("-5 ms").unwrap();
        assert_eq!(parser.parse_function_literal_arg().unwrap(), "-5ms");
        let mut parser = Parser::new("2.0 H").unwrap();
        assert_eq!(parser.parse_function_literal_arg().unwrap(), "2h");
        let mut parser = Parser::new("bad").unwrap();
        assert!(parser.parse_function_literal_arg().is_err());
    }

    #[test]
    fn from_subquery_source_is_preserved() {
        let parsed = super::super::parse("FROM (SELECT id FROM users) AS u RETURN u.id")
            .unwrap()
            .query;
        let QueryExpr::Table(table) = parsed else {
            panic!("expected table query");
        };
        assert_eq!(table.table, "__subq_u");
        assert_eq!(table.alias.as_deref(), Some("u"));
        assert!(matches!(table.source, Some(TableSource::Subquery(_))));
        assert_eq!(table.select_items.len(), 1);

        assert!(super::super::parse("FROM (MATCH (n) RETURN n) AS g").is_err());
    }
}
