//! DDL execution: CREATE TABLE, DROP TABLE, ALTER TABLE via SQL AST
//!
//! Translates DDL statements into collection-level operations on the
//! underlying `UnifiedStore`.  RedDB uses a flexible schema-on-read
//! model, so column definitions are advisory metadata rather than
//! rigid constraints.

use super::*;
use crate::storage::query::{analyze_create_table, resolve_declared_data_type, CreateColumnDef};

impl RedDBRuntime {
    /// Execute CREATE TABLE
    ///
    /// Creates a new collection in the store.  Column definitions are
    /// recorded for introspection but do not enforce rigid schema
    /// constraints.
    pub fn execute_create_table(
        &self,
        raw_query: &str,
        query: &CreateTableQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let store = self.inner.db.store();
        analyze_create_table(query).map_err(|err| RedDBError::Query(err.to_string()))?;

        // Check if the collection already exists.
        let exists = store.get_collection(&query.name).is_some();
        if exists {
            if query.if_not_exists {
                return Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("table '{}' already exists", query.name),
                    "create",
                ));
            }
            return Err(RedDBError::Query(format!(
                "table '{}' already exists",
                query.name
            )));
        }

        // Build and validate the contract before mutating storage so invalid
        // SQL types / duplicate columns do not leave partial side effects.
        let contract = collection_contract_from_create_table(query)?;

        // Create the collection.
        store
            .create_collection(&query.name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;

        if let Some(default_ttl_ms) = query.default_ttl_ms {
            self.inner
                .db
                .set_collection_default_ttl_ms(&query.name, default_ttl_ms);
        }
        self.inner
            .db
            .save_collection_contract(contract)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.inner
            .db
            .persist_metadata()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.refresh_table_planner_stats(&query.name);
        self.invalidate_result_cache();

        // Partition metadata (Phase 2.2 PG parity).
        //
        // When the CREATE TABLE carries a `PARTITION BY RANGE|LIST|HASH (col)`
        // clause, stamp the partition config into `red_config` under
        // `partition.{table}.{by,column}`. Children are registered separately
        // via `ALTER TABLE parent ATTACH PARTITION child ...`.
        if let Some(spec) = &query.partition_by {
            let kind_str = match spec.kind {
                crate::storage::query::ast::PartitionKind::Range => "range",
                crate::storage::query::ast::PartitionKind::List => "list",
                crate::storage::query::ast::PartitionKind::Hash => "hash",
            };
            store.set_config_tree(
                &format!("partition.{}.by", query.name),
                &crate::serde_json::Value::String(kind_str.to_string()),
            );
            store.set_config_tree(
                &format!("partition.{}.column", query.name),
                &crate::serde_json::Value::String(spec.column.clone()),
            );
        }

        // Table-scoped multi-tenancy (Phase 2.5.4).
        //
        // `CREATE TABLE t (...) TENANT BY (col)` declaration:
        //   1. Persists the `tenant_tables.{table}.column` marker so
        //      INSERTs can auto-fill and future opens re-hydrate.
        //   2. Registers the table in the in-memory `tenant_tables`
        //      HashMap used by the DML auto-fill path.
        //   3. Installs an implicit RLS policy equivalent to
        //      `USING (col = CURRENT_TENANT())` across all actions.
        //   4. Flips `rls_enabled_tables` on so the policy applies.
        if let Some(col) = &query.tenant_by {
            store.set_config_tree(
                &format!("tenant_tables.{}.column", query.name),
                &crate::serde_json::Value::String(col.clone()),
            );
            self.register_tenant_table(&query.name, col);
        }

        let ttl_suffix = query
            .default_ttl_ms
            .map(|ttl_ms| format!(" with default TTL {}ms", ttl_ms))
            .unwrap_or_default();

        let tenant_suffix = query
            .tenant_by
            .as_ref()
            .map(|col| format!(" (tenant-scoped by {col})"))
            .unwrap_or_default();

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!(
                "table '{}' created{}{}",
                query.name, ttl_suffix, tenant_suffix
            ),
            "create",
        ))
    }

    /// Execute DROP TABLE
    ///
    /// Drops the collection and all its data from the store.
    pub fn execute_drop_table(
        &self,
        raw_query: &str,
        query: &DropTableQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let store = self.inner.db.store();

        let exists = store.get_collection(&query.name).is_some();
        if !exists {
            if query.if_exists {
                return Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("table '{}' does not exist", query.name),
                    "drop",
                ));
            }
            return Err(RedDBError::NotFound(format!(
                "table '{}' not found",
                query.name
            )));
        }

        let orphaned_indices: Vec<String> = self
            .inner
            .index_store
            .list_indices(&query.name)
            .into_iter()
            .map(|index| index.name)
            .collect();
        for name in &orphaned_indices {
            self.inner.index_store.drop_index(name, &query.name);
        }

        store
            .drop_collection(&query.name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.inner.db.invalidate_vector_index(&query.name);
        self.inner.db.clear_collection_default_ttl_ms(&query.name);
        self.inner
            .db
            .remove_collection_contract(&query.name)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.clear_table_planner_stats(&query.name);
        self.invalidate_result_cache();
        self.inner
            .db
            .persist_metadata()
            .map_err(|err| RedDBError::Internal(err.to_string()))?;

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!("table '{}' dropped", query.name),
            "drop",
        ))
    }

    /// Execute ALTER TABLE
    ///
    /// In RedDB's schema-on-read model, ALTER TABLE operations are advisory.
    /// ADD COLUMN records the schema intent, DROP COLUMN removes it, and
    /// RENAME COLUMN is a metadata rename.  Existing data is not rewritten.
    pub fn execute_alter_table(
        &self,
        raw_query: &str,
        query: &AlterTableQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let store = self.inner.db.store();

        // Verify the table exists.
        if store.get_collection(&query.name).is_none() {
            return Err(RedDBError::NotFound(format!(
                "table '{}' not found",
                query.name
            )));
        }

        let mut messages = Vec::new();

        for op in &query.operations {
            match op {
                AlterOperation::AddColumn(col) => {
                    // Schema-on-read: column will be available on next insert.
                    messages.push(format!("column '{}' added", col.name));
                }
                AlterOperation::DropColumn(name) => {
                    messages.push(format!("column '{}' dropped", name));
                }
                AlterOperation::RenameColumn { from, to } => {
                    messages.push(format!("column '{}' renamed to '{}'", from, to));
                }
                AlterOperation::AttachPartition { child, bound } => {
                    // Persist child → parent binding in red_config so the
                    // future planner-side pruner can enumerate children and
                    // evaluate their bounds.
                    store.set_config_tree(
                        &format!("partition.{}.children.{}", query.name, child),
                        &crate::serde_json::Value::String(bound.clone()),
                    );
                    messages.push(format!(
                        "partition '{child}' attached to '{}' ({bound})",
                        query.name
                    ));
                }
                AlterOperation::DetachPartition { child } => {
                    store.set_config_tree(
                        &format!("partition.{}.children.{}", query.name, child),
                        &crate::serde_json::Value::Null,
                    );
                    messages.push(format!(
                        "partition '{child}' detached from '{}'",
                        query.name
                    ));
                }
                AlterOperation::EnableRowLevelSecurity => {
                    self.inner
                        .rls_enabled_tables
                        .write()
                        .insert(query.name.clone());
                    // Persist flag so RLS survives restart via red_config.
                    store.set_config_tree(
                        &format!("rls.enabled.{}", query.name),
                        &crate::serde_json::Value::Bool(true),
                    );
                    self.invalidate_plan_cache();
                    messages.push(format!("row level security enabled on '{}'", query.name));
                }
                AlterOperation::DisableRowLevelSecurity => {
                    self.inner.rls_enabled_tables.write().remove(&query.name);
                    store.set_config_tree(
                        &format!("rls.enabled.{}", query.name),
                        &crate::serde_json::Value::Null,
                    );
                    self.invalidate_plan_cache();
                    messages.push(format!("row level security disabled on '{}'", query.name));
                }
                // Phase 2.5.4: retrofit tenancy onto an existing table.
                AlterOperation::EnableTenancy { column } => {
                    store.set_config_tree(
                        &format!("tenant_tables.{}.column", query.name),
                        &crate::serde_json::Value::String(column.clone()),
                    );
                    self.register_tenant_table(&query.name, column);
                    self.invalidate_plan_cache();
                    messages.push(format!(
                        "tenancy enabled on '{}' by column '{column}'",
                        query.name
                    ));
                }
                AlterOperation::DisableTenancy => {
                    store.set_config_tree(
                        &format!("tenant_tables.{}.column", query.name),
                        &crate::serde_json::Value::Null,
                    );
                    self.unregister_tenant_table(&query.name);
                    self.invalidate_plan_cache();
                    messages.push(format!("tenancy disabled on '{}'", query.name));
                }
            }
        }

        let mut contract = self
            .inner
            .db
            .collection_contract(&query.name)
            .unwrap_or_else(|| default_collection_contract_for_existing_table(&query.name));
        apply_alter_operations_to_contract(&mut contract, &query.operations);
        contract.version = contract.version.saturating_add(1);
        contract.updated_at_unix_ms = current_unix_ms();
        self.inner
            .db
            .save_collection_contract(contract)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.clear_table_planner_stats(&query.name);
        self.invalidate_result_cache();

        let message = if messages.is_empty() {
            format!("table '{}' altered (no operations)", query.name)
        } else {
            format!("table '{}' altered: {}", query.name, messages.join(", "))
        };

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &message,
            "alter",
        ))
    }

    /// Execute EXPLAIN ALTER FOR CREATE TABLE
    ///
    /// Pure read: computes the schema diff between the target table's
    /// current `CollectionContract` and the embedded `CREATE TABLE` body,
    /// and returns it as SQL `ALTER TABLE` text (default) or structured
    /// JSON. Never mutates storage.
    pub fn execute_explain_alter(
        &self,
        raw_query: &str,
        query: &ExplainAlterQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        // Validate the target CREATE TABLE body so syntactically valid
        // but semantically broken targets (bad SQL types, duplicate
        // columns) are caught here rather than inside the diff engine.
        analyze_create_table(&query.target).map_err(|err| RedDBError::Query(err.to_string()))?;

        let current_contract = self.inner.db.collection_contract(&query.target.name);

        let current_columns: Vec<crate::physical::DeclaredColumnContract> = current_contract
            .as_ref()
            .map(|c| c.declared_columns.clone())
            .unwrap_or_default();

        let diff = super::schema_diff::compute_column_diff(
            &query.target.name,
            &current_columns,
            &query.target.columns,
        );

        let rendered = match query.format {
            ExplainFormat::Sql => super::schema_diff::format_as_sql(&diff),
            ExplainFormat::Json => super::schema_diff::format_as_json(&diff),
        };

        let format_label = match query.format {
            ExplainFormat::Sql => "sql",
            ExplainFormat::Json => "json",
        };

        let columns = vec![
            "table".to_string(),
            "format".to_string(),
            "diff".to_string(),
        ];
        let row = vec![
            ("table".to_string(), Value::Text(query.target.name.clone())),
            ("format".to_string(), Value::Text(format_label.to_string())),
            ("diff".to_string(), Value::Text(rendered)),
        ];

        Ok(RuntimeQueryResult::ok_records(
            raw_query.to_string(),
            columns,
            vec![row],
            "explain",
        ))
    }

    /// Execute CREATE INDEX
    ///
    /// Registers a new index on a collection, builds it from existing data,
    /// and makes it available to the query executor for O(1) lookups.
    pub fn execute_create_index(
        &self,
        raw_query: &str,
        query: &CreateIndexQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let store = self.inner.db.store();

        // Verify the table exists
        let manager = store
            .get_collection(&query.table)
            .ok_or_else(|| RedDBError::NotFound(format!("table '{}' not found", query.table)))?;

        let method_kind = match query.method {
            IndexMethod::Hash => super::index_store::IndexMethodKind::Hash,
            IndexMethod::BTree => super::index_store::IndexMethodKind::BTree,
            IndexMethod::Bitmap => super::index_store::IndexMethodKind::Bitmap,
            IndexMethod::RTree => super::index_store::IndexMethodKind::Spatial,
        };

        // Extract fields from existing entities for indexing. Row
        // entities may arrive in either the "named" HashMap layout
        // (gRPC `BulkInsertBinary` path) OR the columnar layout
        // (HTTP `POST /collections/X/bulk/rows` fast path, which uses
        // `schema: Some(Arc<Vec<String>>)` + `columns: Vec<Value>` and
        // leaves `named == None`). Prior to this commit the columnar
        // branch returned an empty field list, so `CREATE INDEX` built
        // a zero-entity index over HTTP-inserted data even though the
        // data was queryable via `SELECT`.
        let entities = manager.query_all(|_| true);
        let entity_fields: Vec<(crate::storage::unified::EntityId, Vec<(String, Value)>)> =
            entities
                .iter()
                .map(|e| {
                    let fields = match &e.data {
                        crate::storage::EntityData::Row(row) => {
                            if let Some(ref named) = row.named {
                                named.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
                            } else if let Some(ref schema) = row.schema {
                                // Columnar layout — pair each column
                                // with its positional name from the
                                // shared schema Arc.
                                schema
                                    .iter()
                                    .zip(row.columns.iter())
                                    .map(|(k, v)| (k.clone(), v.clone()))
                                    .collect()
                            } else {
                                Vec::new()
                            }
                        }
                        crate::storage::EntityData::Node(node) => node
                            .properties
                            .iter()
                            .map(|(k, v)| (k.clone(), v.clone()))
                            .collect(),
                        _ => Vec::new(),
                    };
                    (e.id, fields)
                })
                .collect();

        // Build the index
        let indexed_count = self
            .inner
            .index_store
            .create_index(
                &query.name,
                &query.table,
                &query.columns,
                method_kind,
                query.unique,
                &entity_fields,
            )
            .map_err(RedDBError::Internal)?;

        let analyzed = crate::storage::query::planner::stats_catalog::analyze_entity_fields(
            &query.table,
            &entity_fields,
        );
        crate::storage::query::planner::stats_catalog::persist_table_stats(&store, &analyzed);
        self.invalidate_plan_cache();

        // Register metadata
        self.inner
            .index_store
            .register(super::index_store::RegisteredIndex {
                name: query.name.clone(),
                collection: query.table.clone(),
                columns: query.columns.clone(),
                method: method_kind,
                unique: query.unique,
            });

        let method_str = format!("{}", query.method);
        let unique_str = if query.unique { "unique " } else { "" };
        let cols = query.columns.join(", ");

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!(
                "{}index '{}' created on '{}' ({}) using {} ({} entities indexed)",
                unique_str, query.name, query.table, cols, method_str, indexed_count
            ),
            "create",
        ))
    }

    /// Execute DROP INDEX
    ///
    /// Removes an index from a collection.
    pub fn execute_drop_index(
        &self,
        raw_query: &str,
        query: &DropIndexQuery,
    ) -> RedDBResult<RuntimeQueryResult> {
        let store = self.inner.db.store();

        // Verify the table exists
        if store.get_collection(&query.table).is_none() {
            if query.if_exists {
                return Ok(RuntimeQueryResult::ok_message(
                    raw_query.to_string(),
                    &format!("table '{}' does not exist", query.table),
                    "drop",
                ));
            }
            return Err(RedDBError::NotFound(format!(
                "table '{}' not found",
                query.table
            )));
        }

        // Remove from IndexStore
        self.inner.index_store.drop_index(&query.name, &query.table);
        self.invalidate_plan_cache();

        Ok(RuntimeQueryResult::ok_message(
            raw_query.to_string(),
            &format!("index '{}' dropped from '{}'", query.name, query.table),
            "drop",
        ))
    }
}

