//! PostgreSQL catalog compatibility views for PG-wire clients.
//!
//! Generic PostgreSQL clients probe `pg_catalog` and `information_schema`
//! before they run user SQL. RedDB's canonical metadata lives in `red.*`
//! virtual tables, so this module maps a small read-only catalog slice onto
//! familiar PostgreSQL shapes without teaching the SQL parser every driver
//! metadata query.

use std::collections::HashMap;

use crate::api::RedDBResult;
use crate::runtime::RedDBRuntime;
use crate::storage::query::unified::{UnifiedRecord, UnifiedResult};
use crate::storage::schema::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PgCatalogView {
    InformationSchemaTables,
    InformationSchemaColumns,
    PgTables,
    PgIndexes,
    PgNamespace,
    PgClass,
    PgAttribute,
}

impl PgCatalogView {
    fn detect(sql: &str) -> Option<Self> {
        let lower = sql.to_ascii_lowercase();
        if !starts_like_select(&lower) {
            return None;
        }
        if contains_relation(&lower, "information_schema.tables") {
            Some(Self::InformationSchemaTables)
        } else if contains_relation(&lower, "information_schema.columns") {
            Some(Self::InformationSchemaColumns)
        } else if contains_relation(&lower, "pg_catalog.pg_tables") {
            Some(Self::PgTables)
        } else if contains_relation(&lower, "pg_catalog.pg_indexes") {
            Some(Self::PgIndexes)
        } else if contains_relation(&lower, "pg_catalog.pg_namespace") {
            Some(Self::PgNamespace)
        } else if contains_relation(&lower, "pg_catalog.pg_class") {
            Some(Self::PgClass)
        } else if contains_relation(&lower, "pg_catalog.pg_attribute") {
            Some(Self::PgAttribute)
        } else {
            None
        }
    }
}

pub(crate) fn translate_pg_catalog_query(
    runtime: &RedDBRuntime,
    sql: &str,
) -> RedDBResult<Option<UnifiedResult>> {
    let Some(view) = PgCatalogView::detect(sql) else {
        return Ok(None);
    };

    let rows = RedCatalogRows::load(runtime)?;
    let lower = sql.to_ascii_lowercase();
    let mut result = match view {
        PgCatalogView::InformationSchemaTables => information_schema_tables(&rows),
        PgCatalogView::InformationSchemaColumns => information_schema_columns(&rows),
        PgCatalogView::PgTables => pg_tables(&rows),
        PgCatalogView::PgIndexes => pg_indexes(&rows),
        PgCatalogView::PgNamespace => pg_namespace(),
        PgCatalogView::PgClass => pg_class(&rows),
        PgCatalogView::PgAttribute => pg_attribute(&rows),
    };

    apply_simple_catalog_filters(&mut result, &lower);
    if lower.contains("count(") {
        return Ok(Some(count_result(result.records.len())));
    }

    Ok(Some(result))
}

#[derive(Debug, Default)]
struct RedCatalogRows {
    collections: Vec<UnifiedRecord>,
    columns: Vec<UnifiedRecord>,
    indices: Vec<UnifiedRecord>,
}

impl RedCatalogRows {
    fn load(runtime: &RedDBRuntime) -> RedDBResult<Self> {
        Ok(Self {
            collections: runtime
                .execute_query("SELECT * FROM red.collections")?
                .result
                .records,
            columns: runtime
                .execute_query("SELECT * FROM red.columns")?
                .result
                .records,
            indices: runtime
                .execute_query("SELECT * FROM red.indices")?
                .result
                .records,
        })
    }
}

fn starts_like_select(lower: &str) -> bool {
    let trimmed = lower.trim_start_matches(|c: char| c.is_whitespace() || c == '(');
    trimmed.starts_with("select ") || trimmed.starts_with("with ")
}

fn contains_relation(lower: &str, relation: &str) -> bool {
    lower.contains(relation) || lower.contains(&relation.replace("pg_catalog.", ""))
}

