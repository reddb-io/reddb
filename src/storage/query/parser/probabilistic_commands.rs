//! Parser for probabilistic data structure commands: HLL, SKETCH, FILTER

use super::super::ast::{ProbabilisticCommand, QueryExpr};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;

impl<'a> Parser<'a> {
    /// Parse HLL subcommand: HLL ADD|COUNT|MERGE|INFO name ...
    pub fn parse_hll_command(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume HLL

        match self.peek().clone() {
            Token::Add => {
                self.advance()?;
                let name = self.expect_ident()?;
                let mut elements = Vec::new();
                loop {
                    match self.peek() {
                        Token::String(_) => elements.push(self.parse_string()?),
                        Token::Eof | Token::Semi => break,
                        _ => break,
                    }
                }
                Ok(QueryExpr::ProbabilisticCommand(
                    ProbabilisticCommand::HllAdd { name, elements },
                ))
            }
            Token::Count => {
                self.advance()?;
                let mut names = Vec::new();
                loop {
                    match self.peek() {
                        Token::Ident(_) => names.push(self.expect_ident()?),
                        _ => break,
                    }
                }
                Ok(QueryExpr::ProbabilisticCommand(
                    ProbabilisticCommand::HllCount { names },
                ))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("MERGE") => {
                self.advance()?;
                let dest = self.expect_ident()?;
                let mut sources = Vec::new();
                loop {
                    match self.peek() {
                        Token::Ident(_) => sources.push(self.expect_ident()?),
                        _ => break,
                    }
                }
                Ok(QueryExpr::ProbabilisticCommand(
                    ProbabilisticCommand::HllMerge { dest, sources },
                ))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("INFO") => {
                self.advance()?;
                let name = self.expect_ident()?;
                Ok(QueryExpr::ProbabilisticCommand(
                    ProbabilisticCommand::HllInfo { name },
                ))
            }
            _ => Err(ParseError::expected(
                vec!["ADD", "COUNT", "MERGE", "INFO"],
                self.peek(),
                self.position(),
            )),
        }
    }

    /// Parse SKETCH subcommand: SKETCH ADD|COUNT|MERGE|INFO name ...
    pub fn parse_sketch_command(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume SKETCH

        match self.peek().clone() {
            Token::Add => {
                self.advance()?;
                let name = self.expect_ident()?;
                let element = self.parse_string()?;
                let count = if matches!(self.peek(), Token::Integer(_)) {
                    self.parse_integer()? as u64
                } else {
                    1
                };
                Ok(QueryExpr::ProbabilisticCommand(
                    ProbabilisticCommand::SketchAdd {
                        name,
                        element,
                        count,
                    },
                ))
            }
            Token::Count => {
                self.advance()?;
                let name = self.expect_ident()?;
                let element = self.parse_string()?;
                Ok(QueryExpr::ProbabilisticCommand(
                    ProbabilisticCommand::SketchCount { name, element },
                ))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("MERGE") => {
                self.advance()?;
                let dest = self.expect_ident()?;
                let mut sources = Vec::new();
                loop {
                    match self.peek() {
                        Token::Ident(_) => sources.push(self.expect_ident()?),
                        _ => break,
                    }
                }
                Ok(QueryExpr::ProbabilisticCommand(
                    ProbabilisticCommand::SketchMerge { dest, sources },
                ))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("INFO") => {
                self.advance()?;
                let name = self.expect_ident()?;
                Ok(QueryExpr::ProbabilisticCommand(
                    ProbabilisticCommand::SketchInfo { name },
                ))
            }
            _ => Err(ParseError::expected(
                vec!["ADD", "COUNT", "MERGE", "INFO"],
                self.peek(),
                self.position(),
            )),
        }
    }

    /// Parse FILTER subcommand: FILTER ADD|CHECK|DELETE|COUNT|INFO name ...
    pub fn parse_filter_command(&mut self) -> Result<QueryExpr, ParseError> {
        self.advance()?; // consume FILTER

        match self.peek().clone() {
            Token::Add => {
                self.advance()?;
                let name = self.expect_ident()?;
                let element = self.parse_string()?;
                Ok(QueryExpr::ProbabilisticCommand(
                    ProbabilisticCommand::FilterAdd { name, element },
                ))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("CHECK") => {
                self.advance()?;
                let name = self.expect_ident()?;
                let element = self.parse_string()?;
                Ok(QueryExpr::ProbabilisticCommand(
                    ProbabilisticCommand::FilterCheck { name, element },
                ))
            }
            Token::Delete => {
                self.advance()?;
                let name = self.expect_ident()?;
                let element = self.parse_string()?;
                Ok(QueryExpr::ProbabilisticCommand(
                    ProbabilisticCommand::FilterDelete { name, element },
                ))
            }
            Token::Count => {
                self.advance()?;
                let name = self.expect_ident()?;
                Ok(QueryExpr::ProbabilisticCommand(
                    ProbabilisticCommand::FilterCount { name },
                ))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("INFO") => {
                self.advance()?;
                let name = self.expect_ident()?;
                Ok(QueryExpr::ProbabilisticCommand(
                    ProbabilisticCommand::FilterInfo { name },
                ))
            }
            _ => Err(ParseError::expected(
                vec!["ADD", "CHECK", "DELETE", "COUNT", "INFO"],
                self.peek(),
                self.position(),
            )),
        }
    }

    /// Parse CREATE HLL|SKETCH|FILTER ... (called after CREATE has been consumed)
    pub fn parse_create_probabilistic(&mut self) -> Result<QueryExpr, ParseError> {
        match self.peek().clone() {
            Token::Ident(ref name) if name.eq_ignore_ascii_case("HLL") => {
                self.advance()?;
                let if_not_exists = self.match_if_not_exists()?;
                let name = self.expect_ident()?;
                Ok(QueryExpr::ProbabilisticCommand(
                    ProbabilisticCommand::CreateHll {
                        name,
                        if_not_exists,
                    },
                ))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("SKETCH") => {
                self.advance()?;
                let if_not_exists = self.match_if_not_exists()?;
                let name = self.expect_ident()?;
                // Optional WIDTH and DEPTH
                let mut width = 1000usize;
                let mut depth = 5usize;
                for _ in 0..2 {
                    if self.consume_ident_ci("WIDTH")? {
                        width = self.parse_integer()? as usize;
                    } else if self.consume_ident_ci("DEPTH")? {
                        depth = self.parse_integer()? as usize;
                    }
                }
                Ok(QueryExpr::ProbabilisticCommand(
                    ProbabilisticCommand::CreateSketch {
                        name,
                        width,
                        depth,
                        if_not_exists,
                    },
                ))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("FILTER") => {
                self.advance()?;
                let if_not_exists = self.match_if_not_exists()?;
                let name = self.expect_ident()?;
                let capacity = if self.consume_ident_ci("CAPACITY")? {
                    self.parse_integer()? as usize
                } else {
                    100_000
                };
                Ok(QueryExpr::ProbabilisticCommand(
                    ProbabilisticCommand::CreateFilter {
                        name,
                        capacity,
                        if_not_exists,
                    },
                ))
            }
            _ => Err(ParseError::expected(
                vec!["HLL", "SKETCH", "FILTER"],
                self.peek(),
                self.position(),
            )),
        }
    }

    /// Parse DROP HLL|SKETCH|FILTER ... (called after DROP has been consumed)
    pub fn parse_drop_probabilistic(&mut self) -> Result<QueryExpr, ParseError> {
        match self.peek().clone() {
            Token::Ident(ref name) if name.eq_ignore_ascii_case("HLL") => {
                self.advance()?;
                let if_exists = self.match_if_exists()?;
                let name = self.expect_ident()?;
                Ok(QueryExpr::ProbabilisticCommand(
                    ProbabilisticCommand::DropHll { name, if_exists },
                ))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("SKETCH") => {
                self.advance()?;
                let if_exists = self.match_if_exists()?;
                let name = self.expect_ident()?;
                Ok(QueryExpr::ProbabilisticCommand(
                    ProbabilisticCommand::DropSketch { name, if_exists },
                ))
            }
            Token::Ident(ref name) if name.eq_ignore_ascii_case("FILTER") => {
                self.advance()?;
                let if_exists = self.match_if_exists()?;
                let name = self.expect_ident()?;
                Ok(QueryExpr::ProbabilisticCommand(
                    ProbabilisticCommand::DropFilter { name, if_exists },
                ))
            }
            _ => Err(ParseError::expected(
                vec!["HLL", "SKETCH", "FILTER"],
                self.peek(),
                self.position(),
            )),
        }
    }
}
