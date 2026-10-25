pub(crate) use reddb_engine::catalog::enforcement::contract_enforces_model;
use reddb_engine::catalog::enforcement::{
    build_row_update_contract_plan_for_contract, collection_model_allows, collection_model_name,
    find_row_uniqueness_conflict_for_contract, normalize_row_fields_for_contract_at,
    resolved_uniqueness_rules, row_uniqueness_error, uniqueness_columns_match, value_signature,
    CollectionContractEnforcer, ContractRow, DefaultTtlPlan, NormalizeMode, UniquenessConflict,
};
pub(crate) use reddb_engine::catalog::schema::has_contract_expressions;

pub(crate) fn validate_contract_expressions(contract: &CollectionContract) -> RedDBResult<()> {
    Ok(reddb_engine::catalog::schema::validate_contract_expressions(contract)?)
}

use crate::application::entity::{RowUpdateColumnRule, RowUpdateContractPlan};
use crate::physical::CollectionContract;
use crate::storage::unified::MetadataValue;
use crate::RedDBResult;
use reddb_types::Value;

/// Enforces a declared collection model on a read without creating a contract.
pub(crate) fn ensure_collection_model_read(
    db: &crate::storage::unified::devx::RedDB,
    collection: &str,
    requested_model: crate::catalog::CollectionModel,
) -> RedDBResult<()> {
    let Some(contract) = db.collection_contract(collection) else {
        return Ok(());
    };
    if !contract_enforces_model(&contract)
        || collection_model_allows(contract.declared_model, requested_model)
    {
        return Ok(());
    }
    Err(crate::RedDBError::InvalidOperation(format!(
        "collection '{}' is declared as '{}' and does not allow '{}' reads",
        collection,
        collection_model_name(contract.declared_model),
        collection_model_name(requested_model)
    )))
}

fn ensure_collection_model_contract(
    db: &crate::storage::unified::devx::RedDB,
    collection: &str,
    requested_model: crate::catalog::CollectionModel,
) -> RedDBResult<()> {
    if let Some(contract) = db.collection_contract(collection) {
        return Ok(CollectionContractEnforcer::new(&contract).ensure_model(requested_model)?);
    }

    let now = implicit_contract_unix_ms();
    db.save_collection_contract(crate::physical::CollectionContract {
        name: collection.to_string(),
        declared_model: requested_model,
        schema_mode: crate::catalog::SchemaMode::Dynamic,
        origin: crate::physical::ContractOrigin::Implicit,
        version: 1,
        created_at_unix_ms: now,
        updated_at_unix_ms: now,
        default_ttl_ms: db.collection_default_ttl_ms(collection),
        vector_dimension: None,
        vector_metric: None,
        context_index_fields: Vec::new(),
        declared_columns: Vec::new(),
        table_def: matches!(requested_model, crate::catalog::CollectionModel::Table)
            .then(|| reddb_types::TableDef::new(collection.to_string())),
        timestamps_enabled: false,
        context_index_enabled: false,
        metrics_raw_retention_ms: None,
        metrics_rollup_policies: Vec::new(),
        metrics_tenant_identity: None,
        metrics_namespace: None,
        // Implicit contracts are created on first write — mutability
        // is the default until the operator runs explicit DDL.
        append_only: false,
        subscriptions: Vec::new(),
        analytics_config: Vec::new(),
        session_key: None,
        session_gap_ms: None,
        retention_duration_ms: None,
        analytical_storage: None,

        ai_policy: None,
    })
    .map(|_| ())
    .map_err(|err| crate::RedDBError::Internal(err.to_string()))
}

fn implicit_contract_unix_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn ensure_vector_dimension_contract(
    db: &crate::storage::unified::devx::RedDB,
    collection: &str,
    actual_dimension: usize,
) -> RedDBResult<()> {
    let Some(contract) = db.collection_contract(collection) else {
        return Ok(());
    };
    Ok(CollectionContractEnforcer::new(&contract).ensure_vector_dimension(actual_dimension)?)
}

mod write_adapter {
    use super::*;

