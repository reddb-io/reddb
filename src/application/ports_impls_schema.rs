use super::*;
use crate::application::schema::{CreateTablePartitionKind, CreateTablePartitionSpec};
use crate::storage::query::ast::{PartitionKind, PartitionSpec};
use crate::storage::query::{
    CreateColumnDef, CreateTableQuery, CreateTimeSeriesQuery, DropTableQuery, DropTimeSeriesQuery,
};
use crate::storage::schema::SqlTypeName;

fn api_query(label: &str, name: &str) -> String {
    format!("api.{label}({name})")
}

fn to_create_column_def(
    column: crate::application::schema::CreateTableColumnInput,
) -> CreateColumnDef {
    let sql_type = SqlTypeName::parse_declared(&column.data_type);
    CreateColumnDef {
        name: column.name,
        data_type: column.data_type,
        sql_type,
        not_null: column.not_null,
        default: column.default,
        compress: column.compress,
        unique: column.unique,
        primary_key: column.primary_key,
        enum_variants: column.enum_variants,
        array_element: column.array_element,
        decimal_precision: column.decimal_precision,
    }
}

fn to_partition_kind(kind: CreateTablePartitionKind) -> PartitionKind {
    match kind {
        CreateTablePartitionKind::Range => PartitionKind::Range,
        CreateTablePartitionKind::List => PartitionKind::List,
        CreateTablePartitionKind::Hash => PartitionKind::Hash,
    }
}

fn to_partition_spec(spec: CreateTablePartitionSpec) -> PartitionSpec {
    PartitionSpec {
        kind: to_partition_kind(spec.kind),
        column: spec.column,
    }
}

impl RuntimeSchemaPort for RedDBRuntime {
    fn create_table(&self, input: CreateTableInput) -> RedDBResult<RuntimeQueryResult> {
        let CreateTableInput {
            name,
            columns,
            if_not_exists,
            default_ttl_ms,
            context_index_fields,
            timestamps,
            partition_by,
            tenant_by,
            append_only,
        } = input;
        let raw_query = api_query("create_table", &name);
        let query = CreateTableQuery {
            name,
            columns: columns.into_iter().map(to_create_column_def).collect(),
            if_not_exists,
            default_ttl_ms,
            context_index_fields: context_index_fields.clone(),
            context_index_enabled: !context_index_fields.is_empty(),
            timestamps,
            partition_by: partition_by.map(to_partition_spec),
            tenant_by,
            append_only,
        };
        RedDBRuntime::execute_create_table(self, &raw_query, &query)
    }

    fn drop_table(&self, input: DropTableInput) -> RedDBResult<RuntimeQueryResult> {
        let raw_query = api_query("drop_table", &input.name);
        let query = DropTableQuery {
            name: input.name,
            if_exists: input.if_exists,
        };
        RedDBRuntime::execute_drop_table(self, &raw_query, &query)
    }

    fn create_timeseries(&self, input: CreateTimeSeriesInput) -> RedDBResult<RuntimeQueryResult> {
        let CreateTimeSeriesInput {
            name,
            retention_ms,
            chunk_size,
            downsample_policies,
            if_not_exists,
        } = input;
        let raw_query = api_query("create_timeseries", &name);
        let query = CreateTimeSeriesQuery {
            name,
            retention_ms,
            chunk_size,
            downsample_policies,
            if_not_exists,
        };
        RedDBRuntime::execute_create_timeseries(self, &raw_query, &query)
    }

    fn drop_timeseries(&self, input: DropTimeSeriesInput) -> RedDBResult<RuntimeQueryResult> {
        let raw_query = api_query("drop_timeseries", &input.name);
        let query = DropTimeSeriesQuery {
            name: input.name,
            if_exists: input.if_exists,
        };
        RedDBRuntime::execute_drop_timeseries(self, &raw_query, &query)
    }
}
