//! Parser for TREE commands and CREATE/DROP TREE.

use super::error::ParseError;
use super::Parser;
use crate::ast::{
    CreateTreeQuery, DropTreeQuery, QueryExpr, TreeCommand, TreeNodeSpec, TreePosition,
};
use crate::lexer::Token;
use reddb_types::json::Value as JsonValue;
use reddb_types::types::Value;

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
                        // F-05: `value` is caller-controlled string-literal
                        // bytes. Render via `{:?}` so embedded CR/LF/NUL/
                        // quotes are escaped before the message reaches the
                        // downstream JSON / audit / log / gRPC sinks.
                        format!("invalid tree entity id {value:?}"),
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
        let decoded = reddb_types::json::from_slice::<JsonValue>(&bytes).map_err(|err| {
            ParseError::new(
                // F-05: serde's parse error string can echo a user fragment.
                // Render via `{:?}` so embedded control bytes / quotes are
                // escaped before the message reaches downstream sinks.
                format!("failed to decode object literal: {:?}", err.to_string()),
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
        JsonValue::String(value) => Value::text(value.clone()),
        JsonValue::Array(_) | JsonValue::Object(_) => {
            Value::Json(reddb_types::json::to_vec(value).map_err(|err| {
                ParseError::new(
                    // F-05: defensively escape encoder error in case it
                    // echoes a user fragment.
                    format!("failed to encode nested JSON value: {:?}", err.to_string()),
                    crate::lexer::Position::default(),
                )
            })?)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_query(input: &str) -> Result<QueryExpr, ParseError> {
        crate::parser::parse(input).map(|query| query.query)
    }

    fn entry_value<'a>(entries: &'a [(String, Value)], key: &str) -> &'a Value {
        entries
            .iter()
            .find_map(|(entry_key, value)| (entry_key == key).then_some(value))
            .unwrap_or_else(|| panic!("missing entry {key}"))
    }

    #[test]
    fn create_tree_accepts_root_options_and_maxchildren_alias() {
        let query = parse_query(
            "CREATE TREE IF NOT EXISTS org IN forest ROOT LABEL 'Company' TYPE root \
             METADATA {active: true, rank: 3, nested: {tier: 'gold'}} MAXCHILDREN 2",
        )
        .unwrap();

        let QueryExpr::CreateTree(tree) = query else {
            panic!("expected create tree");
        };
        assert_eq!(tree.collection, "forest");
        assert_eq!(tree.name, "org");
        assert!(tree.if_not_exists);
        assert_eq!(tree.default_max_children, 2);
        assert_eq!(tree.root.label, "Company");
        assert_eq!(tree.root.node_type.as_deref(), Some("root"));
        assert_eq!(tree.root.max_children, None);
        assert_eq!(
            entry_value(&tree.root.metadata, "active"),
            &Value::Boolean(true)
        );
        assert_eq!(entry_value(&tree.root.metadata, "rank"), &Value::Integer(3));
        assert!(matches!(
            entry_value(&tree.root.metadata, "nested"),
            Value::Json(_)
        ));
    }

    #[test]
    fn tree_insert_defaults_to_last_and_accepts_string_entity_ids() {
        let query = parse_query(
            "TREE INSERT INTO forest.org PARENT 'e42' LABEL child \
             PROPERTIES {name: 'Platform'} MAX_CHILDREN 4",
        )
        .unwrap();

        let QueryExpr::TreeCommand(TreeCommand::Insert {
            collection,
            tree_name,
            parent_id,
            node,
            position,
        }) = query
        else {
            panic!("expected tree insert");
        };
        assert_eq!(collection, "forest");
        assert_eq!(tree_name, "org");
        assert_eq!(parent_id, 42);
        assert_eq!(node.label, "child");
        assert_eq!(node.max_children, Some(4));
        assert!(matches!(
            entry_value(&node.properties, "name"),
            Value::Text(value) if value.as_ref() == "Platform"
        ));
        assert_eq!(position, TreePosition::Last);
    }

    #[test]
    fn tree_commands_cover_move_delete_validate_rebalance_and_drop() {
        let query = parse_query("TREE MOVE forest.org NODE 'e9' TO PARENT 1 POSITION 3").unwrap();
        let QueryExpr::TreeCommand(TreeCommand::Move {
            collection,
            tree_name,
            node_id,
            parent_id,
            position,
        }) = query
        else {
            panic!("expected tree move");
        };
        assert_eq!(collection, "forest");
        assert_eq!(tree_name, "org");
        assert_eq!(node_id, 9);
        assert_eq!(parent_id, 1);
        assert_eq!(position, TreePosition::Index(3));

        let query = parse_query("TREE DELETE forest.org NODE 5").unwrap();
        assert!(matches!(
            query,
            QueryExpr::TreeCommand(TreeCommand::Delete {
                collection,
                tree_name,
                node_id,
            }) if collection == "forest" && tree_name == "org" && node_id == 5
        ));

        let query = parse_query("TREE VALIDATE forest.org").unwrap();
        assert!(matches!(
            query,
            QueryExpr::TreeCommand(TreeCommand::Validate {
                collection,
                tree_name,
            }) if collection == "forest" && tree_name == "org"
        ));

        let query = parse_query("TREE REBALANCE forest.org").unwrap();
        assert!(matches!(
            query,
            QueryExpr::TreeCommand(TreeCommand::Rebalance {
                collection,
                tree_name,
                dry_run,
            }) if collection == "forest" && tree_name == "org" && !dry_run
        ));

        let query = parse_query("DROP TREE IF EXISTS org IN forest").unwrap();
        assert!(matches!(
            query,
            QueryExpr::DropTree(drop) if drop.name == "org"
                && drop.collection == "forest"
                && drop.if_exists
        ));
    }

    #[test]
    fn tree_parser_rejects_malformed_commands() {
        for sql in [
            "CREATE TREE org IN forest ROOT LABEL root",
            "TREE INSERT INTO forest.org PARENT 0 LABEL child",
            "TREE INSERT INTO forest.org PARENT 1 LABEL child POSITION 0",
            "TREE INSERT INTO forest.org PARENT 1 LABEL child PROPERTIES 'not-object'",
            "TREE UNKNOWN forest.org",
        ] {
            assert!(parse_query(sql).is_err(), "{sql} should not parse");
        }
    }
}
