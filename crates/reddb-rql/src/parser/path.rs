//! Path query parsing (PATH FROM ... TO ...)

use crate::ast::{CompareOp, NodeSelector, PathQuery, PropertyFilter, QueryExpr};
use crate::lexer::Token;
use super::error::ParseError;
use super::Parser;

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

        let mut max_length = 10;
        loop {
            if self.consume(&Token::Algorithm)? || self.consume(&Token::Direction)? {
                let _ = self.expect_ident_or_keyword()?;
            } else if self.consume(&Token::Limit)? {
                max_length = self.parse_integer()? as u32;
            } else {
                break;
            }
        }

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
        if let Token::String(id) = self.peek().clone() {
            self.advance()?;
            return Ok(NodeSelector::ById(id));
        }

        let name = self.expect_ident()?;

        if !self.consume(&Token::LParen)? {
            return Ok(NodeSelector::ById(name));
        }

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
                // ByType with optional filter — label kept as canonical string.
                let node_label = self.parse_node_label(type_name)?;
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
                NodeSelector::ByType { node_label, filter }
            }
        };

        self.expect(Token::RParen)?;

        Ok(selector)
    }

    /// Parse edge label list: `[:LABEL1, :LABEL2]`
    fn parse_edge_type_list(&mut self) -> Result<Vec<String>, ParseError> {
        self.expect(Token::LBracket)?;

        let mut labels = Vec::new();
        loop {
            self.expect(Token::Colon)?;
            let type_name = self.expect_ident_or_keyword()?;
            labels.push(self.parse_edge_label(&type_name)?);

            if !self.consume(&Token::Comma)? {
                break;
            }
        }

        self.expect(Token::RBracket)?;
        Ok(labels)
    }
}
