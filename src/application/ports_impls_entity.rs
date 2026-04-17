use std::collections::HashMap;

use crate::application::entity::{
    AppliedEntityMutation, CreateDocumentInput, CreateKvInput, CreateTimeSeriesPointInput,
    RowUpdateColumnRule, RowUpdateContractPlan,
};
use crate::application::ttl_payload::{
    has_internal_ttl_metadata, normalize_ttl_patch_operations, parse_top_level_ttl_metadata_entries,
};
use crate::json::{to_vec as json_to_vec, Value as JsonValue};
use crate::storage::query::resolve_declared_data_type;
use crate::storage::schema::{coerce as coerce_schema_value, DataType, Value};
use crate::storage::unified::MetadataValue;

use super::*;

const TREE_METADATA_PREFIX: &str = "red.tree.";
const TREE_CHILD_EDGE_LABEL: &str = "TREE_CHILD";

fn apply_collection_default_ttl(
    db: &crate::storage::unified::devx::RedDB,
    collection: &str,
    metadata: &mut Vec<(String, MetadataValue)>,
) {
    if has_internal_ttl_metadata(metadata) {
        return;
    }

    let Some(default_ttl_ms) = db.collection_default_ttl_ms(collection) else {
        return;
    };

    metadata.push((
        "_ttl_ms".to_string(),
        if default_ttl_ms <= i64::MAX as u64 {
            MetadataValue::Int(default_ttl_ms as i64)
        } else {
            MetadataValue::Timestamp(default_ttl_ms)
        },
    ));
}

fn refresh_context_index(
    db: &crate::storage::unified::devx::RedDB,
    collection: &str,
    id: crate::storage::EntityId,
) -> RedDBResult<()> {
    let store = db.store();
    let Some(entity) = store.get(collection, id) else {
        return Ok(());
    };

    store.context_index().index_entity(collection, &entity);
    Ok(())
}

/// Pull `(name, value)` pairs for every named column on a row entity.
/// Returns empty if the entity is not a row, or if the row carries
/// neither a `named` map nor a `schema` Arc — both of those mean the
/// names aren't recoverable here, so secondary-index maintenance has
/// nothing to act on. Used by the delete + update paths.
pub(crate) fn entity_row_fields_snapshot(
    entity: &crate::storage::UnifiedEntity,
) -> Vec<(String, Value)> {
    let crate::storage::EntityData::Row(row) = &entity.data else {
        return Vec::new();
    };
    if let Some(named) = &row.named {
        return named.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    }
    if let Some(schema) = &row.schema {
        return schema
            .iter()
            .zip(row.columns.iter())
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();
    }
    Vec::new()
}

