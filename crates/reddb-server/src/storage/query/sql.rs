use crate::catalog::CollectionModel;
use crate::storage::query::ast::{
    AlterQueueQuery, AlterTableQuery, AlterUserStmt, ApplyMigrationQuery, AskQuery, BinOp,
    CompareOp, ConfigCommand, CopyFormat, CopyFromQuery, CreateCollectionQuery,
    CreateForeignTableQuery, CreateIndexQuery, CreateMigrationQuery, CreatePolicyQuery,
    CreateQueueQuery, CreateSchemaQuery, CreateSequenceQuery, CreateServerQuery, CreateTableQuery,
    CreateTimeSeriesQuery, CreateTreeQuery, CreateVectorQuery, CreateViewQuery, DeleteQuery,
    DropCollectionQuery, DropDocumentQuery, DropForeignTableQuery, DropGraphQuery, DropIndexQuery,
    DropKvQuery, DropPolicyQuery, DropQueueQuery, DropSchemaQuery, DropSequenceQuery,
    DropServerQuery, DropTableQuery, DropTimeSeriesQuery, DropTreeQuery, DropVectorQuery,
    DropViewQuery, EventsBackfillQuery, ExplainAlterQuery, ExplainMigrationQuery, Expr, FieldRef,
    Filter, ForeignColumnDef, GrantStmt, GraphCommand, GraphQuery, HybridQuery, InsertQuery,
    JoinQuery, KvCommand, MaintenanceCommand, PathQuery, PolicyAction, ProbabilisticCommand,
    QueryExpr, QueueCommand, QueueSelectQuery, RefreshMaterializedViewQuery, RevokeStmt,
    RollbackMigrationQuery, SearchCommand, Span, TableQuery, TreeCommand, TruncateQuery,
    TxnControl, UpdateQuery, VectorQuery,
};
use crate::storage::query::parser::{ParseError, Parser, SafeTokenDisplay};
use crate::storage::query::sql_lowering::filter_to_expr;
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
#[allow(clippy::large_enum_variant)]
pub enum FrontendStatement {
    Sql(SqlStatement),
    Graph(GraphQuery),
    GraphCommand(GraphCommand),
    Path(PathQuery),
    Vector(VectorQuery),
    Hybrid(HybridQuery),
    Search(SearchCommand),
    Ask(AskQuery),
    QueueSelect(QueueSelectQuery),
    QueueCommand(QueueCommand),
    EventsBackfill(EventsBackfillQuery),
    EventsBackfillStatus { collection: String },
    TreeCommand(TreeCommand),
    ProbabilisticCommand(ProbabilisticCommand),
    KvCommand(KvCommand),
    ConfigCommand(ConfigCommand),
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
    CreateCollection(CreateCollectionQuery),
    CreateVector(CreateVectorQuery),
    DropTable(DropTableQuery),
    DropGraph(DropGraphQuery),
    DropVector(DropVectorQuery),
    DropDocument(DropDocumentQuery),
    DropKv(DropKvQuery),
    DropCollection(DropCollectionQuery),
    Truncate(TruncateQuery),
    AlterTable(AlterTableQuery),
    CreateIndex(CreateIndexQuery),
    DropIndex(DropIndexQuery),
    CreateTimeSeries(CreateTimeSeriesQuery),
    DropTimeSeries(DropTimeSeriesQuery),
    CreateQueue(CreateQueueQuery),
    AlterQueue(AlterQueueQuery),
    DropQueue(DropQueueQuery),
    CreateTree(CreateTreeQuery),
    DropTree(DropTreeQuery),
    Probabilistic(ProbabilisticCommand),
    SetConfig {
        key: String,
        value: Value,
    },
    ShowConfig {
        prefix: Option<String>,
    },
    SetSecret {
        key: String,
        value: Value,
    },
    DeleteSecret {
        key: String,
    },
    ShowSecrets {
        prefix: Option<String>,
    },
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
    /// `GRANT … ON … TO …`
    Grant(GrantStmt),
    /// `REVOKE … ON … FROM …`
    Revoke(RevokeStmt),
    /// `ALTER USER name <attrs>`
    AlterUser(AlterUserStmt),
    /// IAM policy DDL (CREATE POLICY '...' AS '...', DROP POLICY '...',
    /// ATTACH/DETACH POLICY, SHOW POLICIES, SIMULATE, SHOW EFFECTIVE
    /// PERMISSIONS). Stored as a pre-built QueryExpr so the dispatcher
    /// can route the multitude of shapes through a single arm.
    IamPolicy(QueryExpr),
    CreateMigration(CreateMigrationQuery),
    ApplyMigration(ApplyMigrationQuery),
    RollbackMigration(RollbackMigrationQuery),
    ExplainMigration(ExplainMigrationQuery),
}

fn collection_model_filter(model: &str) -> Filter {
    Filter::Compare {
        field: FieldRef::column("", "model"),
        op: CompareOp::Eq,
        value: Value::Text(model.to_string().into()),
    }
}

fn add_table_filter(query: &mut TableQuery, filter: Filter) {
    let combined = match query.filter.take() {
        Some(existing) => existing.and(filter),
        None => filter,
    };
    query.where_expr = Some(filter_to_expr(&combined));
    query.filter = Some(combined);
}

