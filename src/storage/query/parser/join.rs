//! Join query parsing (FROM ... JOIN GRAPH/PATH/TABLE/VECTOR ...)

use super::super::ast::{
    FieldRef, GraphQuery, JoinCondition, JoinQuery, JoinType, QueryExpr, SelectItem, TableQuery,
};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;
use crate::storage::query::sql_lowering::{filter_to_expr, projection_to_select_item};
impl<'a> Parser<'a> {
    /// Parse FROM ... JOIN (GRAPH / PATH / TABLE / VECTOR / HYBRID) query
    pub fn parse_from_query(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::From)?;

        // Fase 1.7 unlock: `FROM (SELECT … FROM t) AS alias` — subquery
        // in FROM position. Detect by peeking LParen before falling
        // through to the legacy identifier-only parse_table_source.
        // The subquery branch builds the outer TableQuery via
        // `TableQuery::from_subquery` which sets `source` to
        // `TableSource::Subquery` and marks `table` with a sentinel
        // `__subq_<alias>` so code that still reads `table.as_str()`
        // errors loudly instead of silently mis-resolving.
        let mut table_query = if self.check(&Token::LParen) {
            self.advance()?; // consume `(`
                             // Only SELECT is allowed in a FROM subquery — reject
                             // `FROM (MATCH … RETURN)` and other non-SELECT shapes.
            if !self.check(&Token::Select) {
                return Err(ParseError::new(
                    "subquery in FROM must start with SELECT".to_string(),
                    self.position(),
                ));
            }
            let inner = self.parse_select_query()?;
            self.expect(Token::RParen)?;
            let alias = if self.consume(&Token::As)?
                || (self.check(&Token::Ident("".into())) && !self.is_join_keyword())
            {
                Some(self.expect_ident()?)
            } else {
                None
            };
            TableQuery::from_subquery(inner, alias)
        } else {
            // Parse table name and alias
            let table = self.parse_table_source()?;
            let alias = if self.consume(&Token::As)?
                || (self.check(&Token::Ident("".into())) && !self.is_join_keyword())
            {
                Some(self.expect_ident()?)
            } else {
                None
            };
            TableQuery {
                table,
                source: None,
                alias,
                select_items: Vec::new(),
                columns: Vec::new(),
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
            }
        };

        // Check for JOIN
        if self.is_join_keyword() {
            return self.parse_join_query(table_query);
        }

        // Parse optional WHERE clause
        if self.consume(&Token::Where)? {
            let filter = self.parse_filter()?;
            table_query.where_expr = Some(filter_to_expr(&filter));
            table_query.filter = Some(filter);
        }

        // Parse optional ORDER BY
        if self.consume(&Token::Order)? {
            self.expect(Token::By)?;
            table_query.order_by = self.parse_order_by_list()?;
        }

        // Parse optional LIMIT/OFFSET
        if self.consume(&Token::Limit)? {
            table_query.limit = Some(self.parse_integer()? as u64);
        }
        if self.consume(&Token::Offset)? {
            table_query.offset = Some(self.parse_integer()? as u64);
        }

        // Check for RETURN (shorthand for column selection)
        if self.consume(&Token::Return)? {
            let (select_items, columns) = self.parse_select_items_and_projections()?;
            table_query.select_items = select_items;
            table_query.columns = columns;
        }