fn ensure_collection_model_contract(
    db: &crate::storage::unified::devx::RedDB,
    collection: &str,
    requested_model: crate::catalog::CollectionModel,
) -> RedDBResult<()> {
    if let Some(contract) = db.collection_contract(collection) {
        if !contract_enforces_model(&contract) {
            return Ok(());
        }
        if collection_model_allows(contract.declared_model, requested_model) {
            return Ok(());
        }
        return Err(crate::RedDBError::Query(format!(
            "collection '{}' is declared as '{}' and does not allow '{}' writes",
            collection,
            collection_model_name(contract.declared_model),
            collection_model_name(requested_model)
        )));
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
        context_index_fields: Vec::new(),
        declared_columns: Vec::new(),
        table_def: matches!(requested_model, crate::catalog::CollectionModel::Table)
            .then(|| crate::storage::schema::TableDef::new(collection.to_string())),
        timestamps_enabled: false,
        context_index_enabled: false,
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

fn collection_model_allows(
    declared_model: crate::catalog::CollectionModel,
    requested_model: crate::catalog::CollectionModel,
) -> bool {
    declared_model == requested_model || declared_model == crate::catalog::CollectionModel::Mixed
}

fn collection_model_name(model: crate::catalog::CollectionModel) -> &'static str {
    match model {
        crate::catalog::CollectionModel::Table => "table",
        crate::catalog::CollectionModel::Document => "document",
        crate::catalog::CollectionModel::Graph => "graph",
        crate::catalog::CollectionModel::Vector => "vector",
        crate::catalog::CollectionModel::Mixed => "mixed",
        crate::catalog::CollectionModel::TimeSeries => "timeseries",
        crate::catalog::CollectionModel::Queue => "queue",
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

fn normalize_row_fields_for_contract(
    db: &crate::storage::unified::devx::RedDB,
    collection: &str,
    fields: Vec<(String, Value)>,
) -> RedDBResult<Vec<(String, Value)>> {
    normalize_row_fields_for_contract_with_mode(db, collection, fields, NormalizeMode::Insert)
}

fn normalize_row_fields_for_contract_with_mode(
    db: &crate::storage::unified::devx::RedDB,
    collection: &str,
    fields: Vec<(String, Value)>,
    mode: NormalizeMode,
) -> RedDBResult<Vec<(String, Value)>> {
    let Some(contract) = db.collection_contract(collection) else {
        return Ok(fields);
    };

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
    let now_ms = current_unix_ms_u64();

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
    if contract.declared_model != crate::catalog::CollectionModel::Table
        && contract.declared_model != crate::catalog::CollectionModel::Mixed
    {
        return Ok(());
    }

    let rules = resolved_uniqueness_rules(&contract);
    if rules.is_empty() {
        return Ok(());
    }

    let store = db.store();
    let Some(manager) = store.get_collection(collection) else {
        return Ok(());
    };

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

        for entity in manager.query_all(|_| true) {
            if exclude_id.map(|id| id == entity.id).unwrap_or(false) {
                continue;
            }
            let Some(existing_fields) = row_fields_from_entity(&entity) else {
                continue;
            };

            let duplicate = expected_signatures.iter().all(|(column, expected)| {
                existing_fields
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
    if contract.declared_model != crate::catalog::CollectionModel::Table
        && contract.declared_model != crate::catalog::CollectionModel::Mixed
    {
        return Ok(());
    }

    let rules = resolved_uniqueness_rules(&contract);
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

    Ok(Some(RowUpdateContractPlan {
        timestamps_enabled: contract.timestamps_enabled,
        strict_schema: matches!(contract.schema_mode, crate::catalog::SchemaMode::Strict),
        declared_rules,
        unique_columns,
    }))
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

fn set_row_field(row: &mut crate::storage::unified::entity::RowData, name: &str, value: Value) {
    if let Some(named) = row.named.as_mut() {
        named.insert(name.to_string(), value);
        return;
    }

    if let Some(schema) = row.schema.as_ref() {
        if let Some(index) = schema.iter().position(|column| column == name) {
            if let Some(slot) = row.columns.get_mut(index) {
                *slot = value;
                return;
            }
        }

        let mut named = HashMap::with_capacity(schema.len().saturating_add(1));
        for (column, current) in schema.iter().zip(row.columns.iter()) {
            named.insert(column.clone(), current.clone());
        }
        named.insert(name.to_string(), value);
        row.named = Some(named);
        return;
    }

    let mut named = HashMap::with_capacity(1);
    named.insert(name.to_string(), value);
    row.named = Some(named);
}

fn collect_row_fields(row: &crate::storage::unified::entity::RowData) -> Vec<(String, Value)> {
    if let Some(named) = row.named.as_ref() {
        named
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect()
    } else if let Some(schema) = row.schema.as_ref() {
        schema
            .iter()
            .cloned()
            .zip(row.columns.iter().cloned())
            .collect()
    } else {
        Vec::new()
    }
}

fn apply_row_field_assignments_raw<I>(
    row: &mut crate::storage::unified::entity::RowData,
    field_assignments: I,
) where
    I: IntoIterator<Item = (String, Value)>,
{
    for (column, value) in field_assignments {
        set_row_field(row, &column, value);
    }
}

fn apply_row_field_assignments_incremental<I>(
    collection: &str,
    row: &mut crate::storage::unified::entity::RowData,
    field_assignments: I,
    row_contract_plan: Option<&RowUpdateContractPlan>,
) -> RedDBResult<()>
where
    I: IntoIterator<Item = (String, Value)>,
{
    for (column, value) in field_assignments {
        let value = normalize_row_update_assignment_with_plan(
            collection,
            &column,
            value,
            row_contract_plan,
        )?;

        set_row_field(row, &column, value);
    }

    Ok(())
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
        DataType::Json => Ok(Value::Json(raw.as_bytes().to_vec())),
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
        Value::Text(value) => Some(value.clone()),
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

fn dedupe_modified_columns(mut modified_columns: Vec<String>) -> Vec<String> {
    if modified_columns.is_empty() {
        return modified_columns;
    }

    let mut unique = Vec::with_capacity(modified_columns.len());
    for column in modified_columns.drain(..) {
        if !unique
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(&column))
        {
            unique.push(column);
        }
    }
    unique
}

impl RedDBRuntime {
    pub(crate) fn apply_loaded_patch_entity_core(
        &self,
        collection: String,
        mut entity: crate::storage::UnifiedEntity,
        payload: JsonValue,
        operations: Vec<PatchEntityOperation>,
    ) -> RedDBResult<AppliedEntityMutation> {
        let id = entity.id;
        let operations = normalize_ttl_patch_operations(operations)?;
        // Snapshot pre-patch row fields for the secondary-index hook —
        // empty for non-row entities, which is the desired no-op.
        let pre_mutation_fields = entity_row_fields_snapshot(&entity);

        let db = self.db();
        let store = db.store();
        let Some(manager) = store.get_collection(&collection) else {
            return Err(crate::RedDBError::NotFound(format!(
                "collection not found: {collection}"
            )));
        };

        let mut patch_metadata: Option<crate::storage::unified::Metadata> = None;
        let mut metadata_changed = false;
        let mut modified_columns: Vec<String> = Vec::new();
        let mut context_index_dirty = false;

        let row_contract_timestamps = db
            .collection_contract(&collection)
            .map(|c| c.timestamps_enabled)
            .unwrap_or(false);

        match &mut entity.data {
            crate::storage::EntityData::Row(row) => {
                let mut field_ops = Vec::new();
                let mut metadata_ops = Vec::new();

                for mut op in operations {
                    let Some(root) = op.path.first().map(String::as_str) else {
                        return Err(crate::RedDBError::Query(
                            "patch path cannot be empty".to_string(),
                        ));
                    };

                    match root {
                        "fields" | "named" => {
                            if op.path.len() < 2 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'fields' requires a nested key".to_string(),
                                ));
                            }
                            if row_contract_timestamps {
                                let leaf = op.path.get(1).map(String::as_str);
                                if matches!(leaf, Some("created_at") | Some("updated_at")) {
                                    return Err(crate::RedDBError::Query(format!(
                                        "collection '{}' manages '{}' automatically — do not set it in UPDATE",
                                        collection,
                                        leaf.unwrap_or("")
                                    )));
                                }
                            }
                            op.path.remove(0);
                            field_ops.push(op);
                        }
                        "metadata" => {
                            if op.path.len() < 2 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'metadata' requires a nested key".to_string(),
                                ));
                            }
                            op.path.remove(0);
                            metadata_ops.push(op);
                        }
                        _ => {
                            return Err(crate::RedDBError::Query(format!(
                                "unsupported patch target '{root}' for table rows. Use fields/*, metadata/*, or weight"
                            )));
                        }
                    }
                }

                if !field_ops.is_empty() {
                    context_index_dirty = true;
                    for op in &field_ops {
                        if let Some(col) = op.path.first() {
                            modified_columns.push(col.clone());
                        }
                    }
                    let named = row.named.get_or_insert_with(Default::default);
                    apply_patch_operations_to_storage_map(named, &field_ops)?;
                }

                if let Some(fields) = payload
                    .get("fields")
                    .and_then(crate::json::Value::as_object)
                {
                    if row_contract_timestamps {
                        for key in fields.keys() {
                            if key == "created_at" || key == "updated_at" {
                                return Err(crate::RedDBError::Query(format!(
                                    "collection '{}' manages '{}' automatically — do not set it in UPDATE",
                                    collection, key
                                )));
                            }
                        }
                    }
                    context_index_dirty = true;
                    let named = row.named.get_or_insert_with(Default::default);
                    for (key, value) in fields {
                        modified_columns.push(key.clone());
                        named.insert(key.clone(), json_to_storage_value(value)?);
                    }
                }

                if !metadata_ops.is_empty() {
                    ensure_non_tree_reserved_metadata_patch_paths(&metadata_ops)?;
                    let metadata = patch_metadata.get_or_insert_with(|| {
                        store.get_metadata(&collection, id).unwrap_or_default()
                    });
                    let mut metadata_json = metadata_to_json(metadata);
                    apply_patch_operations_to_json(&mut metadata_json, &metadata_ops)
                        .map_err(crate::RedDBError::Query)?;
                    *metadata = metadata_from_json(&metadata_json)?;
                    metadata_changed = true;
                }

                if !modified_columns.is_empty() || row_contract_timestamps {
                    let current_fields = if let Some(named) = row.named.take() {
                        named.into_iter().collect::<Vec<_>>()
                    } else if let Some(schema) = row.schema.as_ref() {
                        schema
                            .iter()
                            .cloned()
                            .zip(row.columns.iter().cloned())
                            .collect::<Vec<_>>()
                    } else {
                        Vec::new()
                    };
                    let normalized_fields = normalize_row_fields_for_contract_with_mode(
                        &db,
                        &collection,
                        current_fields,
                        NormalizeMode::Update,
                    )?;
                    if row_contract_timestamps {
                        modified_columns.push("updated_at".to_string());
                        context_index_dirty = true;
                    }
                    if row_update_requires_uniqueness_check(&db, &collection, &modified_columns) {
                        enforce_row_uniqueness(&db, &collection, &normalized_fields, Some(id))?;
                    }
                    row.named = Some(normalized_fields.into_iter().collect());
                }
            }
            crate::storage::EntityData::Node(node) => {
                let mut field_ops = Vec::new();
                let mut metadata_ops = Vec::new();

                for mut op in operations {
                    let Some(root) = op.path.first().map(String::as_str) else {
                        return Err(crate::RedDBError::Query(
                            "patch path cannot be empty".to_string(),
                        ));
                    };

                    match root {
                        "fields" | "properties" => {
                            if op.path.len() < 2 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'fields' requires a nested key".to_string(),
                                ));
                            }
                            op.path.remove(0);
                            field_ops.push(op);
                        }
                        "metadata" => {
                            if op.path.len() < 2 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'metadata' requires a nested key".to_string(),
                                ));
                            }
                            op.path.remove(0);
                            metadata_ops.push(op);
                        }
                        _ => {
                            return Err(crate::RedDBError::Query(format!(
                                "unsupported patch target '{root}' for graph nodes. Use fields/*, properties/*, or metadata/*"
                            )));
                        }
                    }
                }

                if !field_ops.is_empty() {
                    context_index_dirty = true;
                    apply_patch_operations_to_storage_map(&mut node.properties, &field_ops)?;
                }

                if let Some(fields) = payload
                    .get("fields")
                    .and_then(crate::json::Value::as_object)
                {
                    context_index_dirty = true;
                    for (key, value) in fields {
                        node.properties
                            .insert(key.clone(), json_to_storage_value(value)?);
                    }
                }

                if !metadata_ops.is_empty() {
                    ensure_non_tree_reserved_metadata_patch_paths(&metadata_ops)?;
                    let metadata = patch_metadata.get_or_insert_with(|| {
                        store.get_metadata(&collection, id).unwrap_or_default()
                    });
                    let mut metadata_json = metadata_to_json(metadata);
                    apply_patch_operations_to_json(&mut metadata_json, &metadata_ops)
                        .map_err(crate::RedDBError::Query)?;
                    *metadata = metadata_from_json(&metadata_json)?;
                    metadata_changed = true;
                }
            }
            crate::storage::EntityData::Edge(edge) => {
                let mut field_ops = Vec::new();
                let mut metadata_ops = Vec::new();
                let mut weight_ops = Vec::new();

                for mut op in operations {
                    let Some(root) = op.path.first().map(String::as_str) else {
                        return Err(crate::RedDBError::Query(
                            "patch path cannot be empty".to_string(),
                        ));
                    };

                    match root {
                        "fields" | "properties" => {
                            if op.path.len() < 2 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'fields' requires a nested key".to_string(),
                                ));
                            }
                            op.path.remove(0);
                            field_ops.push(op);
                        }
                        "weight" => {
                            if op.path.len() != 1 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'weight' does not allow nested keys".to_string(),
                                ));
                            }
                            op.path.clear();
                            weight_ops.push(op);
                        }
                        "metadata" => {
                            if op.path.len() < 2 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'metadata' requires a nested key".to_string(),
                                ));
                            }
                            op.path.remove(0);
                            metadata_ops.push(op);
                        }
                        _ => {
                            return Err(crate::RedDBError::Query(format!(
                                "unsupported patch target '{root}' for graph edges. Use fields/*, weight, metadata/*"
                            )));
                        }
                    }
                }

                if !field_ops.is_empty() {
                    context_index_dirty = true;
                    apply_patch_operations_to_storage_map(&mut edge.properties, &field_ops)?;
                }

                for op in weight_ops {
                    context_index_dirty = true;
                    let value = op.value.ok_or_else(|| {
                        crate::RedDBError::Query("weight operations require a value".to_string())
                    })?;

                    match op.op {
                        PatchEntityOperationType::Unset => {
                            return Err(crate::RedDBError::Query(
                                "weight cannot be unset through patch operations".to_string(),
                            ));
                        }
                        PatchEntityOperationType::Set | PatchEntityOperationType::Replace => {
                            let Some(weight) = value.as_f64() else {
                                return Err(crate::RedDBError::Query(
                                    "weight operation requires a numeric value".to_string(),
                                ));
                            };
                            edge.weight = weight as f32;
                        }
                    }
                }

                if let Some(fields) = payload
                    .get("fields")
                    .and_then(crate::json::Value::as_object)
                {
                    context_index_dirty = true;
                    for (key, value) in fields {
                        edge.properties
                            .insert(key.clone(), json_to_storage_value(value)?);
                    }
                }

                if !metadata_ops.is_empty() {
                    ensure_non_tree_reserved_metadata_patch_paths(&metadata_ops)?;
                    let metadata = patch_metadata.get_or_insert_with(|| {
                        store.get_metadata(&collection, id).unwrap_or_default()
                    });
                    let mut metadata_json = metadata_to_json(metadata);
                    apply_patch_operations_to_json(&mut metadata_json, &metadata_ops)
                        .map_err(crate::RedDBError::Query)?;
                    *metadata = metadata_from_json(&metadata_json)?;
                    metadata_changed = true;
                }
            }
            crate::storage::EntityData::Vector(vector) => {
                let mut field_ops = Vec::new();
                let mut metadata_ops = Vec::new();

                for mut op in operations {
                    let Some(root) = op.path.first().map(String::as_str) else {
                        return Err(crate::RedDBError::Query(
                            "patch path cannot be empty".to_string(),
                        ));
                    };

                    match root {
                        "fields" => {
                            if op.path.len() < 2 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'fields' requires a nested key".to_string(),
                                ));
                            }
                            op.path.remove(0);
                            let Some(target) = op.path.first().map(String::as_str) else {
                                return Err(crate::RedDBError::Query(
                                    "patch path requires a target under fields".to_string(),
                                ));
                            };
                            if !matches!(target, "dense" | "content" | "sparse") {
                                return Err(crate::RedDBError::Query(format!(
                                    "unsupported vector patch target '{target}'"
                                )));
                            }
                            field_ops.push(op);
                        }
                        "metadata" => {
                            if op.path.len() < 2 {
                                return Err(crate::RedDBError::Query(
                                    "patch path 'metadata' requires a nested key".to_string(),
                                ));
                            }
                            op.path.remove(0);
                            metadata_ops.push(op);
                        }
                        _ => {
                            return Err(crate::RedDBError::Query(format!(
                                "unsupported patch target '{root}' for vectors. Use fields/* or metadata/*"
                            )));
                        }
                    }
                }

                if !field_ops.is_empty() {
                    context_index_dirty = true;
                    apply_patch_operations_to_vector_fields(vector, &field_ops)?;
                }

                if let Some(fields) = payload
                    .get("fields")
                    .and_then(crate::json::Value::as_object)
                {
                    context_index_dirty = true;
                    if let Some(content) =
                        fields.get("content").and_then(crate::json::Value::as_str)
                    {
                        vector.content = Some(content.to_string());
                    }
                    if let Some(dense) = fields.get("dense") {
                        vector.dense = dense
                            .as_array()
                            .ok_or_else(|| {
                                crate::RedDBError::Query(
                                    "field 'dense' must be an array".to_string(),
                                )
                            })?
                            .iter()
                            .map(|value| {
                                value.as_f64().map(|value| value as f32).ok_or_else(|| {
                                    crate::RedDBError::Query(
                                        "field 'dense' must contain only numbers".to_string(),
                                    )
                                })
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                    }
                }

                if !metadata_ops.is_empty() {
                    ensure_non_tree_reserved_metadata_patch_paths(&metadata_ops)?;
                    let metadata = patch_metadata.get_or_insert_with(|| {
                        store.get_metadata(&collection, id).unwrap_or_default()
                    });
                    let mut metadata_json = metadata_to_json(metadata);
                    apply_patch_operations_to_json(&mut metadata_json, &metadata_ops)
                        .map_err(crate::RedDBError::Query)?;
                    *metadata = metadata_from_json(&metadata_json)?;
                    metadata_changed = true;
                }
            }
            crate::storage::EntityData::TimeSeries(_)
            | crate::storage::EntityData::QueueMessage(_) => {
                return Err(crate::RedDBError::Query(
                    "patch operations are not supported for TimeSeries or QueueMessage entities"
                        .to_string(),
                ));
            }
        }

        if let Some(metadata) = payload
            .get("metadata")
            .and_then(crate::json::Value::as_object)
        {
            let patch_metadata = patch_metadata
                .get_or_insert_with(|| store.get_metadata(&collection, id).unwrap_or_default());
            for (key, value) in metadata {
                ensure_non_tree_reserved_metadata_key(key)?;
                patch_metadata.set(key.clone(), json_to_metadata_value(value)?);
            }
            metadata_changed = true;
        }

        for (key, value) in parse_top_level_ttl_metadata_entries(&payload)? {
            let patch_metadata = patch_metadata
                .get_or_insert_with(|| store.get_metadata(&collection, id).unwrap_or_default());
            if matches!(value, crate::storage::unified::MetadataValue::Null) {
                patch_metadata.remove(&key);
            } else {
                patch_metadata.set(key, value);
            }
            metadata_changed = true;
        }

        entity.updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        modified_columns = dedupe_modified_columns(modified_columns);

        Ok(AppliedEntityMutation {
            id,
            collection,
            entity,
            metadata: patch_metadata,
            modified_columns,
            persist_metadata: metadata_changed,
            context_index_dirty,
            pre_mutation_fields,
        })
    }

    pub(crate) fn apply_loaded_sql_update_row_core(
        &self,
        collection: String,
        mut entity: crate::storage::UnifiedEntity,
        static_field_assignments: &[(String, Value)],
        dynamic_field_assignments: Vec<(String, Value)>,
        static_metadata_assignments: &[(String, MetadataValue)],
        dynamic_metadata_assignments: Vec<(String, MetadataValue)>,
        row_contract_plan: Option<&RowUpdateContractPlan>,
        row_modified_columns_template: &[String],
        row_touches_unique_columns: bool,
    ) -> RedDBResult<AppliedEntityMutation> {
        let id = entity.id;
        let db = self.db();
        let store = db.store();
        let Some(_) = store.get_collection(&collection) else {
            return Err(crate::RedDBError::NotFound(format!(
                "collection not found: {collection}"
            )));
        };

        let mut patch_metadata: Option<crate::storage::unified::Metadata> = None;
        let row_contract_timestamps = row_contract_plan
            .map(|plan| plan.timestamps_enabled)
            .unwrap_or(false);
        let mut metadata_changed = false;
        let mut modified_columns = row_modified_columns_template.to_vec();
        let mut context_index_dirty = !modified_columns.is_empty();

        // Snapshot OLD field values BEFORE applying the assignments —
        // the secondary-index maintenance hook needs both before/after to
        // delete-then-insert under changed indexed columns.
        let pre_mutation_fields = entity_row_fields_snapshot(&entity);

        let crate::storage::EntityData::Row(row) = &mut entity.data else {
            return Err(crate::RedDBError::Query(
                "SQL row update fast path requires a row entity".to_string(),
            ));
        };

        let _ = row_contract_plan;
        apply_row_field_assignments_raw(row, static_field_assignments.iter().cloned());
        apply_row_field_assignments_raw(row, dynamic_field_assignments);

        for (key, value) in static_metadata_assignments
            .iter()
            .cloned()
            .chain(dynamic_metadata_assignments.into_iter())
        {
            ensure_non_tree_reserved_metadata_key(&key)?;
            patch_metadata
                .get_or_insert_with(|| store.get_metadata(&collection, id).unwrap_or_default())
                .set(key, value);
            metadata_changed = true;
        }

        if !modified_columns.is_empty() || row_contract_timestamps {
            if row_contract_timestamps {
                context_index_dirty = true;
                set_row_field(
                    row,
                    "updated_at",
                    Value::UnsignedInteger(current_unix_ms_u64()),
                );
                modified_columns.push("updated_at".to_string());
            }
            if row_touches_unique_columns {
                let current_fields = collect_row_fields(row);
                enforce_row_uniqueness(&db, &collection, &current_fields, Some(id))?;
            }
        }

        entity.updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        modified_columns = dedupe_modified_columns(modified_columns);

        Ok(AppliedEntityMutation {
            id,
            collection,
            entity,
            metadata: patch_metadata,
            modified_columns,
            persist_metadata: metadata_changed,
            context_index_dirty,
            pre_mutation_fields,
        })
    }

    pub(crate) fn persist_applied_entity_mutations(
        &self,
        applied: &[AppliedEntityMutation],
    ) -> RedDBResult<()> {
        if applied.is_empty() {
            return Ok(());
        }

        let store = self.db().store();
        let collection = &applied[0].collection;
        let Some(manager) = store.get_collection(collection) else {
            return Err(crate::RedDBError::NotFound(format!(
                "collection not found: {collection}"
            )));
        };

        manager
            .update_hot_batch_with_metadata(applied.iter().map(|item| {
                (
                    &item.entity,
                    item.modified_columns.as_slice(),
                    if item.persist_metadata {
                        item.metadata.as_ref()
                    } else {
                        None
                    },
                )
            }))
            .map_err(|err| crate::RedDBError::Query(err.to_string()))?;

        let entities: Vec<_> = applied.iter().map(|item| item.entity.clone()).collect();
        store
            .persist_entities_to_pager(collection, &entities)
            .map_err(|err| crate::RedDBError::Internal(err.to_string()))
    }

    pub(crate) fn flush_applied_entity_mutation(
        &self,
        applied: &AppliedEntityMutation,
    ) -> RedDBResult<()> {
        let store = self.db().store();
        if applied.context_index_dirty {
            store
                .context_index()
                .index_entity(&applied.collection, &applied.entity);
        }
        // Secondary-index maintenance for SQL UPDATE / JSON-Patch flows.
        // Skip when pre_mutation_fields is empty (entity wasn't a row, or
        // didn't carry recoverable column names) — there's nothing to
        // delete-then-insert in that case.
        //
        // Also build the CDC damage vector here so downstream consumers
        // see which columns changed without re-diffing.
        let mut changed_columns: Option<Vec<String>> = None;
        if !applied.pre_mutation_fields.is_empty() {
            let post = entity_row_fields_snapshot(&applied.entity);
            if !post.is_empty() {
                let damage = crate::application::entity::row_damage_vector(
                    &applied.pre_mutation_fields,
                    &post,
                );
                if !damage.is_empty() {
                    changed_columns = Some(
                        damage
                            .touched_columns()
                            .into_iter()
                            .map(str::to_string)
                            .collect(),
                    );
                }

                // HOT-like fast path (P3.T2/T3): when no modified
                // column is covered by a secondary index, skip the
                // `index_entity_update` call entirely. The function
                // would short-circuit internally, but the call still
                // reads the registry lock + walks the damage vector
                // — avoiding it saves a few microseconds per UPDATE.
                // Page-local replace + t_ctid chain support (true
                // HOT) lives in a follow-up storage spec.
                let indexed_cols: std::collections::HashSet<String> = self
                    .index_store_ref()
                    .list_indices(applied.collection.as_str())
                    .into_iter()
                    .filter_map(|idx| idx.columns.first().cloned())
                    .collect();
                let modified_cols: std::collections::HashSet<String> = damage
                    .touched_columns()
                    .into_iter()
                    .map(str::to_string)
                    .collect();
                let decision = crate::storage::engine::hot_update::decide(
                    &crate::storage::engine::hot_update::HotUpdateInputs {
                        collection: applied.collection.as_str(),
                        indexed_columns: &indexed_cols,
                        modified_columns: &modified_cols,
                        // The storage layer currently handles fit via
                        // the segment abstraction; we bypass the
                        // page-size check here.
                        new_tuple_size: 0,
                        page_free_space: usize::MAX,
                    },
                );
                if !decision.can_hot {
                    self.index_store_ref()
                        .index_entity_update(
                            &applied.collection,
                            applied.id,
                            &applied.pre_mutation_fields,
                            &post,
                        )
                        .map_err(crate::RedDBError::Internal)?;
                } else {
                    tracing::debug!(
                        collection = %applied.collection,
                        "hot_update fast-path: skipped index_entity_update"
                    );
                }
            }
        }
        self.cdc_emit_prebuilt_with_columns(
            crate::replication::cdc::ChangeOperation::Update,
            &applied.collection,
            &applied.entity,
            "entity",
            applied.metadata.as_ref(),
            true,
            changed_columns,
        );
        Ok(())
    }

    pub(crate) fn apply_loaded_patch_entity(
        &self,
        collection: String,
        entity: crate::storage::UnifiedEntity,
        payload: JsonValue,
        operations: Vec<PatchEntityOperation>,
    ) -> RedDBResult<CreateEntityOutput> {
        let applied =
            self.apply_loaded_patch_entity_core(collection, entity, payload, operations)?;
        self.persist_applied_entity_mutations(std::slice::from_ref(&applied))?;
        self.flush_applied_entity_mutation(&applied)?;
        Ok(CreateEntityOutput {
            id: applied.id,
            entity: Some(applied.entity),
        })
    }
}

