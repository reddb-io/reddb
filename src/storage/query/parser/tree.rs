//! Parser for TREE commands and CREATE/DROP TREE.

use super::super::ast::{
    CreateTreeQuery, DropTreeQuery, QueryExpr, TreeCommand, TreeNodeSpec, TreePosition,
};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;
use crate::json::Value as JsonValue;
use crate::storage::schema::Value;

impl<'a> Parser<'a> {
    pub fn parse_create_tree_body(&mut self) -> Result<QueryExpr, ParseError> {
        let if_not_exists = self.match_if_not_exists()?;
        let name = self.expect_ident()?;
        self.expect(Token::In)?;
        let collection = self.expect_ident()?;
        self.expect_tree_ident("ROOT")?;
        let root = self.parse_tree_node_spec(false)?;
        let default_max_children = self.parse_tree_required_max_children()?;

        Ok(QueryExpr::CreateTree(CreateTreeQuery {
            collection,
            name,
            root,
            default_max_children,
            if_not_exists,
        }))
    }

    pub fn parse_drop_tree_body(&mut self) -> Result<QueryExpr, ParseError> {
        let if_exists = self.match_if_exists()?;
        let name = self.expect_ident()?;
        self.expect(Token::In)?;
        let collection = self.expect_ident()?;
        Ok(QueryExpr::DropTree(DropTreeQuery {
            collection,
            name,
            if_exists,
        }))
    }

    pub fn parse_tree_command(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::Tree)?;

        if self.consume(&Token::Insert)? {
            self.expect(Token::Into)?;
            let (collection, tree_name) = self.parse_tree_target()?;
            self.expect_tree_ident("PARENT")?;
            let parent_id = self.parse_tree_entity_id()?;
            let node = self.parse_tree_node_spec(true)?;
            let position = self.parse_tree_position()?;
            return Ok(QueryExpr::TreeCommand(TreeCommand::Insert {
                collection,
                tree_name,
                parent_id,
                node,
                position,
            }));
        }

        if self.consume_ident_ci("MOVE")? {
            let (collection, tree_name) = self.parse_tree_target()?;
            self.expect(Token::Node)?;
            let node_id = self.parse_tree_entity_id()?;
            self.expect(Token::To)?;
            self.expect_tree_ident("PARENT")?;
            let parent_id = self.parse_tree_entity_id()?;
            let position = self.parse_tree_position()?;
            return Ok(QueryExpr::TreeCommand(TreeCommand::Move {
                collection,
                tree_name,
                node_id,
                parent_id,
                position,
            }));
        }

        if self.consume(&Token::Delete)? {
            let (collection, tree_name) = self.parse_tree_target()?;
            self.expect(Token::Node)?;
            let node_id = self.parse_tree_entity_id()?;
            return Ok(QueryExpr::TreeCommand(TreeCommand::Delete {
                collection,
                tree_name,
                node_id,
            }));
        }

        if self.consume_ident_ci("VALIDATE")? {
            let (collection, tree_name) = self.parse_tree_target()?;
            return Ok(QueryExpr::TreeCommand(TreeCommand::Validate {
                collection,
                tree_name,
            }));
        }

        if self.consume_ident_ci("REBALANCE")? {
            let (collection, tree_name) = self.parse_tree_target()?;
            let dry_run = if self.consume_ident_ci("DRY")? {
                self.expect_tree_ident("RUN")?;
                true
            } else {
                false
            };
            return Ok(QueryExpr::TreeCommand(TreeCommand::Rebalance {
                collection,
                tree_name,
                dry_run,
            }));
        }