fn information_schema_tables(rows: &RedCatalogRows) -> UnifiedResult {
    let columns = ["table_catalog", "table_schema", "table_name", "table_type"];
    let records = rows
        .collections
        .iter()
        .filter(|row| !bool_value(row, "internal"))
        .map(|row| {
            record(
                &columns,
                vec![
                    Value::text("reddb"),
                    Value::text("red"),
                    Value::text(text_value(row, "name")),
                    Value::text("BASE TABLE"),
                ],
            )
        })
        .collect();
    result(columns, records)
}

fn information_schema_columns(rows: &RedCatalogRows) -> UnifiedResult {
    let columns = [
        "table_catalog",
        "table_schema",
        "table_name",
        "column_name",
        "ordinal_position",
        "column_default",
        "is_nullable",
        "data_type",
    ];
    let mut ordinals: HashMap<String, i64> = HashMap::new();
    let records = rows
        .columns
        .iter()
        .map(|row| {
            let table = text_value(row, "collection");
            let ordinal = ordinals.entry(table.clone()).or_insert(0);
            *ordinal += 1;
            record(
                &columns,
                vec![
                    Value::text("reddb"),
                    Value::text("red"),
                    Value::text(table),
                    Value::text(text_value(row, "name")),
                    Value::Integer(*ordinal),
                    value_or_null(row, "default_value"),
                    Value::text(if bool_value(row, "nullable") {
                        "YES"
                    } else {
                        "NO"
                    }),
                    Value::text(pg_data_type(&text_value(row, "type"))),
                ],
            )
        })
        .collect();
    result(columns, records)
}

fn pg_tables(rows: &RedCatalogRows) -> UnifiedResult {
    let columns = [
        "schemaname",
        "tablename",
        "tableowner",
        "tablespace",
        "hasindexes",
        "hasrules",
        "hastriggers",
        "rowsecurity",
    ];
    let records = rows
        .collections
        .iter()
        .filter(|row| !bool_value(row, "internal"))
        .map(|row| {
            let table = text_value(row, "name");
            record(
                &columns,
                vec![
                    Value::text("red"),
                    Value::text(table.clone()),
                    Value::text("reddb"),
                    Value::Null,
                    Value::Boolean(collection_has_index(rows, &table)),
                    Value::Boolean(false),
                    Value::Boolean(false),
                    Value::Boolean(false),
                ],
            )
        })
        .collect();
    result(columns, records)
}

fn pg_indexes(rows: &RedCatalogRows) -> UnifiedResult {
    let columns = [
        "schemaname",
        "tablename",
        "indexname",
        "tablespace",
        "indexdef",
    ];
    let records = rows
        .indices
        .iter()
        .filter(|row| bool_value(row, "queryable") || bool_value(row, "declared"))
        .map(|row| {
            let table = text_value(row, "collection");
            let name = text_value(row, "name");
            let kind = text_value(row, "kind");
            record(
                &columns,
                vec![
                    Value::text("red"),
                    Value::text(table.clone()),
                    Value::text(name.clone()),
                    Value::Null,
                    Value::text(format!("CREATE INDEX {name} ON red.{table} USING {kind}")),
                ],
            )
        })
        .collect();
    result(columns, records)
}

fn pg_namespace() -> UnifiedResult {
    let columns = ["oid", "nspname", "nspowner", "nspacl"];
    result(
        columns,
        vec![record(
            &columns,
            vec![
                Value::UnsignedInteger(namespace_oid("red") as u64),
                Value::text("red"),
                Value::UnsignedInteger(10),
                Value::Null,
            ],
        )],
    )
}

fn pg_class(rows: &RedCatalogRows) -> UnifiedResult {
    let columns = [
        "oid",
        "relname",
        "relnamespace",
        "relkind",
        "reltuples",
        "relhasindex",
    ];
    let collection_records = rows
        .collections
        .iter()
        .filter(|row| !bool_value(row, "internal"))
        .map(|row| {
            let table = text_value(row, "name");
            record(
                &columns,
                vec![
                    Value::UnsignedInteger(relation_oid(&table) as u64),
                    Value::text(table.clone()),
                    Value::UnsignedInteger(namespace_oid("red") as u64),
                    Value::text("r"),
                    value_or_null(row, "entities"),
                    Value::Boolean(collection_has_index(rows, &table)),
                ],
            )
        });
    let index_records = rows.indices.iter().map(|row| {
        let name = text_value(row, "name");
        record(
            &columns,
            vec![
                Value::UnsignedInteger(relation_oid(&name) as u64),
                Value::text(name.clone()),
                Value::UnsignedInteger(namespace_oid("red") as u64),
                Value::text("i"),
                Value::Float(0.0),
                Value::Boolean(false),
            ],
        )
    });
    result(columns, collection_records.chain(index_records).collect())
}

