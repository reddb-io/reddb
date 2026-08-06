use std::collections::HashMap;

use crate::application::entity::{RowUpdateColumnRule, RowUpdateContractPlan};
use crate::application::ttl_payload::has_internal_ttl_metadata;
use crate::physical::CollectionContract;
use crate::storage::query::resolve_declared_data_type;
use crate::storage::schema::{coerce as coerce_schema_value, DataType, Value};
use crate::storage::unified::MetadataValue;
use crate::RedDBResult;

/// Pure collection-contract evaluator used by unit tests and non-runtime callers.
pub(crate) struct CollectionContractEnforcer<'a> {
    contract: &'a crate::physical::CollectionContract,
}

#[derive(Clone, Copy)]
pub(crate) struct DefaultTtlPlan {
    ttl_ms: Option<u64>,
}

pub(crate) struct ContractRow {
    pub(crate) id: crate::storage::EntityId,
    pub(crate) fields: std::collections::BTreeMap<String, Value>,
}

impl DefaultTtlPlan {
    pub(crate) fn apply(self, metadata: &mut Vec<(String, MetadataValue)>) {
        if let Some(entry) = default_ttl_metadata(self.ttl_ms, metadata) {
            metadata.push(entry);
        }
    }
}

impl<'a> CollectionContractEnforcer<'a> {
    pub(crate) fn new(contract: &'a crate::physical::CollectionContract) -> Self {
        Self { contract }
    }

    pub(crate) fn normalize_insert_at(
        &self,
        fields: Vec<(String, Value)>,
        now_ms: u64,
    ) -> RedDBResult<Vec<(String, Value)>> {
        normalize_row_fields_for_contract_at(
            self.contract,
            &self.contract.name,
            fields,
            NormalizeMode::Insert,
            now_ms,
        )
    }

    pub(crate) fn ensure_model(
        &self,
        requested_model: crate::catalog::CollectionModel,
    ) -> RedDBResult<()> {
        if !contract_enforces_model(self.contract)
            || collection_model_allows(self.contract.declared_model, requested_model)
        {
            return Ok(());
        }
        if requested_model == crate::catalog::CollectionModel::Vector
            && self
                .contract
                .ai_policy
                .as_ref()
                .is_some_and(|policy| policy.embed.is_some() || policy.vision.is_some())
        {
            return Ok(());
        }
        Err(crate::RedDBError::InvalidOperation(format!(
            "collection '{}' is declared as '{}' and does not allow '{}' writes",
            self.contract.name,
            collection_model_name(self.contract.declared_model),
            collection_model_name(requested_model)
        )))
    }

    pub(crate) fn ensure_vector_dimension(&self, actual_dimension: usize) -> RedDBResult<()> {
        let Some(expected_dimension) = self.contract.vector_dimension else {
            return Ok(());
        };
        if expected_dimension == actual_dimension {
            return Ok(());
        }
        Err(crate::RedDBError::Query(format!(
            "vector dimension mismatch for collection '{}': expected {expected_dimension}, got {actual_dimension}",
            self.contract.name
        )))
    }

    pub(crate) fn normalize_update_at(
        &self,
        fields: Vec<(String, Value)>,
        now_ms: u64,
    ) -> RedDBResult<Vec<(String, Value)>> {
        normalize_row_fields_for_contract_at(
            self.contract,
            &self.contract.name,
            fields,
            NormalizeMode::Update,
            now_ms,
        )
    }

    pub(crate) fn default_ttl_metadata(
        &self,
        metadata: &[(String, MetadataValue)],
    ) -> Option<(String, MetadataValue)> {
        default_ttl_metadata(self.contract.default_ttl_ms, metadata)
    }

    pub(crate) fn default_ttl_plan(&self) -> DefaultTtlPlan {
        DefaultTtlPlan {
            ttl_ms: self.contract.default_ttl_ms,
        }
    }

    pub(crate) fn enforce_batch_uniqueness(
        &self,
        rows: &[Vec<(String, Value)>],
    ) -> RedDBResult<()> {
        enforce_row_batch_uniqueness_for_contract(self.contract, &self.contract.name, rows)
    }

    pub(crate) fn enforce_uniqueness(
        &self,
        fields: &[(String, Value)],
        existing_rows: &[ContractRow],
        exclude_id: Option<crate::storage::EntityId>,
    ) -> RedDBResult<()> {
        enforce_row_uniqueness_for_contract(
            self.contract,
            &self.contract.name,
            fields,
            existing_rows,
            exclude_id,
        )
    }

