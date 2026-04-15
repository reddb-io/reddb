use crate::storage::query::ast::{
    AlterTableQuery, AskQuery, CreateIndexQuery, CreateQueueQuery, CreateTableQuery,
    CreateTimeSeriesQuery, DeleteQuery, DropIndexQuery, DropQueueQuery, DropTableQuery,
    DropTimeSeriesQuery, ExplainAlterQuery, GraphCommand, GraphQuery, HybridQuery, InsertQuery,
    JoinQuery, PathQuery, ProbabilisticCommand, QueryExpr, QueueCommand, SearchCommand, TableQuery,
    UpdateQuery, VectorQuery,
};
use crate::storage::query::parser::{ParseError, Parser};
use crate::storage::query::Token;
use crate::storage::schema::Value;

/// Canonical SQL frontend command surface.
///
/// This is the single entrypoint for SQL/RQL-style commands before they are
/// lowered into the broader multi-backend `QueryExpr` space.
#[derive(Debug, Clone)]
pub enum SqlStatement {
    Query(SqlQuery),
    Mutation(SqlMutation),
    Schema(SqlSchemaCommand),
    Admin(SqlAdminCommand),
}

#[derive(Debug, Clone)]
pub enum FrontendStatement {
    Sql(SqlStatement),
    Graph(GraphQuery),
    GraphCommand(GraphCommand),
    Path(PathQuery),
    Vector(VectorQuery),
    Hybrid(HybridQuery),
    Search(SearchCommand),
    Ask(AskQuery),
    QueueCommand(QueueCommand),
    ProbabilisticCommand(ProbabilisticCommand),
}

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

#[derive(Debug, Clone)]
pub enum SqlQuery {
    Select(TableQuery),
    Join(JoinQuery),
}

#[derive(Debug, Clone)]
pub enum SqlMutation {
    Insert(InsertQuery),
    Update(UpdateQuery),
    Delete(DeleteQuery),
}

#[derive(Debug, Clone)]
pub enum SqlSchemaCommand {
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
}

#[derive(Debug, Clone)]
pub enum SqlAdminCommand {
    SetConfig { key: String, value: Value },
    ShowConfig { prefix: Option<String> },
}

impl SqlStatement {
    pub fn into_command(self) -> SqlCommand {
        match self {
            SqlStatement::Query(SqlQuery::Select(query)) => SqlCommand::Select(query),
            SqlStatement::Query(SqlQuery::Join(query)) => SqlCommand::Join(query),
            SqlStatement::Mutation(SqlMutation::Insert(query)) => SqlCommand::Insert(query),
            SqlStatement::Mutation(SqlMutation::Update(query)) => SqlCommand::Update(query),
            SqlStatement::Mutation(SqlMutation::Delete(query)) => SqlCommand::Delete(query),
            SqlStatement::Schema(SqlSchemaCommand::ExplainAlter(query)) => {
                SqlCommand::ExplainAlter(query)
            }
            SqlStatement::Schema(SqlSchemaCommand::CreateTable(query)) => {
                SqlCommand::CreateTable(query)
            }
            SqlStatement::Schema(SqlSchemaCommand::DropTable(query)) => {
                SqlCommand::DropTable(query)
            }
            SqlStatement::Schema(SqlSchemaCommand::AlterTable(query)) => {
                SqlCommand::AlterTable(query)
            }
            SqlStatement::Schema(SqlSchemaCommand::CreateIndex(query)) => {
                SqlCommand::CreateIndex(query)
            }
            SqlStatement::Schema(SqlSchemaCommand::DropIndex(query)) => {
                SqlCommand::DropIndex(query)
            }
            SqlStatement::Schema(SqlSchemaCommand::CreateTimeSeries(query)) => {
                SqlCommand::CreateTimeSeries(query)
            }
            SqlStatement::Schema(SqlSchemaCommand::DropTimeSeries(query)) => {
                SqlCommand::DropTimeSeries(query)
            }
            SqlStatement::Schema(SqlSchemaCommand::CreateQueue(query)) => {
                SqlCommand::CreateQueue(query)
            }
            SqlStatement::Schema(SqlSchemaCommand::DropQueue(query)) => {
                SqlCommand::DropQueue(query)
            }
            SqlStatement::Schema(SqlSchemaCommand::Probabilistic(command)) => {
                SqlCommand::Probabilistic(command)
            }
            SqlStatement::Admin(SqlAdminCommand::SetConfig { key, value }) => {
                SqlCommand::SetConfig { key, value }
            }
            SqlStatement::Admin(SqlAdminCommand::ShowConfig { prefix }) => {
                SqlCommand::ShowConfig { prefix }
            }
        }
    }

