//! Persisted RQL function definitions; execution remains in the server crate.
use crate::ast::{Expr, QueryExpr};
use crate::lexer::Token;
use crate::parser::{ParseError, Parser, ParserLimits};
use reddb_types::SqlTypeName;

pub const FUNCTION_SOURCE_BYTES_MAX: usize = 65_536;
pub const FUNCTION_PARAMETERS_MAX: usize = 32;
pub const FUNCTION_STATEMENTS_MAX: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FunctionEffect {
    Pure,
    Read,
    Write,
}

#[derive(Debug, Clone)]
pub struct FunctionParameter {
    pub name: String,
    pub sql_type: SqlTypeName,
}

#[derive(Debug, Clone)]
pub enum FunctionReturn {
    Scalar(SqlTypeName),
    Table(Vec<FunctionParameter>),
}

#[derive(Debug, Clone)]
pub struct FunctionDefinition {
    pub name: String,
    pub parameters: Vec<FunctionParameter>,
    pub returns: FunctionReturn,
    pub effect: FunctionEffect,
    pub body: String,
}

#[derive(Debug, Clone)]
pub enum FunctionCommand {
    Create {
        definition: FunctionDefinition,
        replace: bool,
    },
    Drop {
        name: String,
        if_exists: bool,
    },
    Call {
        name: String,
        arguments: Vec<Expr>,
    },
    Show {
        name: Option<String>,
    },
}

impl<'a> Parser<'a> {
    pub(crate) fn starts_function_command(&mut self) -> Result<bool, ParseError> {
        let starts = match self.peek() {
            Token::Create | Token::Alter | Token::Drop => {
                matches!(self.peek_next()?, Token::Ident(word) if word.eq_ignore_ascii_case("FUNCTION"))
            }
            Token::Ident(word) if word.eq_ignore_ascii_case("CALL") => true,
            Token::Ident(word) if word.eq_ignore_ascii_case("SHOW") => {
                matches!(self.peek_next()?, Token::Ident(word) if word.eq_ignore_ascii_case("FUNCTION") || word.eq_ignore_ascii_case("FUNCTIONS"))
            }
            _ => false,
        };
        Ok(starts)
    }

    pub(crate) fn parse_function_command(&mut self) -> Result<FunctionCommand, ParseError> {
        let head = self.peek().clone();
        self.advance()?;
        match head {
            Token::Create | Token::Alter => {
                self.function_keyword("FUNCTION")?;
                let name = self.parse_function_name()?;
                let parameters = self.parse_function_parameters()?;
                self.function_keyword("RETURNS")?;
                let returns = if self.consume(&Token::Table)? {
                    FunctionReturn::Table(self.parse_function_parameters()?)
                } else {
                    FunctionReturn::Scalar(self.parse_column_type()?)
                };
                self.function_keyword("EFFECT")?;
                let effect = if self.consume_ident_ci("PURE")? {
                    FunctionEffect::Pure
                } else if self.consume_ident_ci("READ")? {
                    FunctionEffect::Read
                } else if self.consume_ident_ci("WRITE")? {
                    FunctionEffect::Write
                } else {
                    return Err(ParseError::new(
                        "expected PURE, READ or WRITE effect",
                        self.position(),
                    ));
                };
                self.expect(Token::As)?;
                let body = self.parse_string()?;
                if body.len() > FUNCTION_SOURCE_BYTES_MAX || body.trim().is_empty() {
                    return Err(ParseError::new(
                        "function body must contain 1..65536 bytes",
                        self.position(),
                    ));
                }
                Ok(FunctionCommand::Create {
                    definition: FunctionDefinition {
                        name,
                        parameters,
                        returns,
                        effect,
                        body,
                    },
                    replace: matches!(head, Token::Alter),
                })
            }
            Token::Drop => {
                self.function_keyword("FUNCTION")?;
                let if_exists = self.match_if_exists()?;
                Ok(FunctionCommand::Drop {
                    name: self.parse_function_name()?,
                    if_exists,
                })
            }
            Token::Ident(word) if word.eq_ignore_ascii_case("CALL") => {
                let name = self.parse_function_name()?;
                self.expect(Token::LParen)?;
                let mut arguments = Vec::new();
                if !self.check(&Token::RParen) {
                    loop {
                        if arguments.len() == FUNCTION_PARAMETERS_MAX {
                            return Err(ParseError::new(
                                "at most 32 function arguments are supported",
                                self.position(),
                            ));
                        }
                        arguments.push(self.parse_expr()?);
                        if !self.consume(&Token::Comma)? {
                            break;
                        }
                    }
                }
                self.expect(Token::RParen)?;
                Ok(FunctionCommand::Call { name, arguments })
            }
            _ => {
                let name = if self.consume_ident_ci("FUNCTIONS")? {
                    None
                } else {
                    self.function_keyword("FUNCTION")?;
                    Some(self.parse_function_name()?)
                };
                Ok(FunctionCommand::Show { name })
            }
        }
    }

