use crate::storage::query::ast::{
    AlterTableQuery, CreateIndexQuery, CreateQueueQuery, CreateTableQuery, CreateTimeSeriesQuery,
    DeleteQuery, DropIndexQuery, DropQueueQuery, DropTableQuery, DropTimeSeriesQuery,
    ExplainAlterQuery, InsertQuery, JoinQuery, ProbabilisticCommand, QueryExpr, TableQuery,
    UpdateQuery,
};
use crate::storage::query::parser::{ParseError, Parser};
use crate::storage::query::Token;
use crate::storage::schema::Value;

/// Canonical SQL frontend command surface.
///
/// This is the single entrypoint for SQL/RQL-style commands before they are
/// lowered into the broader multi-backend `QueryExpr` space.
#[derive(Debug, Clone)]
pub enum SqlCommand {
    Select(TableQuery),
    Join(JoinQuery),
    Insert(InsertQuery),
    Update(UpdateQuery),
    Delete(DeleteQuery),
    ExplainAlter(ExplainAlterQuery),
    CreateTable(CreateTableQuery),
    DropTable(DropTableQuery),
    AlterTable(AlterTableQuery),
    CreateIndex(CreateIndexQuery),
    DropIndex(DropIndexQuery),
    CreateTimeSeries(CreateTimeSeriesQuery),
    DropTimeSeries(DropTimeSeriesQuery),
    CreateQueue(CreateQueueQuery),
    DropQueue(DropQueueQuery),
    Probabilistic(ProbabilisticCommand),
    SetConfig { key: String, value: Value },
    ShowConfig { prefix: Option<String> },
}

impl SqlCommand {
    pub fn into_query_expr(self) -> QueryExpr {
        match self {
            SqlCommand::Select(query) => QueryExpr::Table(query),
            SqlCommand::Join(query) => QueryExpr::Join(query),
            SqlCommand::Insert(query) => QueryExpr::Insert(query),
            SqlCommand::Update(query) => QueryExpr::Update(query),
            SqlCommand::Delete(query) => QueryExpr::Delete(query),
            SqlCommand::ExplainAlter(query) => QueryExpr::ExplainAlter(query),
            SqlCommand::CreateTable(query) => QueryExpr::CreateTable(query),
            SqlCommand::DropTable(query) => QueryExpr::DropTable(query),
            SqlCommand::AlterTable(query) => QueryExpr::AlterTable(query),
            SqlCommand::CreateIndex(query) => QueryExpr::CreateIndex(query),
            SqlCommand::DropIndex(query) => QueryExpr::DropIndex(query),
            SqlCommand::CreateTimeSeries(query) => QueryExpr::CreateTimeSeries(query),
            SqlCommand::DropTimeSeries(query) => QueryExpr::DropTimeSeries(query),
            SqlCommand::CreateQueue(query) => QueryExpr::CreateQueue(query),
            SqlCommand::DropQueue(query) => QueryExpr::DropQueue(query),
            SqlCommand::Probabilistic(command) => QueryExpr::ProbabilisticCommand(command),
            SqlCommand::SetConfig { key, value } => QueryExpr::SetConfig { key, value },
            SqlCommand::ShowConfig { prefix } => QueryExpr::ShowConfig { prefix },
        }
    }
}