    pub(crate) struct CollectionContractWriteEnforcer<'a> {
        runtime: &'a crate::RedDBRuntime,
        db: &'a crate::storage::unified::devx::RedDB,
        collection: &'a str,
    }

    impl<'a> CollectionContractWriteEnforcer<'a> {
        pub(crate) fn new(
            runtime: &'a crate::RedDBRuntime,
            db: &'a crate::storage::unified::devx::RedDB,
            collection: &'a str,
        ) -> Self {
            Self {
                runtime,
                db,
                collection,
            }
        }

        pub(crate) fn ensure_model(
            &self,
            requested_model: crate::catalog::CollectionModel,
        ) -> RedDBResult<()> {
            ensure_collection_model_contract(self.db, self.collection, requested_model)
        }

        pub(crate) fn apply_default_ttl(&self, metadata: &mut Vec<(String, MetadataValue)>) {
            self.default_ttl_plan().apply(metadata);
        }

        pub(crate) fn default_ttl_plan(&self) -> DefaultTtlPlan {
            DefaultTtlPlan {
                ttl_ms: self.db.collection_default_ttl_ms(self.collection),
            }
        }

        pub(crate) fn ensure_vector_dimension(&self, actual_dimension: usize) -> RedDBResult<()> {
            ensure_vector_dimension_contract(self.db, self.collection, actual_dimension)
        }

        pub(crate) fn managed_timestamp_value(&self) -> Value {
            Value::UnsignedInteger(current_unix_ms_u64())
        }

        pub(crate) fn normalize_insert_fields(
            &self,
            fields: Vec<(String, Value)>,
        ) -> RedDBResult<Vec<(String, Value)>> {
            normalize_row_fields_for_contract_with_mode(
                self.db,
                self.collection,
                fields,
                NormalizeMode::Insert,
            )
        }

        pub(crate) fn normalize_update_fields(
            &self,
            fields: Vec<(String, Value)>,
        ) -> RedDBResult<Vec<(String, Value)>> {
            normalize_row_fields_for_contract_with_mode(
                self.db,
                self.collection,
                fields,
                NormalizeMode::Update,
            )
        }

        pub(crate) fn enforce_row_uniqueness(
            &self,
            fields: &[(String, Value)],
            exclude_id: Option<crate::storage::EntityId>,
        ) -> RedDBResult<()> {
            enforce_row_uniqueness(self.runtime, self.db, self.collection, fields, exclude_id)
        }

        pub(crate) fn row_uniqueness_conflict_id(
            &self,
            fields: &[(String, Value)],
            target: Option<&[String]>,
        ) -> RedDBResult<Option<crate::storage::EntityId>> {
            let conflict = find_row_uniqueness_conflict(
                self.runtime,
                self.db,
                self.collection,
                fields,
                target,
                None,
            )?;
            if let Some(conflict) = conflict {
                return Ok(Some(conflict.entity_id));
            }
            self.runtime
                .unique_conflict_id(self.collection, fields, target)
        }

        pub(crate) fn has_row_uniqueness_conflict_with_rows(
            &self,
            fields: &[(String, Value)],
            existing_fields: &[Vec<(String, Value)>],
            target: Option<&[String]>,
        ) -> RedDBResult<bool> {
            if self.runtime.has_unique_batch_conflict(
                self.collection,
                fields,
                existing_fields,
                target,
            ) {
                return Ok(true);
            }
            let Some(contract) = self.db.collection_contract(self.collection) else {
                return Ok(false);
            };
            if target.is_some_and(|target| {
                self.runtime.has_unique_target(self.collection, target)
                    && !resolved_uniqueness_rules(&contract)
                        .iter()
                        .any(|rule| uniqueness_columns_match(&rule.columns, target))
            }) {
                return Ok(false);
            }
            let existing_rows = existing_fields
                .iter()
                .enumerate()
                .map(|(index, fields)| ContractRow {
                    id: crate::storage::EntityId::new(index as u64 + 1),
                    fields: fields.iter().cloned().collect(),
                })
                .collect::<Vec<_>>();
            find_row_uniqueness_conflict_for_contract(
                &contract,
                self.collection,
                fields,
                &existing_rows,
                target,
                None,
            )
            .map(|conflict| conflict.is_some())
            .map_err(Into::into)
        }

        pub(crate) fn enforce_batch_uniqueness(
            &self,
            rows: &[Vec<(String, Value)>],
        ) -> RedDBResult<()> {
            enforce_row_batch_uniqueness(self.db, self.collection, rows)
        }

        pub(crate) fn requires_uniqueness_check(&self, modified_columns: &[String]) -> bool {
            row_update_requires_uniqueness_check(self.db, self.collection, modified_columns)
        }
    }
}

