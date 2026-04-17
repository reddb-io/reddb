use crate::storage::query::ast::{
    AlterTableQuery, AskQuery, CopyFormat, CopyFromQuery, CreateForeignTableQuery,
    CreateIndexQuery, CreatePolicyQuery, CreateQueueQuery, CreateSchemaQuery, CreateSequenceQuery,
    CreateServerQuery, CreateTableQuery, CreateTimeSeriesQuery, CreateTreeQuery, CreateViewQuery,
    DeleteQuery, DropForeignTableQuery, DropIndexQuery, DropPolicyQuery, DropQueueQuery,
    DropSchemaQuery, DropSequenceQuery, DropServerQuery, DropTableQuery, DropTimeSeriesQuery,
    DropTreeQuery, DropViewQuery, ExplainAlterQuery, ForeignColumnDef, GraphCommand, GraphQuery,
    HybridQuery, InsertQuery, JoinQuery, MaintenanceCommand, PathQuery, PolicyAction,
    ProbabilisticCommand, QueryExpr, QueueCommand, RefreshMaterializedViewQuery, SearchCommand,
    TableQuery, TreeCommand, TxnControl, UpdateQuery, VectorQuery,
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
    TreeCommand(TreeCommand),
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
    CreateTree(CreateTreeQuery),
    DropTree(DropTreeQuery),
    Probabilistic(ProbabilisticCommand),
    SetConfig { key: String, value: Value },
    ShowConfig { prefix: Option<String> },
    SetTenant(Option<String>),
    ShowTenant,
    TransactionControl(TxnControl),
    Maintenance(MaintenanceCommand),
    CreateSchema(CreateSchemaQuery),
    DropSchema(DropSchemaQuery),
    CreateSequence(CreateSequenceQuery),
    DropSequence(DropSequenceQuery),
    CopyFrom(CopyFromQuery),
    CreateView(CreateViewQuery),
    DropView(DropViewQuery),
    RefreshMaterializedView(RefreshMaterializedViewQuery),
    CreatePolicy(CreatePolicyQuery),
    DropPolicy(DropPolicyQuery),
    CreateServer(CreateServerQuery),
    DropServer(DropServerQuery),
    CreateForeignTable(CreateForeignTableQuery),
    DropForeignTable(DropForeignTableQuery),
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
    CreateTree(CreateTreeQuery),
    DropTree(DropTreeQuery),
    Probabilistic(ProbabilisticCommand),
    CreateSchema(CreateSchemaQuery),
    DropSchema(DropSchemaQuery),
    CreateSequence(CreateSequenceQuery),
    DropSequence(DropSequenceQuery),
    CopyFrom(CopyFromQuery),
    CreateView(CreateViewQuery),
    DropView(DropViewQuery),
    RefreshMaterializedView(RefreshMaterializedViewQuery),
    CreatePolicy(CreatePolicyQuery),
    DropPolicy(DropPolicyQuery),
    CreateServer(CreateServerQuery),
    DropServer(DropServerQuery),
    CreateForeignTable(CreateForeignTableQuery),
    DropForeignTable(DropForeignTableQuery),
}