fn collection_contract_from_create_table(
    query: &CreateTableQuery,
) -> RedDBResult<crate::physical::CollectionContract> {
    let now = current_unix_ms();
    let mut declared_columns: Vec<crate::physical::DeclaredColumnContract> = query
        .columns
        .iter()
        .map(declared_column_contract_from_ddl)
        .collect();
    if query.timestamps {
        // Opt-in `WITH timestamps = true` auto-adds two user-visible
        // columns that the write path populates from
        // UnifiedEntity::created_at/updated_at. BIGINT unix-ms, NOT NULL.
        declared_columns.push(crate::physical::DeclaredColumnContract {
            name: "created_at".to_string(),
            data_type: "BIGINT".to_string(),
            sql_type: Some(crate::storage::schema::SqlTypeName::simple("BIGINT")),
            not_null: true,
            default: None,
            compress: None,
            unique: false,
            primary_key: false,
            enum_variants: Vec::new(),
            array_element: None,
            decimal_precision: None,
        });
        declared_columns.push(crate::physical::DeclaredColumnContract {
            name: "updated_at".to_string(),
            data_type: "BIGINT".to_string(),
            sql_type: Some(crate::storage::schema::SqlTypeName::simple("BIGINT")),
            not_null: true,
            default: None,
            compress: None,
            unique: false,
            primary_key: false,
            enum_variants: Vec::new(),
            array_element: None,
            decimal_precision: None,
        });
    }
    Ok(crate::physical::CollectionContract {
        name: query.name.clone(),
        declared_model: crate::catalog::CollectionModel::Table,
        schema_mode: crate::catalog::SchemaMode::SemiStructured,
        origin: crate::physical::ContractOrigin::Explicit,
        version: 1,
        created_at_unix_ms: now,
        updated_at_unix_ms: now,
        default_ttl_ms: query.default_ttl_ms,
        context_index_fields: query.context_index_fields.clone(),
        declared_columns,
        table_def: Some(build_table_def_from_create_table(query)?),
        timestamps_enabled: query.timestamps,
    })
}

