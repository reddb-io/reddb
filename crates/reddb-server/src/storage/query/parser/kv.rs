//! Parser for KV commands: `KV PUT key = value [EXPIRE n unit] [IF NOT EXISTS]`,
//! `KV GET key`, `KV DELETE key`, `KV INCR key [BY n] [EXPIRE dur]`,
//! `KV CAS key EXPECT <val|NULL> SET <val> [EXPIRE dur]`.
//!
//! Syntax summary:
//! ```text
//! KV PUT  <key> = <value> [EXPIRE <n> [unit]] [IF NOT EXISTS]
//! KV GET  <key>
//! KV DELETE <key>
//! KV INCR <key> [BY <n>] [EXPIRE <n> [unit]]
//! KV DECR <key> [BY <n>] [EXPIRE <n> [unit]]   -- sugar for INCR BY -n
//! KV CAS  <key> EXPECT <value|NULL> SET <value> [EXPIRE <n> [unit]]
//! ```
//!
//! Key forms:
//! - Bare:   `name`          → collection = "kv_default", key = "name"
//! - Dotted: `sessions.abc`  → collection = "sessions", key = "abc"

use super::super::ast::{KvCommand, QueryExpr};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;
use crate::catalog::CollectionModel;

/// Default collection used when a bare (non-dotted) key is specified.
pub const KV_DEFAULT_COLLECTION: &str = "kv_default";

