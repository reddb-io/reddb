//! Join query parsing (FROM ... JOIN GRAPH/PATH/TABLE/VECTOR ...)

use super::super::ast::{
    FieldRef, GraphQuery, JoinCondition, JoinQuery, JoinType, QueryExpr, TableQuery,
};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;
impl<'a> Parser<'a> {
    /// Parse FROM ... JOIN (GRAPH / PATH / TABLE / VECTOR / HYBRID) query
    pub fn parse_from_query(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::From)?;

        // Parse table name and alias
        let table = self.parse_table_source()?;
        let alias = if self.consume(&Token::As)?
            || (self.check(&Token::Ident("".into())) && !self.is_join_keyword())
        {
            Some(self.expect_ident()?)
        } else {
            None
        };

        let mut table_query = TableQuery {
            table,
            alias,
            columns: Vec::new(),
            filter: None,
            group_by: Vec::new(),
            having: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            expand: None,
        };

        // Check for JOIN
        if self.is_join_keyword() {
            return self.parse_join_query(table_query);
        }

        // Parse optional WHERE clause
        if self.consume(&Token::Where)? {
            table_query.filter = Some(self.parse_filter()?);
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
            table_query.columns = self.parse_projection_list()?;
        }

        Ok(QueryExpr::Table(table_query))
    }

    /// Check if current token is a join keyword
    pub fn is_join_keyword(&self) -> bool {
        matches!(
            self.peek(),
            Token::Join | Token::Inner | Token::Left | Token::Right
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
        let (filter, order_by, limit, offset, return_) = self.parse_join_post_clauses()?;
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

        self.expect(Token::On)?;
        let on = self.parse_table_join_condition()?;
        let (filter, order_by, limit, offset, return_) = self.parse_join_post_clauses()?;
        let table_query = TableQuery {
            table,
            alias,
            columns: Vec::new(),
            filter: None,
            group_by: Vec::new(),
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
        let (filter, order_by, limit, offset, return_) = self.parse_join_post_clauses()?;

        Ok(QueryExpr::Join(JoinQuery {
            left: Box::new(QueryExpr::Table(left_table)),
            right: Box::new(QueryExpr::Vector(right)),
            join_type,
            on,
            filter,
            order_by,
            limit,
            offset,
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
        let (filter, order_by, limit, offset, return_) = self.parse_join_post_clauses()?;

        Ok(QueryExpr::Join(JoinQuery {
            left: Box::new(QueryExpr::Table(left_table)),
            right: Box::new(QueryExpr::Path(right)),
            join_type,
            on,
            filter,
            order_by,
            limit,
            offset,
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
        let (filter, order_by, limit, offset, return_) = self.parse_join_post_clauses()?;

        Ok(QueryExpr::Join(JoinQuery {
            left: Box::new(QueryExpr::Table(left_table)),
            right: Box::new(QueryExpr::Hybrid(right)),
            join_type,
            on,
            filter,
            order_by,
            limit,
            offset,
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

        let return_ = if self.consume(&Token::Return)? {
            self.parse_return_list()?
        } else {
            Vec::new()
        };

        Ok((filter, order_by, limit, offset, return_))
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