fn default_collection_contract_for_existing_table(
    name: &str,
) -> crate::physical::CollectionContract {
    let now = current_unix_ms();
    crate::physical::CollectionContract {
        name: name.to_string(),
        declared_model: crate::catalog::CollectionModel::Table,
        schema_mode: crate::catalog::SchemaMode::SemiStructured,
        origin: crate::physical::ContractOrigin::Explicit,
        version: 0,
        created_at_unix_ms: now,
        updated_at_unix_ms: now,
        default_ttl_ms: None,
        context_index_fields: Vec::new(),
        declared_columns: Vec::new(),
        table_def: Some(crate::storage::schema::TableDef::new(name.to_string())),
        timestamps_enabled: false,
    }
}

fn declared_column_contract_from_ddl(
    column: &CreateColumnDef,
) -> crate::physical::DeclaredColumnContract {
    crate::physical::DeclaredColumnContract {
        name: column.name.clone(),
        data_type: column.data_type.clone(),
        sql_type: Some(column.sql_type.clone()),
        not_null: column.not_null,
        default: column.default.clone(),
        compress: column.compress,
        unique: column.unique,
        primary_key: column.primary_key,
        enum_variants: column.enum_variants.clone(),
        array_element: column.array_element.clone(),
        decimal_precision: column.decimal_precision,
    }
}

