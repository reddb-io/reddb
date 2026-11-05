//! Canonical SQL table contracts shared by native and browser execution.
use reddb_rql::{
    analyzer::{analyze_create_table, resolve_declared_data_type},
    ast::{CreateColumnDef, CreateTableQuery},
};

pub fn collection_contract_from_create_table(
    query: &CreateTableQuery,
    now: u128,
) -> Result<super::CollectionContract, String> {
    let mut declared_columns: Vec<super::DeclaredColumnContract> = query
        .columns
        .iter()
        .map(declared_column_contract_from_ddl)
        .collect();
    if query.timestamps {
        // Opt-in `WITH timestamps = true` auto-adds two user-visible
        // columns that the write path populates from
        // UnifiedEntity::created_at/updated_at. BIGINT unix-ms, NOT NULL.
        declared_columns.push(super::DeclaredColumnContract {
            name: "created_at".to_string(),
            data_type: "BIGINT".to_string(),
            sql_type: Some(reddb_types::SqlTypeName::simple("BIGINT")),
            not_null: true,
            default: None,
            generated: None,
            check: None,
            compress: None,
            unique: false,
            primary_key: false,
            enum_variants: Vec::new(),
            array_element: None,
            decimal_precision: None,
        });
        declared_columns.push(super::DeclaredColumnContract {
            name: "updated_at".to_string(),
            data_type: "BIGINT".to_string(),
            sql_type: Some(reddb_types::SqlTypeName::simple("BIGINT")),
            not_null: true,
            default: None,
            generated: None,
            check: None,
            compress: None,
            unique: false,
            primary_key: false,
            enum_variants: Vec::new(),
            array_element: None,
            decimal_precision: None,
        });
    }
    Ok(super::CollectionContract {
        name: query.name.clone(),
        declared_model: super::CollectionModel::Table,
        schema_mode: super::SchemaMode::SemiStructured,
        origin: super::ContractOrigin::Explicit,
        version: 1,
        created_at_unix_ms: now,
        updated_at_unix_ms: now,
        default_ttl_ms: query.default_ttl_ms,
        vector_dimension: None,
        vector_metric: None,
        context_index_fields: query.context_index_fields.clone(),
        declared_columns,
        table_def: Some(build_table_def_from_create_table(query, now)?),
        timestamps_enabled: query.timestamps,
        context_index_enabled: query.context_index_enabled
            || !query.context_index_fields.is_empty(),
        metrics_raw_retention_ms: None,
        metrics_rollup_policies: Vec::new(),
        metrics_tenant_identity: None,
        metrics_namespace: None,
        append_only: query.append_only,
        subscriptions: query.subscriptions.clone(),
        analytics_config: Vec::new(),
        session_key: None,
        session_gap_ms: None,
        retention_duration_ms: None,
        analytical_storage: None,
        ai_policy: query.ai_policy.clone(),
    })
}

pub fn declared_column_contract_from_ddl(
    column: &CreateColumnDef,
) -> super::DeclaredColumnContract {
    super::DeclaredColumnContract {
        name: column.name.clone(),
        data_type: column.data_type.clone(),
        sql_type: Some(column.sql_type.clone()),
        not_null: column.not_null,
        default: column.default.clone(),
        generated: column.generated.clone(),
        check: column.check.clone(),
        compress: column.compress,
        unique: column.unique,
        primary_key: column.primary_key,
        enum_variants: column.enum_variants.clone(),
        array_element: column.array_element.clone(),
        decimal_precision: column.decimal_precision,
    }
}

fn build_table_def_from_create_table(
    query: &CreateTableQuery,
    now_ms: u128,
) -> Result<reddb_types::TableDef, String> {
    let now_secs =
        u64::try_from(now_ms / 1000).map_err(|_| "table timestamp exceeds u64".to_string())?;
    let mut table = reddb_types::TableDef::new_at(query.name.clone(), now_secs);
    for column in &query.columns {
        if column.primary_key {
            table.primary_key.push(column.name.clone());
            table.constraints.push(
                reddb_types::Constraint::new(
                    format!("pk_{}", column.name),
                    reddb_types::ConstraintType::PrimaryKey,
                )
                .on_columns(vec![column.name.clone()]),
            );
        }
        if column.unique {
            table.constraints.push(
                reddb_types::Constraint::new(
                    format!("uniq_{}", column.name),
                    reddb_types::ConstraintType::Unique,
                )
                .on_columns(vec![column.name.clone()]),
            );
        }
        if column.not_null {
            table.constraints.push(
                reddb_types::Constraint::new(
                    format!("not_null_{}", column.name),
                    reddb_types::ConstraintType::NotNull,
                )
                .on_columns(vec![column.name.clone()]),
            );
        }
        table.columns.push(column_def_from_ddl(column)?);
    }
    table.constraints.extend(
        analyze_create_table(query)
            .map_err(|err| err.to_string())?
            .unique_constraints,
    );
    // WITH timestamps = true: append the two runtime-managed columns
    // to the schema so resolved_contract_columns exposes them to the
    // normalize/validate path. Declared as UnsignedInteger (unix-ms),
    // not-nullable; the write path auto-fills them.
    if query.timestamps {
        table.columns.push(
            reddb_types::ColumnDef::new(
                "created_at".to_string(),
                reddb_types::DataType::UnsignedInteger,
            )
            .not_null(),
        );
        table.columns.push(
            reddb_types::ColumnDef::new(
                "updated_at".to_string(),
                reddb_types::DataType::UnsignedInteger,
            )
            .not_null(),
        );
        table.constraints.push(
            reddb_types::Constraint::new(
                "not_null_created_at".to_string(),
                reddb_types::ConstraintType::NotNull,
            )
            .on_columns(vec!["created_at".to_string()]),
        );
        table.constraints.push(
            reddb_types::Constraint::new(
                "not_null_updated_at".to_string(),
                reddb_types::ConstraintType::NotNull,
            )
            .on_columns(vec!["updated_at".to_string()]),
        );
    }
    table
        .validate()
        .map_err(|err| format!("invalid table definition: {err}"))?;
    Ok(table)
}

pub fn column_def_from_ddl(column: &CreateColumnDef) -> Result<reddb_types::ColumnDef, String> {
    let data_type = resolve_declared_data_type(&column.data_type).map_err(|err| err.to_string())?;
    let mut column_def = reddb_types::ColumnDef::new(column.name.clone(), data_type);
    if column.not_null {
        column_def = column_def.not_null();
    }
    if let Some(default) = &column.default {
        column_def = column_def.with_default(default.as_bytes().to_vec());
    }
    if column.compress.unwrap_or(0) > 0 {
        column_def = column_def.compressed();
    }
    if !column.enum_variants.is_empty() {
        column_def = column_def.with_variants(column.enum_variants.clone());
    }
    if let Some(precision) = column.decimal_precision {
        column_def = column_def.with_precision(precision);
    }
    if let Some(element_type) = &column.array_element {
        column_def = column_def.with_element_type(
            resolve_declared_data_type(element_type).map_err(|err| err.to_string())?,
        );
    }
    column_def = column_def.with_metadata("ddl_data_type", column.data_type.clone());
    if column.unique {
        column_def = column_def.with_metadata("unique", "true");
    }
    if column.primary_key {
        column_def = column_def.with_metadata("primary_key", "true");
    }
    Ok(column_def)
}