fn ensure_non_tree_reserved_metadata_patch_paths(
    operations: &[PatchEntityOperation],
) -> RedDBResult<()> {
    for operation in operations {
        let Some(key) = operation.path.first().map(String::as_str) else {
            continue;
        };
        ensure_non_tree_reserved_metadata_key(key)?;
    }
    Ok(())
}

fn ensure_non_tree_reserved_metadata_key(key: &str) -> RedDBResult<()> {
    if key.starts_with(TREE_METADATA_PREFIX) {
        return Err(crate::RedDBError::Query(format!(
            "metadata key '{}' is reserved for managed trees",
            key
        )));
    }
    Ok(())
}

fn ensure_non_tree_reserved_metadata_entries(
    metadata: &[(String, MetadataValue)],
) -> RedDBResult<()> {
    for (key, _) in metadata {
        ensure_non_tree_reserved_metadata_key(key)?;
    }
    Ok(())
}

fn ensure_non_tree_structural_edge_label(label: &str) -> RedDBResult<()> {
    if label.eq_ignore_ascii_case(TREE_CHILD_EDGE_LABEL) {
        return Err(crate::RedDBError::Query(format!(
            "edge label '{}' is reserved for managed trees",
            TREE_CHILD_EDGE_LABEL
        )));
    }
    Ok(())
}