fn apply_alter_operations_to_contract(
    contract: &mut crate::physical::CollectionContract,
    operations: &[AlterOperation],
) {
    if contract.table_def.is_none() {
        contract.table_def = Some(crate::storage::schema::TableDef::new(contract.name.clone()));
    }
    for operation in operations {
        match operation {
            AlterOperation::AddColumn(column) => {
                if !contract
                    .declared_columns
                    .iter()
                    .any(|existing| existing.name == column.name)
                {
                    contract
                        .declared_columns
                        .push(declared_column_contract_from_ddl(column));
                }
                if let Some(table_def) = contract.table_def.as_mut() {
                    if table_def.get_column(&column.name).is_none() {
                        if let Ok(column_def) = column_def_from_ddl(column) {
                            if column.primary_key {
                                table_def.primary_key.push(column.name.clone());
                                table_def.constraints.push(
                                    crate::storage::schema::Constraint::new(
                                        format!("pk_{}", column.name),
                                        crate::storage::schema::ConstraintType::PrimaryKey,
                                    )
                                    .on_columns(vec![column.name.clone()]),
                                );
                            }
                            if column.unique {
                                table_def.constraints.push(
                                    crate::storage::schema::Constraint::new(
                                        format!("uniq_{}", column.name),
                                        crate::storage::schema::ConstraintType::Unique,
                                    )
                                    .on_columns(vec![column.name.clone()]),
                                );
                            }
                            if column.not_null {
                                table_def.constraints.push(
                                    crate::storage::schema::Constraint::new(
                                        format!("not_null_{}", column.name),
                                        crate::storage::schema::ConstraintType::NotNull,
                                    )
                                    .on_columns(vec![column.name.clone()]),
                                );
                            }
                            table_def.columns.push(column_def);
                        }
                    }
                }
            }
            AlterOperation::DropColumn(name) => {
                contract
                    .declared_columns
                    .retain(|column| column.name != *name);
                if let Some(table_def) = contract.table_def.as_mut() {
                    if let Some(index) = table_def.column_index(name) {
                        table_def.columns.remove(index);
                    }
                    table_def.primary_key.retain(|column| column != name);
                    table_def.constraints.retain(|constraint| {
                        !constraint.columns.iter().any(|column| column == name)
                    });
                    table_def
                        .indexes
                        .retain(|index| !index.columns.iter().any(|column| column == name));
                }
            }
            AlterOperation::RenameColumn { from, to } => {
                if contract
                    .declared_columns
                    .iter()
                    .any(|column| column.name == *to)
                {
                    continue;
                }
                if let Some(column) = contract
                    .declared_columns
                    .iter_mut()
                    .find(|column| column.name == *from)
                {
                    column.name = to.clone();
                }
                if let Some(table_def) = contract.table_def.as_mut() {
                    if let Some(column) = table_def
                        .columns
                        .iter_mut()
                        .find(|column| column.name == *from)
                    {
                        column.name = to.clone();
                    }
                    for primary_key in &mut table_def.primary_key {
                        if *primary_key == *from {
                            *primary_key = to.clone();
                        }
                    }
                    for constraint in &mut table_def.constraints {
                        for column in &mut constraint.columns {
                            if *column == *from {
                                *column = to.clone();
                            }
                        }
                        if let Some(ref_columns) = constraint.ref_columns.as_mut() {
                            for column in ref_columns {
                                if *column == *from {
                                    *column = to.clone();
                                }
                            }
                        }
                    }
                    for index in &mut table_def.indexes {
                        for column in &mut index.columns {
                            if *column == *from {
                                *column = to.clone();
                            }
                        }
                    }
                }
            }
            // Partition ops don't touch the column contract — metadata is
            // persisted separately via `red_config.partition.*`.
            AlterOperation::AttachPartition { .. } | AlterOperation::DetachPartition { .. } => {}
            // RLS toggles don't touch the column contract — flag is persisted
            // separately via `red_config.rls.enabled.{table}` and enforced
            // through the in-memory `rls_enabled_tables` set.
            AlterOperation::EnableRowLevelSecurity | AlterOperation::DisableRowLevelSecurity => {}
            // Phase 2.5.4: tenancy toggles persist via `red_config.tenant_tables.*`
            // and are enforced through `tenant_tables` + RLS auto-policy.
            AlterOperation::EnableTenancy { .. } | AlterOperation::DisableTenancy => {}
        }
    }
}