fn pg_attribute(rows: &RedCatalogRows) -> UnifiedResult {
    let columns = [
        "attrelid",
        "attname",
        "atttypid",
        "attnum",
        "attnotnull",
        "attisdropped",
    ];
    let mut ordinals: HashMap<String, i64> = HashMap::new();
    let records = rows
        .columns
        .iter()
        .map(|row| {
            let table = text_value(row, "collection");
            let ordinal = ordinals.entry(table.clone()).or_insert(0);
            *ordinal += 1;
            record(
                &columns,
                vec![
                    Value::UnsignedInteger(relation_oid(&table) as u64),
                    Value::text(text_value(row, "name")),
                    Value::UnsignedInteger(pg_type_oid(&text_value(row, "type")) as u64),
                    Value::Integer(*ordinal),
                    Value::Boolean(!bool_value(row, "nullable")),
                    Value::Boolean(false),
                ],
            )
        })
        .collect();
    result(columns, records)
}

fn result<const N: usize>(columns: [&str; N], records: Vec<UnifiedRecord>) -> UnifiedResult {
    UnifiedResult {
        columns: columns.iter().map(|column| (*column).to_string()).collect(),
        records,
        ..UnifiedResult::empty()
    }
}

fn record<const N: usize>(columns: &[&str; N], values: Vec<Value>) -> UnifiedRecord {
    let mut record = UnifiedRecord::new();
    for (column, value) in columns.iter().zip(values.into_iter()) {
        record.set(column, value);
    }
    record
}

fn count_result(count: usize) -> UnifiedResult {
    result(
        ["count"],
        vec![record(
            &["count"],
            vec![Value::UnsignedInteger(count as u64)],
        )],
    )
}

fn apply_simple_catalog_filters(result: &mut UnifiedResult, lower: &str) {
    for (column, aliases) in [
        ("table_name", &["table_name", "tablename", "relname"][..]),
        (
            "table_schema",
            &["table_schema", "schemaname", "nspname"][..],
        ),
        ("column_name", &["column_name", "attname"][..]),
    ] {
        if let Some(expected) = aliases
            .iter()
            .find_map(|alias| equality_literal(lower, alias))
        {
            result.records.retain(|row| {
                aliases
                    .iter()
                    .chain(std::iter::once(&column))
                    .any(|candidate| {
                        text_value_opt(row, candidate).as_deref() == Some(expected.as_str())
                    })
            });
        }
    }
}

fn equality_literal(lower: &str, column: &str) -> Option<String> {
    let needle = format!("{column} = '");
    let start = lower.find(&needle)? + needle.len();
    let end = lower[start..].find('\'')?;
    Some(lower[start..start + end].to_string())
}

fn text_value(row: &UnifiedRecord, field: &str) -> String {
    text_value_opt(row, field).unwrap_or_default()
}

fn text_value_opt(row: &UnifiedRecord, field: &str) -> Option<String> {
    match row.get(field) {
        Some(Value::Text(value)) => Some(value.to_string()),
        Some(Value::Integer(value)) => Some(value.to_string()),
        Some(Value::UnsignedInteger(value)) => Some(value.to_string()),
        Some(Value::BigInt(value)) => Some(value.to_string()),
        Some(Value::Boolean(value)) => Some(if *value { "true" } else { "false" }.to_string()),
        _ => None,
    }
}

fn bool_value(row: &UnifiedRecord, field: &str) -> bool {
    matches!(row.get(field), Some(Value::Boolean(true)))
}