impl RedDBRuntime {
    pub(crate) fn create_node_unchecked(
        &self,
        input: CreateNodeInput,
    ) -> RedDBResult<CreateEntityOutput> {
        let db = self.db();
        ensure_collection_model_contract(
            &db,
            &input.collection,
            crate::catalog::CollectionModel::Graph,
        )?;
        let mut metadata = input.metadata;
        apply_collection_default_ttl(&db, &input.collection, &mut metadata);
        let mut builder = db.node(&input.collection, &input.label);

        if let Some(node_type) = input.node_type {
            builder = builder.node_type(node_type);
        }

        for (key, value) in input.properties {
            builder = builder.property(key, value);
        }

        for (key, value) in metadata {
            builder = builder.metadata(key, value);
        }

        for embedding in input.embeddings {
            if let Some(model) = embedding.model {
                builder = builder.embedding_with_model(embedding.name, embedding.vector, model);
            } else {
                builder = builder.embedding(embedding.name, embedding.vector);
            }
        }

        for link in input.table_links {
            builder = builder.link_to_table(link.key, link.table);
        }

        for link in input.node_links {
            builder = builder.link_to_weighted(link.target, link.edge_label, link.weight);
        }

        let id = builder.save()?;
        // Phase 1.1 MVCC universal: stamp xmin so concurrent snapshots
        // don't see this node until the transaction commits.
        self.stamp_xmin_if_in_txn(&input.collection, id);
        refresh_context_index(&db, &input.collection, id)?;
        self.cdc_emit(
            crate::replication::cdc::ChangeOperation::Insert,
            &input.collection,
            id.raw(),
            "graph_node",
        );
        Ok(CreateEntityOutput {
            id,
            entity: db.store().get(&input.collection, id),
        })
    }