    pub fn into_query_expr(self) -> QueryExpr {
        self.into_command().into_query_expr()
    }
}

impl FrontendStatement {
    pub fn into_query_expr(self) -> QueryExpr {
        match self {
            FrontendStatement::Sql(statement) => statement.into_query_expr(),
            FrontendStatement::Graph(query) => QueryExpr::Graph(query),
            FrontendStatement::GraphCommand(command) => QueryExpr::GraphCommand(command),
            FrontendStatement::Path(query) => QueryExpr::Path(query),
            FrontendStatement::Vector(query) => QueryExpr::Vector(query),
            FrontendStatement::Hybrid(query) => QueryExpr::Hybrid(query),
            FrontendStatement::Search(command) => QueryExpr::SearchCommand(command),
            FrontendStatement::Ask(query) => QueryExpr::Ask(query),
            FrontendStatement::QueueCommand(command) => QueryExpr::QueueCommand(command),
            FrontendStatement::ProbabilisticCommand(command) => {
                QueryExpr::ProbabilisticCommand(command)
            }
        }
    }
}

pub fn parse_frontend(input: &str) -> Result<FrontendStatement, ParseError> {
    let mut parser = Parser::new(input)?;
    let statement = parser.parse_frontend_statement()?;
    if !parser.check(&Token::Eof) {
        return Err(ParseError::new(
            format!("Unexpected token after query: {}", parser.current.token),
            parser.position(),
        ));
    }
    Ok(statement)
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

    pub fn into_statement(self) -> SqlStatement {
        match self {
            SqlCommand::Select(query) => SqlStatement::Query(SqlQuery::Select(query)),
            SqlCommand::Join(query) => SqlStatement::Query(SqlQuery::Join(query)),
            SqlCommand::Insert(query) => SqlStatement::Mutation(SqlMutation::Insert(query)),
            SqlCommand::Update(query) => SqlStatement::Mutation(SqlMutation::Update(query)),
            SqlCommand::Delete(query) => SqlStatement::Mutation(SqlMutation::Delete(query)),
            SqlCommand::ExplainAlter(query) => {
                SqlStatement::Schema(SqlSchemaCommand::ExplainAlter(query))
            }
            SqlCommand::CreateTable(query) => {
                SqlStatement::Schema(SqlSchemaCommand::CreateTable(query))
            }
            SqlCommand::DropTable(query) => {
                SqlStatement::Schema(SqlSchemaCommand::DropTable(query))
            }
            SqlCommand::AlterTable(query) => {
                SqlStatement::Schema(SqlSchemaCommand::AlterTable(query))
            }
            SqlCommand::CreateIndex(query) => {
                SqlStatement::Schema(SqlSchemaCommand::CreateIndex(query))
            }
            SqlCommand::DropIndex(query) => {
                SqlStatement::Schema(SqlSchemaCommand::DropIndex(query))
            }
            SqlCommand::CreateTimeSeries(query) => {
                SqlStatement::Schema(SqlSchemaCommand::CreateTimeSeries(query))
            }
            SqlCommand::DropTimeSeries(query) => {
                SqlStatement::Schema(SqlSchemaCommand::DropTimeSeries(query))
            }
            SqlCommand::CreateQueue(query) => {
                SqlStatement::Schema(SqlSchemaCommand::CreateQueue(query))
            }
            SqlCommand::DropQueue(query) => {
                SqlStatement::Schema(SqlSchemaCommand::DropQueue(query))
            }
            SqlCommand::Probabilistic(command) => {
                SqlStatement::Schema(SqlSchemaCommand::Probabilistic(command))
            }
            SqlCommand::SetConfig { key, value } => {
                SqlStatement::Admin(SqlAdminCommand::SetConfig { key, value })
            }
            SqlCommand::ShowConfig { prefix } => {
                SqlStatement::Admin(SqlAdminCommand::ShowConfig { prefix })
            }
        }
    }
}