fn parse_show_collections_by_model(
    parser: &mut Parser<'_>,
    model: &str,
) -> Result<TableQuery, ParseError> {
    let mut query = TableQuery::new("red.collections");
    parser.parse_table_clauses(&mut query)?;
    add_table_filter(&mut query, collection_model_filter(model));
    Ok(query)
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
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
    CreateCollection(CreateCollectionQuery),
    CreateVector(CreateVectorQuery),
    DropTable(DropTableQuery),
    DropGraph(DropGraphQuery),
    DropVector(DropVectorQuery),
    DropDocument(DropDocumentQuery),
    DropKv(DropKvQuery),
    DropCollection(DropCollectionQuery),
    Truncate(TruncateQuery),
    AlterTable(AlterTableQuery),
    CreateIndex(CreateIndexQuery),
    DropIndex(DropIndexQuery),
    CreateTimeSeries(CreateTimeSeriesQuery),
    DropTimeSeries(DropTimeSeriesQuery),
    CreateQueue(CreateQueueQuery),
    AlterQueue(AlterQueueQuery),
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
    CreateMigration(CreateMigrationQuery),
    ApplyMigration(ApplyMigrationQuery),
    RollbackMigration(RollbackMigrationQuery),
    ExplainMigration(ExplainMigrationQuery),
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum SqlAdminCommand {
    SetConfig { key: String, value: Value },
    ShowConfig { prefix: Option<String> },
    SetSecret { key: String, value: Value },
    DeleteSecret { key: String },
    ShowSecrets { prefix: Option<String> },
    SetTenant(Option<String>),
    ShowTenant,
    TransactionControl(TxnControl),
    Maintenance(MaintenanceCommand),
    Grant(GrantStmt),
    Revoke(RevokeStmt),
    AlterUser(AlterUserStmt),
    IamPolicy(QueryExpr),
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
            SqlStatement::Schema(SqlSchemaCommand::CreateCollection(query)) => {
                SqlCommand::CreateCollection(query)
            }
            SqlStatement::Schema(SqlSchemaCommand::CreateVector(query)) => {
                SqlCommand::CreateVector(query)
            }
            SqlStatement::Schema(SqlSchemaCommand::DropTable(query)) => {
                SqlCommand::DropTable(query)
            }
            SqlStatement::Schema(SqlSchemaCommand::DropGraph(query)) => {
                SqlCommand::DropGraph(query)
            }
            SqlStatement::Schema(SqlSchemaCommand::DropVector(query)) => {
                SqlCommand::DropVector(query)
            }
            SqlStatement::Schema(SqlSchemaCommand::DropDocument(query)) => {
                SqlCommand::DropDocument(query)
            }
            SqlStatement::Schema(SqlSchemaCommand::DropKv(query)) => SqlCommand::DropKv(query),
            SqlStatement::Schema(SqlSchemaCommand::DropCollection(query)) => {
                SqlCommand::DropCollection(query)
            }
            SqlStatement::Schema(SqlSchemaCommand::Truncate(query)) => SqlCommand::Truncate(query),
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
            SqlStatement::Schema(SqlSchemaCommand::AlterQueue(query)) => {
                SqlCommand::AlterQueue(query)
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
            SqlStatement::Admin(SqlAdminCommand::SetSecret { key, value }) => {
                SqlCommand::SetSecret { key, value }
            }
            SqlStatement::Admin(SqlAdminCommand::DeleteSecret { key }) => {
                SqlCommand::DeleteSecret { key }
            }
            SqlStatement::Admin(SqlAdminCommand::ShowSecrets { prefix }) => {
                SqlCommand::ShowSecrets { prefix }
            }
            SqlStatement::Admin(SqlAdminCommand::SetTenant(value)) => SqlCommand::SetTenant(value),
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
            SqlStatement::Admin(SqlAdminCommand::Grant(s)) => SqlCommand::Grant(s),
            SqlStatement::Admin(SqlAdminCommand::Revoke(s)) => SqlCommand::Revoke(s),
            SqlStatement::Admin(SqlAdminCommand::AlterUser(s)) => SqlCommand::AlterUser(s),
            SqlStatement::Admin(SqlAdminCommand::IamPolicy(e)) => SqlCommand::IamPolicy(e),
            SqlStatement::Schema(SqlSchemaCommand::CreateMigration(q)) => {
                SqlCommand::CreateMigration(q)
            }
            SqlStatement::Schema(SqlSchemaCommand::ApplyMigration(q)) => {
                SqlCommand::ApplyMigration(q)
            }
            SqlStatement::Schema(SqlSchemaCommand::RollbackMigration(q)) => {
                SqlCommand::RollbackMigration(q)
            }
            SqlStatement::Schema(SqlSchemaCommand::ExplainMigration(q)) => {
                SqlCommand::ExplainMigration(q)
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
            FrontendStatement::QueueSelect(query) => QueryExpr::QueueSelect(query),
            FrontendStatement::QueueCommand(command) => QueryExpr::QueueCommand(command),
            FrontendStatement::EventsBackfill(query) => QueryExpr::EventsBackfill(query),
            FrontendStatement::EventsBackfillStatus { collection } => {
                QueryExpr::EventsBackfillStatus { collection }
            }
            FrontendStatement::TreeCommand(command) => QueryExpr::TreeCommand(command),
            FrontendStatement::ProbabilisticCommand(command) => {
                QueryExpr::ProbabilisticCommand(command)
            }
            FrontendStatement::KvCommand(command) => QueryExpr::KvCommand(command),
            FrontendStatement::ConfigCommand(command) => QueryExpr::ConfigCommand(command),
        }
    }
}

pub fn parse_frontend(input: &str) -> Result<FrontendStatement, ParseError> {
    let mut parser = Parser::new(input)?;
    let statement = parser.parse_frontend_statement()?;
    if !parser.check(&Token::Eof) {
        return Err(ParseError::new(
            // F-05: `Token::Ident` / `Token::String` / `Token::JsonLiteral`
            // Display arms emit raw user bytes. Render via `{:?}` so
            // embedded CR/LF/NUL/quotes are escaped before the message
            // reaches downstream JSON / audit / log / gRPC sinks.
            format!("Unexpected token after query: {:?}", parser.current.token),
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
            SqlCommand::CreateCollection(query) => QueryExpr::CreateCollection(query),
            SqlCommand::CreateVector(query) => QueryExpr::CreateVector(query),
            SqlCommand::DropTable(query) => QueryExpr::DropTable(query),
            SqlCommand::DropGraph(query) => QueryExpr::DropGraph(query),
            SqlCommand::DropVector(query) => QueryExpr::DropVector(query),
            SqlCommand::DropDocument(query) => QueryExpr::DropDocument(query),
            SqlCommand::DropKv(query) => QueryExpr::DropKv(query),
            SqlCommand::DropCollection(query) => QueryExpr::DropCollection(query),
            SqlCommand::Truncate(query) => QueryExpr::Truncate(query),
            SqlCommand::AlterTable(query) => QueryExpr::AlterTable(query),
            SqlCommand::CreateIndex(query) => QueryExpr::CreateIndex(query),
            SqlCommand::DropIndex(query) => QueryExpr::DropIndex(query),
            SqlCommand::CreateTimeSeries(query) => QueryExpr::CreateTimeSeries(query),
            SqlCommand::DropTimeSeries(query) => QueryExpr::DropTimeSeries(query),
            SqlCommand::CreateQueue(query) => QueryExpr::CreateQueue(query),
            SqlCommand::AlterQueue(query) => QueryExpr::AlterQueue(query),
            SqlCommand::DropQueue(query) => QueryExpr::DropQueue(query),
            SqlCommand::CreateTree(query) => QueryExpr::CreateTree(query),
            SqlCommand::DropTree(query) => QueryExpr::DropTree(query),
            SqlCommand::Probabilistic(command) => QueryExpr::ProbabilisticCommand(command),
            SqlCommand::SetConfig { key, value } => QueryExpr::SetConfig { key, value },
            SqlCommand::ShowConfig { prefix } => QueryExpr::ShowConfig { prefix },
            SqlCommand::SetSecret { key, value } => QueryExpr::SetSecret { key, value },
            SqlCommand::DeleteSecret { key } => QueryExpr::DeleteSecret { key },
            SqlCommand::ShowSecrets { prefix } => QueryExpr::ShowSecrets { prefix },
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
            SqlCommand::Grant(s) => QueryExpr::Grant(s),
            SqlCommand::Revoke(s) => QueryExpr::Revoke(s),
            SqlCommand::AlterUser(s) => QueryExpr::AlterUser(s),
            SqlCommand::IamPolicy(e) => e,
            SqlCommand::CreateMigration(q) => QueryExpr::CreateMigration(q),
            SqlCommand::ApplyMigration(q) => QueryExpr::ApplyMigration(q),
            SqlCommand::RollbackMigration(q) => QueryExpr::RollbackMigration(q),
            SqlCommand::ExplainMigration(q) => QueryExpr::ExplainMigration(q),
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
            SqlCommand::CreateCollection(query) => {
                SqlStatement::Schema(SqlSchemaCommand::CreateCollection(query))
            }
            SqlCommand::CreateVector(query) => {
                SqlStatement::Schema(SqlSchemaCommand::CreateVector(query))
            }
            SqlCommand::DropTable(query) => {
                SqlStatement::Schema(SqlSchemaCommand::DropTable(query))
            }
            SqlCommand::DropGraph(query) => {
                SqlStatement::Schema(SqlSchemaCommand::DropGraph(query))
            }
            SqlCommand::DropVector(query) => {
                SqlStatement::Schema(SqlSchemaCommand::DropVector(query))
            }
            SqlCommand::DropDocument(query) => {
                SqlStatement::Schema(SqlSchemaCommand::DropDocument(query))
            }
            SqlCommand::DropKv(query) => SqlStatement::Schema(SqlSchemaCommand::DropKv(query)),
            SqlCommand::DropCollection(query) => {
                SqlStatement::Schema(SqlSchemaCommand::DropCollection(query))
            }
            SqlCommand::Truncate(query) => SqlStatement::Schema(SqlSchemaCommand::Truncate(query)),
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
            SqlCommand::AlterQueue(query) => {
                SqlStatement::Schema(SqlSchemaCommand::AlterQueue(query))
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
            SqlCommand::SetSecret { key, value } => {
                SqlStatement::Admin(SqlAdminCommand::SetSecret { key, value })
            }
            SqlCommand::DeleteSecret { key } => {
                SqlStatement::Admin(SqlAdminCommand::DeleteSecret { key })
            }
            SqlCommand::ShowSecrets { prefix } => {
                SqlStatement::Admin(SqlAdminCommand::ShowSecrets { prefix })
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
            SqlCommand::Grant(s) => SqlStatement::Admin(SqlAdminCommand::Grant(s)),
            SqlCommand::Revoke(s) => SqlStatement::Admin(SqlAdminCommand::Revoke(s)),
            SqlCommand::AlterUser(s) => SqlStatement::Admin(SqlAdminCommand::AlterUser(s)),
            SqlCommand::IamPolicy(e) => SqlStatement::Admin(SqlAdminCommand::IamPolicy(e)),
            SqlCommand::CreateMigration(q) => {
                SqlStatement::Schema(SqlSchemaCommand::CreateMigration(q))
            }
            SqlCommand::ApplyMigration(q) => {
                SqlStatement::Schema(SqlSchemaCommand::ApplyMigration(q))
            }
            SqlCommand::RollbackMigration(q) => {
                SqlStatement::Schema(SqlSchemaCommand::RollbackMigration(q))
            }
            SqlCommand::ExplainMigration(q) => {
                SqlStatement::Schema(SqlSchemaCommand::ExplainMigration(q))
            }
        }
    }
}

impl<'a> Parser<'a> {
    fn parse_events_command(&mut self) -> Result<QueryExpr, ParseError> {
        self.expect_ident()?; // EVENTS
        if self.consume_ident_ci("STATUS")? {
            let mut query = TableQuery::new("red.subscriptions");
            let collection = match self.peek().clone() {
                Token::Ident(name) => {
                    self.advance()?;
                    Some(name)
                }
                Token::String(name) => {
                    self.advance()?;
                    Some(name)
                }
                _ => None,
            };
            self.parse_table_clauses(&mut query)?;
            if let Some(collection) = collection {
                let filter = Filter::compare(
                    FieldRef::column("red.subscriptions", "collection"),
                    CompareOp::Eq,
                    Value::text(collection),
                );
                let expr = filter_to_expr(&filter);
                query.where_expr = Some(match query.where_expr.take() {
                    Some(existing) => Expr::binop(BinOp::And, existing, expr),
                    None => expr,
                });
                query.filter = Some(match query.filter.take() {
                    Some(existing) => existing.and(filter),
                    None => filter,
                });
            }
            return Ok(QueryExpr::Table(query));
        }

        if !self.consume_ident_ci("BACKFILL")? {
            return Err(ParseError::expected(
                vec!["BACKFILL", "STATUS"],
                self.peek(),
                self.position(),
            ));
        }

        if self.consume_ident_ci("STATUS")? {
            let collection = self.expect_ident()?;
            return Ok(QueryExpr::EventsBackfillStatus { collection });
        }

        let collection = self.expect_ident()?;
        let where_filter = if self.consume(&Token::Where)? {
            let mut parts = Vec::new();
            while !self.check(&Token::Eof) && !self.check(&Token::To) {
                parts.push(self.peek().to_string());
                self.advance()?;
            }
            if parts.is_empty() {
                return Err(ParseError::expected(
                    vec!["predicate"],
                    self.peek(),
                    self.position(),
                ));
            }
            Some(parts.join(" "))
        } else {
            None
        };

        self.expect(Token::To)?;
        let target_queue = self.expect_ident()?;
        let limit = if self.consume(&Token::Limit)? {
            Some(self.parse_positive_integer("LIMIT")? as u64)
        } else {
            None
        };

        Ok(QueryExpr::EventsBackfill(EventsBackfillQuery {
            collection,
            where_filter,
            target_queue,
            limit,
        }))
    }

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
            // Option keys frequently collide with reserved words
            // (`path`, `format`, `delimiter`, `header`, …) — accept
            // the keyword form and lowercase it so downstream
            // option-name matching stays case-insensitive.
            let was_ident = matches!(self.peek(), Token::Ident(_));
            let raw = self.expect_ident_or_keyword()?;
            let key = if was_ident {
                raw
            } else {
                raw.to_ascii_lowercase()
            };
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
            Token::Select => match self.parse_select_query()? {
                QueryExpr::Table(query) => Ok(FrontendStatement::Sql(SqlStatement::Query(
                    SqlQuery::Select(query),
                ))),
                QueryExpr::QueueSelect(query) => Ok(FrontendStatement::QueueSelect(query)),
                other => Err(ParseError::new(
                    format!("internal: SELECT produced unexpected query kind {other:?}"),
                    self.position(),
                )),
            },
            Token::From
            | Token::Insert
            | Token::Update
            | Token::Truncate
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
            Token::Explain => {
                if matches!(
                    self.peek_next()?,
                    Token::Ident(name) if name.eq_ignore_ascii_case("ASK")
                ) {
                    match self.parse_explain_ask_query()? {
                        QueryExpr::Ask(query) => Ok(FrontendStatement::Ask(query)),
                        other => Err(ParseError::new(
                            format!(
                                "internal: EXPLAIN ASK produced unexpected query kind {other:?}"
                            ),
                            self.position(),
                        )),
                    }
                } else {
                    self.parse_sql_statement().map(FrontendStatement::Sql)
                }
            }
            Token::Ident(name) if name.eq_ignore_ascii_case("SHOW") => {
                self.parse_sql_statement().map(FrontendStatement::Sql)
            }
            Token::Ident(name)
                if name.eq_ignore_ascii_case("GRANT")
                    || name.eq_ignore_ascii_case("REVOKE")
                    || name.eq_ignore_ascii_case("SIMULATE")
                    || name.eq_ignore_ascii_case("APPLY") =>
            {
                self.parse_sql_statement().map(FrontendStatement::Sql)
            }
            Token::Ident(name) if name.eq_ignore_ascii_case("WATCH") => {
                self.advance()?;
                if matches!(
                    self.peek(),
                    Token::Ident(name) if name.eq_ignore_ascii_case("CONFIG")
                ) {
                    match self.parse_config_watch_after_watch()? {
                        QueryExpr::ConfigCommand(command) => {
                            Ok(FrontendStatement::ConfigCommand(command))
                        }
                        other => Err(ParseError::new(
                            format!(
                                "internal: WATCH CONFIG produced unexpected query kind {other:?}"
                            ),
                            self.position(),
                        )),
                    }
                } else if matches!(
                    self.peek(),
                    Token::Ident(name) if name.eq_ignore_ascii_case("VAULT")
                ) {
                    match self.parse_vault_watch_after_watch()? {
                        QueryExpr::KvCommand(command) => Ok(FrontendStatement::KvCommand(command)),
                        other => Err(ParseError::new(
                            format!(
                                "internal: WATCH VAULT produced unexpected query kind {other:?}"
                            ),
                            self.position(),
                        )),
                    }
                } else {
                    match self.parse_kv_watch(crate::catalog::CollectionModel::Kv)? {
                        QueryExpr::KvCommand(command) => Ok(FrontendStatement::KvCommand(command)),
                        other => Err(ParseError::new(
                            format!("internal: WATCH produced unexpected query kind {other:?}"),
                            self.position(),
                        )),
                    }
                }
            }
            Token::List => {
                self.advance()?;
                if matches!(
                    self.peek(),
                    Token::Ident(name) if name.eq_ignore_ascii_case("CONFIG")
                ) {
                    match self.parse_config_list_after_list()? {
                        QueryExpr::ConfigCommand(command) => {
                            Ok(FrontendStatement::ConfigCommand(command))
                        }
                        other => Err(ParseError::new(
                            format!(
                                "internal: LIST CONFIG produced unexpected query kind {other:?}"
                            ),
                            self.position(),
                        )),
                    }
                } else if matches!(
                    self.peek(),
                    Token::Ident(name) if name.eq_ignore_ascii_case("VAULT")
                ) {
                    match self.parse_vault_list_after_list()? {
                        QueryExpr::KvCommand(command) => Ok(FrontendStatement::KvCommand(command)),
                        other => Err(ParseError::new(
                            format!(
                                "internal: LIST VAULT produced unexpected query kind {other:?}"
                            ),
                            self.position(),
                        )),
                    }
                } else {
                    Err(ParseError::expected(
                        vec!["CONFIG", "VAULT"],
                        self.peek(),
                        self.position(),
                    ))
                }
            }
            Token::Ident(name) if name.eq_ignore_ascii_case("LIST") => {
                self.advance()?;
                if matches!(
                    self.peek(),
                    Token::Ident(name) if name.eq_ignore_ascii_case("CONFIG")
                ) {
                    match self.parse_config_list_after_list()? {
                        QueryExpr::ConfigCommand(command) => {
                            Ok(FrontendStatement::ConfigCommand(command))
                        }
                        other => Err(ParseError::new(
                            format!(
                                "internal: LIST CONFIG produced unexpected query kind {other:?}"
                            ),
                            self.position(),
                        )),
                    }
                } else if matches!(
                    self.peek(),
                    Token::Ident(name) if name.eq_ignore_ascii_case("VAULT")
                ) {
                    match self.parse_vault_list_after_list()? {
                        QueryExpr::KvCommand(command) => Ok(FrontendStatement::KvCommand(command)),
                        other => Err(ParseError::new(
                            format!(
                                "internal: LIST VAULT produced unexpected query kind {other:?}"
                            ),
                            self.position(),
                        )),
                    }
                } else {
                    Err(ParseError::expected(
                        vec!["CONFIG", "VAULT"],
                        self.peek(),
                        self.position(),
                    ))
                }
            }
            Token::Ident(name) if name.eq_ignore_ascii_case("INVALIDATE") => {
                if matches!(
                    self.peek_next()?,
                    Token::Ident(next) if next.eq_ignore_ascii_case("CONFIG")
                ) {
                    match self.parse_config_command()? {
                        QueryExpr::ConfigCommand(command) => {
                            Ok(FrontendStatement::ConfigCommand(command))
                        }
                        other => Err(ParseError::new(
                            format!("internal: CONFIG produced unexpected query kind {other:?}"),
                            self.position(),
                        )),
                    }
                } else {
                    self.advance()?;
                    match self.parse_kv_invalidate_tags_after_invalidate()? {
                        QueryExpr::KvCommand(command) => Ok(FrontendStatement::KvCommand(command)),
                        other => Err(ParseError::new(
                            format!(
                                "internal: INVALIDATE produced unexpected query kind {other:?}"
                            ),
                            self.position(),
                        )),
                    }
                }
            }
            Token::Attach | Token::Detach => self.parse_sql_statement().map(FrontendStatement::Sql),
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
            Token::Ident(name) if name.eq_ignore_ascii_case("UNSEAL") => {
                match self.parse_unseal_vault_command()? {
                    QueryExpr::KvCommand(command) => Ok(FrontendStatement::KvCommand(command)),
                    other => Err(ParseError::new(
                        format!("internal: UNSEAL VAULT produced unexpected query kind {other:?}"),
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
            Token::Ident(name) if name.eq_ignore_ascii_case("EVENTS") => {
                match self.parse_events_command()? {
                    QueryExpr::Table(query) => Ok(FrontendStatement::Sql(SqlStatement::Query(
                        SqlQuery::Select(query),
                    ))),
                    QueryExpr::EventsBackfill(query) => {
                        Ok(FrontendStatement::EventsBackfill(query))
                    }
                    QueryExpr::EventsBackfillStatus { collection } => {
                        Ok(FrontendStatement::EventsBackfillStatus { collection })
                    }
                    other => Err(ParseError::new(
                        format!("internal: EVENTS produced unexpected query kind {other:?}"),
                        self.position(),
                    )),
                }
            }
            Token::Kv => match self.parse_kv_command()? {
                QueryExpr::KvCommand(command) => Ok(FrontendStatement::KvCommand(command)),
                other => Err(ParseError::new(
                    format!("internal: KV produced unexpected query kind {other:?}"),
                    self.position(),
                )),
            },
            Token::Delete => {
                if matches!(
                    self.peek_next()?,
                    Token::Ident(name) if name.eq_ignore_ascii_case("CONFIG")
                ) {
                    match self.parse_config_command()? {
                        QueryExpr::ConfigCommand(command) => {
                            Ok(FrontendStatement::ConfigCommand(command))
                        }
                        other => Err(ParseError::new(
                            format!("internal: CONFIG produced unexpected query kind {other:?}"),
                            self.position(),
                        )),
                    }
                } else if matches!(
                    self.peek_next()?,
                    Token::Ident(name) if name.eq_ignore_ascii_case("VAULT")
                ) {
                    match self.parse_vault_lifecycle_command()? {
                        QueryExpr::KvCommand(command) => Ok(FrontendStatement::KvCommand(command)),
                        other => Err(ParseError::new(
                            format!("internal: VAULT produced unexpected query kind {other:?}"),
                            self.position(),
                        )),
                    }
                } else {
                    self.parse_sql_statement().map(FrontendStatement::Sql)
                }
            }
            Token::Add => match self.parse_config_command()? {
                QueryExpr::ConfigCommand(command) => Ok(FrontendStatement::ConfigCommand(command)),
                other => Err(ParseError::new(
                    format!("internal: CONFIG produced unexpected query kind {other:?}"),
                    self.position(),
                )),
            },
            Token::Purge => match self.parse_vault_lifecycle_command()? {
                QueryExpr::KvCommand(command) => Ok(FrontendStatement::KvCommand(command)),
                other => Err(ParseError::new(
                    format!("internal: VAULT produced unexpected query kind {other:?}"),
                    self.position(),
                )),
            },
            Token::Ident(name)
                if name.eq_ignore_ascii_case("PUT")
                    || name.eq_ignore_ascii_case("GET")
                    || name.eq_ignore_ascii_case("RESOLVE")
                    || name.eq_ignore_ascii_case("ROTATE")
                    || name.eq_ignore_ascii_case("HISTORY")
                    || name.eq_ignore_ascii_case("PURGE")
                    || name.eq_ignore_ascii_case("INCR")
                    || name.eq_ignore_ascii_case("DECR")
                    || name.eq_ignore_ascii_case("INVALIDATE") =>
            {
                if matches!(
                    self.peek_next()?,
                    Token::Ident(next) if next.eq_ignore_ascii_case("VAULT")
                ) {
                    match self.parse_vault_lifecycle_command()? {
                        QueryExpr::KvCommand(command) => Ok(FrontendStatement::KvCommand(command)),
                        other => Err(ParseError::new(
                            format!("internal: VAULT produced unexpected query kind {other:?}"),
                            self.position(),
                        )),
                    }
                } else {
                    match self.parse_config_command()? {
                        QueryExpr::ConfigCommand(command) => {
                            Ok(FrontendStatement::ConfigCommand(command))
                        }
                        other => Err(ParseError::new(
                            format!("internal: CONFIG produced unexpected query kind {other:?}"),
                            self.position(),
                        )),
                    }
                }
            }
            Token::Ident(name) if name.eq_ignore_ascii_case("VAULT") => {
                match self.parse_vault_command()? {
                    QueryExpr::KvCommand(command) => Ok(FrontendStatement::KvCommand(command)),
                    other => Err(ParseError::new(
                        format!("internal: VAULT produced unexpected query kind {other:?}"),
                        self.position(),
                    )),
                }
            }
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
            Token::Ident(name) if name.eq_ignore_ascii_case("EVENTS") => self
                .parse_sql_command()
                .map(SqlCommand::into_statement)
                .map(FrontendStatement::Sql),
            other => Err(ParseError::expected(
                vec![
                    "SELECT", "MATCH", "PATH", "FROM", "VECTOR", "HYBRID", "INSERT", "UPDATE",
                    "DELETE", "TRUNCATE", "CREATE", "DROP", "ALTER", "GRAPH", "SEARCH", "ASK",
                    "QUEUE", "EVENTS", "KV", "HLL", "TREE", "SKETCH", "FILTER", "SET", "SHOW",
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

    fn parse_dotted_admin_path(&mut self, lowercase: bool) -> Result<String, ParseError> {
        let mut path = self.expect_ident()?;
        while self.consume(&Token::Dot)? {
            let next = self.expect_ident_or_keyword()?;
            path = format!("{path}.{next}");
        }
        Ok(if lowercase {
            path.to_ascii_lowercase()
        } else {
            path
        })
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
            Token::Delete => {
                if matches!(self.peek_next()?, Token::Ident(n) if n.eq_ignore_ascii_case("SECRET"))
                {
                    self.advance()?; // DELETE
                    self.advance()?; // SECRET
                    let key = self.parse_dotted_admin_path(true)?;
                    Ok(SqlCommand::DeleteSecret { key })
                } else {
                    match self.parse_delete_query()? {
                        QueryExpr::Delete(query) => Ok(SqlCommand::Delete(query)),
                        other => Err(ParseError::new(
                            format!("internal: DELETE produced unexpected query kind {other:?}"),
                            self.position(),
                        )),
                    }
                }
            }
            Token::Truncate => {
                self.advance()?;
                let model = if self.consume(&Token::Table)? {
                    Some(CollectionModel::Table)
                } else if self.consume(&Token::Graph)? {
                    Some(CollectionModel::Graph)
                } else if self.consume(&Token::Vector)? {
                    Some(CollectionModel::Vector)
                } else if self.consume(&Token::Document)? {
                    Some(CollectionModel::Document)
                } else if self.consume(&Token::Timeseries)? {
                    Some(CollectionModel::TimeSeries)
                } else if self.consume(&Token::Kv)? {
                    Some(CollectionModel::Kv)
                } else if self.consume(&Token::Queue)? {
                    Some(CollectionModel::Queue)
                } else if self.consume(&Token::Collection)? {
                    None
                } else {
                    return Err(ParseError::expected(
                        vec![
                            "TABLE",
                            "GRAPH",
                            "VECTOR",
                            "DOCUMENT",
                            "TIMESERIES",
                            "KV",
                            "QUEUE",
                            "COLLECTION",
                        ],
                        self.peek(),
                        self.position(),
                    ));
                };
                match self.parse_truncate_body(model)? {
                    QueryExpr::Truncate(query) => Ok(SqlCommand::Truncate(query)),
                    other => Err(ParseError::new(
                        format!("internal: TRUNCATE produced unexpected kind {other:?}"),
                        self.position(),
                    )),
                }
            }
            Token::Explain => {
                // Peek ahead: EXPLAIN MIGRATION name → ExplainMigration
                // EXPLAIN ALTER FOR ... → ExplainAlter (existing path)
                if matches!(self.peek_next()?, Token::Ident(n) if n.eq_ignore_ascii_case("MIGRATION"))
                {
                    self.advance()?; // consume EXPLAIN
                    match self.parse_explain_migration_after_keyword()? {
                        QueryExpr::ExplainMigration(q) => Ok(SqlCommand::ExplainMigration(q)),
                        other => Err(ParseError::new(
                            format!(
                                "internal: EXPLAIN MIGRATION produced unexpected kind {other:?}"
                            ),
                            self.position(),
                        )),
                    }
                } else {
                    match self.parse_explain_alter_query()? {
                        QueryExpr::ExplainAlter(query) => Ok(SqlCommand::ExplainAlter(query)),
                        other => Err(ParseError::new(
                            format!("internal: EXPLAIN produced unexpected query kind {other:?}"),
                            self.position(),
                        )),
                    }
                }
            }
            Token::Create => {
                let pos = self.position();
                self.advance()?;

                // CREATE [OR REPLACE] [MATERIALIZED] VIEW [IF NOT EXISTS] name AS <select>
                // Detect the VIEW path early so OR REPLACE / MATERIALIZED modifiers
                // don't collide with other CREATE variants (TABLE, INDEX, etc.).
                let mut or_replace = false;
                if self.consume(&Token::Or)? || self.consume_ident_ci("OR")? {
                    let _ = self.consume_ident_ci("REPLACE")?;
                    or_replace = true;
                }
                let materialized = self.consume(&Token::Materialized)?;
                if self.check(&Token::View) {
                    self.advance()?;
                    let if_not_exists = self.match_if_not_exists()?;
                    let name = self.expect_ident()?;
                    // Accept `AS` — the lexer promotes it to `Token::As`
                    // (keyword) but some paths still see it as an ident.
                    if !self.consume(&Token::As)? && !self.consume_ident_ci("AS")? {
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
                } else if self.check(&Token::Graph) {
                    self.advance()?;
                    match self.parse_create_collection_model_body(CollectionModel::Graph)? {
                        QueryExpr::CreateTable(query) => Ok(SqlCommand::CreateTable(query)),
                        other => Err(ParseError::new(
                            format!("internal: CREATE GRAPH produced unexpected kind {other:?}"),
                            self.position(),
                        )),
                    }
                } else if self.check(&Token::Document) {
                    self.advance()?;
                    match self.parse_create_collection_model_body(CollectionModel::Document)? {
                        QueryExpr::CreateTable(query) => Ok(SqlCommand::CreateTable(query)),
                        other => Err(ParseError::new(
                            format!("internal: CREATE DOCUMENT produced unexpected kind {other:?}"),
                            self.position(),
                        )),
                    }
                } else if self.check(&Token::Vector) {
                    self.advance()?;
                    match self.parse_create_vector_body()? {
                        QueryExpr::CreateVector(query) => Ok(SqlCommand::CreateVector(query)),
                        other => Err(ParseError::new(
                            format!("internal: CREATE VECTOR produced unexpected kind {other:?}"),
                            self.position(),
                        )),
                    }
                } else if self.check(&Token::Collection) {
                    self.advance()?;
                    match self.parse_create_collection_body()? {
                        QueryExpr::CreateCollection(query) => {
                            Ok(SqlCommand::CreateCollection(query))
                        }
                        other => Err(ParseError::new(
                            format!(
                                "internal: CREATE COLLECTION produced unexpected kind {other:?}"
                            ),
                            self.position(),
                        )),
                    }
                } else if self.check(&Token::Kv) {
                    self.advance()?;
                    match self.parse_create_keyed_body(CollectionModel::Kv)? {
                        QueryExpr::CreateTable(query) => Ok(SqlCommand::CreateTable(query)),
                        other => Err(ParseError::new(
                            format!("internal: CREATE KV produced unexpected kind {other:?}"),
                            self.position(),
                        )),
                    }
                } else if self.consume_ident_ci("CONFIG")? {
                    match self.parse_create_keyed_body(CollectionModel::Config)? {
                        QueryExpr::CreateTable(query) => Ok(SqlCommand::CreateTable(query)),
                        other => Err(ParseError::new(
                            format!("internal: CREATE CONFIG produced unexpected kind {other:?}"),
                            self.position(),
                        )),
                    }
                } else if self.consume_ident_ci("VAULT")? {
                    match self.parse_create_keyed_body(CollectionModel::Vault)? {
                        QueryExpr::CreateTable(query) => Ok(SqlCommand::CreateTable(query)),
                        other => Err(ParseError::new(
                            format!("internal: CREATE VAULT produced unexpected kind {other:?}"),
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
                } else if matches!(self.peek(), Token::Ident(s) if s.eq_ignore_ascii_case("HYPERTABLE"))
                {
                    self.advance()?;
                    match self.parse_create_hypertable_body()? {
                        QueryExpr::CreateTimeSeries(query) => {
                            Ok(SqlCommand::CreateTimeSeries(query))
                        }
                        other => Err(ParseError::new(
                            format!(
                                "internal: CREATE HYPERTABLE produced unexpected kind {other:?}"
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
                    // Two forms share the leading `CREATE POLICY` tokens:
                    //   * IAM:   CREATE POLICY '<id>' AS '<json>'          (string literal id)
                    //   * RLS:   CREATE POLICY <name> ON <target> ...      (bare ident name)
                    // Disambiguate by peeking the token after POLICY.
                    self.advance()?;
                    if matches!(self.peek(), Token::String(_)) {
                        // IAM form — short-circuit out of the SQL command stack.
                        let expr = self.parse_create_iam_policy_after_keywords()?;
                        // Inline command-wrapping: produce a synthetic SqlCommand by
                        // routing through a generic IAM admin holder. We don't
                        // have a dedicated SqlCommand variant for IAM yet, so we
                        // bounce through the existing Grant-shaped Admin slot
                        // which expects no further tokens.
                        return Ok(SqlCommand::IamPolicy(expr));
                    }
                    let name = self.expect_ident()?;
                    self.expect(Token::On)?;

                    let (target_kind, table) = {
                        use crate::storage::query::ast::PolicyTargetKind;
                        let kw = match self.peek() {
                            Token::Ident(s) => Some(s.to_ascii_uppercase()),
                            _ => None,
                        };
                        let kind = kw.as_deref().and_then(|k| match k {
                            "NODES" => Some(PolicyTargetKind::Nodes),
                            "EDGES" => Some(PolicyTargetKind::Edges),
                            "VECTORS" => Some(PolicyTargetKind::Vectors),
                            "MESSAGES" => Some(PolicyTargetKind::Messages),
                            "POINTS" => Some(PolicyTargetKind::Points),
                            "DOCUMENTS" => Some(PolicyTargetKind::Documents),
                            _ => None,
                        });
                        if let Some(k) = kind {
                            self.advance()?;
                            self.expect(Token::Of)?;
                            let coll = self.expect_ident()?;
                            (k, coll)
                        } else {
                            let coll = self.expect_ident()?;
                            (PolicyTargetKind::Table, coll)
                        }
                    };

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

                    Ok(SqlCommand::CreatePolicy(CreatePolicyQuery {
                        name,
                        table,
                        action,
                        role,
                        using: Box::new(filter),
                        target_kind,
                    }))
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
                    Ok(SqlCommand::CreateServer(CreateServerQuery {
                        name,
                        wrapper,
                        options,
                        if_not_exists,
                    }))
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
                    Ok(SqlCommand::CreateForeignTable(CreateForeignTableQuery {
                        name,
                        server,
                        columns,
                        options,
                        if_not_exists,
                    }))
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
                            // Accept `START 100` or `START WITH 100`.
                            let _ = self.consume(&Token::With)? || self.consume_ident_ci("WITH")?;
                            start = self.parse_integer()?;
                        } else if self.consume(&Token::Increment)? {
                            // Accept `INCREMENT 5` or `INCREMENT BY 5`.
                            let _ = self.consume(&Token::By)? || self.consume_ident_ci("BY")?;
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
                } else if matches!(self.peek(), Token::Ident(n) if n.eq_ignore_ascii_case("MIGRATION"))
                {
                    self.advance()?; // consume MIGRATION
                    match self.parse_create_migration_body()? {
                        QueryExpr::CreateMigration(q) => Ok(SqlCommand::CreateMigration(q)),
                        other => Err(ParseError::new(
                            format!(
                                "internal: CREATE MIGRATION produced unexpected kind {other:?}"
                            ),
                            self.position(),
                        )),
                    }
                } else if let Some(err) =
                    ParseError::unsupported_recognized_token(self.peek(), self.position())
                {
                    Err(err)
                } else {
                    Err(ParseError::expected(
                        vec![
                            "TABLE",
                            "GRAPH",
                            "VECTOR",
                            "DOCUMENT",
                            "KV",
                            "COLLECTION",
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
                            "MIGRATION",
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
                } else if self.check(&Token::Graph) {
                    self.advance()?;
                    match self.parse_drop_graph_body()? {
                        QueryExpr::DropGraph(query) => Ok(SqlCommand::DropGraph(query)),
                        other => Err(ParseError::new(
                            format!("internal: DROP GRAPH produced unexpected kind {other:?}"),
                            self.position(),
                        )),
                    }
                } else if self.check(&Token::Vector) {
                    self.advance()?;
                    match self.parse_drop_vector_body()? {
                        QueryExpr::DropVector(query) => Ok(SqlCommand::DropVector(query)),
                        other => Err(ParseError::new(
                            format!("internal: DROP VECTOR produced unexpected kind {other:?}"),
                            self.position(),
                        )),
                    }
                } else if self.check(&Token::Document) {
                    self.advance()?;
                    match self.parse_drop_document_body()? {
                        QueryExpr::DropDocument(query) => Ok(SqlCommand::DropDocument(query)),
                        other => Err(ParseError::new(
                            format!("internal: DROP DOCUMENT produced unexpected kind {other:?}"),
                            self.position(),
                        )),
                    }
                } else if self.check(&Token::Kv) {
                    self.advance()?;
                    match self.parse_drop_kv_body()? {
                        QueryExpr::DropKv(query) => Ok(SqlCommand::DropKv(query)),
                        other => Err(ParseError::new(
                            format!("internal: DROP KV produced unexpected kind {other:?}"),
                            self.position(),
                        )),
                    }
                } else if self.consume_ident_ci("CONFIG")? {
                    match self.parse_drop_keyed_body(CollectionModel::Config)? {
                        QueryExpr::DropKv(query) => Ok(SqlCommand::DropKv(query)),
                        other => Err(ParseError::new(
                            format!("internal: DROP CONFIG produced unexpected kind {other:?}"),
                            self.position(),
                        )),
                    }
                } else if self.consume_ident_ci("VAULT")? {
                    match self.parse_drop_keyed_body(CollectionModel::Vault)? {
                        QueryExpr::DropKv(query) => Ok(SqlCommand::DropKv(query)),
                        other => Err(ParseError::new(
                            format!("internal: DROP VAULT produced unexpected kind {other:?}"),
                            self.position(),
                        )),
                    }
                } else if self.check(&Token::Collection) {
                    self.advance()?;
                    match self.parse_drop_collection_body()? {
                        QueryExpr::DropCollection(query) => Ok(SqlCommand::DropCollection(query)),
                        other => Err(ParseError::new(
                            format!("internal: DROP COLLECTION produced unexpected kind {other:?}"),
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
                } else if matches!(self.peek(), Token::Ident(s) if s.eq_ignore_ascii_case("HYPERTABLE"))
                {
                    // DROP HYPERTABLE name reuses the same AST as
                    // DROP TIMESERIES — runtime clears the registry
                    // entry *and* drops the backing collection.
                    self.advance()?;
                    match self.parse_drop_timeseries_body()? {
                        QueryExpr::DropTimeSeries(query) => Ok(SqlCommand::DropTimeSeries(query)),
                        other => Err(ParseError::new(
                            format!("internal: DROP HYPERTABLE produced unexpected kind {other:?}"),
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
                    // Two forms:
                    //   * IAM:   DROP POLICY '<id>'
                    //   * RLS:   DROP POLICY [IF EXISTS] name ON table
                    self.advance()?;
                    if matches!(self.peek(), Token::String(_)) {
                        let expr = self.parse_drop_iam_policy_after_keywords()?;
                        return Ok(SqlCommand::IamPolicy(expr));
                    }
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
                } else if let Some(err) =
                    ParseError::unsupported_recognized_token(self.peek(), self.position())
                {
                    Err(err)
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
            Token::Alter => {
                // Disambiguate ALTER USER / ALTER QUEUE / ALTER TABLE without
                // committing to a path until we've seen the target.
                // We peek the *next* token (without consuming) and
                // dispatch accordingly.
                let next = self.peek_next()?.clone();
                if matches!(next, Token::Ident(ref s) if s.eq_ignore_ascii_case("USER")) {
                    self.advance()?; // consume ALTER
                    let stmt = self.parse_alter_user_statement()?;
                    Ok(SqlCommand::AlterUser(stmt))
                } else if matches!(next, Token::Queue) {
                    self.advance()?; // consume ALTER
                    self.advance()?; // consume QUEUE
                    match self.parse_alter_queue_body()? {
                        QueryExpr::AlterQueue(query) => Ok(SqlCommand::AlterQueue(query)),
                        other => Err(ParseError::new(
                            format!("internal: ALTER QUEUE produced unexpected kind {other:?}"),
                            self.position(),
                        )),
                    }
                } else if matches!(next, Token::Table) {
                    match self.parse_alter_table_query()? {
                        QueryExpr::AlterTable(query) => Ok(SqlCommand::AlterTable(query)),
                        other => Err(ParseError::new(
                            format!(
                                "internal: ALTER TABLE produced unexpected query kind {other:?}"
                            ),
                            self.position(),
                        )),
                    }
                } else if let Some(err) =
                    ParseError::unsupported_recognized_token(&next, self.position())
                {
                    Err(err)
                } else {
                    match self.parse_alter_table_query()? {
                        QueryExpr::AlterTable(query) => Ok(SqlCommand::AlterTable(query)),
                        other => Err(ParseError::new(
                            format!("internal: ALTER produced unexpected query kind {other:?}"),
                            self.position(),
                        )),
                    }
                }
            }
            Token::Ident(name) if name.eq_ignore_ascii_case("GRANT") => {
                let stmt = self.parse_grant_statement()?;
                Ok(SqlCommand::Grant(stmt))
            }
            Token::Ident(name) if name.eq_ignore_ascii_case("REVOKE") => {
                let stmt = self.parse_revoke_statement()?;
                Ok(SqlCommand::Revoke(stmt))
            }
            Token::Ident(name) if name.eq_ignore_ascii_case("EVENTS") => {
                self.advance()?;
                if self.consume_ident_ci("BACKFILL")? {
                    return Err(ParseError::new(
                        "EVENTS BACKFILL STATUS is not implemented; EVENTS BACKFILL runtime is available but durable progress tracking is not"
                            .to_string(),
                        self.position(),
                    ));
                }
                if !self.consume_ident_ci("STATUS")? {
                    return Err(ParseError::expected(
                        vec!["STATUS"],
                        self.peek(),
                        self.position(),
                    ));
                }

                let mut query = TableQuery::new("red.subscriptions");
                let collection = match self.peek().clone() {
                    Token::Ident(name) => {
                        self.advance()?;
                        Some(name)
                    }
                    Token::String(name) => {
                        self.advance()?;
                        Some(name)
                    }
                    _ => None,
                };
                self.parse_table_clauses(&mut query)?;
                if let Some(collection) = collection {
                    let filter = Filter::compare(
                        FieldRef::column("red.subscriptions", "collection"),
                        CompareOp::Eq,
                        Value::text(collection),
                    );
                    let expr = filter_to_expr(&filter);
                    query.where_expr = Some(match query.where_expr.take() {
                        Some(existing) => Expr::binop(BinOp::And, existing, expr),
                        None => expr,
                    });
                    query.filter = Some(match query.filter.take() {
                        Some(existing) => existing.and(filter),
                        None => filter,
                    });
                }
                Ok(SqlCommand::Select(query))
            }
            Token::Attach => {
                let expr = self.parse_attach_policy()?;
                Ok(SqlCommand::IamPolicy(expr))
            }
            Token::Detach => {
                let expr = self.parse_detach_policy()?;
                Ok(SqlCommand::IamPolicy(expr))
            }
            Token::Ident(name) if name.eq_ignore_ascii_case("SIMULATE") => {
                let expr = self.parse_simulate_policy()?;
                Ok(SqlCommand::IamPolicy(expr))
            }
            Token::Set => {
                self.advance()?;
                if self.consume_ident_ci("CONFIG")? {
                    let full_key = self.parse_dotted_admin_path(true)?;
                    self.expect(Token::Eq)?;
                    let value = self.parse_literal_value()?;
                    Ok(SqlCommand::SetConfig {
                        key: full_key,
                        value,
                    })
                } else if self.consume_ident_ci("SECRET")? {
                    let key = self.parse_dotted_admin_path(true)?;
                    self.expect(Token::Eq)?;
                    let value = self.parse_literal_value()?;
                    Ok(SqlCommand::SetSecret { key, value })
                } else if self.consume_ident_ci("TENANT")? {
                    // SET TENANT 'id'  |  SET TENANT = 'id'  |
                    // SET TENANT NULL  |  SET TENANT = NULL
                    let _ = self.consume(&Token::Eq)?;
                    if self.consume_ident_ci("NULL")? {
                        Ok(SqlCommand::SetTenant(None))
                    } else {
                        let value = self.parse_literal_value()?;
                        match value {
                            Value::Text(s) => Ok(SqlCommand::SetTenant(Some(s.to_string()))),
                            Value::Null => Ok(SqlCommand::SetTenant(None)),
                            other => Err(ParseError::new(
                                format!("SET TENANT expects a text literal or NULL, got {other:?}"),
                                self.position(),
                            )),
                        }
                    }
                } else {
                    Err(ParseError::expected(
                        vec!["CONFIG", "SECRET", "TENANT"],
                        self.peek(),
                        self.position(),
                    ))
                }
            }
            Token::Ident(name) if name.eq_ignore_ascii_case("APPLY") => {
                self.advance()?;
                match self.parse_apply_migration()? {
                    QueryExpr::ApplyMigration(q) => Ok(SqlCommand::ApplyMigration(q)),
                    other => Err(ParseError::new(
                        format!("internal: APPLY MIGRATION produced unexpected kind {other:?}"),
                        self.position(),
                    )),
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
                    // Accept dotted prefixes the same way SET CONFIG does
                    // (`SHOW CONFIG durability.mode`), and empty prefix
                    // (`SHOW CONFIG`) for a catalog-wide listing.
                    let prefix = if !self.check(&Token::Eof) {
                        let first = self.expect_ident()?;
                        let mut full = first;
                        while self.consume(&Token::Dot)? {
                            let next = self.expect_ident_or_keyword()?;
                            full = format!("{full}.{next}");
                        }
                        // Match SET CONFIG: lowercase so keyword segments
                        // come out consistent with the stored keys.
                        Some(full.to_ascii_lowercase())
                    } else {
                        None
                    };
                    Ok(SqlCommand::ShowConfig { prefix })
                } else if self.consume_ident_ci("COLLECTIONS")? {
                    let mut query = TableQuery::new("red.collections");
                    let include_internal = if self.consume_ident_ci("INCLUDING")? {
                        if !self.consume_ident_ci("INTERNAL")? {
                            return Err(ParseError::expected(
                                vec!["INTERNAL"],
                                self.peek(),
                                self.position(),
                            ));
                        }
                        true
                    } else {
                        false
                    };
                    self.parse_table_clauses(&mut query)?;
                    if !include_internal {
                        let user_filter = query.filter.take();
                        let hide_internal = crate::storage::query::ast::Filter::Compare {
                            field: FieldRef::column("", "internal"),
                            op: CompareOp::Eq,
                            value: Value::Boolean(false),
                        };
                        query.filter = Some(match user_filter {
                            Some(filter) => filter.and(hide_internal),
                            None => hide_internal,
                        });
                    }
                    Ok(SqlCommand::Select(query))
                } else if self.consume_ident_ci("TABLES")? {
                    Ok(SqlCommand::Select(parse_show_collections_by_model(
                        self, "table",
                    )?))
                } else if self.consume_ident_ci("QUEUES")? {
                    Ok(SqlCommand::Select(parse_show_collections_by_model(
                        self, "queue",
                    )?))
                } else if self.consume(&Token::Vectors)? || self.consume_ident_ci("VECTORS")? {
                    Ok(SqlCommand::Select(parse_show_collections_by_model(
                        self, "vector",
                    )?))
                } else if self.consume_ident_ci("DOCUMENTS")? {
                    Ok(SqlCommand::Select(parse_show_collections_by_model(
                        self, "document",
                    )?))
                } else if self.consume(&Token::Timeseries)?
                    || self.consume_ident_ci("TIMESERIES")?
                {
                    Ok(SqlCommand::Select(parse_show_collections_by_model(
                        self,
                        "timeseries",
                    )?))
                } else if self.consume_ident_ci("GRAPHS")? {
                    Ok(SqlCommand::Select(parse_show_collections_by_model(
                        self, "graph",
                    )?))
                } else if self.consume_ident_ci("CONFIGS")? {
                    Ok(SqlCommand::Select(parse_show_collections_by_model(
                        self, "config",
                    )?))
                } else if self.consume_ident_ci("VAULTS")? {
                    Ok(SqlCommand::Select(parse_show_collections_by_model(
                        self, "vault",
                    )?))
                } else if self.consume(&Token::Kv)?
                    || self.consume_ident_ci("KV")?
                    || self.consume_ident_ci("KVS")?
                {
                    Ok(SqlCommand::Select(parse_show_collections_by_model(
                        self, "kv",
                    )?))
                } else if self.consume(&Token::Schema)? || self.consume_ident_ci("SCHEMA")? {
                    let collection = self.parse_dotted_admin_path(false)?;
                    let mut query = TableQuery::new("red.columns");
                    query.filter = Some(Filter::compare(
                        FieldRef::column("", "collection"),
                        CompareOp::Eq,
                        Value::text(collection),
                    ));
                    Ok(SqlCommand::Select(query))
                } else if self.consume_ident_ci("INDICES")? {
                    let mut query = TableQuery::new("red.indices");
                    if self.consume(&Token::On)? {
                        let collection = self.expect_ident_or_keyword()?;
                        let filter = Filter::Compare {
                            field: FieldRef::column("red.indices", "collection"),
                            op: CompareOp::Eq,
                            value: Value::text(collection),
                        };
                        query.where_expr = Some(filter_to_expr(&filter));
                        query.filter = Some(filter);
                    }
                    self.parse_table_clauses(&mut query)?;
                    Ok(SqlCommand::Select(query))
                } else if self.consume_ident_ci("POLICIES")? {
                    if self.consume(&Token::For)? || self.consume_ident_ci("FOR")? {
                        let principal = self.parse_iam_principal_kind()?;
                        return Ok(SqlCommand::IamPolicy(QueryExpr::ShowPolicies {
                            filter: Some(principal),
                        }));
                    }
                    let mut query = TableQuery::new("red.policies");
                    let collection_filter =
                        if self.consume(&Token::On)? || self.consume_ident_ci("ON")? {
                            let collection = self.parse_dotted_admin_path(false)?;
                            Some(Filter::Compare {
                                field: FieldRef::TableColumn {
                                    table: String::new(),
                                    column: "collection".to_string(),
                                },
                                op: CompareOp::Eq,
                                value: Value::text(collection),
                            })
                        } else {
                            None
                        };
                    self.parse_table_clauses(&mut query)?;
                    if let Some(collection_filter) = collection_filter {
                        let combined = match query.filter.take() {
                            Some(existing) => {
                                Filter::And(Box::new(collection_filter), Box::new(existing))
                            }
                            None => collection_filter,
                        };
                        query.where_expr = Some(filter_to_expr(&combined));
                        query.filter = Some(combined);
                    }
                    Ok(SqlCommand::Select(query))
                } else if self.consume_ident_ci("STATS")? {
                    let mut query = TableQuery::new("red.stats");
                    let collection = match self.peek().clone() {
                        Token::Ident(name) => {
                            self.advance()?;
                            Some(name)
                        }
                        Token::String(name) => {
                            self.advance()?;
                            Some(name)
                        }
                        _ => None,
                    };
                    self.parse_table_clauses(&mut query)?;
                    if let Some(collection) = collection {
                        let filter = Filter::compare(
                            FieldRef::column("red.stats", "collection"),
                            CompareOp::Eq,
                            Value::text(collection),
                        );
                        let expr = filter_to_expr(&filter);
                        query.where_expr = Some(match query.where_expr.take() {
                            Some(existing) => Expr::binop(BinOp::And, existing, expr),
                            None => expr,
                        });
                        query.filter = Some(match query.filter.take() {
                            Some(existing) => existing.and(filter),
                            None => filter,
                        });
                    }
                    Ok(SqlCommand::Select(query))
                } else if self.consume_ident_ci("SAMPLE")? {
                    let mut query = TableQuery::new(&self.expect_ident()?);
                    query.limit = if self.consume(&Token::Limit)? {
                        Some(self.parse_integer()? as u64)
                    } else {
                        Some(10)
                    };
                    Ok(SqlCommand::Select(query))
                } else if self.consume_ident_ci("SECRET")? || self.consume_ident_ci("SECRETS")? {
                    let prefix = if !self.check(&Token::Eof) {
                        Some(self.parse_dotted_admin_path(true)?)
                    } else {
                        None
                    };
                    Ok(SqlCommand::ShowSecrets { prefix })
                } else if self.consume_ident_ci("TENANT")? {
                    Ok(SqlCommand::ShowTenant)
                } else if let Some(expr) = self.parse_show_iam_after_show()? {
                    Ok(SqlCommand::IamPolicy(expr))
                } else {
                    Err(ParseError::expected(
                        vec![
                            "CONFIG",
                            "SECRET",
                            "SECRETS",
                            "COLLECTIONS",
                            "TABLES",
                            "QUEUES",
                            "VECTORS",
                            "DOCUMENTS",
                            "TIMESERIES",
                            "GRAPHS",
                            "KV",
                            "SCHEMA",
                            "INDICES",
                            "SAMPLE",
                            "POLICIES",
                            "STATS",
                            "TENANT",
                            "EFFECTIVE",
                        ],
                        self.peek(),
                        self.position(),
                    ))
                }
            }
            // Transaction control statements (Phase 1.1 PG parity).
            // BEGIN [WORK | TRANSACTION] [ISOLATION LEVEL <mode>]
            // START TRANSACTION [ISOLATION LEVEL <mode>]
            //
            // We only implement SNAPSHOT ISOLATION (our default). We
            // accept READ UNCOMMITTED / READ COMMITTED / REPEATABLE
            // READ / SNAPSHOT as PG-compatible no-ops, but reject
            // SERIALIZABLE outright — the previous behaviour of
            // silently degrading to snapshot made the parser
            // dishonest. Real SSI (Serializable Snapshot Isolation)
            // is tracked as a future milestone.
            Token::Begin | Token::Start => {
                self.advance()?;
                let _ = self.consume(&Token::Work)? || self.consume(&Token::Transaction)?;
                // Optional ISOLATION LEVEL clause.
                if self.consume_ident_ci("ISOLATION")? {
                    self.expect(Token::Level)?;
                    // The level identifier can span multiple words
                    // (READ UNCOMMITTED / READ COMMITTED / REPEATABLE
                    // READ). Collect them case-insensitively.
                    let mut parts: Vec<String> = Vec::new();
                    if self.consume_ident_ci("READ")? {
                        parts.push("READ".to_string());
                        if self.consume_ident_ci("UNCOMMITTED")? {
                            parts.push("UNCOMMITTED".to_string());
                        } else if self.consume_ident_ci("COMMITTED")? {
                            parts.push("COMMITTED".to_string());
                        } else {
                            return Err(ParseError::expected(
                                vec!["UNCOMMITTED", "COMMITTED"],
                                self.peek(),
                                self.position(),
                            ));
                        }
                    } else if self.consume_ident_ci("REPEATABLE")? {
                        parts.push("REPEATABLE".to_string());
                        if !self.consume_ident_ci("READ")? {
                            return Err(ParseError::expected(
                                vec!["READ"],
                                self.peek(),
                                self.position(),
                            ));
                        }
                        parts.push("READ".to_string());
                    } else if self.consume_ident_ci("SNAPSHOT")? {
                        parts.push("SNAPSHOT".to_string());
                    } else if self.consume_ident_ci("SERIALIZABLE")? {
                        return Err(ParseError::new(
                            "ISOLATION LEVEL SERIALIZABLE is not yet supported — reddb \
                             currently provides SNAPSHOT ISOLATION (which PG calls \
                             REPEATABLE READ). Use REPEATABLE READ / SNAPSHOT / \
                             READ COMMITTED, or omit ISOLATION LEVEL for the default."
                                .to_string(),
                            self.position(),
                        ));
                    } else {
                        return Err(ParseError::expected(
                            vec!["READ", "REPEATABLE", "SNAPSHOT", "SERIALIZABLE"],
                            self.peek(),
                            self.position(),
                        ));
                    }
                    // All accepted modes map to our snapshot engine today.
                    let _ = parts;
                }
                Ok(SqlCommand::TransactionControl(TxnControl::Begin))
            }
            // COMMIT [WORK | TRANSACTION]
            Token::Commit => {
                self.advance()?;
                let _ = self.consume(&Token::Work)? || self.consume(&Token::Transaction)?;
                Ok(SqlCommand::TransactionControl(TxnControl::Commit))
            }
            // ROLLBACK [WORK | TRANSACTION] [TO [SAVEPOINT] name]
            // ROLLBACK MIGRATION name
            Token::Rollback => {
                self.advance()?;
                if matches!(self.peek(), Token::Ident(n) if n.eq_ignore_ascii_case("MIGRATION")) {
                    match self.parse_rollback_migration_after_keyword()? {
                        QueryExpr::RollbackMigration(q) => Ok(SqlCommand::RollbackMigration(q)),
                        other => Err(ParseError::new(
                            format!(
                                "internal: ROLLBACK MIGRATION produced unexpected kind {other:?}"
                            ),
                            self.position(),
                        )),
                    }
                } else {
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
                // `WITH` is a reserved keyword token — accept both the keyword
                // form and the ident form that non-CTE callers sometimes emit.
                if self.consume(&Token::With)? || self.consume_ident_ci("WITH")? {
                    self.expect(Token::LParen)?;
                    loop {
                        if self.consume(&Token::Format)? || self.consume_ident_ci("FORMAT")? {
                            let _ = self.consume(&Token::Eq)?;
                            // Only CSV for now — accept the ident and move on.
                            let _ = self.expect_ident()?;
                        } else if self.consume(&Token::Header)? {
                            let _ = self.consume(&Token::Eq)?;
                            // Accept `HEADER`, `HEADER = true`, `HEADER = false`,
                            // or an ident spelling of true/false.
                            has_header = match self.peek().clone() {
                                Token::True => {
                                    self.advance()?;
                                    true
                                }
                                Token::False => {
                                    self.advance()?;
                                    false
                                }
                                Token::Ident(ref n) if n.eq_ignore_ascii_case("true") => {
                                    self.advance()?;
                                    true
                                }
                                Token::Ident(ref n) if n.eq_ignore_ascii_case("false") => {
                                    self.advance()?;
                                    false
                                }
                                _ => true,
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
