//! Join query parsing (FROM ... JOIN GRAPH ...)

use super::super::ast::{
    FieldRef, GraphQuery, JoinCondition, JoinQuery, JoinType, QueryExpr, TableQuery,
};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;

impl<'a> Parser<'a> {
    /// Parse FROM ... JOIN GRAPH ... query
    pub fn parse_from_query(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::From)?;

        // Parse table name and alias
        let table = self.expect_ident()?;
        let alias = if self.check(&Token::Ident("".into())) && !self.is_join_keyword() {
            Some(self.expect_ident()?)
        } else if self.consume(&Token::As)? {
            Some(self.expect_ident()?)
        } else {
            None
        };

        let mut table_query = TableQuery {
            table,
            alias,
            columns: Vec::new(),
            filter: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
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

        self.expect(Token::Graph)?;

        // Parse graph pattern
        let pattern = self.parse_graph_pattern()?;

        // Parse ON condition
        self.expect(Token::On)?;
        let on = self.parse_join_condition()?;

        // Parse optional WHERE
        let filter = if self.consume(&Token::Where)? {
            Some(self.parse_filter()?)
        } else {
            None
        };

        let mut graph_query = GraphQuery {
            pattern,
            filter,
            return_: Vec::new(),
        };

        // Parse optional RETURN
        if self.consume(&Token::Return)? {
            graph_query.return_ = self.parse_return_list()?;
        }

        Ok(QueryExpr::Join(JoinQuery {
            left: Box::new(QueryExpr::Table(left_table)),
            right: Box::new(QueryExpr::Graph(graph_query)),
            join_type,
            on,
        }))
    }

    /// Parse join condition: table.col = node.prop
    fn parse_join_condition(&mut self) -> Result<JoinCondition, ParseError> {
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
}