fn build_table_def_from_create_table(
    query: &CreateTableQuery,
) -> RedDBResult<crate::storage::schema::TableDef> {
    let mut table = crate::storage::schema::TableDef::new(query.name.clone());
    for column in &query.columns {
        if column.primary_key {
            table.primary_key.push(column.name.clone());
            table.constraints.push(
                crate::storage::schema::Constraint::new(
                    format!("pk_{}", column.name),
                    crate::storage::schema::ConstraintType::PrimaryKey,
                )
                .on_columns(vec![column.name.clone()]),
            );
        }
        if column.unique {
            table.constraints.push(
                crate::storage::schema::Constraint::new(
                    format!("uniq_{}", column.name),
                    crate::storage::schema::ConstraintType::Unique,
                )
                .on_columns(vec![column.name.clone()]),
            );
        }
        if column.not_null {
            table.constraints.push(
                crate::storage::schema::Constraint::new(
                    format!("not_null_{}", column.name),
                    crate::storage::schema::ConstraintType::NotNull,
                )
                .on_columns(vec![column.name.clone()]),
            );
        }
        table.columns.push(column_def_from_ddl(column)?);
    }
    // WITH timestamps = true: append the two runtime-managed columns
    // to the schema so resolved_contract_columns exposes them to the
    // normalize/validate path. Declared as UnsignedInteger (unix-ms),
    // not-nullable; the write path auto-fills them.
    if query.timestamps {
        table.columns.push(
            crate::storage::schema::ColumnDef::new(
                "created_at".to_string(),
                crate::storage::schema::DataType::UnsignedInteger,
            )
            .not_null(),
        );
        table.columns.push(
            crate::storage::schema::ColumnDef::new(
                "updated_at".to_string(),
                crate::storage::schema::DataType::UnsignedInteger,
            )
            .not_null(),
        );
        table.constraints.push(
            crate::storage::schema::Constraint::new(
                "not_null_created_at".to_string(),
                crate::storage::schema::ConstraintType::NotNull,
            )
            .on_columns(vec!["created_at".to_string()]),
        );
        table.constraints.push(
            crate::storage::schema::Constraint::new(
                "not_null_updated_at".to_string(),
                crate::storage::schema::ConstraintType::NotNull,
            )
            .on_columns(vec!["updated_at".to_string()]),
        );
    }
    table
        .validate()
        .map_err(|err| RedDBError::Query(format!("invalid table definition: {err}")))?;
    Ok(table)
}

fn column_def_from_ddl(column: &CreateColumnDef) -> RedDBResult<crate::storage::schema::ColumnDef> {
    let data_type = resolve_declared_data_type(&column.data_type)
        .map_err(|err| RedDBError::Query(err.to_string()))?;
    let mut column_def = crate::storage::schema::ColumnDef::new(column.name.clone(), data_type);
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
            resolve_declared_data_type(element_type)
                .map_err(|err| RedDBError::Query(err.to_string()))?,
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

fn current_unix_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}