impl<'a> Parser<'a> {
    /// Parse any top-level frontend statement through a single shared surface.
    pub fn parse_frontend_statement(&mut self) -> Result<FrontendStatement, ParseError> {
        match self.peek() {
            Token::Select
            | Token::From
            | Token::Insert
            | Token::Update
            | Token::Delete
            | Token::Explain
            | Token::Create
            | Token::Drop
            | Token::Alter
            | Token::Set => self.parse_sql_statement().map(FrontendStatement::Sql),
            Token::Ident(name) if name.eq_ignore_ascii_case("SHOW") => {
                self.parse_sql_statement().map(FrontendStatement::Sql)
            }
            Token::Match => match self.parse_match_query()? {
                QueryExpr::Graph(query) => Ok(FrontendStatement::Graph(query)),
                other => Err(ParseError::new(
                    format!("internal: MATCH produced unexpected query kind {other:?}"),
                    self.position(),
                )),
            },
            Token::Path => match self.parse_path_query()? {
                QueryExpr::Path(query) => Ok(FrontendStatement::Path(query)),
                other => Err(ParseError::new(
                    format!("internal: PATH produced unexpected query kind {other:?}"),
                    self.position(),
                )),
            },
            Token::Vector => match self.parse_vector_query()? {
                QueryExpr::Vector(query) => Ok(FrontendStatement::Vector(query)),
                other => Err(ParseError::new(
                    format!("internal: VECTOR produced unexpected query kind {other:?}"),
                    self.position(),
                )),
            },
            Token::Hybrid => match self.parse_hybrid_query()? {
                QueryExpr::Hybrid(query) => Ok(FrontendStatement::Hybrid(query)),
                other => Err(ParseError::new(
                    format!("internal: HYBRID produced unexpected query kind {other:?}"),
                    self.position(),
                )),
            },
            Token::Graph => match self.parse_graph_command()? {
                QueryExpr::GraphCommand(command) => Ok(FrontendStatement::GraphCommand(command)),
                other => Err(ParseError::new(
                    format!("internal: GRAPH produced unexpected query kind {other:?}"),
                    self.position(),
                )),
            },
            Token::Search => match self.parse_search_command()? {
                QueryExpr::SearchCommand(command) => Ok(FrontendStatement::Search(command)),
                other => Err(ParseError::new(
                    format!("internal: SEARCH produced unexpected query kind {other:?}"),
                    self.position(),
                )),
            },
            Token::Ident(name) if name.eq_ignore_ascii_case("ASK") => {
                match self.parse_ask_query()? {
                    QueryExpr::Ask(query) => Ok(FrontendStatement::Ask(query)),
                    other => Err(ParseError::new(
                        format!("internal: ASK produced unexpected query kind {other:?}"),
                        self.position(),
                    )),
                }
            }
            Token::Queue => match self.parse_queue_command()? {
                QueryExpr::QueueCommand(command) => Ok(FrontendStatement::QueueCommand(command)),
                other => Err(ParseError::new(
                    format!("internal: QUEUE produced unexpected query kind {other:?}"),
                    self.position(),
                )),
            },
            Token::Ident(name) if name.eq_ignore_ascii_case("HLL") => {
                match self.parse_hll_command()? {
                    QueryExpr::ProbabilisticCommand(command) => {
                        Ok(FrontendStatement::ProbabilisticCommand(command))
                    }
                    other => Err(ParseError::new(
                        format!("internal: HLL produced unexpected query kind {other:?}"),
                        self.position(),
                    )),
                }
            }
            Token::Ident(name) if name.eq_ignore_ascii_case("SKETCH") => {
                match self.parse_sketch_command()? {
                    QueryExpr::ProbabilisticCommand(command) => {
                        Ok(FrontendStatement::ProbabilisticCommand(command))
                    }
                    other => Err(ParseError::new(
                        format!("internal: SKETCH produced unexpected query kind {other:?}"),
                        self.position(),
                    )),
                }
            }
            Token::Ident(name) if name.eq_ignore_ascii_case("FILTER") => {
                match self.parse_filter_command()? {
                    QueryExpr::ProbabilisticCommand(command) => {
                        Ok(FrontendStatement::ProbabilisticCommand(command))
                    }
                    other => Err(ParseError::new(
                        format!("internal: FILTER produced unexpected query kind {other:?}"),
                        self.position(),
                    )),
                }
            }
            other => Err(ParseError::expected(
                vec![
                    "SELECT", "MATCH", "PATH", "FROM", "VECTOR", "HYBRID", "INSERT", "UPDATE",
                    "DELETE", "CREATE", "DROP", "ALTER", "GRAPH", "SEARCH", "ASK", "QUEUE", "HLL",
                    "SKETCH", "FILTER", "SET", "SHOW",
                ],
                other,
                self.position(),
            )),
        }
    }

    /// Parse any SQL/RQL-style command into the canonical SQL frontend IR.
    pub fn parse_sql_statement(&mut self) -> Result<SqlStatement, ParseError> {
        self.parse_sql_command().map(SqlCommand::into_statement)
    }

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