impl<'a> Parser<'a> {
    /// Parse any SQL/RQL-style command through a single frontend module.
    pub fn parse_sql_command(&mut self) -> Result<SqlCommand, ParseError> {
        match self.peek() {
            Token::Select => match self.parse_select_query()? {
                QueryExpr::Table(query) => Ok(SqlCommand::Select(query)),
                other => Err(ParseError::new(
                    format!("internal: SELECT produced unexpected query kind {other:?}"),
                    self.position(),
                )),
            },
            Token::From => match self.parse_from_query()? {
                QueryExpr::Table(query) => Ok(SqlCommand::Select(query)),
                QueryExpr::Join(query) => Ok(SqlCommand::Join(query)),
                other => Err(ParseError::new(
                    format!("internal: FROM produced unexpected query kind {other:?}"),
                    self.position(),
                )),
            },
            Token::Insert => match self.parse_insert_query()? {
                QueryExpr::Insert(query) => Ok(SqlCommand::Insert(query)),
                other => Err(ParseError::new(
                    format!("internal: INSERT produced unexpected query kind {other:?}"),
                    self.position(),
                )),
            },
            Token::Update => match self.parse_update_query()? {
                QueryExpr::Update(query) => Ok(SqlCommand::Update(query)),
                other => Err(ParseError::new(
                    format!("internal: UPDATE produced unexpected query kind {other:?}"),
                    self.position(),
                )),
            },
            Token::Delete => match self.parse_delete_query()? {
                QueryExpr::Delete(query) => Ok(SqlCommand::Delete(query)),
                other => Err(ParseError::new(
                    format!("internal: DELETE produced unexpected query kind {other:?}"),
                    self.position(),
                )),
            },
            Token::Explain => match self.parse_explain_alter_query()? {
                QueryExpr::ExplainAlter(query) => Ok(SqlCommand::ExplainAlter(query)),
                other => Err(ParseError::new(
                    format!("internal: EXPLAIN produced unexpected query kind {other:?}"),
                    self.position(),
                )),
            },
            Token::Create => {
                let pos = self.position();
                self.advance()?;
                if self.check(&Token::Index) || self.check(&Token::Unique) {
                    match self.parse_create_index_query()? {
                        QueryExpr::CreateIndex(query) => Ok(SqlCommand::CreateIndex(query)),
                        other => Err(ParseError::new(
                            format!("internal: CREATE INDEX produced unexpected kind {other:?}"),
                            self.position(),
                        )),
                    }
                } else if self.check(&Token::Table) {
                    self.expect(Token::Table)?;
                    match self.parse_create_table_body()? {
                        QueryExpr::CreateTable(query) => Ok(SqlCommand::CreateTable(query)),
                        other => Err(ParseError::new(
                            format!("internal: CREATE TABLE produced unexpected kind {other:?}"),
                            self.position(),
                        )),
                    }
                } else if self.check(&Token::Timeseries) {
                    self.advance()?;
                    match self.parse_create_timeseries_body()? {
                        QueryExpr::CreateTimeSeries(query) => {
                            Ok(SqlCommand::CreateTimeSeries(query))
                        }
                        other => Err(ParseError::new(
                            format!(
                                "internal: CREATE TIMESERIES produced unexpected kind {other:?}"
                            ),
                            self.position(),
                        )),
                    }
                } else if self.check(&Token::Queue) {
                    self.advance()?;
                    match self.parse_create_queue_body()? {
                        QueryExpr::CreateQueue(query) => Ok(SqlCommand::CreateQueue(query)),
                        other => Err(ParseError::new(
                            format!("internal: CREATE QUEUE produced unexpected kind {other:?}"),
                            self.position(),
                        )),
                    }
                } else if matches!(self.peek(), Token::Ident(n) if
                    n.eq_ignore_ascii_case("HLL") ||
                    n.eq_ignore_ascii_case("SKETCH") ||
                    n.eq_ignore_ascii_case("FILTER"))
                {
                    match self.parse_create_probabilistic()? {
                        QueryExpr::ProbabilisticCommand(command) => {
                            Ok(SqlCommand::Probabilistic(command))
                        }
                        other => Err(ParseError::new(
                            format!(
                                "internal: CREATE probabilistic produced unexpected kind {other:?}"
                            ),
                            self.position(),
                        )),
                    }
                } else {
                    Err(ParseError::expected(
                        vec![
                            "TABLE",
                            "INDEX",
                            "UNIQUE",
                            "TIMESERIES",
                            "QUEUE",
                            "HLL",
                            "SKETCH",
                            "FILTER",
                        ],
                        self.peek(),
                        pos,
                    ))
                }
            }
            Token::Drop => {
                let pos = self.position();
                self.advance()?;
                if self.check(&Token::Index) {
                    match self.parse_drop_index_query()? {
                        QueryExpr::DropIndex(query) => Ok(SqlCommand::DropIndex(query)),
                        other => Err(ParseError::new(
                            format!("internal: DROP INDEX produced unexpected kind {other:?}"),
                            self.position(),
                        )),
                    }
                } else if self.check(&Token::Table) {
                    self.expect(Token::Table)?;
                    match self.parse_drop_table_body()? {
                        QueryExpr::DropTable(query) => Ok(SqlCommand::DropTable(query)),
                        other => Err(ParseError::new(
                            format!("internal: DROP TABLE produced unexpected kind {other:?}"),
                            self.position(),
                        )),
                    }
                } else if self.check(&Token::Timeseries) {
                    self.advance()?;
                    match self.parse_drop_timeseries_body()? {
                        QueryExpr::DropTimeSeries(query) => Ok(SqlCommand::DropTimeSeries(query)),
                        other => Err(ParseError::new(
                            format!("internal: DROP TIMESERIES produced unexpected kind {other:?}"),
                            self.position(),
                        )),
                    }
                } else if self.check(&Token::Queue) {
                    self.advance()?;
                    match self.parse_drop_queue_body()? {
                        QueryExpr::DropQueue(query) => Ok(SqlCommand::DropQueue(query)),
                        other => Err(ParseError::new(
                            format!("internal: DROP QUEUE produced unexpected kind {other:?}"),
                            self.position(),
                        )),
                    }
                } else if matches!(self.peek(), Token::Ident(n) if
                    n.eq_ignore_ascii_case("HLL") ||
                    n.eq_ignore_ascii_case("SKETCH") ||
                    n.eq_ignore_ascii_case("FILTER"))
                {
                    match self.parse_drop_probabilistic()? {
                        QueryExpr::ProbabilisticCommand(command) => {
                            Ok(SqlCommand::Probabilistic(command))
                        }
                        other => Err(ParseError::new(
                            format!(
                                "internal: DROP probabilistic produced unexpected kind {other:?}"
                            ),
                            self.position(),
                        )),
                    }
                } else {
                    Err(ParseError::expected(
                        vec![
                            "TABLE",
                            "INDEX",
                            "TIMESERIES",
                            "QUEUE",
                            "HLL",
                            "SKETCH",
                            "FILTER",
                        ],
                        self.peek(),
                        pos,
                    ))
                }
            }
            Token::Alter => match self.parse_alter_table_query()? {
                QueryExpr::AlterTable(query) => Ok(SqlCommand::AlterTable(query)),
                other => Err(ParseError::new(
                    format!("internal: ALTER produced unexpected query kind {other:?}"),
                    self.position(),
                )),
            },
            Token::Set => {
                self.advance()?;
                if self.consume_ident_ci("CONFIG")? {
                    let key = self.expect_ident()?;
                    let mut full_key = key;
                    while self.consume(&Token::Dot)? {
                        let next = self.expect_ident_or_keyword()?;
                        full_key = format!("{full_key}.{next}");
                    }
                    self.expect(Token::Eq)?;
                    let value = self.parse_literal_value()?;
                    Ok(SqlCommand::SetConfig {
                        key: full_key,
                        value,
                    })
                } else {
                    Err(ParseError::expected(
                        vec!["CONFIG"],
                        self.peek(),
                        self.position(),
                    ))
                }
            }
            Token::Ident(name) if name.eq_ignore_ascii_case("SHOW") => {
                self.advance()?;
                if self.consume_ident_ci("CONFIG")? {
                    let prefix = if !self.check(&Token::Eof) {
                        Some(self.expect_ident()?)
                    } else {
                        None
                    };
                    Ok(SqlCommand::ShowConfig { prefix })
                } else {
                    Err(ParseError::expected(
                        vec!["CONFIG"],
                        self.peek(),
                        self.position(),
                    ))
                }
            }
            other => Err(ParseError::expected(
                vec![
                    "SELECT", "FROM", "INSERT", "UPDATE", "DELETE", "EXPLAIN", "CREATE", "DROP",
                    "ALTER", "SET", "SHOW",
                ],
                other,
                self.position(),
            )),
        }
    }
}
