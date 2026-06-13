//! Application-level `create_table` parity with the SQL parser path.
//!
//! Previously the app-level DTO could not express `PARTITION BY` or
//! `TENANT BY`, so callers that needed either feature were forced
//! through raw SQL. This pins the parity fix.

use reddb::application::{
    CreateTableColumnInput, CreateTableInput, CreateTablePartitionKind, CreateTablePartitionSpec,
    SchemaUseCases,
};
use reddb::RedDBRuntime;

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

fn column(name: &str, ty: &str) -> CreateTableColumnInput {
    CreateTableColumnInput {
        name: name.to_string(),
        data_type: ty.to_string(),
        not_null: false,
        default: None,
        compress: None,
        unique: false,
        primary_key: false,
        enum_variants: Vec::new(),
        array_element: None,
        decimal_precision: None,
    }
}

#[test]
fn create_table_accepts_partition_by_range() {
    let rt = rt();
    let schema = SchemaUseCases::new(&rt);
    schema
        .create_table(CreateTableInput {
            name: "events".to_string(),
            columns: vec![column("id", "INTEGER"), column("event_ts", "INTEGER")],
            if_not_exists: false,
            default_ttl_ms: None,
            context_index_fields: Vec::new(),
            timestamps: false,
            partition_by: Some(CreateTablePartitionSpec {
                kind: CreateTablePartitionKind::Range,
                column: "event_ts".to_string(),
            }),
            tenant_by: None,
            append_only: false,
        })
        .expect("partitioned table should land via app-level port");
}

#[test]
fn create_table_accepts_tenant_by() {
    let rt = rt();
    let schema = SchemaUseCases::new(&rt);
    schema
        .create_table(CreateTableInput {
            name: "tenant_rows".to_string(),
            columns: vec![column("id", "INTEGER"), column("tenant_id", "TEXT")],
            if_not_exists: false,
            default_ttl_ms: None,
            context_index_fields: Vec::new(),
            timestamps: false,
            partition_by: None,
            tenant_by: Some("tenant_id".to_string()),
            append_only: false,
        })
        .expect("tenant-scoped table should land via app-level port");
}