#[derive(Debug, Clone)]
pub enum SqlAdminCommand {
    SetConfig { key: String, value: Value },
    ShowConfig { prefix: Option<String> },
    SetTenant(Option<String>),
    ShowTenant,
    TransactionControl(TxnControl),
    Maintenance(MaintenanceCommand),
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
            SqlStatement::Schema(SqlSchemaCommand::CreateTree(query)) => {
                SqlCommand::CreateTree(query)
            }
            SqlStatement::Schema(SqlSchemaCommand::DropTree(query)) => SqlCommand::DropTree(query),
            SqlStatement::Schema(SqlSchemaCommand::Probabilistic(command)) => {
                SqlCommand::Probabilistic(command)
            }
            SqlStatement::Admin(SqlAdminCommand::SetConfig { key, value }) => {
                SqlCommand::SetConfig { key, value }
            }
            SqlStatement::Admin(SqlAdminCommand::ShowConfig { prefix }) => {
                SqlCommand::ShowConfig { prefix }
            }
            SqlStatement::Admin(SqlAdminCommand::SetTenant(value)) => {
                SqlCommand::SetTenant(value)
            }
            SqlStatement::Admin(SqlAdminCommand::ShowTenant) => SqlCommand::ShowTenant,
            SqlStatement::Admin(SqlAdminCommand::TransactionControl(ctl)) => {
                SqlCommand::TransactionControl(ctl)
            }
            SqlStatement::Admin(SqlAdminCommand::Maintenance(cmd)) => SqlCommand::Maintenance(cmd),
            SqlStatement::Schema(SqlSchemaCommand::CreateSchema(q)) => SqlCommand::CreateSchema(q),
            SqlStatement::Schema(SqlSchemaCommand::DropSchema(q)) => SqlCommand::DropSchema(q),
            SqlStatement::Schema(SqlSchemaCommand::CreateSequence(q)) => {
                SqlCommand::CreateSequence(q)
            }
            SqlStatement::Schema(SqlSchemaCommand::DropSequence(q)) => SqlCommand::DropSequence(q),
            SqlStatement::Schema(SqlSchemaCommand::CopyFrom(q)) => SqlCommand::CopyFrom(q),
            SqlStatement::Schema(SqlSchemaCommand::CreateView(q)) => SqlCommand::CreateView(q),
            SqlStatement::Schema(SqlSchemaCommand::DropView(q)) => SqlCommand::DropView(q),
            SqlStatement::Schema(SqlSchemaCommand::RefreshMaterializedView(q)) => {
                SqlCommand::RefreshMaterializedView(q)
            }
            SqlStatement::Schema(SqlSchemaCommand::CreatePolicy(q)) => SqlCommand::CreatePolicy(q),
            SqlStatement::Schema(SqlSchemaCommand::DropPolicy(q)) => SqlCommand::DropPolicy(q),
            SqlStatement::Schema(SqlSchemaCommand::CreateServer(q)) => SqlCommand::CreateServer(q),
            SqlStatement::Schema(SqlSchemaCommand::DropServer(q)) => SqlCommand::DropServer(q),
            SqlStatement::Schema(SqlSchemaCommand::CreateForeignTable(q)) => {
                SqlCommand::CreateForeignTable(q)
            }
            SqlStatement::Schema(SqlSchemaCommand::DropForeignTable(q)) => {
                SqlCommand::DropForeignTable(q)
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
            FrontendStatement::TreeCommand(command) => QueryExpr::TreeCommand(command),
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
            SqlCommand::CreateTree(query) => QueryExpr::CreateTree(query),
            SqlCommand::DropTree(query) => QueryExpr::DropTree(query),
            SqlCommand::Probabilistic(command) => QueryExpr::ProbabilisticCommand(command),
            SqlCommand::SetConfig { key, value } => QueryExpr::SetConfig { key, value },
            SqlCommand::ShowConfig { prefix } => QueryExpr::ShowConfig { prefix },
            SqlCommand::SetTenant(value) => QueryExpr::SetTenant(value),
            SqlCommand::ShowTenant => QueryExpr::ShowTenant,
            SqlCommand::TransactionControl(ctl) => QueryExpr::TransactionControl(ctl),
            SqlCommand::Maintenance(cmd) => QueryExpr::MaintenanceCommand(cmd),
            SqlCommand::CreateSchema(q) => QueryExpr::CreateSchema(q),
            SqlCommand::DropSchema(q) => QueryExpr::DropSchema(q),
            SqlCommand::CreateSequence(q) => QueryExpr::CreateSequence(q),
            SqlCommand::DropSequence(q) => QueryExpr::DropSequence(q),
            SqlCommand::CopyFrom(q) => QueryExpr::CopyFrom(q),
            SqlCommand::CreateView(q) => QueryExpr::CreateView(q),
            SqlCommand::DropView(q) => QueryExpr::DropView(q),
            SqlCommand::RefreshMaterializedView(q) => QueryExpr::RefreshMaterializedView(q),
            SqlCommand::CreatePolicy(q) => QueryExpr::CreatePolicy(q),
            SqlCommand::DropPolicy(q) => QueryExpr::DropPolicy(q),
            SqlCommand::CreateServer(q) => QueryExpr::CreateServer(q),
            SqlCommand::DropServer(q) => QueryExpr::DropServer(q),
            SqlCommand::CreateForeignTable(q) => QueryExpr::CreateForeignTable(q),
            SqlCommand::DropForeignTable(q) => QueryExpr::DropForeignTable(q),
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
            SqlCommand::CreateTree(query) => {
                SqlStatement::Schema(SqlSchemaCommand::CreateTree(query))
            }
            SqlCommand::DropTree(query) => SqlStatement::Schema(SqlSchemaCommand::DropTree(query)),
            SqlCommand::Probabilistic(command) => {
                SqlStatement::Schema(SqlSchemaCommand::Probabilistic(command))
            }
            SqlCommand::SetConfig { key, value } => {
                SqlStatement::Admin(SqlAdminCommand::SetConfig { key, value })
            }
            SqlCommand::ShowConfig { prefix } => {
                SqlStatement::Admin(SqlAdminCommand::ShowConfig { prefix })
            }
            SqlCommand::SetTenant(value) => SqlStatement::Admin(SqlAdminCommand::SetTenant(value)),
            SqlCommand::ShowTenant => SqlStatement::Admin(SqlAdminCommand::ShowTenant),
            SqlCommand::TransactionControl(ctl) => {
                SqlStatement::Admin(SqlAdminCommand::TransactionControl(ctl))
            }
            SqlCommand::Maintenance(cmd) => SqlStatement::Admin(SqlAdminCommand::Maintenance(cmd)),
            SqlCommand::CreateSchema(q) => SqlStatement::Schema(SqlSchemaCommand::CreateSchema(q)),
            SqlCommand::DropSchema(q) => SqlStatement::Schema(SqlSchemaCommand::DropSchema(q)),
            SqlCommand::CreateSequence(q) => {
                SqlStatement::Schema(SqlSchemaCommand::CreateSequence(q))
            }
            SqlCommand::DropSequence(q) => SqlStatement::Schema(SqlSchemaCommand::DropSequence(q)),
            SqlCommand::CopyFrom(q) => SqlStatement::Schema(SqlSchemaCommand::CopyFrom(q)),
            SqlCommand::CreateView(q) => SqlStatement::Schema(SqlSchemaCommand::CreateView(q)),
            SqlCommand::DropView(q) => SqlStatement::Schema(SqlSchemaCommand::DropView(q)),
            SqlCommand::RefreshMaterializedView(q) => {
                SqlStatement::Schema(SqlSchemaCommand::RefreshMaterializedView(q))
            }
            SqlCommand::CreatePolicy(q) => SqlStatement::Schema(SqlSchemaCommand::CreatePolicy(q)),
            SqlCommand::DropPolicy(q) => SqlStatement::Schema(SqlSchemaCommand::DropPolicy(q)),
            SqlCommand::CreateServer(q) => SqlStatement::Schema(SqlSchemaCommand::CreateServer(q)),
            SqlCommand::DropServer(q) => SqlStatement::Schema(SqlSchemaCommand::DropServer(q)),
            SqlCommand::CreateForeignTable(q) => {
                SqlStatement::Schema(SqlSchemaCommand::CreateForeignTable(q))
            }
            SqlCommand::DropForeignTable(q) => {
                SqlStatement::Schema(SqlSchemaCommand::DropForeignTable(q))
            }
        }
    }
}