        Ok(QueryExpr::Table(table_query))
    }

    /// Check if current token is a join keyword
    pub fn is_join_keyword(&self) -> bool {
        matches!(
            self.peek(),
            Token::Join | Token::Inner | Token::Left | Token::Right | Token::Full | Token::Cross
        )
    }

    /// Parse JOIN query
    fn parse_join_query(&mut self, left_table: TableQuery) -> Result<QueryExpr, ParseError> {
        // Parse join type
        let join_type = if self.consume(&Token::Inner)? {
            self.expect(Token::Join)?;
            JoinType::Inner
        } else if self.consume(&Token::Left)? {
            self.consume(&Token::Outer)?;
            self.expect(Token::Join)?;
            JoinType::LeftOuter
        } else if self.consume(&Token::Right)? {
            self.consume(&Token::Outer)?;
            self.expect(Token::Join)?;
            JoinType::RightOuter
        } else if self.consume(&Token::Full)? {
            // `FULL JOIN` and `FULL OUTER JOIN` are aliases.
            self.consume(&Token::Outer)?;
            self.expect(Token::Join)?;
            JoinType::FullOuter
        } else if self.consume(&Token::Cross)? {
            self.expect(Token::Join)?;
            JoinType::Cross
        } else {
            self.expect(Token::Join)?;
            JoinType::Inner
        };

        if self.consume(&Token::Graph)? {
            return self.parse_graph_join_query(left_table, join_type);
        }
        if self.check(&Token::Path) {
            return self.parse_path_join_query(left_table, join_type);
        }
        if self.check(&Token::Vector) {
            return self.parse_vector_join_query(left_table, join_type);
        }
        if self.check(&Token::Hybrid) {
            return self.parse_hybrid_join_query(left_table, join_type);
        }

        self.parse_table_join_query(left_table, join_type)
    }

    fn parse_graph_join_query(
        &mut self,
        left_table: TableQuery,
        join_type: JoinType,
    ) -> Result<QueryExpr, ParseError> {
        // Parse graph pattern
        let pattern = self.parse_graph_pattern()?;
        let alias = self.parse_join_rhs_alias()?;

        // Parse ON condition
        self.expect(Token::On)?;
        let on = self.parse_graph_join_condition()?;
        let (filter, order_by, limit, offset, return_items, return_) =
            self.parse_join_post_clauses()?;
        let graph_query = GraphQuery {
            alias,
            pattern,
            filter: None,
            return_: Vec::new(),
        };

        Ok(QueryExpr::Join(JoinQuery {
            left: Box::new(QueryExpr::Table(left_table)),
            right: Box::new(QueryExpr::Graph(graph_query)),
            join_type,
            on,
            filter,
            order_by,
            limit,
            offset,
            return_items,
            return_,
        }))
    }

    fn parse_table_join_query(
        &mut self,
        left_table: TableQuery,
        join_type: JoinType,
    ) -> Result<QueryExpr, ParseError> {
        let table = self.parse_table_source()?;
        let alias = if self.consume(&Token::As)?
            || (self.check(&Token::Ident("".into())) && !self.is_clause_keyword())
        {
            Some(self.expect_ident()?)
        } else {
            None
        };

        // CROSS JOIN has no ON clause — emit a sentinel JoinCondition
        // that the runtime join loops treat as "always matches".
        let on = if matches!(join_type, JoinType::Cross) {
            cross_join_sentinel()
        } else {
            self.expect(Token::On)?;
            self.parse_table_join_condition()?
        };
        let (filter, order_by, limit, offset, return_items, return_) =
            self.parse_join_post_clauses()?;
        let table_query = TableQuery {
            table,
            source: None,
            alias,
            select_items: Vec::new(),
            columns: Vec::new(),
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

        Ok(QueryExpr::Join(JoinQuery {
            left: Box::new(QueryExpr::Table(left_table)),
            right: Box::new(QueryExpr::Table(table_query)),
            join_type,
            on,
            filter,
            order_by,
            limit,
            offset,
            return_items,
            return_,
        }))
    }

    fn parse_vector_join_query(
        &mut self,
        left_table: TableQuery,
        join_type: JoinType,
    ) -> Result<QueryExpr, ParseError> {
        let mut right = match self.parse_vector_query()? {
            QueryExpr::Vector(query) => query,
            _ => unreachable!("vector parser must return QueryExpr::Vector"),
        };
        right.alias = self.parse_join_rhs_alias()?;
        self.expect(Token::On)?;
        let on = self.parse_table_join_condition()?;
        let (filter, order_by, limit, offset, return_items, return_) =
            self.parse_join_post_clauses()?;

        Ok(QueryExpr::Join(JoinQuery {
            left: Box::new(QueryExpr::Table(left_table)),
            right: Box::new(QueryExpr::Vector(right)),
            join_type,
            on,
            filter,
            order_by,
            limit,
            offset,
            return_items,
            return_,
        }))
    }

    fn parse_path_join_query(
        &mut self,
        left_table: TableQuery,
        join_type: JoinType,
    ) -> Result<QueryExpr, ParseError> {
        let mut right = match self.parse_path_query()? {
            QueryExpr::Path(query) => query,
            _ => unreachable!("path parser must return QueryExpr::Path"),
        };
        right.alias = self.parse_join_rhs_alias()?;
        self.expect(Token::On)?;
        let on = self.parse_table_join_condition()?;
        let (filter, order_by, limit, offset, return_items, return_) =
            self.parse_join_post_clauses()?;

        Ok(QueryExpr::Join(JoinQuery {
            left: Box::new(QueryExpr::Table(left_table)),
            right: Box::new(QueryExpr::Path(right)),
            join_type,
            on,
            filter,
            order_by,
            limit,
            offset,
            return_items,
            return_,
        }))
    }

    fn parse_hybrid_join_query(
        &mut self,
        left_table: TableQuery,
        join_type: JoinType,
    ) -> Result<QueryExpr, ParseError> {
        let mut right = match self.parse_hybrid_query()? {
            QueryExpr::Hybrid(query) => query,
            _ => unreachable!("hybrid parser must return QueryExpr::Hybrid"),
        };
        right.alias = self.parse_join_rhs_alias()?;
        self.expect(Token::On)?;
        let on = self.parse_table_join_condition()?;
        let (filter, order_by, limit, offset, return_items, return_) =
            self.parse_join_post_clauses()?;

        Ok(QueryExpr::Join(JoinQuery {
            left: Box::new(QueryExpr::Table(left_table)),
            right: Box::new(QueryExpr::Hybrid(right)),
            join_type,
            on,
            filter,
            order_by,
            limit,
            offset,
            return_items,
            return_,
        }))
    }

    fn parse_join_post_clauses(
        &mut self,
    ) -> Result<
        (
            Option<super::super::ast::Filter>,
            Vec<super::super::ast::OrderByClause>,
            Option<u64>,
            Option<u64>,
            Vec<SelectItem>,
            Vec<super::super::ast::Projection>,
        ),
        ParseError,
    > {
        let filter = if self.consume(&Token::Where)? {
            Some(self.parse_filter()?)
        } else {
            None
        };

        let order_by = if self.consume(&Token::Order)? {
            self.expect(Token::By)?;
            self.parse_order_by_list()?
        } else {
            Vec::new()
        };

        let limit = if self.consume(&Token::Limit)? {
            Some(self.parse_integer()? as u64)
        } else {
            None
        };

        let offset = if self.consume(&Token::Offset)? {
            Some(self.parse_integer()? as u64)
        } else {
            None
        };

        let (return_items, return_) = if self.consume(&Token::Return)? {
            let projections = self.parse_return_list()?;
            let items = projections
                .iter()
                .filter_map(projection_to_select_item)
                .collect();
            (items, projections)
        } else {
            (Vec::new(), Vec::new())
        };

        Ok((filter, order_by, limit, offset, return_items, return_))
    }

    fn parse_join_rhs_alias(&mut self) -> Result<Option<String>, ParseError> {
        if self.consume(&Token::As)?
            || (self.check(&Token::Ident("".into())) && !self.is_join_rhs_clause_keyword())
        {
            Ok(Some(self.expect_ident()?))
        } else {
            Ok(None)
        }
    }

    fn is_join_rhs_clause_keyword(&self) -> bool {
        matches!(
            self.peek(),
            Token::On
                | Token::Where
                | Token::Order
                | Token::Limit
                | Token::Offset
                | Token::Return
                | Token::Join
                | Token::Inner
                | Token::Left
                | Token::Right
        )
    }

    fn parse_table_source(&mut self) -> Result<String, ParseError> {
        if self.consume(&Token::Star)? {
            Ok("*".to_string())
        } else if self.consume(&Token::All)? {
            Ok("all".to_string())
        } else {
            self.expect_ident()
        }
    }

    /// Parse join condition: table.col = node.prop
    fn parse_graph_join_condition(&mut self) -> Result<JoinCondition, ParseError> {
        let left_field = self.parse_field_ref()?;
        self.expect(Token::Eq)?;
        let right_first = self.expect_ident()?;
        self.expect(Token::Dot)?;
        let right_second = self.expect_ident()?;

        // Try to determine if right side is node property or ID
        let right_field = if right_second == "id" {
            FieldRef::NodeId { alias: right_first }
        } else {
            FieldRef::NodeProperty {
                alias: right_first,
                property: right_second,
            }
        };

        Ok(JoinCondition {
            left_field,
            right_field,
        })
    }

    fn parse_table_join_condition(&mut self) -> Result<JoinCondition, ParseError> {
        let left_field = self.parse_field_ref()?;
        self.expect(Token::Eq)?;
        let right_field = self.parse_field_ref()?;

        Ok(JoinCondition {
            left_field,
            right_field,
        })
    }
}

/// Sentinel JoinCondition used for CROSS JOIN — neither field references
/// anything real. The runtime join loops must detect
/// `join_type == JoinType::Cross` and skip predicate evaluation entirely
/// rather than resolving these empty fields.
fn cross_join_sentinel() -> JoinCondition {
    JoinCondition {
        left_field: FieldRef::TableColumn {
            table: String::new(),
            column: String::new(),
        },
        right_field: FieldRef::TableColumn {
            table: String::new(),
            column: String::new(),
        },
    }
}