    pub(crate) fn row_update_plan(&self) -> RedDBResult<RowUpdateContractPlan> {
        build_row_update_contract_plan_for_contract(self.contract)
    }
}

fn default_ttl_metadata(
    default_ttl_ms: Option<u64>,
    metadata: &[(String, MetadataValue)],
) -> Option<(String, MetadataValue)> {
    if has_internal_ttl_metadata(metadata) {
        return None;
    }

    default_ttl_ms.map(|ttl| {
        (
            "_ttl_ms".to_string(),
            if ttl <= i64::MAX as u64 {
                MetadataValue::Int(ttl as i64)
            } else {
                MetadataValue::Timestamp(ttl)
            },
        )
    })
}

fn ensure_collection_model_contract(
    db: &crate::storage::unified::devx::RedDB,
    collection: &str,
    requested_model: crate::catalog::CollectionModel,
) -> RedDBResult<()> {
    if let Some(contract) = db.collection_contract(collection) {
        return CollectionContractEnforcer::new(&contract).ensure_model(requested_model);
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
            .then(|| crate::storage::schema::TableDef::new(collection.to_string())),
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

pub(crate) fn contract_enforces_model(contract: &CollectionContract) -> bool {
    !matches!(
        (&contract.origin, &contract.schema_mode),
        (
            crate::physical::ContractOrigin::Implicit,
            crate::catalog::SchemaMode::Dynamic,
        )
    )
}

fn implicit_contract_unix_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn collection_model_allows(
    declared_model: crate::catalog::CollectionModel,
    requested_model: crate::catalog::CollectionModel,
) -> bool {
    declared_model == requested_model || declared_model == crate::catalog::CollectionModel::Mixed
}

fn ensure_vector_dimension_contract(
    db: &crate::storage::unified::devx::RedDB,
    collection: &str,
    actual_dimension: usize,
) -> RedDBResult<()> {
    let Some(contract) = db.collection_contract(collection) else {
        return Ok(());
    };
    CollectionContractEnforcer::new(&contract).ensure_vector_dimension(actual_dimension)
}

fn collection_model_name(model: crate::catalog::CollectionModel) -> &'static str {
    match model {
        crate::catalog::CollectionModel::Table => "table",
        crate::catalog::CollectionModel::Document => "document",
        crate::catalog::CollectionModel::Graph => "graph",
        crate::catalog::CollectionModel::Vector => "vector",
        crate::catalog::CollectionModel::Hll => "hll",
        crate::catalog::CollectionModel::Sketch => "sketch",
        crate::catalog::CollectionModel::Filter => "filter",
        crate::catalog::CollectionModel::Kv => "kv",
        crate::catalog::CollectionModel::Config => "config",
        crate::catalog::CollectionModel::Vault => "vault",
        crate::catalog::CollectionModel::Mixed => "mixed",
        crate::catalog::CollectionModel::TimeSeries => "timeseries",
        crate::catalog::CollectionModel::Queue => "queue",
        crate::catalog::CollectionModel::Metrics => "metrics",
    }
}

#[derive(Clone)]
struct UniquenessRule {
    name: String,
    columns: Vec<String>,
    primary_key: bool,
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum NormalizeMode {
    /// First write for this row. Timestamps auto-filled from now on
    /// both `created_at` and `updated_at`; user attempts to set
    /// either column are rejected.
    Insert,
    /// Update/patch path. `created_at` is preserved from the existing
    /// row (immutable after insert); `updated_at` is bumped to now.
    /// User attempts to set either via the patch are rejected.
    Update,
}

mod write_adapter {
    use super::*;

    pub(crate) struct CollectionContractWriteEnforcer<'a> {
        db: &'a crate::storage::unified::devx::RedDB,
        collection: &'a str,
    }

    impl<'a> CollectionContractWriteEnforcer<'a> {
        pub(crate) fn new(
            db: &'a crate::storage::unified::devx::RedDB,
            collection: &'a str,
        ) -> Self {
            Self { db, collection }
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
            enforce_row_uniqueness(self.db, self.collection, fields, exclude_id)
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
    normalize_row_fields_for_contract_at(&contract, collection, fields, mode, current_unix_ms_u64())
}

fn normalize_row_fields_for_contract_at(
    contract: &crate::physical::CollectionContract,
    collection: &str,
    fields: Vec<(String, Value)>,
    mode: NormalizeMode,
    now_ms: u64,
) -> RedDBResult<Vec<(String, Value)>> {
    if contract.declared_model != crate::catalog::CollectionModel::Table
        || (contract.declared_columns.is_empty()
            && contract
                .table_def
                .as_ref()
                .map(|table| table.columns.is_empty())
                .unwrap_or(true))
    {
        return Ok(fields);
    }

    // Capture the pre-normalize value of created_at (if present) so
    // Update mode can preserve it. Also capture updated_at to detect
    // user attempts to set it via the patch payload.
    //
    // Heuristic for Update mode: if fields ALREADY contains a
    // `created_at` whose value matches the row's on-disk entity, the
    // caller is the patch pipeline carrying forward an auto-populated
    // column — not a user mutation. Allow pass-through in that case,
    // then restore the original value at the end.
    let existing_created_at = if contract.timestamps_enabled && mode == NormalizeMode::Update {
        fields
            .iter()
            .find(|(n, _)| n == "created_at")
            .map(|(_, v)| v.clone())
    } else {
        None
    };

    // Reject user attempts to set runtime-managed timestamp columns.
    // On Insert we reject any mention; on Update we only reject when
    // the patch pipeline handed us a NEW value (not the one we
    // auto-populated during the last insert).
    if contract.timestamps_enabled && mode == NormalizeMode::Insert {
        for (name, _) in &fields {
            if name == "created_at" || name == "updated_at" {
                return Err(crate::RedDBError::Query(format!(
                    "collection '{}' manages '{}' automatically — do not set it in INSERT",
                    collection, name
                )));
            }
        }
    }

    let mut provided = std::collections::BTreeMap::new();
    for (name, value) in &fields {
        provided.insert(name.clone(), value.clone());
    }

    let resolved_columns = resolved_contract_columns(&contract)?;
    let declared_names: std::collections::BTreeSet<String> = resolved_columns
        .iter()
        .map(|column| column.name.clone())
        .collect();
    let unknown_fields: Vec<String> = fields
        .iter()
        .filter(|(name, _)| !declared_names.contains(name))
        .map(|(name, _)| name.clone())
        .collect();
    if matches!(contract.schema_mode, crate::catalog::SchemaMode::Strict)
        && !unknown_fields.is_empty()
    {
        return Err(crate::RedDBError::Query(format!(
            "collection '{}' is strict and does not allow undeclared fields: {}",
            collection,
            unknown_fields.join(", ")
        )));
    }
    let mut normalized = Vec::new();

    for column in &resolved_columns {
        match provided.remove(&column.name) {
            Some(value) => {
                // Runtime-managed columns on Update: always overwrite
                // with the runtime's own value (preserved created_at
                // or fresh updated_at). User mutations are silently
                // discarded because we reject them earlier.
                if contract.timestamps_enabled && mode == NormalizeMode::Update {
                    match column.name.as_str() {
                        "created_at" => {
                            normalized.push((
                                column.name.clone(),
                                existing_created_at
                                    .clone()
                                    .unwrap_or(Value::UnsignedInteger(now_ms)),
                            ));
                            continue;
                        }
                        "updated_at" => {
                            normalized.push((column.name.clone(), Value::UnsignedInteger(now_ms)));
                            continue;
                        }
                        _ => {}
                    }
                }
                normalized.push((
                    column.name.clone(),
                    normalize_contract_value(collection, column, value)?,
                ));
            }
            None => {
                // Runtime-managed timestamp columns: auto-fill with now
                // when the contract opted in. Both get the same value on
                // first insert so callers can order by either.
                if contract.timestamps_enabled
                    && (column.name == "created_at" || column.name == "updated_at")
                {
                    normalized.push((column.name.clone(), Value::UnsignedInteger(now_ms)));
                    continue;
                }
                if let Some(default) = &column.default {
                    normalized.push((
                        column.name.clone(),
                        coerce_contract_literal(collection, &column.name, column, default)?,
                    ));
                } else if column.not_null {
                    return Err(crate::RedDBError::Query(format!(
                        "missing required column '{}' for collection '{}'",
                        column.name, collection
                    )));
                }
            }
        }
    }

    for (name, value) in fields {
        if !declared_names.contains(&name) {
            normalized.push((name, value));
        }
    }

    Ok(normalized)
}

fn current_unix_ms_u64() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn enforce_row_uniqueness(
    db: &crate::storage::unified::devx::RedDB,
    collection: &str,
    fields: &[(String, Value)],
    exclude_id: Option<crate::storage::EntityId>,
) -> RedDBResult<()> {
    let Some(contract) = db.collection_contract(collection) else {
        return Ok(());
    };
    let Some(manager) = db.store().get_collection(collection) else {
        return Ok(());
    };
    let existing_rows: Vec<ContractRow> = manager
        .query_all(|_| true)
        .into_iter()
        .filter_map(|entity| {
            row_fields_from_entity(&entity).map(|fields| ContractRow {
                id: entity.id,
                fields,
            })
        })
        .collect();

    enforce_row_uniqueness_for_contract(&contract, collection, fields, &existing_rows, exclude_id)
}

fn enforce_row_uniqueness_for_contract(
    contract: &CollectionContract,
    collection: &str,
    fields: &[(String, Value)],
    existing_rows: &[ContractRow],
    exclude_id: Option<crate::storage::EntityId>,
) -> RedDBResult<()> {
    if !matches!(
        contract.declared_model,
        crate::catalog::CollectionModel::Table | crate::catalog::CollectionModel::Mixed
    ) {
        return Ok(());
    }

    let rules = resolved_uniqueness_rules(contract);
    if rules.is_empty() {
        return Ok(());
    }

    let input_fields: std::collections::BTreeMap<String, Value> = fields.iter().cloned().collect();

    for rule in &rules {
        let mut expected_signatures = Vec::new();
        let mut skip_rule = false;

        for column in &rule.columns {
            match input_fields.get(column) {
                Some(Value::Null) | None if rule.primary_key => {
                    return Err(crate::RedDBError::Query(format!(
                        "primary key '{}' in collection '{}' requires non-null column '{}'",
                        rule.name, collection, column
                    )))
                }
                Some(Value::Null) | None => {
                    skip_rule = true;
                    break;
                }
                Some(value) => {
                    expected_signatures.push((column.clone(), value_signature(value)));
                }
            }
        }

        if skip_rule {
            continue;
        }

        for existing in existing_rows {
            if exclude_id.is_some_and(|id| id == existing.id) {
                continue;
            }

            let duplicate = expected_signatures.iter().all(|(column, expected)| {
                existing
                    .fields
                    .get(column)
                    .map(|value| value_signature(value) == *expected)
                    .unwrap_or(false)
            });

            if duplicate {
                let qualifier = if rule.primary_key {
                    "primary key"
                } else {
                    "unique constraint"
                };
                return Err(crate::RedDBError::Query(format!(
                    "{} '{}' violated on collection '{}' for columns [{}]",
                    qualifier,
                    rule.name,
                    collection,
                    rule.columns.join(", ")
                )));
            }
        }
    }

    Ok(())
}

fn enforce_row_batch_uniqueness(
    db: &crate::storage::unified::devx::RedDB,
    collection: &str,
    rows: &[Vec<(String, Value)>],
) -> RedDBResult<()> {
    let Some(contract) = db.collection_contract(collection) else {
        return Ok(());
    };
    enforce_row_batch_uniqueness_for_contract(&contract, collection, rows)
}

fn enforce_row_batch_uniqueness_for_contract(
    contract: &CollectionContract,
    collection: &str,
    rows: &[Vec<(String, Value)>],
) -> RedDBResult<()> {
    if contract.declared_model != crate::catalog::CollectionModel::Table
        && contract.declared_model != crate::catalog::CollectionModel::Mixed
    {
        return Ok(());
    }

    let rules = resolved_uniqueness_rules(contract);
    if rules.is_empty() {
        return Ok(());
    }

    for rule in &rules {
        let mut seen = std::collections::HashMap::<String, usize>::new();
        for (row_index, fields) in rows.iter().enumerate() {
            let input_fields: std::collections::BTreeMap<String, Value> =
                fields.iter().cloned().collect();
            let mut signatures = Vec::new();
            let mut skip_rule = false;

            for column in &rule.columns {
                match input_fields.get(column) {
                    Some(Value::Null) | None if rule.primary_key => {
                        return Err(crate::RedDBError::Query(format!(
                            "primary key '{}' in collection '{}' requires non-null column '{}'",
                            rule.name, collection, column
                        )))
                    }
                    Some(Value::Null) | None => {
                        skip_rule = true;
                        break;
                    }
                    Some(value) => signatures.push(format!("{column}={}", value_signature(value))),
                }
            }

            if skip_rule {
                continue;
            }

            let signature = signatures.join("|");
            if let Some(previous_index) = seen.insert(signature, row_index) {
                return Err(crate::RedDBError::Query(format!(
                    "batch insert violates uniqueness rule '{}' in collection '{}' between rows {} and {}",
                    rule.name,
                    collection,
                    previous_index + 1,
                    row_index + 1
                )));
            }
        }
    }

    Ok(())
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

fn build_row_update_contract_plan_for_contract(
    contract: &CollectionContract,
) -> RedDBResult<RowUpdateContractPlan> {
    let declared_rules = if contract.declared_model == crate::catalog::CollectionModel::Table
        && !(contract.declared_columns.is_empty()
            && contract
                .table_def
                .as_ref()
                .map(|table| table.columns.is_empty())
                .unwrap_or(true))
    {
        resolved_contract_columns(&contract)?
            .into_iter()
            .map(|rule| {
                (
                    rule.name.clone(),
                    RowUpdateColumnRule {
                        name: rule.name,
                        data_type: rule.data_type,
                        data_type_name: rule.data_type_name,
                        not_null: rule.not_null,
                        enum_variants: rule.enum_variants,
                    },
                )
            })
            .collect()
    } else {
        HashMap::new()
    };

    let unique_columns = if matches!(
        contract.declared_model,
        crate::catalog::CollectionModel::Table | crate::catalog::CollectionModel::Mixed
    ) {
        resolved_uniqueness_rules(&contract)
            .into_iter()
            .flat_map(|rule| rule.columns.into_iter())
            .map(|column| (column, ()))
            .collect()
    } else {
        HashMap::new()
    };

    Ok(RowUpdateContractPlan {
        timestamps_enabled: contract.timestamps_enabled,
        strict_schema: matches!(contract.schema_mode, crate::catalog::SchemaMode::Strict),
        declared_rules,
        unique_columns,
    })
}

pub(crate) fn normalize_row_update_assignment_with_plan(
    collection: &str,
    column: &str,
    value: Value,
    row_contract_plan: Option<&RowUpdateContractPlan>,
) -> RedDBResult<Value> {
    let Some(plan) = row_contract_plan else {
        return Ok(value);
    };

    if plan.timestamps_enabled && (column == "created_at" || column == "updated_at") {
        return Err(crate::RedDBError::Query(format!(
            "collection '{}' manages '{}' automatically — do not set it in UPDATE",
            collection, column
        )));
    }

    if let Some(rule) = plan.declared_rules.get(column) {
        let rule = ResolvedColumnRule {
            name: rule.name.clone(),
            data_type: rule.data_type,
            data_type_name: rule.data_type_name.clone(),
            not_null: rule.not_null,
            default: None,
            enum_variants: rule.enum_variants.clone(),
        };
        normalize_contract_value(collection, &rule, value)
    } else if plan.strict_schema {
        Err(crate::RedDBError::Query(format!(
            "collection '{}' is strict and does not allow undeclared fields: {}",
            collection, column
        )))
    } else {
        Ok(value)
    }
}

pub(crate) fn normalize_row_update_value_for_rule(
    collection: &str,
    value: Value,
    row_rule: Option<&RowUpdateColumnRule>,
) -> RedDBResult<Value> {
    let Some(rule) = row_rule else {
        return Ok(value);
    };

    let rule = ResolvedColumnRule {
        name: rule.name.clone(),
        data_type: rule.data_type,
        data_type_name: rule.data_type_name.clone(),
        not_null: rule.not_null,
        default: None,
        enum_variants: rule.enum_variants.clone(),
    };
    normalize_contract_value(collection, &rule, value)
}

fn resolved_uniqueness_rules(
    contract: &crate::physical::CollectionContract,
) -> Vec<UniquenessRule> {
    let mut rules = Vec::new();

    if let Some(table_def) = &contract.table_def {
        if !table_def.primary_key.is_empty() {
            rules.push(UniquenessRule {
                name: "primary_key".to_string(),
                columns: table_def.primary_key.clone(),
                primary_key: true,
            });
        }

        for constraint in &table_def.constraints {
            if matches!(
                constraint.constraint_type,
                crate::storage::schema::ConstraintType::PrimaryKey
            ) && !constraint.columns.is_empty()
            {
                rules.push(UniquenessRule {
                    name: constraint.name.clone(),
                    columns: constraint.columns.clone(),
                    primary_key: true,
                });
            } else if matches!(
                constraint.constraint_type,
                crate::storage::schema::ConstraintType::Unique
            ) && !constraint.columns.is_empty()
            {
                rules.push(UniquenessRule {
                    name: constraint.name.clone(),
                    columns: constraint.columns.clone(),
                    primary_key: false,
                });
            }
        }
    } else {
        for column in &contract.declared_columns {
            if column.primary_key {
                rules.push(UniquenessRule {
                    name: format!("pk_{}", column.name),
                    columns: vec![column.name.clone()],
                    primary_key: true,
                });
            } else if column.unique {
                rules.push(UniquenessRule {
                    name: format!("uniq_{}", column.name),
                    columns: vec![column.name.clone()],
                    primary_key: false,
                });
            }
        }
    }

    let mut dedup = std::collections::BTreeSet::new();
    rules
        .into_iter()
        .filter(|rule| dedup.insert((rule.primary_key, rule.columns.clone())))
        .collect()
}

fn row_fields_from_entity(
    entity: &crate::storage::UnifiedEntity,
) -> Option<std::collections::BTreeMap<String, Value>> {
    match &entity.data {
        crate::storage::EntityData::Row(row) => {
            if let Some(named) = &row.named {
                Some(
                    named
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect(),
                )
            } else {
                row.schema.as_ref().map(|schema| {
                    schema
                        .iter()
                        .cloned()
                        .zip(row.columns.iter().cloned())
                        .collect()
                })
            }
        }
        _ => None,
    }
}

fn value_signature(value: &Value) -> String {
    format!("{value:?}")
}

fn normalize_contract_value(
    collection: &str,
    column: &ResolvedColumnRule,
    value: Value,
) -> RedDBResult<Value> {
    if matches!(value, Value::Null) {
        if column.not_null {
            return Err(crate::RedDBError::Query(format!(
                "column '{}' in collection '{}' cannot be null",
                column.name, collection
            )));
        }
        return Ok(Value::Null);
    }

    let target = column.data_type;
    if value_matches_declared_type(&value, target) {
        return Ok(value);
    }

    let Some(raw) = value_to_coercion_input(&value) else {
        return Err(crate::RedDBError::Query(format!(
            "column '{}' in collection '{}' requires type '{}' but value cannot be coerced",
            column.name, collection, column.data_type_name
        )));
    };

    coerce_contract_literal(collection, &column.name, column, &raw)
}

fn coerce_contract_literal(
    collection: &str,
    column_name: &str,
    column: &ResolvedColumnRule,
    raw: &str,
) -> RedDBResult<Value> {
    let target = column.data_type;
    match target {
        DataType::Blob => Ok(Value::Blob(raw.as_bytes().to_vec())),
        // ADR 0067 (#1721): a JSON value is written as an inline strict-JSON
        // literal — a bare string is not silently wrapped as JSON. Wrap a
        // runtime string with `JSON_PARSE(<expr>)`. (An inline literal already
        // arrives as `Value::Json` and never reaches this coercion.)
        DataType::Json => Err(crate::RedDBError::Query(format!(
            "column '{column_name}' in collection '{collection}' requires an inline strict-JSON \
             literal (e.g. `{{\"k\": \"v\"}}`) or `JSON_PARSE(<expr>)`; a bare string is not \
             coerced to JSON (ADR 0067)"
        ))),
        DataType::Timestamp => raw.parse::<i64>().map(Value::Timestamp).map_err(|err| {
            crate::RedDBError::Query(format!(
                "failed to coerce column '{}' in collection '{}' to '{}': {}",
                column_name, collection, column.data_type_name, err
            ))
        }),
        DataType::Duration => raw.parse::<i64>().map(Value::Duration).map_err(|err| {
            crate::RedDBError::Query(format!(
                "failed to coerce column '{}' in collection '{}' to '{}': {}",
                column_name, collection, column.data_type_name, err
            ))
        }),
        DataType::Vector | DataType::Array => Err(crate::RedDBError::Query(format!(
            "column '{}' in collection '{}' requires '{}' and only typed values are accepted for this type",
            column_name, collection, column.data_type_name
        ))),
        _ => coerce_schema_value(raw, target, Some(column.enum_variants.as_slice())).map_err(
            |err| {
                crate::RedDBError::Query(format!(
                    "failed to coerce column '{}' in collection '{}' to '{}': {}",
                    column_name, collection, column.data_type_name, err
                ))
            },
        ),
    }
}

struct ResolvedColumnRule {
    name: String,
    data_type: DataType,
    data_type_name: String,
    not_null: bool,
    default: Option<String>,
    enum_variants: Vec<String>,
}

fn resolved_contract_columns(
    contract: &crate::physical::CollectionContract,
) -> RedDBResult<Vec<ResolvedColumnRule>> {
    if let Some(table_def) = &contract.table_def {
        return Ok(table_def
            .columns
            .iter()
            .map(|column| ResolvedColumnRule {
                name: column.name.clone(),
                data_type: column.data_type,
                data_type_name: data_type_name(column.data_type).to_string(),
                not_null: !column.nullable,
                default: column
                    .default
                    .as_ref()
                    .map(|bytes| String::from_utf8_lossy(bytes).to_string()),
                enum_variants: column.enum_variants.clone(),
            })
            .collect());
    }

    contract
        .declared_columns
        .iter()
        .map(|column| {
            let data_type = column
                .sql_type
                .as_ref()
                .map(crate::storage::query::resolve_sql_type_name)
                .transpose()
                .map_err(|err| crate::RedDBError::Query(err.to_string()))?
                .unwrap_or(parse_declared_data_type(&column.data_type)?);
            Ok(ResolvedColumnRule {
                name: column.name.clone(),
                data_type,
                data_type_name: column.data_type.clone(),
                not_null: column.not_null,
                default: column.default.clone(),
                enum_variants: column.enum_variants.clone(),
            })
        })
        .collect()
}

fn parse_declared_data_type(value: &str) -> RedDBResult<DataType> {
    resolve_declared_data_type(value).map_err(|err| crate::RedDBError::Query(err.to_string()))
}

fn data_type_name(data_type: DataType) -> &'static str {
    match data_type {
        DataType::Integer => "integer",
        DataType::UnsignedInteger => "unsigned_integer",
        DataType::Float => "float",
        DataType::Text => "text",
        DataType::Blob => "blob",
        DataType::Boolean => "boolean",
        DataType::Timestamp => "timestamp",
        DataType::Duration => "duration",
        DataType::IpAddr => "ipaddr",
        DataType::MacAddr => "macaddr",
        DataType::Vector => "vector",
        DataType::Nullable => "nullable",
        DataType::Unknown => "unknown",
        DataType::Json => "json",
        DataType::Uuid => "uuid",
        DataType::NodeRef => "noderef",
        DataType::EdgeRef => "edgeref",
        DataType::VectorRef => "vectorref",
        DataType::RowRef => "rowref",
        DataType::Color => "color",
        DataType::Email => "email",
        DataType::Url => "url",
        DataType::Phone => "phone",
        DataType::Semver => "semver",
        DataType::Cidr => "cidr",
        DataType::Date => "date",
        DataType::Time => "time",
        DataType::Decimal => "decimal",
        DataType::DecimalText => "decimal_text",
        DataType::Enum => "enum",
        DataType::Array => "array",
        DataType::TimestampMs => "timestamp_ms",
        DataType::Ipv4 => "ipv4",
        DataType::Ipv6 => "ipv6",
        DataType::Subnet => "subnet",
        DataType::Port => "port",
        DataType::Latitude => "latitude",
        DataType::Longitude => "longitude",
        DataType::GeoPoint => "geopoint",
        DataType::Country2 => "country2",
        DataType::Country3 => "country3",
        DataType::Lang2 => "lang2",
        DataType::Lang5 => "lang5",
        DataType::Currency => "currency",
        DataType::AssetCode => "asset_code",
        DataType::Money => "money",
        DataType::ColorAlpha => "color_alpha",
        DataType::BigInt => "bigint",
        DataType::KeyRef => "keyref",
        DataType::DocRef => "docref",
        DataType::TableRef => "tableref",
        DataType::PageRef => "pageref",
        DataType::Secret => "secret",
        DataType::Password => "password",
        DataType::TextZstd => "text",
        DataType::BlobZstd => "blob",
    }
}

fn value_matches_declared_type(value: &Value, target: DataType) -> bool {
    matches!(
        (value, target),
        (Value::Null, _)
            | (Value::Integer(_), DataType::Integer)
            | (Value::UnsignedInteger(_), DataType::UnsignedInteger)
            | (Value::Float(_), DataType::Float)
            | (Value::Text(_), DataType::Text)
            | (Value::Blob(_), DataType::Blob)
            | (Value::Boolean(_), DataType::Boolean)
            | (Value::Timestamp(_), DataType::Timestamp)
            | (Value::Duration(_), DataType::Duration)
            | (Value::IpAddr(_), DataType::IpAddr)
            | (Value::MacAddr(_), DataType::MacAddr)
            | (Value::Vector(_), DataType::Vector)
            | (Value::Json(_), DataType::Json)
            | (Value::Uuid(_), DataType::Uuid)
            | (Value::NodeRef(_), DataType::NodeRef)
            | (Value::EdgeRef(_), DataType::EdgeRef)
            | (Value::VectorRef(_, _), DataType::VectorRef)
            | (Value::RowRef(_, _), DataType::RowRef)
            | (Value::Color(_), DataType::Color)
            | (Value::Email(_), DataType::Email)
            | (Value::Url(_), DataType::Url)
            | (Value::Phone(_), DataType::Phone)
            | (Value::Semver(_), DataType::Semver)
            | (Value::Cidr(_, _), DataType::Cidr)
            | (Value::Date(_), DataType::Date)
            | (Value::Time(_), DataType::Time)
            | (Value::Decimal(_), DataType::Decimal)
            | (Value::DecimalText(_), DataType::DecimalText)
            | (Value::EnumValue(_), DataType::Enum)
            | (Value::Array(_), DataType::Array)
            | (Value::TimestampMs(_), DataType::TimestampMs)
            | (Value::Ipv4(_), DataType::Ipv4)
            | (Value::Ipv6(_), DataType::Ipv6)
            | (Value::Subnet(_, _), DataType::Subnet)
            | (Value::Port(_), DataType::Port)
            | (Value::Latitude(_), DataType::Latitude)
            | (Value::Longitude(_), DataType::Longitude)
            | (Value::GeoPoint(_, _), DataType::GeoPoint)
            | (Value::Country2(_), DataType::Country2)
            | (Value::Country3(_), DataType::Country3)
            | (Value::Lang2(_), DataType::Lang2)
            | (Value::Lang5(_), DataType::Lang5)
            | (Value::Currency(_), DataType::Currency)
            | (Value::ColorAlpha(_), DataType::ColorAlpha)
            | (Value::BigInt(_), DataType::BigInt)
            | (Value::KeyRef(_, _), DataType::KeyRef)
            | (Value::DocRef(_, _), DataType::DocRef)
            | (Value::TableRef(_), DataType::TableRef)
            | (Value::PageRef(_), DataType::PageRef)
            | (Value::Secret(_), DataType::Secret)
            | (Value::Password(_), DataType::Password)
    )
}

fn value_to_coercion_input(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Integer(value) => Some(value.to_string()),
        Value::UnsignedInteger(value) => Some(value.to_string()),
        Value::Float(value) => Some(value.to_string()),
        Value::Text(value) => Some(value.to_string()),
        Value::Blob(value) => String::from_utf8(value.clone()).ok(),
        Value::Boolean(value) => Some(value.to_string()),
        Value::Timestamp(value) => Some(value.to_string()),
        Value::Duration(value) => Some(value.to_string()),
        Value::IpAddr(value) => Some(value.to_string()),
        Value::MacAddr(value) => Some(format!(
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            value[0], value[1], value[2], value[3], value[4], value[5]
        )),
        Value::Json(value) => Some(String::from_utf8_lossy(value).to_string()),
        Value::Email(value) => Some(value.clone()),
        Value::Url(value) => Some(value.clone()),
        Value::Phone(value) => Some(value.to_string()),
        Value::Semver(value) => Some(format!(
            "{}.{}.{}",
            value / 1_000_000,
            (value / 1_000) % 1_000,
            value % 1_000
        )),
        Value::Date(value) => Some(value.to_string()),
        Value::Time(value) => Some(value.to_string()),
        Value::Decimal(value) => Some(value.to_string()),
        Value::DecimalText(value) => Some(value.clone()),
        Value::TimestampMs(value) => Some(value.to_string()),
        Value::Ipv4(value) => Some(format!(
            "{}.{}.{}.{}",
            (value >> 24) & 0xFF,
            (value >> 16) & 0xFF,
            (value >> 8) & 0xFF,
            value & 0xFF
        )),
        Value::Port(value) => Some(value.to_string()),
        Value::Latitude(value) => Some((*value as f64 / 1_000_000.0).to_string()),
        Value::Longitude(value) => Some((*value as f64 / 1_000_000.0).to_string()),
        Value::GeoPoint(lat, lon) => Some(format!(
            "{},{}",
            *lat as f64 / 1_000_000.0,
            *lon as f64 / 1_000_000.0
        )),
        Value::BigInt(value) => Some(value.to_string()),
        Value::TableRef(value) => Some(value.clone()),
        Value::PageRef(value) => Some(value.to_string()),
        Value::Password(value) => Some(value.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{CollectionModel, SchemaMode};
    use crate::physical::ContractOrigin;
    use crate::storage::schema::{ColumnDef, Constraint, ConstraintType, DataType, TableDef};

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
