//! Parser for QUEUE commands and CREATE/DROP QUEUE

use super::super::ast::{
    AlterQueueQuery, CreateQueueQuery, DropQueueQuery, QueryExpr, QueueCommand, QueueMode,
    QueueSide, DEFAULT_QUEUE_IN_FLIGHT_CAP_PER_GROUP, DEFAULT_QUEUE_LOCK_DEADLINE_MS,
    DEFAULT_QUEUE_MAX_ATTEMPTS,
};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;

impl<'a> Parser<'a> {
    /// Parse CREATE QUEUE body (after CREATE QUEUE consumed)
    pub fn parse_create_queue_body(&mut self) -> Result<QueryExpr, ParseError> {
        let if_not_exists = self.match_if_not_exists()?;
        let name = self.expect_ident()?;

        let mut mode = QueueMode::Work;
        let mut priority = false;
        let mut max_size = None;
        let mut ttl_ms = None;
        let mut dlq = None;
        let mut max_attempts = DEFAULT_QUEUE_MAX_ATTEMPTS;
        let mut lock_deadline_ms = DEFAULT_QUEUE_LOCK_DEADLINE_MS;
        let mut in_flight_cap_per_group = DEFAULT_QUEUE_IN_FLIGHT_CAP_PER_GROUP;

        // Parse optional clauses in any order
        loop {
            if let Some(parsed_mode) = self.consume_queue_mode()? {
                mode = parsed_mode;
            } else if self.consume(&Token::Priority)? {
                priority = true;
            } else if self.consume_ident_ci("MAX_SIZE")? || self.consume_ident_ci("MAXSIZE")? {
                max_size = Some(self.parse_positive_integer("MAX_SIZE")? as usize);
            } else if self.consume_ident_ci("MAX_ATTEMPTS")?
                || self.consume_ident_ci("MAXATTEMPTS")?
            {
                max_attempts = self.parse_integer()?.max(1) as u32;
            } else if self.consume_ident_ci("LOCK_DEADLINE_MS")? {
                lock_deadline_ms = self.parse_integer()?.max(1) as u64;
            } else if self.consume_ident_ci("IN_FLIGHT_CAP_PER_GROUP")? {
                in_flight_cap_per_group = self.parse_integer()?.max(1) as u32;
            } else if self.consume(&Token::With)? {
                if self.consume_ident_ci("EVENTS")? {
                    return Err(ParseError::new(
                        "queues cannot have event subscriptions".to_string(),
                        self.position(),
                    ));
                } else if self.consume_ident_ci("TTL")? {
                    let value = self.parse_float()?;
                    let unit = self.parse_queue_duration_unit()?;
                    ttl_ms = Some((value * unit) as u64);
                } else if self.consume_ident_ci("DLQ")? {
                    dlq = Some(self.expect_ident()?);
                }
            } else {
                break;
            }
        }

        Ok(QueryExpr::CreateQueue(CreateQueueQuery {
            name,
            mode,
            priority,
            max_size,
            ttl_ms,
            dlq,
            max_attempts,
            lock_deadline_ms,
            in_flight_cap_per_group,
            if_not_exists,
        }))
    }

    /// Parse ALTER QUEUE body (after ALTER QUEUE consumed)
    ///
    /// Syntax: `ALTER QUEUE <name> SET <clause>` where `<clause>` is one of
    ///   `MODE <WORK|FANOUT>`
    ///   `MAX_ATTEMPTS <int>`
    ///   `LOCK_DEADLINE_MS <int>`
    ///   `IN_FLIGHT_CAP_PER_GROUP <int>`
    ///   `DLQ <ident>`
    pub fn parse_alter_queue_body(&mut self) -> Result<QueryExpr, ParseError> {
        let name = self.expect_ident()?;
        if !self.consume(&Token::Set)? && !self.consume_ident_ci("SET")? {
            return Err(ParseError::expected(
                vec!["SET"],
                self.peek(),
                self.position(),
            ));
        }

        let mut alter = AlterQueueQuery {
            name,
            ..Default::default()
        };

        if self.consume(&Token::Mode)? || self.consume_ident_ci("MODE")? {
            alter.mode = Some(self.parse_queue_mode()?);
        } else if self.consume_ident_ci("MAX_ATTEMPTS")?
            || self.consume_ident_ci("MAXATTEMPTS")?
        {
            alter.max_attempts = Some(self.parse_integer()?.max(1) as u32);
        } else if self.consume_ident_ci("LOCK_DEADLINE_MS")? {
            alter.lock_deadline_ms = Some(self.parse_integer()?.max(1) as u64);
        } else if self.consume_ident_ci("IN_FLIGHT_CAP_PER_GROUP")? {
            alter.in_flight_cap_per_group = Some(self.parse_integer()?.max(1) as u32);
        } else if self.consume_ident_ci("DLQ")? {
            alter.dlq = Some(self.expect_ident()?);
        } else {
            return Err(ParseError::expected(
                vec![
                    "MODE",
                    "MAX_ATTEMPTS",
                    "LOCK_DEADLINE_MS",
                    "IN_FLIGHT_CAP_PER_GROUP",
                    "DLQ",
                ],
                self.peek(),
                self.position(),
            ));
        }

        Ok(QueryExpr::AlterQueue(alter))
    }

