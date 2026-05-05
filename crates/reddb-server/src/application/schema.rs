use crate::application::ports::RuntimeSchemaPort;
use crate::runtime::RuntimeQueryResult;
use crate::RedDBResult;

#[derive(Debug, Clone)]
pub struct CreateTableColumnInput {
    pub name: String,
    pub data_type: String,
    pub not_null: bool,
    pub default: Option<String>,
    pub compress: Option<u8>,
    pub unique: bool,
    pub primary_key: bool,
    pub enum_variants: Vec<String>,
    pub array_element: Option<String>,
    pub decimal_precision: Option<u8>,
}

/// How the application-level DTO expresses a `PARTITION BY` clause.
/// Mirrors `storage::query::PartitionKind` but stays decoupled from the
/// SQL-parser crate so drivers can call this port without pulling the
/// AST module into their dependency graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreateTablePartitionKind {
    Range,
    List,
    Hash,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateTablePartitionSpec {
    pub kind: CreateTablePartitionKind,
    pub column: String,
}

#[derive(Debug, Clone)]
pub struct CreateTableInput {
    pub name: String,
    pub columns: Vec<CreateTableColumnInput>,
    pub if_not_exists: bool,
    pub default_ttl_ms: Option<u64>,
    pub context_index_fields: Vec<String>,
    pub timestamps: bool,
    /// Optional `PARTITION BY RANGE|LIST|HASH (column)` declaration.
    /// Reaches parity with the SQL parser path — callers no longer
    /// need to route partitioned DDL through raw SQL.
    pub partition_by: Option<CreateTablePartitionSpec>,
    /// Optional `TENANT BY (column)` declaration (auto-install RLS).
    pub tenant_by: Option<String>,
    /// Reject UPDATE / DELETE at parse time when `true`.
    /// Corresponds to `CREATE TABLE ... APPEND ONLY`.
    pub append_only: bool,
}

#[derive(Debug, Clone)]
pub struct DropTableInput {
    pub name: String,
    pub if_exists: bool,
}

#[derive(Debug, Clone)]
pub struct CreateTimeSeriesInput {
    pub name: String,
    pub retention_ms: Option<u64>,
    pub chunk_size: Option<usize>,
    pub downsample_policies: Vec<String>,
    pub if_not_exists: bool,
}

#[derive(Debug, Clone)]
pub struct DropTimeSeriesInput {
    pub name: String,
    pub if_exists: bool,
}

pub struct SchemaUseCases<'a, P: ?Sized> {
    runtime: &'a P,
}

impl<'a, P: RuntimeSchemaPort + ?Sized> SchemaUseCases<'a, P> {
    pub fn new(runtime: &'a P) -> Self {
        Self { runtime }
    }

    pub fn create_table(&self, input: CreateTableInput) -> RedDBResult<RuntimeQueryResult> {
        self.runtime.create_table(input)
    }

    pub fn drop_table(&self, input: DropTableInput) -> RedDBResult<RuntimeQueryResult> {
        self.runtime.drop_table(input)
    }

    pub fn create_timeseries(
        &self,
        input: CreateTimeSeriesInput,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.runtime.create_timeseries(input)
    }

    pub fn drop_timeseries(&self, input: DropTimeSeriesInput) -> RedDBResult<RuntimeQueryResult> {
        self.runtime.drop_timeseries(input)
    }
}
