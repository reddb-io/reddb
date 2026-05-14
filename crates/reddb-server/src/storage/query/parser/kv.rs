//! Parser for KV commands: `KV PUT key = value [EXPIRE n unit] [IF NOT EXISTS]`,
//! `KV GET key`, `KV DELETE key`, `KV INCR key [BY n] [EXPIRE dur]`,
//! `KV CAS key EXPECT <val|NULL> SET <val> [EXPIRE dur]`.
//!
//! Syntax summary:
//! ```text
//! KV PUT  <key> = <value> [EXPIRE <n> [unit]] [IF NOT EXISTS]
//! KV PUT  <key> = <value> [EXPIRE <n> [unit]] [TAGS [tag, ...]]
//! KV GET  <key>
//! KV DELETE <key>
//! INVALIDATE TAGS [tag, ...] FROM <collection>
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
                let (collection, key) = self.parse_kv_key(model)?;
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
                let (collection, key) = self.parse_kv_key(model)?;
                let version = self.parse_optional_vault_version()?;
                Ok(QueryExpr::KvCommand(KvCommand::Unseal {
                    collection,
                    key,
                    version,
                }))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("ROTATE") => {
                self.advance()?;
                if model != CollectionModel::Vault {
                    return Err(ParseError::expected(
                        vec!["PUT", "GET", "DELETE", "INCR", "DECR", "CAS"],
                        self.peek(),
                        self.position(),
                    ));
                }
                self.parse_vault_rotate_body()
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("HISTORY") => {
                self.advance()?;
                if model != CollectionModel::Vault {
                    return Err(ParseError::expected(
                        vec!["PUT", "GET", "DELETE", "INCR", "DECR", "CAS"],
                        self.peek(),
                        self.position(),
                    ));
                }
                let (collection, key) = self.parse_kv_key(model)?;
                Ok(QueryExpr::KvCommand(KvCommand::History { collection, key }))
            }
            Token::Purge => {
                self.advance()?;
                if model != CollectionModel::Vault {
                    return Err(ParseError::expected(
                        vec!["PUT", "GET", "DELETE", "INCR", "DECR", "CAS"],
                        self.peek(),
                        self.position(),
                    ));
                }
                let (collection, key) = self.parse_kv_key(model)?;
                Ok(QueryExpr::KvCommand(KvCommand::Purge { collection, key }))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("PURGE") => {
                self.advance()?;
                if model != CollectionModel::Vault {
                    return Err(ParseError::expected(
                        vec!["PUT", "GET", "DELETE", "INCR", "DECR", "CAS"],
                        self.peek(),
                        self.position(),
                    ));
                }
                let (collection, key) = self.parse_kv_key(model)?;
                Ok(QueryExpr::KvCommand(KvCommand::Purge { collection, key }))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("LIST") => {
                self.advance()?;
                self.parse_keyed_list(model)
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("WATCH") => {
                self.advance()?;
                self.parse_kv_watch(model)
            }
            Token::Delete => {
                self.advance()?;
                let (collection, key) = self.parse_kv_key(model)?;
                Ok(QueryExpr::KvCommand(KvCommand::Delete {
                    model,
                    collection,
                    key,
                }))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("DELETE") => {
                self.advance()?;
                let (collection, key) = self.parse_kv_key(model)?;
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
            Token::Ident(ref name) if name.eq_ignore_ascii_case("INVALIDATE") => {
                self.advance()?;
                self.parse_kv_invalidate_tags_after_invalidate()
            }
            _ => Err(ParseError::expected(
                if model == CollectionModel::Vault {
                    vec![
                        "PUT", "GET", "UNSEAL", "ROTATE", "HISTORY", "LIST", "WATCH", "DELETE",
                        "PURGE", "INCR", "DECR", "CAS",
                    ]
                } else {
                    vec![
                        "PUT",
                        "GET",
                        "WATCH",
                        "DELETE",
                        "INCR",
                        "DECR",
                        "CAS",
                        "INVALIDATE",
                    ]
                },
                self.peek(),
                self.position(),
            )),
        }
    }

    pub(crate) fn parse_vault_list_after_list(&mut self) -> Result<QueryExpr, ParseError> {
        if !self.consume_ident_ci("VAULT")? {
            return Err(ParseError::expected(
                vec!["VAULT"],
                self.peek(),
                self.position(),
            ));
        }
        self.parse_keyed_list(CollectionModel::Vault)
    }

    pub(crate) fn parse_vault_watch_after_watch(&mut self) -> Result<QueryExpr, ParseError> {
        if !self.consume_ident_ci("VAULT")? {
            return Err(ParseError::expected(
                vec!["VAULT"],
                self.peek(),
                self.position(),
            ));
        }
        self.parse_kv_watch(CollectionModel::Vault)
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
        let (collection, key) = self.parse_kv_key(CollectionModel::Vault)?;
        let version = self.parse_optional_vault_version()?;
        Ok(QueryExpr::KvCommand(KvCommand::Unseal {
            collection,
            key,
            version,
        }))
    }

    /// Parse top-level `ROTATE/HISTORY/DELETE/PURGE VAULT <collection.key>`.
    pub fn parse_vault_lifecycle_command(&mut self) -> Result<QueryExpr, ParseError> {
        let operation = if matches!(self.peek(), Token::Purge) {
            self.advance()?;
            "PURGE".to_string()
        } else {
            self.expect_ident_or_keyword()?.to_ascii_uppercase()
        };
        if !self.consume_ident_ci("VAULT")? {
            return Err(ParseError::expected(
                vec!["VAULT"],
                self.peek(),
                self.position(),
            ));
        }
        match operation.as_str() {
            "ROTATE" => self.parse_vault_rotate_body(),
            "HISTORY" => {
                let (collection, key) = self.parse_kv_key(CollectionModel::Vault)?;
                Ok(QueryExpr::KvCommand(KvCommand::History { collection, key }))
            }
            "DELETE" => {
                let (collection, key) = self.parse_kv_key(CollectionModel::Vault)?;
                Ok(QueryExpr::KvCommand(KvCommand::Delete {
                    model: CollectionModel::Vault,
                    collection,
                    key,
                }))
            }
            "PURGE" => {
                let (collection, key) = self.parse_kv_key(CollectionModel::Vault)?;
                Ok(QueryExpr::KvCommand(KvCommand::Purge { collection, key }))
            }
            _ => Err(ParseError::expected(
                vec!["ROTATE", "HISTORY", "DELETE", "PURGE"],
                self.peek(),
                self.position(),
            )),
        }
    }

    fn parse_vault_rotate_body(&mut self) -> Result<QueryExpr, ParseError> {
        let (collection, key) = self.parse_kv_key(CollectionModel::Vault)?;
        self.expect(Token::Eq)?;
        let value = self.parse_value()?;
        let tags = if self.consume_ident_ci("TAGS")? {
            self.parse_kv_tag_list()?
        } else {
            Vec::new()
        };
        Ok(QueryExpr::KvCommand(KvCommand::Rotate {
            collection,
            key,
            value,
            tags,
        }))
    }

    fn parse_optional_vault_version(&mut self) -> Result<Option<i64>, ParseError> {
        if self.consume_ident_ci("VERSION")? {
            return Ok(Some(self.parse_float()?.round() as i64));
        }
        Ok(None)
    }

    fn parse_kv_put(&mut self, model: CollectionModel) -> Result<QueryExpr, ParseError> {
        let (collection, key) = self.parse_kv_key(model)?;

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
        let mut tags: Vec<String> = Vec::new();
        let mut if_not_exists = false;

        loop {
            if self.consume_ident_ci("EXPIRE")? {
                let n = self.parse_float()?;
                let unit = self.parse_kv_duration_unit()?;
                ttl_ms = Some((n * unit) as u64);
            } else if self.consume_ident_ci("TAGS")? {
                tags = self.parse_kv_tag_list()?;
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
            tags,
            if_not_exists,
        }))
    }

    /// Parse `INVALIDATE TAGS [tag, ...] FROM collection`.
    pub(crate) fn parse_kv_invalidate_tags_after_invalidate(
        &mut self,
    ) -> Result<QueryExpr, ParseError> {
        if !self.consume_ident_ci("TAGS")? {
            return Err(ParseError::expected(
                vec!["TAGS"],
                self.peek(),
                self.position(),
            ));
        }
        let tags = self.parse_kv_tag_list()?;
        if !self.consume(&Token::From)? && !self.consume_ident_ci("FROM")? {
            return Err(ParseError::expected(
                vec!["FROM"],
                self.peek(),
                self.position(),
            ));
        }
        let collection = self.parse_keyed_collection_name()?;
        Ok(QueryExpr::KvCommand(KvCommand::InvalidateTags {
            collection,
            tags,
        }))
    }

    /// Parse a key that may be bare (`name`), dotted (`collection.key`), or
    /// colon-qualified (`collection:key`).
    /// Returns `(collection, key)`.
    pub(crate) fn parse_kv_key(
        &mut self,
        model: CollectionModel,
    ) -> Result<(String, String), ParseError> {
        let first = self.parse_kv_key_part()?;
        if self.consume(&Token::Colon)? {
            let mut segments = vec![self.parse_kv_key_part()?];
            while self.consume(&Token::Colon)? {
                segments.push(self.parse_kv_key_part()?);
            }
            return Ok((first, segments.join(":")));
        }

        if !self.consume(&Token::Dot)? {
            return Ok((KV_DEFAULT_COLLECTION.to_string(), first));
        }

        let mut segments = vec![first, self.parse_kv_key_part()?];
        while self.consume(&Token::Dot)? {
            segments.push(self.parse_kv_key_part()?);
        }

        if model == CollectionModel::Vault {
            let lower_segments: Vec<String> = segments
                .iter()
                .map(|segment| segment.to_ascii_lowercase())
                .collect();
            if lower_segments.len() >= 3
                && lower_segments[0] == "red"
                && lower_segments[1] == "vault"
            {
                return Ok(("red.vault".to_string(), lower_segments[2..].join(".")));
            }
            if lower_segments.len() >= 3
                && lower_segments[0] == "red"
                && lower_segments[1] == "secret"
            {
                return Ok(("red.vault".to_string(), lower_segments[2..].join(".")));
            }
            if lower_segments.len() >= 2 && lower_segments[0] == "secret" {
                return Ok(("red.vault".to_string(), lower_segments[1..].join(".")));
            }
        }

        Ok((segments.remove(0), segments.join(".")))
    }

    fn parse_kv_key_part(&mut self) -> Result<String, ParseError> {
        match self.peek().clone() {
            Token::String(value) => {
                self.advance()?;
                Ok(value)
            }
            Token::Ident(_) => self.expect_ident(),
            _ => self.expect_ident_or_keyword(),
        }
    }

    fn parse_keyed_list(&mut self, model: CollectionModel) -> Result<QueryExpr, ParseError> {
        let collection = self.expect_ident_or_keyword()?;
        let mut prefix = None;
        let mut limit = None;
        let mut offset = 0usize;
        loop {
            if self.consume_ident_ci("PREFIX")? {
                prefix = Some(self.expect_ident_or_keyword()?.to_string());
            } else if self.consume(&Token::Limit)? || self.consume_ident_ci("LIMIT")? {
                limit = Some(self.parse_float()?.round().max(0.0) as usize);
            } else if self.consume(&Token::Offset)? || self.consume_ident_ci("OFFSET")? {
                offset = self.parse_float()?.round().max(0.0) as usize;
            } else {
                break;
            }
        }
        Ok(QueryExpr::KvCommand(KvCommand::List {
            model,
            collection,
            prefix,
            limit,
            offset,
        }))
    }

    pub(crate) fn parse_kv_watch(
        &mut self,
        model: CollectionModel,
    ) -> Result<QueryExpr, ParseError> {
        let first = self.expect_ident()?;
        let (collection, key, prefix) = if model != CollectionModel::Kv {
            let mut collection = first;
            if self.consume(&Token::Dot)? {
                let next = self.expect_ident_or_keyword()?;
                collection = format!("{collection}.{next}");
            }
            if self.consume_ident_ci("PREFIX")? {
                (collection, self.expect_ident_or_keyword()?, true)
            } else {
                (collection, self.expect_ident_or_keyword()?, false)
            }
        } else if self.consume(&Token::Dot)? {
            if self.consume(&Token::Star)? {
                (KV_DEFAULT_COLLECTION.to_string(), first, true)
            } else {
                let key = self.expect_ident_or_keyword()?;
                if self.consume(&Token::Dot)? {
                    self.expect(Token::Star)?;
                    (first, key, true)
                } else {
                    (first, key, false)
                }
            }
        } else {
            (KV_DEFAULT_COLLECTION.to_string(), first, false)
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

        Ok(QueryExpr::KvCommand(KvCommand::Watch {
            model,
            collection,
            key,
            prefix,
            from_lsn,
        }))
    }

    fn parse_keyed_collection_name(&mut self) -> Result<String, ParseError> {
        let mut collection = self.expect_ident_or_keyword()?;
        if self.consume(&Token::Dot)? {
            let next = self.expect_ident_or_keyword()?;
            collection = format!("{collection}.{next}");
        }
        Ok(collection)
    }

    /// Parse `INCR/DECR key [BY n] [EXPIRE dur]`. `sign` is +1 or -1.
    fn parse_kv_incr(
        &mut self,
        model: CollectionModel,
        sign: i64,
    ) -> Result<QueryExpr, ParseError> {
        let (collection, key) = self.parse_kv_key(model)?;
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

    pub(crate) fn parse_kv_tag_list(&mut self) -> Result<Vec<String>, ParseError> {
        self.expect(Token::LBracket)?;
        let mut tags = Vec::new();
        while !self.check(&Token::RBracket) {
            let tag = self.parse_kv_tag()?;
            if !tag.is_empty() {
                tags.push(tag);
            }
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        self.expect(Token::RBracket)?;
        Ok(tags)
    }

    fn parse_kv_tag(&mut self) -> Result<String, ParseError> {
        let mut tag = String::new();
        loop {
            match self.peek().clone() {
                Token::Comma | Token::RBracket | Token::Eof => break,
                Token::Ident(part) | Token::String(part) => {
                    self.advance()?;
                    tag.push_str(&part);
                }
                Token::Integer(n) => {
                    self.advance()?;
                    tag.push_str(&n.to_string());
                }
                Token::Float(n) => {
                    self.advance()?;
                    tag.push_str(&n.to_string());
                }
                Token::Colon => {
                    self.advance()?;
                    tag.push(':');
                }
                Token::Dot => {
                    self.advance()?;
                    tag.push('.');
                }
                Token::Dash => {
                    self.advance()?;
                    tag.push('-');
                }
                other => {
                    return Err(ParseError::expected(vec!["tag"], &other, self.position()));
                }
            }
        }
        Ok(tag)
    }

    /// Parse `KV CAS key EXPECT <val|NULL> SET <val> [EXPIRE dur]`.
    fn parse_kv_cas(&mut self, model: CollectionModel) -> Result<QueryExpr, ParseError> {
        let (collection, key) = self.parse_kv_key(model)?;

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
