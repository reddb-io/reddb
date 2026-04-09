//! Path query parsing (PATH FROM ... TO ...)

use super::super::ast::{CompareOp, NodeSelector, PathQuery, PropertyFilter, QueryExpr};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;
use crate::storage::engine::graph_store::GraphEdgeType;

impl<'a> Parser<'a> {
    /// Parse PATH FROM ... TO ... query
    pub fn parse_path_query(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::Path)?;
        self.expect(Token::From)?;

        let from = self.parse_node_selector()?;

        self.expect(Token::To)?;

        let to = self.parse_node_selector()?;

        let via = if self.consume(&Token::Via)? {
            self.parse_edge_type_list()?
        } else {
            Vec::new()
        };

        let max_length = if self.consume(&Token::Limit)? {
            self.parse_integer()? as u32
        } else {
            10 // Default
        };

        let filter = if self.consume(&Token::Where)? {
            Some(self.parse_filter()?)
        } else {
            None
        };

        let return_ = if self.consume(&Token::Return)? {
            self.parse_return_list()?
        } else {
            Vec::new()
        };

        Ok(QueryExpr::Path(PathQuery {
            alias: None,
            from,
            to,
            via,
            max_length,
            filter,
            return_,
        }))
    }

    /// Parse node selector: host('id'), ByType, etc.
    fn parse_node_selector(&mut self) -> Result<NodeSelector, ParseError> {
        let name = self.expect_ident()?;

        self.expect(Token::LParen)?;

        let selector = match name.to_lowercase().as_str() {
            "host" | "node" | "id" => {
                let id = self.parse_string()?;
                NodeSelector::ById(id)
            }
            "row" => {
                let table = self.parse_string()?;
                self.expect(Token::Comma)?;
                let row_id = self.parse_integer()? as u64;
                NodeSelector::ByRow { table, row_id }
            }
            type_name => {
                // ByType with optional filter
                let node_type = self.parse_node_type(type_name)?;
                let filter = if !self.check(&Token::RParen) {
                    let name = self.expect_ident()?;
                    self.expect(Token::Eq)?;
                    let value = self.parse_value()?;
                    Some(PropertyFilter {
                        name,
                        op: CompareOp::Eq,
                        value,
                    })
                } else {
                    None
                };
                NodeSelector::ByType { node_type, filter }
            }
        };

        self.expect(Token::RParen)?;

        Ok(selector)
    }

    /// Parse edge type list: [:TYPE1, :TYPE2]
    fn parse_edge_type_list(&mut self) -> Result<Vec<GraphEdgeType>, ParseError> {
        self.expect(Token::LBracket)?;

        let mut types = Vec::new();
        loop {
            self.expect(Token::Colon)?;
            let type_name = self.expect_ident_or_keyword()?;
            types.push(self.parse_edge_type(&type_name)?);

            if !self.consume(&Token::Comma)? {
                break;
            }
        }

        self.expect(Token::RBracket)?;
        Ok(types)
    }
}
