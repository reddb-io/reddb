//! Parser for stable CONFIG keyed commands.

use super::error::ParseError;
use super::Parser;
use crate::ast::{ConfigCommand, ConfigValueType, QueryExpr};
use crate::lexer::Token;

impl<'a> Parser<'a> {
    pub fn parse_config_command(&mut self) -> Result<QueryExpr, ParseError> {
        let operation = self.expect_ident_or_keyword()?.to_ascii_uppercase();
        if operation != "PUT"
            && operation != "GET"
            && operation != "RESOLVE"
            && operation != "ROTATE"
            && operation != "DELETE"
            && operation != "HISTORY"
            && operation != "LIST"
            && operation != "WATCH"
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
                    "LIST",
                    "WATCH",
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
        let key = if operation == "LIST"
            || (operation == "WATCH"
                && matches!(self.peek(), Token::Ident(name) if name.eq_ignore_ascii_case("PREFIX")))
        {
            None
        } else if !matches!(self.peek(), Token::Eof) {
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
                let tags = self.parse_optional_config_tags()?;
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
                    tags,
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
                let tags = self.parse_optional_config_tags()?;
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
                    tags,
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
            "LIST" => {
                if key.is_some() {
                    return Err(ParseError::expected(
                        vec!["PREFIX", "LIMIT", "OFFSET"],
                        self.peek(),
                        self.position(),
                    ));
                }
                let (prefix, limit, offset) = self.parse_config_list_tail()?;
                Ok(QueryExpr::ConfigCommand(ConfigCommand::List {
                    collection,
                    prefix,
                    limit,
                    offset,
                }))
            }
            "WATCH" => {
                let (key, prefix) = if self.consume_ident_ci("PREFIX")? {
                    (self.expect_ident_or_keyword()?.to_ascii_lowercase(), true)
                } else {
                    (
                        key.ok_or_else(|| {
                            ParseError::expected(
                                vec!["config key", "PREFIX"],
                                self.peek(),
                                self.position(),
                            )
                        })?,
                        false,
                    )
                };
                let from_lsn = if self.consume(&Token::From)? || self.consume_ident_ci("FROM")? {
                    if !self.consume_ident_ci("LSN")? {
                        return Err(ParseError::expected(
                            vec!["LSN"],
                            self.peek(),
                            self.position(),
                        ));
                    }
                    Some(self.parse_float()?.round() as u64)
                } else {
                    None
                };
                Ok(QueryExpr::ConfigCommand(ConfigCommand::Watch {
                    collection,
                    key,
                    prefix,
                    from_lsn,
                }))
            }
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

    pub(crate) fn parse_config_list_after_list(&mut self) -> Result<QueryExpr, ParseError> {
        if !self.consume_ident_ci("CONFIG")? {
            return Err(ParseError::expected(
                vec!["CONFIG"],
                self.peek(),
                self.position(),
            ));
        }
        let collection = self.parse_config_collection_name()?;
        let (prefix, limit, offset) = self.parse_config_list_tail()?;
        Ok(QueryExpr::ConfigCommand(ConfigCommand::List {
            collection,
            prefix,
            limit,
            offset,
        }))
    }

    pub(crate) fn parse_config_watch_after_watch(&mut self) -> Result<QueryExpr, ParseError> {
        if !self.consume_ident_ci("CONFIG")? {
            return Err(ParseError::expected(
                vec!["CONFIG"],
                self.peek(),
                self.position(),
            ));
        }
        let collection = self.parse_config_collection_name()?;
        let (key, prefix) = if self.consume_ident_ci("PREFIX")? {
            (self.expect_ident_or_keyword()?.to_ascii_lowercase(), true)
        } else {
            (self.expect_ident_or_keyword()?.to_ascii_lowercase(), false)
        };
        let from_lsn = if self.consume(&Token::From)? || self.consume_ident_ci("FROM")? {
            if !self.consume_ident_ci("LSN")? {
                return Err(ParseError::expected(
                    vec!["LSN"],
                    self.peek(),
                    self.position(),
                ));
            }
            Some(self.parse_float()?.round() as u64)
        } else {
            None
        };
        Ok(QueryExpr::ConfigCommand(ConfigCommand::Watch {
            collection,
            key,
            prefix,
            from_lsn,
        }))
    }

    fn parse_config_list_tail(
        &mut self,
    ) -> Result<(Option<String>, Option<usize>, usize), ParseError> {
        let mut prefix = None;
        let mut limit = None;
        let mut offset = 0usize;
        loop {
            if self.consume_ident_ci("PREFIX")? {
                prefix = Some(self.expect_ident_or_keyword()?.to_ascii_lowercase());
            } else if self.consume(&Token::Limit)? || self.consume_ident_ci("LIMIT")? {
                limit = Some(self.parse_float()?.round().max(0.0) as usize);
            } else if self.consume(&Token::Offset)? || self.consume_ident_ci("OFFSET")? {
                offset = self.parse_float()?.round().max(0.0) as usize;
            } else {
                break;
            }
        }
        Ok((prefix, limit, offset))
    }

    fn parse_config_collection_name(&mut self) -> Result<String, ParseError> {
        let mut collection = self.expect_ident_or_keyword()?.to_ascii_lowercase();
        if self.consume(&Token::Dot)? {
            let next = self.expect_ident_or_keyword()?.to_ascii_lowercase();
            collection = format!("{collection}.{next}");
        }
        Ok(collection)
    }

    fn parse_optional_config_tags(&mut self) -> Result<Vec<String>, ParseError> {
        if self.consume_ident_ci("TAGS")? {
            self.parse_kv_tag_list()
        } else {
            Ok(Vec::new())
        }
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