pub(crate) use write_adapter::CollectionContractWriteEnforcer;

fn normalize_row_fields_for_contract_with_mode(
    db: &crate::storage::unified::devx::RedDB,
    collection: &str,
    fields: Vec<(String, Value)>,
    mode: NormalizeMode,
) -> RedDBResult<Vec<(String, Value)>> {
    let Some(contract) = db.collection_contract(collection) else {
        return Ok(fields);
    };
    Ok(normalize_row_fields_for_contract_at(
        &contract,
        collection,
        fields,
        mode,
        current_unix_ms_u64(),
    )?)
}

fn current_unix_ms_u64() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Hold across validation and installation, not for the transaction lifetime.
/// Unconstrained collections do not acquire this gate. Pending rows themselves
/// reserve keys until their creator/deleter's outcome is known.
pub(crate) fn row_constraint_lock(
    runtime: &crate::RedDBRuntime,
    collection: &str,
) -> Option<std::sync::Arc<parking_lot::ReentrantMutex<()>>> {
    let db = runtime.db();
    let constrained = db
        .collection_contract_arc(collection)
        .is_some_and(|contract| {
            matches!(
                contract.declared_model,
                crate::catalog::CollectionModel::Table | crate::catalog::CollectionModel::Mixed
            ) && !resolved_uniqueness_rules(&contract).is_empty()
        });
    if !constrained && !runtime.index_store_ref().has_unique_index(collection) {
        return None;
    }
    db.store()
        .get_collection(collection)
        .map(|manager| manager.row_constraint_lock())
}

fn enforce_row_uniqueness(
    runtime: &crate::RedDBRuntime,
    db: &crate::storage::unified::devx::RedDB,
    collection: &str,
    fields: &[(String, Value)],
    exclude_id: Option<crate::storage::EntityId>,
) -> RedDBResult<()> {
    if let Some(conflict) =
        find_row_uniqueness_conflict(runtime, db, collection, fields, None, exclude_id)?
    {
        return Err(row_uniqueness_error(&conflict.rule, collection).into());
    }
    Ok(())
}

fn find_row_uniqueness_conflict(
    runtime: &crate::RedDBRuntime,
    db: &crate::storage::unified::devx::RedDB,
    collection: &str,
    fields: &[(String, Value)],
    target: Option<&[String]>,
    exclude_id: Option<crate::storage::EntityId>,
) -> RedDBResult<Option<UniquenessConflict>> {
    let Some(contract) = db.collection_contract(collection) else {
        return Ok(None);
    };
    if !matches!(
        contract.declared_model,
        crate::catalog::CollectionModel::Table | crate::catalog::CollectionModel::Mixed
    ) || resolved_uniqueness_rules(&contract).is_empty()
    {
        return Ok(None);
    }
    let Some(manager) = db.store().get_collection(collection) else {
        return Ok(None);
    };
    let rules = resolved_uniqueness_rules(&contract);
    let columns: Vec<Vec<String>> = rules.iter().map(|rule| rule.columns.clone()).collect();
    if let Some(target) = target {
        if !rules
            .iter()
            .any(|rule| uniqueness_columns_match(&rule.columns, target))
            && !runtime.has_unique_target(collection, target)
        {
            return Err(crate::RedDBError::Query(format!(
                "no unique or primary-key constraint on collection '{}' matches ON CONFLICT ({})",
                collection,
                target.join(", ")
            )));
        }
    }
    let input_fields: std::collections::BTreeMap<&str, &Value> = fields
        .iter()
        .map(|(name, value)| (name.as_str(), value))
        .collect();
    let snapshot_manager = runtime.snapshot_manager();
    let own_xids = runtime.own_transaction_xids();
    let reserves_key = |entity: &crate::storage::UnifiedEntity| {
        snapshot_manager.row_reserves_unique_key(entity.xmin, entity.xmax, &own_xids)
    };
    for (key_index, rule) in rules.into_iter().enumerate() {
        if target.is_some_and(|target| !uniqueness_columns_match(&rule.columns, target)) {
            continue;
        }
        let mut signatures = Vec::new();
        let mut skip_rule = false;
        for column in &rule.columns {
            match input_fields.get(column.as_str()).copied() {
                Some(Value::Null) | None if rule.primary_key => {
                    return Err(crate::RedDBError::Query(format!(
                        "primary key '{}' in collection '{}' requires non-null column '{}'",
                        rule.name, collection, column
                    )));
                }
                Some(Value::Null) | None => {
                    skip_rule = true;
                    break;
                }
                Some(value) => signatures.push(value_signature(value)),
            }
        }
        if skip_rule {
            continue;
        }
        if let Some(entity_id) = manager.find_unique_key_conflict(
            &columns,
            key_index,
            &signatures,
            exclude_id,
            &reserves_key,
        ) {
            return Ok(Some(UniquenessConflict { rule, entity_id }));
        }
    }
    Ok(None)
}