    fn parse_function_name(&mut self) -> Result<String, ParseError> {
        let name = self.expect_ident()?;
        if self.consume(&Token::Dot)? {
            Ok(format!("{name}.{}", self.expect_ident()?))
        } else {
            Ok(name)
        }
    }

    fn function_keyword(&mut self, keyword: &'static str) -> Result<(), ParseError> {
        if self.consume_ident_ci(keyword)? {
            return Ok(());
        }
        Err(ParseError::expected(
            vec![keyword],
            self.peek(),
            self.position(),
        ))
    }

    fn parse_function_parameters(&mut self) -> Result<Vec<FunctionParameter>, ParseError> {
        self.expect(Token::LParen)?;
        let mut parameters: Vec<FunctionParameter> = Vec::new();
        if !self.check(&Token::RParen) {
            loop {
                if parameters.len() == FUNCTION_PARAMETERS_MAX {
                    return Err(ParseError::new(
                        "at most 32 function parameters are supported",
                        self.position(),
                    ));
                }
                let name = self.expect_ident()?;
                if parameters.iter().any(|parameter| parameter.name == name) {
                    return Err(ParseError::new(
                        "duplicate function parameter name",
                        self.position(),
                    ));
                }
                parameters.push(FunctionParameter {
                    name,
                    sql_type: self.parse_column_type()?,
                });
                if !self.consume(&Token::Comma)? {
                    break;
                }
            }
        }
        self.expect(Token::RParen)?;
        Ok(parameters)
    }
}

/// Token-aware splitting preserves semicolons inside strings and comments.
/// Statements are parsed separately so parameters remain numbered per call.
pub fn parse_function_body(body: &str) -> Result<Vec<(String, QueryExpr)>, ParseError> {
    let limits = ParserLimits {
        max_input_bytes: FUNCTION_SOURCE_BYTES_MAX,
        max_tokens: 1024,
        ..ParserLimits::default()
    };
    let mut parser = Parser::with_limits(body, limits)?;
    let mut statements = Vec::new();
    while !parser.check(&Token::Eof) {
        if statements.len() == FUNCTION_STATEMENTS_MAX {
            return Err(ParseError::new(
                "at most 32 function statements are supported",
                parser.position(),
            ));
        }
        let start = parser.position().offset;
        // A closed set prevents nested function declarations from recursively
        // re-entering the function compiler with a fresh parser depth budget.
        if !matches!(
            parser.peek(),
            Token::Select
                | Token::From
                | Token::Insert
                | Token::Update
                | Token::Delete
                | Token::Match
                | Token::Vector
                | Token::Hybrid
                | Token::Queue
                | Token::Kv
                | Token::Search
        ) && !matches!(parser.peek(), Token::Ident(word) if word.eq_ignore_ascii_case("QUEUE") || word.eq_ignore_ascii_case("KV") || word.eq_ignore_ascii_case("SEARCH"))
        {
            return Err(ParseError::new("function bodies allow data queries and mutations only; DDL, transaction control and nested CALL are unsupported", parser.position()));
        }
        let query = parser.parse_query_expr()?;
        let end = parser.position().offset;
        statements.push((body[start as usize..end as usize].trim().to_string(), query));
        if !parser.consume(&Token::Semi)? && !parser.check(&Token::Eof) {
            return Err(ParseError::new(
                "expected semicolon between function statements",
                parser.position(),
            ));
        }
    }
    if statements.is_empty() {
        return Err(ParseError::new(
            "function body must not be empty",
            parser.position(),
        ));
    }
    Ok(statements)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_definitions_calls_and_lifecycle() {
        for sql in [
            "CREATE FUNCTION total(price INTEGER, quantity INTEGER) RETURNS INTEGER EFFECT PURE AS 'SELECT $1 * $2'",
            "ALTER FUNCTION orders_for(owner INTEGER) RETURNS TABLE (id INTEGER) EFFECT READ AS 'SELECT id FROM orders WHERE owner = $1'",
            "CALL total($1, 3)", "SHOW FUNCTIONS", "SHOW FUNCTION total", "DROP FUNCTION IF EXISTS total",
        ] {
            assert!(matches!(crate::parser::parse(sql).expect(sql).query, QueryExpr::Function(_)));
        }
    }

    #[test]
    fn body_split_preserves_strings_and_rejects_control_and_nesting() {
        let statements = parse_function_body("SELECT 'a;b'; SELECT 2").expect("two statements");
        assert_eq!(statements.len(), 2);
        assert_eq!(statements[0].0, "SELECT 'a;b'");
        for source in [
            "BEGIN",
            "CALL recurse()",
            "SET TENANT 'other'",
            "CREATE FUNCTION nested()",
            "SELECT 1 SELECT 2",
        ] {
            assert!(parse_function_body(source).is_err(), "{source}");
        }
        assert!(parse_function_body(&"SELECT 1;".repeat(33)).is_err());
    }
}
