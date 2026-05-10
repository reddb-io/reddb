//! Parser for stable CONFIG keyed commands.

use super::super::ast::{ConfigCommand, ConfigValueType, QueryExpr};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;

impl<'a> Parser<'a> {
    pub fn parse_config_command(&mut self) -> Result<QueryExpr, ParseError> {
        let operation = self.expect_ident_or_keyword()?.to_ascii_uppercase();
        if operation != "PUT"
            && operation != "GET"
            && operation != "RESOLVE"
            && operation != "ROTATE"
            && operation != "DELETE"
            && operation != "HISTORY"
            && operation != "INCR"
            && operation != "DECR"
            && operation != "ADD"
            && operation != "INVALIDATE"
        {
            return Err(ParseError::expected(
                vec![
                    "PUT",
                    "GET",
                    "RESOLVE",
                    "ROTATE",
                    "DELETE",
                    "HISTORY",
                    "INCR",
                    "DECR",
                    "ADD",
                    "INVALIDATE",
                ],
                self.peek(),
                self.position(),
            ));
        }

        if !self.consume_ident_ci("CONFIG")? {
            return Err(ParseError::expected(
                vec!["CONFIG"],
                self.peek(),
                self.position(),
            ));
        }

        let mut collection = self.expect_ident_or_keyword()?.to_ascii_lowercase();
        if self.consume(&Token::Dot)? {
            let next = self.expect_ident_or_keyword()?.to_ascii_lowercase();
            collection = format!("{collection}.{next}");
        }
        let key = if !matches!(self.peek(), Token::Eof) {
            Some(self.expect_ident_or_keyword()?.to_ascii_lowercase())
        } else {
            None
        };

        match operation.as_str() {
            "PUT" => {
                let key = key.ok_or_else(|| {
                    ParseError::expected(vec!["config key"], self.peek(), self.position())
                })?;
                self.expect(Token::Eq)?;
                let value = self.parse_value()?;
                let value_type = self.parse_config_value_type()?;
                if self.consume_ident_ci("TTL")? || self.consume_ident_ci("EXPIRE")? {
                    self.consume_config_tail()?;
                    return Ok(QueryExpr::ConfigCommand(
                        ConfigCommand::InvalidVolatileOperation {
                            operation: "TTL/EXPIRE".to_string(),
                            collection,
                            key: Some(key),
                        },
                    ));
                }
                Ok(QueryExpr::ConfigCommand(ConfigCommand::Put {
                    collection,
                    key,
                    value,
                    value_type,
                }))
            }
            "GET" => Ok(QueryExpr::ConfigCommand(ConfigCommand::Get {
                collection,
                key: key.ok_or_else(|| {
                    ParseError::expected(vec!["config key"], self.peek(), self.position())
                })?,
            })),
            "RESOLVE" => Ok(QueryExpr::ConfigCommand(ConfigCommand::Resolve {
                collection,
                key: key.ok_or_else(|| {
                    ParseError::expected(vec!["config key"], self.peek(), self.position())
                })?,
            })),
            "ROTATE" => {
                let key = key.ok_or_else(|| {
                    ParseError::expected(vec!["config key"], self.peek(), self.position())
                })?;
                self.expect(Token::Eq)?;
                let value = self.parse_value()?;
                let value_type = self.parse_config_value_type()?;
                if self.consume_ident_ci("TTL")? || self.consume_ident_ci("EXPIRE")? {
                    self.consume_config_tail()?;
                    return Ok(QueryExpr::ConfigCommand(
                        ConfigCommand::InvalidVolatileOperation {
                            operation: "TTL/EXPIRE".to_string(),
                            collection,
                            key: Some(key),
                        },
                    ));
                }
                Ok(QueryExpr::ConfigCommand(ConfigCommand::Rotate {
                    collection,
                    key,
                    value,
                    value_type,
                }))
            }
            "DELETE" => Ok(QueryExpr::ConfigCommand(ConfigCommand::Delete {
                collection,
                key: key.ok_or_else(|| {
                    ParseError::expected(vec!["config key"], self.peek(), self.position())
                })?,
            })),
            "HISTORY" => Ok(QueryExpr::ConfigCommand(ConfigCommand::History {
                collection,
                key: key.ok_or_else(|| {
                    ParseError::expected(vec!["config key"], self.peek(), self.position())
                })?,
            })),
            _ => Ok(QueryExpr::ConfigCommand(
                ConfigCommand::InvalidVolatileOperation {
                    operation,
                    collection,
                    key,
                },
            )),
        }
    }

    fn consume_config_tail(&mut self) -> Result<(), ParseError> {
        while !matches!(self.peek(), Token::Eof) {
            self.advance()?;
        }
        Ok(())
    }

    fn parse_config_value_type(&mut self) -> Result<Option<ConfigValueType>, ParseError> {
        let has_with = self.consume(&Token::With)?;
        let has_type = self.consume_ident_ci("TYPE")?;
        let has_schema = if !has_type {
            self.consume(&Token::Schema)?
        } else {
            false
        };
        if !has_with && !has_type && !has_schema {
            return Ok(None);
        }
        if has_with && !has_type && !has_schema {
            return Err(ParseError::expected(
                vec!["TYPE", "SCHEMA"],
                self.peek(),
                self.position(),
            ));
        }
        let raw_type = self.expect_ident_or_keyword()?;
        let Some(value_type) = ConfigValueType::parse(&raw_type) else {
            return Err(ParseError::expected(
                vec!["bool", "int", "string", "url", "object", "array"],
                self.peek(),
                self.position(),
            ));
        };
        Ok(Some(value_type))
    }
}