    pub(crate) fn create_edge_unchecked(
        &self,
        input: CreateEdgeInput,
    ) -> RedDBResult<CreateEntityOutput> {
        let db = self.db();
        ensure_collection_model_contract(
            &db,
            &input.collection,
            crate::catalog::CollectionModel::Graph,
        )?;
        let mut metadata = input.metadata;
        apply_collection_default_ttl(&db, &input.collection, &mut metadata);
        let mut builder = db
            .edge(&input.collection, &input.label)
            .from(input.from)
            .to(input.to);

        if let Some(weight) = input.weight {
            builder = builder.weight(weight);
        }

        for (key, value) in input.properties {
            builder = builder.property(key, value);
        }

        for (key, value) in metadata {
            builder = builder.metadata(key, value);
        }

        let id = builder.save()?;
        // Phase 1.1 MVCC universal: stamp xmin on the edge so other
        // sessions don't follow it until COMMIT.
        self.stamp_xmin_if_in_txn(&input.collection, id);
        refresh_context_index(&db, &input.collection, id)?;
        self.cdc_emit(
            crate::replication::cdc::ChangeOperation::Insert,
            &input.collection,
            id.raw(),
            "graph_edge",
        );
        Ok(CreateEntityOutput {
            id,
            entity: db.store().get(&input.collection, id),
        })
    }
}