fn value_or_null(row: &UnifiedRecord, field: &str) -> Value {
    row.get(field).cloned().unwrap_or(Value::Null)
}

fn collection_has_index(rows: &RedCatalogRows, collection: &str) -> bool {
    rows.indices
        .iter()
        .any(|row| text_value_opt(row, "collection").as_deref() == Some(collection))
}

fn pg_data_type(red_type: &str) -> String {
    match red_type.to_ascii_uppercase().as_str() {
        "INT" | "INTEGER" => "integer",
        "BIGINT" => "bigint",
        "FLOAT" | "DOUBLE" | "REAL" => "double precision",
        "BOOLEAN" | "BOOL" => "boolean",
        "JSON" | "JSONB" => "jsonb",
        "UUID" => "uuid",
        "DATE" => "date",
        "TIMESTAMP" | "TIMESTAMPTZ" => "timestamp with time zone",
        _ => "text",
    }
    .to_string()
}

fn pg_type_oid(red_type: &str) -> u32 {
    match pg_data_type(red_type).as_str() {
        "boolean" => 16,
        "bigint" => 20,
        "integer" => 23,
        "text" => 25,
        "jsonb" => 3802,
        "double precision" => 701,
        "uuid" => 2950,
        "date" => 1082,
        "timestamp with time zone" => 1184,
        _ => 25,
    }
}

fn namespace_oid(name: &str) -> u32 {
    stable_oid(&format!("namespace:{name}"))
}

fn relation_oid(name: &str) -> u32 {
    stable_oid(&format!("relation:{name}"))
}

fn stable_oid(input: &str) -> u32 {
    let mut hash = 2166136261u32;
    for byte in input.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(16777619);
    }
    16_384 + (hash % 1_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RedDBOptions, RedDBRuntime};

    fn runtime() -> RedDBRuntime {
        RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime")
    }

    #[test]
    fn information_schema_columns_comes_from_red_columns() {
        let rt = runtime();
        rt.execute_query("CREATE TABLE users (id INTEGER PRIMARY KEY, email TEXT NOT NULL)")
            .expect("create users");

        let result = translate_pg_catalog_query(
            &rt,
            "SELECT * FROM information_schema.columns WHERE table_name = 'users'",
        )
        .expect("translate")
        .expect("catalog result");

        assert_eq!(
            result.columns,
            vec![
                "table_catalog",
                "table_schema",
                "table_name",
                "column_name",
                "ordinal_position",
                "column_default",
                "is_nullable",
                "data_type"
            ]
        );
        assert!(result
            .records
            .iter()
            .any(|row| text_value(row, "column_name") == "email"
                && text_value(row, "is_nullable") == "NO"
                && text_value(row, "data_type") == "text"));
    }

    #[test]
    fn pg_indexes_comes_from_red_indices() {
        let rt = runtime();
        rt.execute_query("CREATE TABLE users (id INT, email TEXT)")
            .expect("create users");
        rt.execute_query("CREATE INDEX users_email_idx ON users (email) USING HASH")
            .expect("create index");

        let result = translate_pg_catalog_query(&rt, "SELECT * FROM pg_catalog.pg_indexes")
            .expect("translate")
            .expect("catalog result");

        assert!(result.records.iter().any(|row| {
            text_value(row, "tablename") == "users"
                && text_value(row, "indexname") == "users_email_idx"
                && text_value(row, "indexdef").contains("USING hash")
        }));
    }

    #[test]
    fn pg_class_lists_tables_and_indexes() {
        let rt = runtime();
        rt.execute_query("CREATE TABLE users (id INT, email TEXT)")
            .expect("create users");
        rt.execute_query("CREATE INDEX users_email_idx ON users (email)")
            .expect("create index");

        let result = translate_pg_catalog_query(&rt, "SELECT * FROM pg_catalog.pg_class")
            .expect("translate")
            .expect("catalog result");

        assert!(result.records.iter().any(|row| {
            text_value(row, "relname") == "users" && text_value(row, "relkind") == "r"
        }));
        assert!(result.records.iter().any(|row| {
            text_value(row, "relname") == "users_email_idx" && text_value(row, "relkind") == "i"
        }));
    }
}