    /// Parse DROP QUEUE body (after DROP QUEUE consumed)
    pub fn parse_drop_queue_body(&mut self) -> Result<QueryExpr, ParseError> {
        let if_exists = self.match_if_exists()?;
        let name = self.parse_drop_collection_name()?;
        Ok(QueryExpr::DropQueue(DropQueueQuery { name, if_exists }))
    }

    /// Parse QUEUE subcommand (after QUEUE token consumed)
    pub fn parse_queue_command(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect(Token::Queue)?;

        match self.peek().clone() {
            Token::Push => {
                self.advance()?;
                let queue = self.expect_ident()?;
                let value = self.parse_value()?;
                let priority = if self.consume(&Token::Priority)? {
                    Some(self.parse_integer()? as i32)
                } else {
                    None
                };
                Ok(QueryExpr::QueueCommand(QueueCommand::Push {
                    queue,
                    value,
                    side: QueueSide::Right,
                    priority,
                }))
            }
            Token::Pop => {
                self.advance()?;
                let queue = self.expect_ident()?;
                let count = if self.consume(&Token::Count)? {
                    self.parse_integer()? as usize
                } else {
                    1
                };
                Ok(QueryExpr::QueueCommand(QueueCommand::Pop {
                    queue,
                    side: QueueSide::Left,
                    count,
                }))
            }
            Token::Peek => {
                self.advance()?;
                let queue = self.expect_ident()?;
                let count = if self.consume(&Token::Count)? {
                    self.parse_integer()? as usize
                } else if matches!(self.peek(), Token::Integer(_)) {
                    self.parse_integer()? as usize
                } else {
                    1
                };
                Ok(QueryExpr::QueueCommand(QueueCommand::Peek { queue, count }))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("LEN") => {
                self.advance()?;
                let queue = self.expect_ident()?;
                Ok(QueryExpr::QueueCommand(QueueCommand::Len { queue }))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("MOVE") => {
                self.advance()?;
                self.expect(Token::From)?;
                let source = self.expect_ident()?;
                self.expect(Token::To)?;
                let destination = self.expect_ident()?;
                let filter = if self.consume(&Token::Where)? {
                    Some(self.parse_filter()?)
                } else {
                    None
                };
                let limit = if self.consume(&Token::Limit)? {
                    self.parse_positive_integer("LIMIT")? as usize
                } else if filter.is_some() {
                    return Err(ParseError::expected(
                        vec!["LIMIT"],
                        self.peek(),
                        self.position(),
                    ));
                } else {
                    1
                };
                Ok(QueryExpr::QueueCommand(QueueCommand::Move {
                    source,
                    destination,
                    filter,
                    limit,
                }))
            }
            Token::Purge => {
                self.advance()?;
                let queue = self.expect_ident()?;
                Ok(QueryExpr::QueueCommand(QueueCommand::Purge { queue }))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("LPOP") => {
                self.advance()?;
                let queue = self.expect_ident()?;
                Ok(QueryExpr::QueueCommand(QueueCommand::Pop {
                    queue,
                    side: QueueSide::Left,
                    count: 1,
                }))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("RPOP") => {
                self.advance()?;
                let queue = self.expect_ident()?;
                Ok(QueryExpr::QueueCommand(QueueCommand::Pop {
                    queue,
                    side: QueueSide::Right,
                    count: 1,
                }))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("LPUSH") => {
                self.advance()?;
                let queue = self.expect_ident()?;
                let value = self.parse_value()?;
                Ok(QueryExpr::QueueCommand(QueueCommand::Push {
                    queue,
                    value,
                    side: QueueSide::Left,
                    priority: None,
                }))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("RPUSH") => {
                self.advance()?;
                let queue = self.expect_ident()?;
                let value = self.parse_value()?;
                let priority = if self.consume(&Token::Priority)? {
                    Some(self.parse_integer()? as i32)
                } else {
                    None
                };
                Ok(QueryExpr::QueueCommand(QueueCommand::Push {
                    queue,
                    value,
                    side: QueueSide::Right,
                    priority,
                }))
            }
            Token::Group => {
                self.advance()?;
                self.expect(Token::Create)?;
                let queue = self.expect_ident()?;
                let group = self.expect_ident()?;
                Ok(QueryExpr::QueueCommand(QueueCommand::GroupCreate {
                    queue,
                    group,
                }))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("READ") => {
                self.advance()?;
                let queue = self.expect_ident()?;
                let group = if self.consume(&Token::Group)? {
                    Some(self.expect_ident()?)
                } else {
                    None
                };
                // CONSUMER consumer_name
                if !self.consume_ident_ci("CONSUMER")? {
                    return Err(ParseError::expected(
                        vec!["CONSUMER"],
                        self.peek(),
                        self.position(),
                    ));
                }
                let consumer = self.expect_ident()?;
                let count = if self.consume(&Token::Count)? {
                    self.parse_integer()? as usize
                } else {
                    1
                };
                Ok(QueryExpr::QueueCommand(QueueCommand::GroupRead {
                    queue,
                    group,
                    consumer,
                    count,
                }))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("PENDING") => {
                self.advance()?;
                let queue = self.expect_ident()?;
                self.expect(Token::Group)?;
                let group = self.expect_ident()?;
                Ok(QueryExpr::QueueCommand(QueueCommand::Pending {
                    queue,
                    group,
                }))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("CLAIM") => {
                self.advance()?;
                let queue = self.expect_ident()?;
                self.expect(Token::Group)?;
                let group = self.expect_ident()?;
                if !self.consume_ident_ci("CONSUMER")? {
                    return Err(ParseError::expected(
                        vec!["CONSUMER"],
                        self.peek(),
                        self.position(),
                    ));
                }
                let consumer = self.expect_ident()?;
                if !self.consume_ident_ci("MIN_IDLE")? {
                    return Err(ParseError::expected(
                        vec!["MIN_IDLE"],
                        self.peek(),
                        self.position(),
                    ));
                }
                let min_idle_ms = self.parse_integer()?.max(0) as u64;
                Ok(QueryExpr::QueueCommand(QueueCommand::Claim {
                    queue,
                    group,
                    consumer,
                    min_idle_ms,
                }))
            }
            Token::Ack => {
                self.advance()?;
                let queue = self.expect_ident()?;
                self.expect(Token::Group)?;
                let group = self.expect_ident()?;
                let message_id = self.parse_string()?;
                Ok(QueryExpr::QueueCommand(QueueCommand::Ack {
                    queue,
                    group,
                    message_id,
                }))
            }
            Token::Nack => {
                self.advance()?;
                let queue = self.expect_ident()?;
                self.expect(Token::Group)?;
                let group = self.expect_ident()?;
                let message_id = self.parse_string()?;
                Ok(QueryExpr::QueueCommand(QueueCommand::Nack {
                    queue,
                    group,
                    message_id,
                }))
            }
            _ => Err(ParseError::expected(
                vec![
                    "PUSH", "POP", "PEEK", "LEN", "PURGE", "GROUP", "READ", "ACK", "NACK", "LPUSH",
                    "RPUSH", "LPOP", "RPOP", "PENDING", "CLAIM", "MOVE",
                ],
                self.peek(),
                self.position(),
            )),
        }
    }

    fn consume_queue_mode(&mut self) -> Result<Option<QueueMode>, ParseError> {
        match self.peek() {
            Token::Work => {
                self.advance()?;
                Ok(Some(QueueMode::Work))
            }
            Token::Ident(name) => {
                if let Some(mode) = QueueMode::parse(name) {
                    self.advance()?;
                    Ok(Some(mode))
                } else {
                    Ok(None)
                }
            }
            _ => Ok(None),
        }
    }

    fn parse_queue_mode(&mut self) -> Result<QueueMode, ParseError> {
        match self.consume_queue_mode()? {
            Some(mode) => Ok(mode),
            None => Err(ParseError::expected(
                vec!["FANOUT", "WORK"],
                self.peek(),
                self.position(),
            )),
        }
    }

    /// Parse duration unit for queue TTL
    fn parse_queue_duration_unit(&mut self) -> Result<f64, ParseError> {
        match self.peek().clone() {
            Token::Ident(ref unit) => {
                let mult = match unit.to_ascii_lowercase().as_str() {
                    "ms" => 1.0,
                    "s" | "sec" | "secs" => 1_000.0,
                    "m" | "min" | "mins" => 60_000.0,
                    "h" | "hr" | "hrs" => 3_600_000.0,
                    "d" | "day" | "days" => 86_400_000.0,
                    _ => return Ok(1_000.0),
                };
                self.advance()?;
                Ok(mult)
            }
            _ => Ok(1_000.0),
        }
    }
}