impl RuntimeEntityPort for RedDBRuntime {
    fn create_row(&self, input: CreateRowInput) -> RedDBResult<CreateEntityOutput> {
        let db = self.db();
        ensure_collection_model_contract(
            &db,
            &input.collection,
            crate::catalog::CollectionModel::Table,
        )?;
        let CreateRowInput {
            collection,
            fields,
            metadata: input_metadata,
            node_links,
            vector_links,
        } = input;
        let mut metadata = input_metadata;
        apply_collection_default_ttl(&db, &collection, &mut metadata);
        let fields = normalize_row_fields_for_contract(&db, &collection, fields)?;
        enforce_row_uniqueness(&db, &collection, &fields, None)?;
        // Route through MutationEngine for unified hot path.
        let engine = self.mutation_engine();
        let result = engine.apply(
            collection.clone(),
            vec![crate::runtime::mutation::MutationRow {
                fields,
                metadata,
                node_links,
                vector_links,
            }],
        )?;
        let id = result.ids[0];
        // Perf: `db.get(id)` does a *cross-collection* scan (get_any)
        // that also takes a write lock on the entity cache. We know
        // the collection — hit the manager directly. Cuts
        // create_row() p50 roughly in half on the hot path.
        Ok(CreateEntityOutput {
            id,
            entity: db.store().get(&collection, id),
        })
    }