        Err(ParseError::expected(
            vec!["INSERT", "MOVE", "DELETE", "VALIDATE", "REBALANCE"],
            self.peek(),
            self.position(),
        ))
    }

    fn parse_tree_target(&mut self) -> Result<(String, String), ParseError> {
        let collection = self.expect_ident()?;
        self.expect(Token::Dot)?;
        let tree_name = self.expect_ident()?;
        Ok((collection, tree_name))
    }

    fn parse_tree_node_spec(
        &mut self,
        allow_max_children: bool,
    ) -> Result<TreeNodeSpec, ParseError> {
        self.expect_tree_ident("LABEL")?;
        let label = self.parse_tree_string_like()?;
        let mut node_type = None;
        let mut properties = Vec::new();
        let mut metadata = Vec::new();
        let mut max_children = None;

        loop {
            if self.consume_ident_ci("TYPE")? {
                node_type = Some(self.parse_tree_string_like()?);
            } else if self.consume(&Token::Properties)? {
                properties = self.parse_tree_object_literal_entries()?;
            } else if self.consume(&Token::Metadata)? {
                metadata = self.parse_tree_object_literal_entries()?;
            } else if allow_max_children
                && (self.consume_ident_ci("MAX_CHILDREN")?
                    || self.consume_ident_ci("MAXCHILDREN")?)
            {
                max_children = Some(self.parse_tree_positive_usize()?);
            } else {
                break;
            }
        }

        Ok(TreeNodeSpec {
            label,
            node_type,
            properties,
            metadata,
            max_children,
        })
    }

    fn parse_tree_position(&mut self) -> Result<TreePosition, ParseError> {
        if !(self.consume_ident_ci("POSITION")?) {
            return Ok(TreePosition::Last);
        }

        if self.consume(&Token::First)? {
            return Ok(TreePosition::First);
        }
        if self.consume(&Token::Last)? {
            return Ok(TreePosition::Last);
        }

        Ok(TreePosition::Index(self.parse_tree_positive_usize()?))
    }

    fn parse_tree_required_max_children(&mut self) -> Result<usize, ParseError> {
        if !(self.consume_ident_ci("MAX_CHILDREN")? || self.consume_ident_ci("MAXCHILDREN")?) {
            return Err(ParseError::expected(
                vec!["MAX_CHILDREN"],
                self.peek(),
                self.position(),
            ));
        }
        self.parse_tree_positive_usize()
    }

    fn parse_tree_entity_id(&mut self) -> Result<u64, ParseError> {
        match self.peek().clone() {
            Token::Integer(value) if value > 0 => {
                self.advance()?;
                Ok(value as u64)
            }
            Token::String(value) => {
                self.advance()?;
                parse_tree_entity_id_text(&value).ok_or_else(|| {
                    ParseError::new(
                        format!("invalid tree entity id '{}'", value),
                        self.position(),
                    )
                })
            }
            other => Err(ParseError::expected(
                vec!["entity id"],
                &other,
                self.position(),
            )),
        }
    }

    fn parse_tree_positive_usize(&mut self) -> Result<usize, ParseError> {
        let value = self.parse_integer()?;
        if value <= 0 {
            return Err(ParseError::new(
                "expected a positive integer".to_string(),
                self.position(),
            ));
        }
        Ok(value as usize)
    }

    fn parse_tree_string_like(&mut self) -> Result<String, ParseError> {
        match self.peek().clone() {
            Token::String(value) => {
                self.advance()?;
                Ok(value)
            }
            _ => self.expect_ident_or_keyword(),
        }
    }

    fn expect_tree_ident(&mut self, expected: &str) -> Result<(), ParseError> {
        if self.consume_ident_ci(expected)? {
            return Ok(());
        }
        Err(ParseError::expected(
            vec![expected],
            self.peek(),
            self.position(),
        ))
    }

    fn parse_tree_object_literal_entries(&mut self) -> Result<Vec<(String, Value)>, ParseError> {
        let literal = self.parse_literal_value()?;
        let Value::Json(bytes) = literal else {
            return Err(ParseError::new(
                "expected object literal".to_string(),
                self.position(),
            ));
        };
        let decoded = crate::json::from_slice::<JsonValue>(&bytes).map_err(|err| {
            ParseError::new(
                format!("failed to decode object literal: {err}"),
                self.position(),
            )
        })?;
        let JsonValue::Object(object) = decoded else {
            return Err(ParseError::new(
                "expected object literal".to_string(),
                self.position(),
            ));
        };
        object
            .into_iter()
            .map(|(key, value)| Ok((key, tree_json_value_to_storage_value(&value)?)))
            .collect()
    }
}

fn parse_tree_entity_id_text(value: &str) -> Option<u64> {
    value
        .strip_prefix('e')
        .unwrap_or(value)
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
}

fn tree_json_value_to_storage_value(value: &JsonValue) -> Result<Value, ParseError> {
    Ok(match value {
        JsonValue::Null => Value::Null,
        JsonValue::Bool(value) => Value::Boolean(*value),
        JsonValue::Number(value) => {
            if value.fract().abs() < f64::EPSILON {
                Value::Integer(*value as i64)
            } else {
                Value::Float(*value)
            }
        }
        JsonValue::String(value) => Value::Text(value.clone()),
        JsonValue::Array(_) | JsonValue::Object(_) => {
            Value::Json(crate::json::to_vec(value).map_err(|err| {
                ParseError::new(
                    format!("failed to encode nested JSON value: {err}"),
                    crate::storage::query::lexer::Position::default(),
                )
            })?)
        }
    })
}