impl<'a> Parser<'a> {
    /// Parse an optional `OPTIONS (key 'value', key2 'value2', ...)` clause
    /// used by Phase 3.2 FDW DDL statements. Returns an empty vec when the
    /// clause is absent. Values are always single-quoted string literals —
    /// consistent with PG's generic-options model.
    pub(crate) fn parse_fdw_options_clause(&mut self) -> Result<Vec<(String, String)>, ParseError> {
        if !self.consume(&Token::Options)? {
            return Ok(Vec::new());
        }
        self.expect(Token::LParen)?;
        let mut out: Vec<(String, String)> = Vec::new();
        loop {
            let key = self.expect_ident()?;
            // Value is a single-quoted string literal.
            let value = self.parse_string()?;
            out.push((key, value));
            if !self.consume(&Token::Comma)? {
                break;
            }
        }
        self.expect(Token::RParen)?;
        Ok(out)
    }

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
            | Token::Set
            | Token::Begin
            | Token::Commit
            | Token::Rollback
            | Token::Savepoint
            | Token::Release
            | Token::Start
            | Token::Vacuum
            | Token::Analyze
            | Token::Copy
            | Token::Refresh => self.parse_sql_statement().map(FrontendStatement::Sql),
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
            Token::Tree => match self.parse_tree_command()? {
                QueryExpr::TreeCommand(command) => Ok(FrontendStatement::TreeCommand(command)),
                other => Err(ParseError::new(
                    format!("internal: TREE produced unexpected query kind {other:?}"),
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
                    "TREE", "SKETCH", "FILTER", "SET", "SHOW",
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

                // CREATE [OR REPLACE] [MATERIALIZED] VIEW [IF NOT EXISTS] name AS <select>
                // Detect the VIEW path early so OR REPLACE / MATERIALIZED modifiers
                // don't collide with other CREATE variants (TABLE, INDEX, etc.).
                let mut or_replace = false;
                if self.consume_ident_ci("OR")? {
                    let _ = self.consume_ident_ci("REPLACE")?;
                    or_replace = true;
                }
                let materialized = self.consume(&Token::Materialized)?;
                if self.check(&Token::View) {
                    self.advance()?;
                    let if_not_exists = self.match_if_not_exists()?;
                    let name = self.expect_ident()?;
                    // Expect `AS` (tokenised as an Ident for case-sensitivity).
                    if !self.consume_ident_ci("AS")? {
                        return Err(ParseError::expected(
                            vec!["AS"],
                            self.peek(),
                            self.position(),
                        ));
                    }
                    // Recursive parse of the body. Any QueryExpr that the
                    // rest of the grammar accepts is valid (Select, Join, etc.).
                    let body = self.parse_sql_command()?.into_query_expr();
                    return Ok(SqlCommand::CreateView(CreateViewQuery {
                        name,
                        query: Box::new(body),
                        materialized,
                        if_not_exists,
                        or_replace,
                    }));
                }
                // If OR REPLACE / MATERIALIZED was consumed but VIEW was not,
                // bail out — no other CREATE form accepts those modifiers.
                if or_replace || materialized {
                    return Err(ParseError::expected(
                        vec!["VIEW"],
                        self.peek(),
                        self.position(),
                    ));
                }

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
                } else if self.check(&Token::Tree) {
                    self.advance()?;
                    match self.parse_create_tree_body()? {
                        QueryExpr::CreateTree(query) => Ok(SqlCommand::CreateTree(query)),
                        other => Err(ParseError::new(
                            format!("internal: CREATE TREE produced unexpected kind {other:?}"),
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
                } else if self.check(&Token::Schema) {
                    // CREATE SCHEMA [IF NOT EXISTS] name
                    self.advance()?;
                    let if_not_exists = self.match_if_not_exists()?;
                    let name = self.expect_ident()?;
                    Ok(SqlCommand::CreateSchema(CreateSchemaQuery {
                        name,
                        if_not_exists,
                    }))
                } else if self.check(&Token::Policy) {
                    // CREATE POLICY name ON table [FOR action] [TO role] USING (filter)
                    self.advance()?;
                    let name = self.expect_ident()?;
                    self.expect(Token::On)?;
                    let table = self.expect_ident()?;

                    let action = if self.consume(&Token::For)? {
                        let a = match self.peek() {
                            Token::Select => {
                                self.advance()?;
                                Some(PolicyAction::Select)
                            }
                            Token::Insert => {
                                self.advance()?;
                                Some(PolicyAction::Insert)
                            }
                            Token::Update => {
                                self.advance()?;
                                Some(PolicyAction::Update)
                            }
                            Token::Delete => {
                                self.advance()?;
                                Some(PolicyAction::Delete)
                            }
                            Token::All => {
                                self.advance()?;
                                None
                            }
                            _ => None,
                        };
                        a
                    } else {
                        None
                    };

                    let role = if self.consume(&Token::To)? {
                        Some(self.expect_ident()?)
                    } else {
                        None
                    };

                    self.expect(Token::Using)?;
                    self.expect(Token::LParen)?;
                    let filter = self.parse_filter()?;
                    self.expect(Token::RParen)?;

                    return Ok(SqlCommand::CreatePolicy(CreatePolicyQuery {
                        name,
                        table,
                        action,
                        role,
                        using: Box::new(filter),
                    }));
                } else if self.check(&Token::Server) {
                    // CREATE SERVER [IF NOT EXISTS] name
                    //   FOREIGN DATA WRAPPER kind
                    //   [OPTIONS (key 'value', ...)]
                    self.advance()?;
                    let if_not_exists = self.match_if_not_exists()?;
                    let name = self.expect_ident()?;
                    self.expect(Token::Foreign)?;
                    self.expect(Token::Data)?;
                    self.expect(Token::Wrapper)?;
                    let wrapper = self.expect_ident()?;
                    let options = self.parse_fdw_options_clause()?;
                    return Ok(SqlCommand::CreateServer(CreateServerQuery {
                        name,
                        wrapper,
                        options,
                        if_not_exists,
                    }));
                } else if self.check(&Token::Foreign) {
                    // CREATE FOREIGN TABLE [IF NOT EXISTS] name (cols)
                    //   SERVER server_name
                    //   [OPTIONS (key 'value', ...)]
                    self.advance()?;
                    self.expect(Token::Table)?;
                    let if_not_exists = self.match_if_not_exists()?;
                    let name = self.expect_ident()?;
                    self.expect(Token::LParen)?;
                    let mut columns = Vec::new();
                    loop {
                        let col_name = self.expect_ident()?;
                        let data_type = self.expect_ident_or_keyword()?;
                        // Inline NOT NULL check — the CREATE TABLE path's helper is
                        // private and coupling to it just for FDW columns isn't worth it.
                        let mut not_null = false;
                        if matches!(self.peek(), Token::Ident(n) if n.eq_ignore_ascii_case("NOT")) {
                            self.advance()?;
                            if matches!(self.peek(), Token::Ident(n) if n.eq_ignore_ascii_case("NULL"))
                            {
                                self.advance()?;
                                not_null = true;
                            }
                        }
                        columns.push(ForeignColumnDef {
                            name: col_name,
                            data_type,
                            not_null,
                        });
                        if !self.consume(&Token::Comma)? {
                            break;
                        }
                    }
                    self.expect(Token::RParen)?;
                    self.expect(Token::Server)?;
                    let server = self.expect_ident()?;
                    let options = self.parse_fdw_options_clause()?;
                    return Ok(SqlCommand::CreateForeignTable(CreateForeignTableQuery {
                        name,
                        server,
                        columns,
                        options,
                        if_not_exists,
                    }));
                } else if self.check(&Token::Sequence) {
                    // CREATE SEQUENCE [IF NOT EXISTS] name
                    //   [START [WITH] n] [INCREMENT [BY] n]
                    self.advance()?;
                    let if_not_exists = self.match_if_not_exists()?;
                    let name = self.expect_ident()?;
                    let mut start: i64 = 1;
                    let mut increment: i64 = 1;
                    // Loop over optional clauses in any order.
                    loop {
                        if self.consume(&Token::Start)? {
                            let _ = self.consume_ident_ci("WITH")?;
                            start = self.parse_integer()?;
                        } else if self.consume(&Token::Increment)? {
                            let _ = self.consume_ident_ci("BY")?;
                            increment = self.parse_integer()?;
                        } else {
                            break;
                        }
                    }
                    Ok(SqlCommand::CreateSequence(CreateSequenceQuery {
                        name,
                        if_not_exists,
                        start,
                        increment,
                    }))
                } else {
                    Err(ParseError::expected(
                        vec![
                            "TABLE",
                            "INDEX",
                            "UNIQUE",
                            "TIMESERIES",
                            "QUEUE",
                            "TREE",
                            "HLL",
                            "SKETCH",
                            "FILTER",
                            "SCHEMA",
                            "SEQUENCE",
                        ],
                        self.peek(),
                        pos,
                    ))
                }
            }
            Token::Drop => {
                let pos = self.position();
                self.advance()?;

                // DROP [MATERIALIZED] VIEW [IF EXISTS] name
                let materialized = self.consume(&Token::Materialized)?;
                if self.check(&Token::View) {
                    self.advance()?;
                    let if_exists = self.match_if_exists()?;
                    let name = self.expect_ident()?;
                    return Ok(SqlCommand::DropView(DropViewQuery {
                        name,
                        materialized,
                        if_exists,
                    }));
                }
                if materialized {
                    return Err(ParseError::expected(
                        vec!["VIEW"],
                        self.peek(),
                        self.position(),
                    ));
                }

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
                } else if self.check(&Token::Tree) {
                    self.advance()?;
                    match self.parse_drop_tree_body()? {
                        QueryExpr::DropTree(query) => Ok(SqlCommand::DropTree(query)),
                        other => Err(ParseError::new(
                            format!("internal: DROP TREE produced unexpected kind {other:?}"),
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
                } else if self.check(&Token::Schema) {
                    // DROP SCHEMA [IF EXISTS] name [CASCADE]
                    self.advance()?;
                    let if_exists = self.match_if_exists()?;
                    let name = self.expect_ident()?;
                    let cascade = self.consume(&Token::Cascade)?;
                    Ok(SqlCommand::DropSchema(DropSchemaQuery {
                        name,
                        if_exists,
                        cascade,
                    }))
                } else if self.check(&Token::Policy) {
                    // DROP POLICY [IF EXISTS] name ON table
                    self.advance()?;
                    let if_exists = self.match_if_exists()?;
                    let name = self.expect_ident()?;
                    self.expect(Token::On)?;
                    let table = self.expect_ident()?;
                    Ok(SqlCommand::DropPolicy(DropPolicyQuery {
                        name,
                        table,
                        if_exists,
                    }))
                } else if self.check(&Token::Server) {
                    // DROP SERVER [IF EXISTS] name [CASCADE]
                    self.advance()?;
                    let if_exists = self.match_if_exists()?;
                    let name = self.expect_ident()?;
                    let cascade = self.consume(&Token::Cascade)?;
                    Ok(SqlCommand::DropServer(DropServerQuery {
                        name,
                        if_exists,
                        cascade,
                    }))
                } else if self.check(&Token::Foreign) {
                    // DROP FOREIGN TABLE [IF EXISTS] name
                    self.advance()?;
                    self.expect(Token::Table)?;
                    let if_exists = self.match_if_exists()?;
                    let name = self.expect_ident()?;
                    Ok(SqlCommand::DropForeignTable(DropForeignTableQuery {
                        name,
                        if_exists,
                    }))
                } else if self.check(&Token::Sequence) {
                    // DROP SEQUENCE [IF EXISTS] name
                    self.advance()?;
                    let if_exists = self.match_if_exists()?;
                    let name = self.expect_ident()?;
                    Ok(SqlCommand::DropSequence(DropSequenceQuery {
                        name,
                        if_exists,
                    }))
                } else {
                    Err(ParseError::expected(
                        vec![
                            "TABLE",
                            "INDEX",
                            "TIMESERIES",
                            "QUEUE",
                            "TREE",
                            "HLL",
                            "SKETCH",
                            "FILTER",
                            "SCHEMA",
                            "SEQUENCE",
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
                } else if self.consume_ident_ci("TENANT")? {
                    // SET TENANT 'id'  |  SET TENANT = 'id'  |
                    // SET TENANT NULL  |  SET TENANT = NULL
                    let _ = self.consume(&Token::Eq)?;
                    if self.consume_ident_ci("NULL")? {
                        Ok(SqlCommand::SetTenant(None))
                    } else {
                        let value = self.parse_literal_value()?;
                        match value {
                            Value::Text(s) => Ok(SqlCommand::SetTenant(Some(s))),
                            Value::Null => Ok(SqlCommand::SetTenant(None)),
                            other => Err(ParseError::new(
                                format!(
                                    "SET TENANT expects a text literal or NULL, got {other:?}"
                                ),
                                self.position(),
                            )),
                        }
                    }
                } else {
                    Err(ParseError::expected(
                        vec!["CONFIG", "TENANT"],
                        self.peek(),
                        self.position(),
                    ))
                }
            }
            Token::Ident(name) if name.eq_ignore_ascii_case("RESET") => {
                // RESET TENANT — session-local clear
                self.advance()?;
                if self.consume_ident_ci("TENANT")? {
                    Ok(SqlCommand::SetTenant(None))
                } else {
                    Err(ParseError::expected(
                        vec!["TENANT"],
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
                } else if self.consume_ident_ci("TENANT")? {
                    Ok(SqlCommand::ShowTenant)
                } else {
                    Err(ParseError::expected(
                        vec!["CONFIG", "TENANT"],
                        self.peek(),
                        self.position(),
                    ))
                }
            }
            // Transaction control statements (Phase 1.1 PG parity).
            // BEGIN [WORK | TRANSACTION], START TRANSACTION
            Token::Begin | Token::Start => {
                self.advance()?;
                // Optional trailing WORK / TRANSACTION keywords.
                let _ = self.consume(&Token::Work)? || self.consume(&Token::Transaction)?;
                Ok(SqlCommand::TransactionControl(TxnControl::Begin))
            }
            // COMMIT [WORK | TRANSACTION]
            Token::Commit => {
                self.advance()?;
                let _ = self.consume(&Token::Work)? || self.consume(&Token::Transaction)?;
                Ok(SqlCommand::TransactionControl(TxnControl::Commit))
            }
            // ROLLBACK [WORK | TRANSACTION] [TO [SAVEPOINT] name]
            Token::Rollback => {
                self.advance()?;
                let _ = self.consume(&Token::Work)? || self.consume(&Token::Transaction)?;
                if self.consume(&Token::To)? {
                    let _ = self.consume(&Token::Savepoint)?;
                    let name = self.expect_ident()?;
                    Ok(SqlCommand::TransactionControl(
                        TxnControl::RollbackToSavepoint(name),
                    ))
                } else {
                    Ok(SqlCommand::TransactionControl(TxnControl::Rollback))
                }
            }
            // SAVEPOINT name
            Token::Savepoint => {
                self.advance()?;
                let name = self.expect_ident()?;
                Ok(SqlCommand::TransactionControl(TxnControl::Savepoint(name)))
            }
            // RELEASE [SAVEPOINT] name
            Token::Release => {
                self.advance()?;
                let _ = self.consume(&Token::Savepoint)?;
                let name = self.expect_ident()?;
                Ok(SqlCommand::TransactionControl(
                    TxnControl::ReleaseSavepoint(name),
                ))
            }
            // VACUUM [FULL] [table]
            Token::Vacuum => {
                self.advance()?;
                let full = self.consume(&Token::Full)?;
                let target = if self.check(&Token::Eof) {
                    None
                } else {
                    Some(self.expect_ident()?)
                };
                Ok(SqlCommand::Maintenance(MaintenanceCommand::Vacuum {
                    target,
                    full,
                }))
            }
            // REFRESH MATERIALIZED VIEW name
            Token::Refresh => {
                self.advance()?;
                self.expect(Token::Materialized)?;
                self.expect(Token::View)?;
                let name = self.expect_ident()?;
                Ok(SqlCommand::RefreshMaterializedView(
                    RefreshMaterializedViewQuery { name },
                ))
            }
            // ANALYZE [table]
            Token::Analyze => {
                self.advance()?;
                let target = if self.check(&Token::Eof) {
                    None
                } else {
                    Some(self.expect_ident()?)
                };
                Ok(SqlCommand::Maintenance(MaintenanceCommand::Analyze {
                    target,
                }))
            }
            // COPY table FROM 'path' [WITH (...)] [DELIMITER 'x'] [HEADER [true|false]]
            //
            // Accepts both PG-style `WITH (FORMAT csv, HEADER true)` and the
            // short-form `DELIMITER ',' HEADER`. The only supported format
            // today is CSV.
            Token::Copy => {
                self.advance()?;
                let table = self.expect_ident()?;
                self.expect(Token::From)?;
                let path = self.parse_string()?;

                let mut delimiter: Option<char> = None;
                let mut has_header = false;
                let format = CopyFormat::Csv;

                // Optional `WITH (FORMAT csv, HEADER true, DELIMITER ',')` block.
                if self.consume_ident_ci("WITH")? {
                    self.expect(Token::LParen)?;
                    loop {
                        if self.consume_ident_ci("FORMAT")? {
                            let _ = self.consume(&Token::Eq)?;
                            // Only CSV for now — accept the ident and move on.
                            let _ = self.expect_ident()?;
                        } else if self.consume(&Token::Header)? {
                            let _ = self.consume(&Token::Eq)?;
                            has_header = if let Token::Ident(n) = self.peek() {
                                let v = n.eq_ignore_ascii_case("true");
                                self.advance()?;
                                v
                            } else {
                                true
                            };
                        } else if self.consume(&Token::Delimiter)? {
                            let _ = self.consume(&Token::Eq)?;
                            let s = self.parse_string()?;
                            delimiter = s.chars().next();
                        } else {
                            break;
                        }
                        if !self.consume(&Token::Comma)? {
                            break;
                        }
                    }
                    self.expect(Token::RParen)?;
                }

                // Short form clauses outside WITH (in either order).
                loop {
                    if self.consume(&Token::Delimiter)? {
                        let s = self.parse_string()?;
                        delimiter = s.chars().next();
                    } else if self.consume(&Token::Header)? {
                        has_header = true;
                    } else {
                        break;
                    }
                }

                Ok(SqlCommand::CopyFrom(CopyFromQuery {
                    table,
                    path,
                    format,
                    delimiter,
                    has_header,
                }))
            }
            other => Err(ParseError::expected(
                vec![
                    "SELECT",
                    "FROM",
                    "INSERT",
                    "UPDATE",
                    "DELETE",
                    "EXPLAIN",
                    "CREATE",
                    "DROP",
                    "ALTER",
                    "SET",
                    "SHOW",
                    "BEGIN",
                    "COMMIT",
                    "ROLLBACK",
                    "SAVEPOINT",
                    "RELEASE",
                    "START",
                    "VACUUM",
                    "ANALYZE",
                    "COPY",
                    "REFRESH",
                ],
                other,
                self.position(),
            )),
        }
    }
}