    fn create_rows_batch(
        &self,
        input: CreateRowsBatchInput,
    ) -> RedDBResult<Vec<CreateEntityOutput>> {
        if input.rows.is_empty() {
            return Ok(Vec::new());
        }

        let db = self.db();
        let collection = input.collection;
        ensure_collection_model_contract(&db, &collection, crate::catalog::CollectionModel::Table)?;

        let mut prepared_rows = Vec::with_capacity(input.rows.len());
        let mut uniqueness_rows = Vec::with_capacity(input.rows.len());
        for row in input.rows {
            if row.collection != collection {
                return Err(crate::RedDBError::Query(format!(
                    "batch row collection mismatch: expected '{}', got '{}'",
                    collection, row.collection
                )));
            }

            let mut metadata = row.metadata;
            apply_collection_default_ttl(&db, &collection, &mut metadata);
            let fields = normalize_row_fields_for_contract(&db, &collection, row.fields)?;
            enforce_row_uniqueness(&db, &collection, &fields, None)?;
            uniqueness_rows.push(fields.clone());
            prepared_rows.push((fields, metadata, row.node_links, row.vector_links));
        }

        enforce_row_batch_uniqueness(&db, &collection, &uniqueness_rows)?;

        // Route through MutationEngine: single bulk_insert + one CDC batch
        // instead of N separate cdc_emit() calls (each acquires a write lock).
        let engine = self.mutation_engine();
        let mutation_rows: Vec<crate::runtime::mutation::MutationRow> = prepared_rows
            .into_iter()
            .map(|(fields, metadata, node_links, vector_links)| {
                crate::runtime::mutation::MutationRow {
                    fields,
                    metadata,
                    node_links,
                    vector_links,
                }
            })
            .collect();

        let result = engine
            .apply(collection.clone(), mutation_rows)
            .map_err(|e| crate::RedDBError::Internal(e.to_string()))?;

        let store = db.store();
        Ok(result
            .ids
            .into_iter()
            .map(|id| CreateEntityOutput {
                id,
                entity: store.get(&collection, id),
            })
            .collect())
    }

    fn create_node(&self, input: CreateNodeInput) -> RedDBResult<CreateEntityOutput> {
        ensure_non_tree_reserved_metadata_entries(&input.metadata)?;
        self.create_node_unchecked(input)
    }

    fn create_edge(&self, input: CreateEdgeInput) -> RedDBResult<CreateEntityOutput> {
        ensure_non_tree_structural_edge_label(&input.label)?;
        ensure_non_tree_reserved_metadata_entries(&input.metadata)?;
        self.create_edge_unchecked(input)
    }

    fn create_vector(&self, input: CreateVectorInput) -> RedDBResult<CreateEntityOutput> {
        let db = self.db();
        ensure_collection_model_contract(
            &db,
            &input.collection,
            crate::catalog::CollectionModel::Vector,
        )?;
        let mut metadata = input.metadata;
        apply_collection_default_ttl(&db, &input.collection, &mut metadata);
        let mut builder = db.vector(&input.collection).dense(input.dense);

        if let Some(content) = input.content {
            builder = builder.content(content);
        }

        for (key, value) in metadata {
            builder = builder.metadata(key, value);
        }

        if let Some(link_row) = input.link_row {
            builder = builder.link_to_table(link_row);
        }

        if let Some(link_node) = input.link_node {
            builder = builder.link_to_node(link_node);
        }

        let id = builder.save()?;
        // Phase 1.1 MVCC universal: stamp xmin on the vector so
        // concurrent ANN scans hide it until the transaction commits.
        self.stamp_xmin_if_in_txn(&input.collection, id);
        refresh_context_index(&db, &input.collection, id)?;
        self.cdc_emit(
            crate::replication::cdc::ChangeOperation::Insert,
            &input.collection,
            id.raw(),
            "vector",
        );
        Ok(CreateEntityOutput {
            id,
            entity: db.store().get(&input.collection, id),
        })
    }

    fn create_document(&self, input: CreateDocumentInput) -> RedDBResult<CreateEntityOutput> {
        let db = self.db();
        ensure_collection_model_contract(
            &db,
            &input.collection,
            crate::catalog::CollectionModel::Document,
        )?;

        // Serialize the full body as Value::Json for the "body" field
        let body_bytes = json_to_vec(&input.body).map_err(|err| {
            crate::RedDBError::Query(format!("failed to serialize document body: {err}"))
        })?;
        let mut fields: Vec<(String, crate::storage::schema::Value)> = vec![(
            "body".to_string(),
            crate::storage::schema::Value::Json(body_bytes),
        )];

        // Flatten top-level keys from the body into named fields for filtering
        if let JsonValue::Object(ref map) = input.body {
            for (key, value) in map {
                let storage_value = json_to_storage_value(value)?;
                fields.push((key.clone(), storage_value));
            }
        }

        let mut metadata = input.metadata;
        apply_collection_default_ttl(&db, &input.collection, &mut metadata);
        let columns: Vec<(&str, crate::storage::schema::Value)> = fields
            .iter()
            .map(|(key, value)| (key.as_str(), value.clone()))
            .collect();
        let mut builder = db.row(&input.collection, columns);

        for (key, value) in metadata {
            builder = builder.metadata(key, value);
        }

        for node in input.node_links {
            builder = builder.link_to_node(node);
        }

        for vector in input.vector_links {
            builder = builder.link_to_vector(vector);
        }

        let id = builder.save()?;
        // Phase 1.1 MVCC universal: stamp xmin on the document.
        self.stamp_xmin_if_in_txn(&input.collection, id);
        refresh_context_index(&db, &input.collection, id)?;
        self.cdc_emit(
            crate::replication::cdc::ChangeOperation::Insert,
            &input.collection,
            id.raw(),
            "document",
        );
        Ok(CreateEntityOutput {
            id,
            entity: db.store().get(&input.collection, id),
        })
    }