fn enforce_row_batch_uniqueness(
    db: &crate::storage::unified::devx::RedDB,
    collection: &str,
    rows: &[Vec<(String, Value)>],
) -> RedDBResult<()> {
    let Some(contract) = db.collection_contract(collection) else {
        return Ok(());
    };
    Ok(
        reddb_engine::catalog::enforcement::enforce_row_batch_uniqueness_for_contract(
            &contract, collection, rows,
        )?,
    )
}

fn row_update_requires_uniqueness_check(
    db: &crate::storage::unified::devx::RedDB,
    collection: &str,
    modified_columns: &[String],
) -> bool {
    if modified_columns.is_empty() {
        return false;
    }

    let Some(contract) = db.collection_contract(collection) else {
        return false;
    };
    if contract.declared_model != crate::catalog::CollectionModel::Table
        && contract.declared_model != crate::catalog::CollectionModel::Mixed
    {
        return false;
    }

    let rules = resolved_uniqueness_rules(&contract);
    if rules.is_empty() {
        return false;
    }

    rules.iter().any(|rule| {
        rule.columns.iter().any(|column| {
            modified_columns
                .iter()
                .any(|modified| modified.eq_ignore_ascii_case(column))
        })
    })
}

pub(crate) fn build_row_update_contract_plan(
    db: &crate::storage::unified::devx::RedDB,
    collection: &str,
) -> RedDBResult<Option<RowUpdateContractPlan>> {
    let Some(contract) = db.collection_contract(collection) else {
        return Ok(None);
    };

    Ok(Some(build_row_update_contract_plan_for_contract(
        &contract,
    )?))
}