impl<'a> Parser<'a> {
    /// Parse `KV <verb> …` (called after the leading `KV` token is consumed).
    pub fn parse_kv_command(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::Kv)?;
        self.parse_keyed_command_body(CollectionModel::Kv)
    }

    /// Parse `VAULT <verb> …` (called before consuming the leading identifier).
    pub fn parse_vault_command(&mut self) -> Result<QueryExpr, ParseError> {
        if !self.consume_ident_ci("VAULT")? {
            return Err(ParseError::expected(
                vec!["VAULT"],
                self.peek(),
                self.position(),
            ));
        }
        self.parse_keyed_command_body(CollectionModel::Vault)
    }

    fn parse_keyed_command_body(
        &mut self,
        model: CollectionModel,
    ) -> Result<QueryExpr, ParseError> {
        match self.peek().clone() {
            Token::Ident(ref name) if name.eq_ignore_ascii_case("PUT") => {
                self.advance()?;
                self.parse_kv_put(model)
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("GET") => {
                self.advance()?;
                let (collection, key) = self.parse_kv_key()?;
                Ok(QueryExpr::KvCommand(KvCommand::Get {
                    model,
                    collection,
                    key,
                }))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("UNSEAL") => {
                self.advance()?;
                if model != CollectionModel::Vault {
                    return Err(ParseError::expected(
                        vec!["PUT", "GET", "DELETE", "INCR", "DECR", "CAS"],
                        self.peek(),
                        self.position(),
                    ));
                }
                let (collection, key) = self.parse_kv_key()?;
                Ok(QueryExpr::KvCommand(KvCommand::Unseal { collection, key }))
            }
            Token::Delete => {
                self.advance()?;
                let (collection, key) = self.parse_kv_key()?;
                Ok(QueryExpr::KvCommand(KvCommand::Delete {
                    model,
                    collection,
                    key,
                }))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("DELETE") => {
                self.advance()?;
                let (collection, key) = self.parse_kv_key()?;
                Ok(QueryExpr::KvCommand(KvCommand::Delete {
                    model,
                    collection,
                    key,
                }))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("INCR") => {
                self.advance()?;
                self.parse_kv_incr(model, 1)
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("DECR") => {
                self.advance()?;
                self.parse_kv_incr(model, -1)
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("CAS") => {
                self.advance()?;
                self.parse_kv_cas(model)
            }
            _ => Err(ParseError::expected(
                if model == CollectionModel::Vault {
                    vec!["PUT", "GET", "UNSEAL", "DELETE", "INCR", "DECR", "CAS"]
                } else {
                    vec!["PUT", "GET", "DELETE", "INCR", "DECR", "CAS"]
                },
                self.peek(),
                self.position(),
            )),
        }
    }

    /// Parse `UNSEAL VAULT <collection.key>`.
    pub fn parse_unseal_vault_command(&mut self) -> Result<QueryExpr, ParseError> {
        if !self.consume_ident_ci("UNSEAL")? {
            return Err(ParseError::expected(
                vec!["UNSEAL"],
                self.peek(),
                self.position(),
            ));
        }
        if !self.consume_ident_ci("VAULT")? {
            return Err(ParseError::expected(
                vec!["VAULT"],
                self.peek(),
                self.position(),
            ));
        }
        let (collection, key) = self.parse_kv_key()?;
        Ok(QueryExpr::KvCommand(KvCommand::Unseal { collection, key }))
    }

    fn parse_kv_put(&mut self, model: CollectionModel) -> Result<QueryExpr, ParseError> {
        let (collection, key) = self.parse_kv_key()?;

        // Expect `=`
        if !self.consume(&Token::Eq)? {
            return Err(ParseError::expected(
                vec!["="],
                self.peek(),
                self.position(),
            ));
        }

        let value = self.parse_value()?;

        let mut ttl_ms: Option<u64> = None;
        let mut if_not_exists = false;

        loop {
            if self.consume_ident_ci("EXPIRE")? {
                let n = self.parse_float()?;
                let unit = self.parse_kv_duration_unit()?;
                ttl_ms = Some((n * unit) as u64);
            } else if self.consume(&Token::If)? {
                // IF NOT EXISTS
                if !self.consume(&Token::Not)? && !self.consume_ident_ci("NOT")? {
                    return Err(ParseError::expected(
                        vec!["NOT"],
                        self.peek(),
                        self.position(),
                    ));
                }
                if !self.consume(&Token::Exists)? && !self.consume_ident_ci("EXISTS")? {
                    return Err(ParseError::expected(
                        vec!["EXISTS"],
                        self.peek(),
                        self.position(),
                    ));
                }
                if_not_exists = true;
            } else {
                break;
            }
        }

        Ok(QueryExpr::KvCommand(KvCommand::Put {
            model,
            collection,
            key,
            value,
            ttl_ms,
            if_not_exists,
        }))
    }

    /// Parse a key that may be bare (`name`) or dotted (`collection.key`).
    /// Returns `(collection, key)`.
    fn parse_kv_key(&mut self) -> Result<(String, String), ParseError> {
        let first = self.expect_ident()?;
        if self.consume(&Token::Dot)? {
            let key = self.expect_ident_or_keyword()?;
            Ok((first, key))
        } else {
            Ok((KV_DEFAULT_COLLECTION.to_string(), first))
        }
    }

    /// Parse `INCR/DECR key [BY n] [EXPIRE dur]`. `sign` is +1 or -1.
    fn parse_kv_incr(
        &mut self,
        model: CollectionModel,
        sign: i64,
    ) -> Result<QueryExpr, ParseError> {
        let (collection, key) = self.parse_kv_key()?;
        let mut by: i64 = sign;
        let mut ttl_ms: Option<u64> = None;

        loop {
            if self.consume(&Token::By)? || self.consume_ident_ci("BY")? {
                let n = self.parse_float()?;
                by = sign * (n.round() as i64).max(1);
            } else if self.consume_ident_ci("EXPIRE")? {
                let n = self.parse_float()?;
                let unit = self.parse_kv_duration_unit()?;
                ttl_ms = Some((n * unit) as u64);
            } else {
                break;
            }
        }

        Ok(QueryExpr::KvCommand(KvCommand::Incr {
            model,
            collection,
            key,
            by,
            ttl_ms,
        }))
    }

    /// Parse `KV CAS key EXPECT <val|NULL> SET <val> [EXPIRE dur]`.
    fn parse_kv_cas(&mut self, model: CollectionModel) -> Result<QueryExpr, ParseError> {
        let (collection, key) = self.parse_kv_key()?;

        // EXPECT <value | NULL>
        if !self.consume_ident_ci("EXPECT")? {
            return Err(ParseError::expected(
                vec!["EXPECT"],
                self.peek(),
                self.position(),
            ));
        }
        let expected = if matches!(self.peek(), Token::Null) {
            self.advance()?;
            None
        } else {
            Some(self.parse_value()?)
        };

        // SET <value>
        if !self.consume(&Token::Set)? && !self.consume_ident_ci("SET")? {
            return Err(ParseError::expected(
                vec!["SET"],
                self.peek(),
                self.position(),
            ));
        }
        let new_value = self.parse_value()?;

        // Optional EXPIRE
        let mut ttl_ms: Option<u64> = None;
        if self.consume_ident_ci("EXPIRE")? {
            let n = self.parse_float()?;
            let unit = self.parse_kv_duration_unit()?;
            ttl_ms = Some((n * unit) as u64);
        }

        Ok(QueryExpr::KvCommand(KvCommand::Cas {
            model,
            collection,
            key,
            expected,
            new_value,
            ttl_ms,
        }))
    }

    /// Duration unit multiplier to milliseconds, defaulting to seconds.
    fn parse_kv_duration_unit(&mut self) -> Result<f64, ParseError> {
        let mult = match self.peek().clone() {
            Token::Min => 60_000.0,
            Token::Ident(ref unit) => match unit.to_ascii_lowercase().as_str() {
                "ms" => 1.0,
                "s" | "sec" | "secs" => 1_000.0,
                "m" | "min" | "mins" => 60_000.0,
                "h" | "hr" | "hrs" => 3_600_000.0,
                "d" | "day" | "days" => 86_400_000.0,
                _ => return Ok(1_000.0),
            },
            _ => return Ok(1_000.0),
        };
        self.advance()?;
        Ok(mult)
    }
}