    fn create_kv(&self, input: CreateKvInput) -> RedDBResult<CreateEntityOutput> {
        let fields = vec![
            (
                "key".to_string(),
                crate::storage::schema::Value::Text(input.key),
            ),
            ("value".to_string(), input.value),
        ];
        let row_input = CreateRowInput {
            collection: input.collection,
            fields,
            metadata: input.metadata,
            node_links: Vec::new(),
            vector_links: Vec::new(),
        };
        self.create_row(row_input)
    }

    fn create_timeseries_point(
        &self,
        input: CreateTimeSeriesPointInput,
    ) -> RedDBResult<CreateEntityOutput> {
        let db = self.db();
        ensure_collection_model_contract(
            &db,
            &input.collection,
            crate::catalog::CollectionModel::TimeSeries,
        )?;

        let mut fields = vec![
            (
                "metric".to_string(),
                crate::storage::schema::Value::Text(input.metric),
            ),
            (
                "value".to_string(),
                crate::storage::schema::Value::Float(input.value),
            ),
        ];

        if let Some(timestamp_ns) = input.timestamp_ns {
            fields.push((
                "timestamp".to_string(),
                crate::storage::schema::Value::UnsignedInteger(timestamp_ns),
            ));
        }

        if !input.tags.is_empty() {
            let tags_json = JsonValue::Object(
                input
                    .tags
                    .into_iter()
                    .map(|(key, value)| (key, JsonValue::String(value)))
                    .collect(),
            );
            let tags_bytes = json_to_vec(&tags_json).map_err(|err| {
                crate::RedDBError::Query(format!("failed to serialize timeseries tags: {err}"))
            })?;
            fields.push((
                "tags".to_string(),
                crate::storage::schema::Value::Json(tags_bytes),
            ));
        }

        let collection = input.collection;
        let id = self.insert_timeseries_point(&collection, fields, input.metadata)?;
        // Phase 1.1 MVCC universal: stamp xmin on the point so
        // concurrent range scans hide it until COMMIT.
        self.stamp_xmin_if_in_txn(&collection, id);
        refresh_context_index(&db, &collection, id)?;

        Ok(CreateEntityOutput {
            id,
            entity: db.store().get(&collection, id),
        })
    }

    fn get_kv(
        &self,
        collection: &str,
        key: &str,
    ) -> RedDBResult<Option<(crate::storage::schema::Value, crate::storage::EntityId)>> {
        let db = self.db();
        ensure_collection_model_read(&db, collection, crate::catalog::CollectionModel::Table)?;
        let store = db.store();
        let Some(manager) = store.get_collection(collection) else {
            return Ok(None);
        };
        let entities = manager.query_all(|_| true);
        for entity in entities {
            if let crate::storage::EntityData::Row(ref row) = entity.data {
                if let Some(ref named) = row.named {
                    if let Some(crate::storage::schema::Value::Text(ref k)) = named.get("key") {
                        if k == key {
                            let value = named
                                .get("value")
                                .cloned()
                                .unwrap_or(crate::storage::schema::Value::Null);
                            return Ok(Some((value, entity.id)));
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    fn delete_kv(&self, collection: &str, key: &str) -> RedDBResult<bool> {
        let found = self.get_kv(collection, key)?;
        if let Some((_, id)) = found {
            let db = self.db();
            let store = db.store();
            let deleted = store
                .delete(collection, id)
                .map_err(|err| crate::RedDBError::Internal(err.to_string()))?;
            if deleted {
                store.context_index().remove_entity(id);
            }
            Ok(deleted)
        } else {
            Ok(false)
        }
    }

    fn patch_entity(&self, input: PatchEntityInput) -> RedDBResult<CreateEntityOutput> {
        let PatchEntityInput {
            collection,
            id,
            payload,
            operations,
        } = input;
        let db = self.db();
        let store = db.store();
        let Some(manager) = store.get_collection(&collection) else {
            return Err(crate::RedDBError::NotFound(format!(
                "collection not found: {collection}"
            )));
        };
        let Some(entity) = manager.get(id) else {
            return Err(crate::RedDBError::NotFound(format!(
                "entity not found: {}",
                id.raw()
            )));
        };
        self.apply_loaded_patch_entity(collection, entity, payload, operations)
    }

    fn delete_entity(&self, input: DeleteEntityInput) -> RedDBResult<DeleteEntityOutput> {
        let store = self.db().store();
        // Snapshot row fields before delete so we can mirror the removal
        // into every secondary index. The fetch is best-effort: if the
        // entity is already gone, the delete below is a no-op anyway.
        let pre_delete_fields = store
            .get(&input.collection, input.id)
            .as_ref()
            .map(entity_row_fields_snapshot)
            .unwrap_or_default();
        // Store delete first (source of truth). Crash between here and index removal
        // leaves the entity invisible to most queries but recoverable; the reverse
        // (remove index first, then crash) leaves the entity permanently orphaned.
        let deleted = store
            .delete(&input.collection, input.id)
            .map_err(|err| crate::RedDBError::Internal(err.to_string()))?;
        if deleted {
            store.context_index().remove_entity(input.id);
            // Secondary index maintenance — surface only registry-shape
            // errors; missing-index removals are tolerated inside the call.
            if !pre_delete_fields.is_empty() {
                self.index_store_ref()
                    .index_entity_delete(&input.collection, input.id, &pre_delete_fields)
                    .map_err(crate::RedDBError::Internal)?;
            }
            self.cdc_emit(
                crate::replication::cdc::ChangeOperation::Delete,
                &input.collection,
                input.id.raw(),
                "entity",
            );
        }
        Ok(DeleteEntityOutput {
            deleted,
            id: input.id,
        })
    }
}