pub(crate) fn normalize_declared_value(
    context: &str,
    name: &str,
    sql_type: &reddb_types::SqlTypeName,
    value: Value,
) -> RedDBResult<Value> {
    Ok(
        reddb_engine::catalog::enforcement::normalize_declared_value(
            context, name, sql_type, value,
        )?,
    )
}
pub(crate) fn normalize_row_update_assignment_with_plan(
    collection: &str,
    column: &str,
    value: Value,
    plan: Option<&RowUpdateContractPlan>,
) -> RedDBResult<Value> {
    Ok(
        reddb_engine::catalog::enforcement::normalize_row_update_assignment_with_plan(
            collection, column, value, plan,
        )?,
    )
}
pub(crate) fn normalize_row_update_value_for_rule(
    collection: &str,
    value: Value,
    rule: Option<&RowUpdateColumnRule>,
) -> RedDBResult<Value> {
    Ok(
        reddb_engine::catalog::enforcement::normalize_row_update_value_for_rule(
            collection, value, rule,
        )?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{CollectionModel, SchemaMode};
    use crate::physical::ContractOrigin;
    use reddb_types::{ColumnDef, Constraint, ConstraintType, DataType, TableDef};

    fn table_contract(schema_mode: SchemaMode) -> CollectionContract {
        let mut table_def = TableDef::new("people");
        table_def
            .columns
            .push(ColumnDef::new("name", DataType::Text));
        CollectionContract {
            name: "people".to_string(),
            declared_model: CollectionModel::Table,
            schema_mode,
            origin: ContractOrigin::Explicit,
            version: 1,
            created_at_unix_ms: 0,
            updated_at_unix_ms: 0,
            default_ttl_ms: None,
            vector_dimension: None,
            vector_metric: None,
            context_index_fields: Vec::new(),
            declared_columns: Vec::new(),
            table_def: Some(table_def),
            timestamps_enabled: false,
            context_index_enabled: false,
            metrics_raw_retention_ms: None,
            metrics_rollup_policies: Vec::new(),
            metrics_tenant_identity: None,
            metrics_namespace: None,
            append_only: false,
            subscriptions: Vec::new(),
            analytics_config: Vec::new(),
            session_key: None,
            session_gap_ms: None,
            retention_duration_ms: None,
            analytical_storage: None,
            ai_policy: None,
        }
    }

    #[test]
    fn schema_modes_decide_whether_undeclared_row_fields_are_allowed() {
        let cases = [
            (SchemaMode::Strict, false),
            (SchemaMode::SemiStructured, true),
            (SchemaMode::Dynamic, true),
        ];

        for (mode, allowed) in cases {
            let contract = table_contract(mode);
            let result = CollectionContractEnforcer::new(&contract)
                .normalize_insert_at(vec![("nickname".to_string(), Value::Text("Al".into()))], 7);

            assert_eq!(result.is_ok(), allowed, "schema mode: {mode:?}");
        }
    }

    #[test]
    fn default_ttl_plan_respects_existing_metadata_and_integer_bounds() {
        let cases = [
            (None, Vec::new(), None),
            (
                Some(50),
                vec![("_ttl_ms".to_string(), MetadataValue::Int(10))],
                None,
            ),
            (
                Some(i64::MAX as u64),
                Vec::new(),
                Some(MetadataValue::Int(i64::MAX)),
            ),
            (
                Some(i64::MAX as u64 + 1),
                Vec::new(),
                Some(MetadataValue::Timestamp(i64::MAX as u64 + 1)),
            ),
        ];

        for (default_ttl_ms, metadata, expected) in cases {
            let mut contract = table_contract(SchemaMode::Strict);
            contract.default_ttl_ms = default_ttl_ms;
            let plan = CollectionContractEnforcer::new(&contract).default_ttl_metadata(&metadata);
            assert_eq!(plan.map(|(_, value)| value), expected);
        }
    }

    #[test]
    fn uniqueness_rules_cover_primary_unique_and_null_edges() {
        let mut contract = table_contract(SchemaMode::Strict);
        let table = contract.table_def.as_mut().expect("table contract");
        table.columns.push(ColumnDef::new("id", DataType::Integer));
        table.columns.push(ColumnDef::new("email", DataType::Text));
        table.primary_key = vec!["id".to_string()];
        table.constraints.push(
            Constraint::new("unique_email", ConstraintType::Unique)
                .on_columns(vec!["email".to_string()]),
        );
        let cases = [
            (
                vec![
                    vec![("id".to_string(), Value::Integer(1))],
                    vec![("id".to_string(), Value::Integer(1))],
                ],
                false,
            ),
            (
                vec![
                    vec![
                        ("id".to_string(), Value::Integer(1)),
                        ("email".to_string(), Value::Text("a@example.com".into())),
                    ],
                    vec![
                        ("id".to_string(), Value::Integer(2)),
                        ("email".to_string(), Value::Text("a@example.com".into())),
                    ],
                ],
                false,
            ),
            (
                vec![
                    vec![
                        ("id".to_string(), Value::Integer(1)),
                        ("email".to_string(), Value::Null),
                    ],
                    vec![
                        ("id".to_string(), Value::Integer(2)),
                        ("email".to_string(), Value::Null),
                    ],
                ],
                true,
            ),
            (vec![vec![("id".to_string(), Value::Null)]], false),
        ];

        let enforcer = CollectionContractEnforcer::new(&contract);
        for (rows, allowed) in cases {
            assert_eq!(enforcer.enforce_batch_uniqueness(&rows).is_ok(), allowed);
        }
    }

    #[test]
    fn uniqueness_verdict_compares_rows_and_excludes_the_updated_entity() {
        let mut contract = table_contract(SchemaMode::Strict);
        let table = contract.table_def.as_mut().expect("table contract");
        table.columns.push(ColumnDef::new("id", DataType::Integer));
        table.primary_key = vec!["id".to_string()];
        let existing = ContractRow {
            id: crate::storage::EntityId::new(9),
            fields: [("id".to_string(), Value::Integer(1))]
                .into_iter()
                .collect(),
        };
        let candidate = vec![("id".to_string(), Value::Integer(1))];
        let enforcer = CollectionContractEnforcer::new(&contract);

        assert!(enforcer
            .enforce_uniqueness(&candidate, &[existing], None)
            .is_err());

        let existing = ContractRow {
            id: crate::storage::EntityId::new(9),
            fields: [("id".to_string(), Value::Integer(1))]
                .into_iter()
                .collect(),
        };
        assert!(enforcer
            .enforce_uniqueness(
                &candidate,
                &[existing],
                Some(crate::storage::EntityId::new(9)),
            )
            .is_ok());
    }

    #[test]
    fn insert_and_update_normalization_apply_defaults_and_managed_timestamps() {
        let mut contract = table_contract(SchemaMode::Strict);
        contract.timestamps_enabled = true;
        contract.table_def.as_mut().expect("table contract").columns = vec![
            ColumnDef::new("count", DataType::Integer)
                .not_null()
                .with_default(b"5".to_vec()),
            ColumnDef::new("created_at", DataType::UnsignedInteger).not_null(),
            ColumnDef::new("updated_at", DataType::UnsignedInteger).not_null(),
        ];
        let enforcer = CollectionContractEnforcer::new(&contract);

        let inserted = enforcer.normalize_insert_at(Vec::new(), 100).unwrap();
        assert_eq!(
            inserted,
            vec![
                ("count".to_string(), Value::Integer(5)),
                ("created_at".to_string(), Value::UnsignedInteger(100)),
                ("updated_at".to_string(), Value::UnsignedInteger(100)),
            ]
        );

        let updated = enforcer
            .normalize_update_at(
                vec![
                    ("count".to_string(), Value::Text("7".into())),
                    ("created_at".to_string(), Value::UnsignedInteger(100)),
                    ("updated_at".to_string(), Value::UnsignedInteger(100)),
                ],
                200,
            )
            .unwrap();
        assert_eq!(
            updated,
            vec![
                ("count".to_string(), Value::Integer(7)),
                ("created_at".to_string(), Value::UnsignedInteger(100)),
                ("updated_at".to_string(), Value::UnsignedInteger(200)),
            ]
        );
    }

    #[test]
    fn literal_coercion_edges_return_values_or_verdicts() {
        let cases = [
            (DataType::Integer, Value::Text("42".into()), true),
            (DataType::Integer, Value::Text("forty-two".into()), false),
            (DataType::Integer, Value::Null, false),
            (DataType::Json, Value::Text("{\"ok\":true}".into()), false),
            (
                DataType::Json,
                Value::Json(br#"{\"ok\":true}"#.to_vec()),
                true,
            ),
        ];

        for (data_type, value, allowed) in cases {
            let mut contract = table_contract(SchemaMode::Strict);
            contract.table_def.as_mut().expect("table contract").columns =
                vec![ColumnDef::new("value", data_type).not_null()];
            let result = CollectionContractEnforcer::new(&contract)
                .normalize_insert_at(vec![("value".to_string(), value)], 1);
            assert_eq!(result.is_ok(), allowed, "data type: {data_type:?}");
        }
    }
}
