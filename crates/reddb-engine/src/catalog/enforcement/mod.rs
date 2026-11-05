//! Canonical contract admission. Hosts retain locking, MVCC and durable publication.
use super::schema::{has_contract_expressions, resolved_contract_columns, ResolvedColumnRule};
use super::CollectionContract;
use crate::storage::metadata::MetadataValue;
use reddb_types::coerce::coerce as coerce_schema_value;
use reddb_types::{DataType, Value};
use std::collections::HashMap;

mod expressions;

#[derive(Debug)]
pub enum ContractError {
    Query(String),
    InvalidOperation(String),
}
impl std::fmt::Display for ContractError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Query(message) => write!(formatter, "query error: {message}"),
            Self::InvalidOperation(message) => write!(formatter, "INVALID_OPERATION: {message}"),
        }
    }
}
impl std::error::Error for ContractError {}
impl From<super::schema::ContractExpressionError> for ContractError {
    fn from(error: super::schema::ContractExpressionError) -> Self {
        Self::Query(error.message)
    }
}
type ContractResult<T> = Result<T, ContractError>;

pub const INTERNAL_TTL_SECONDS_KEY: &str = "_ttl";
pub const INTERNAL_TTL_MILLIS_KEY: &str = "_ttl_ms";
pub const INTERNAL_EXPIRES_AT_KEY: &str = "_expires_at";
pub const INTERNAL_TTL_METADATA_KEYS: [&str; 3] = [
    INTERNAL_TTL_SECONDS_KEY,
    INTERNAL_TTL_MILLIS_KEY,
    INTERNAL_EXPIRES_AT_KEY,
];

pub fn has_internal_ttl_metadata(metadata: &[(String, MetadataValue)]) -> bool {
    metadata.iter().any(|(key, _)| {
        INTERNAL_TTL_METADATA_KEYS
            .iter()
            .any(|reserved| reserved == &key.as_str())
    })
}

#[derive(Debug, Clone)]
pub struct RowUpdateColumnRule {
    pub name: String,
    pub data_type: DataType,
    pub data_type_name: String,
    pub not_null: bool,
    pub enum_variants: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct RowUpdateContractPlan {
    pub has_expressions: bool,
    pub timestamps_enabled: bool,
    pub strict_schema: bool,
    pub declared_rules: HashMap<String, RowUpdateColumnRule>,
    pub unique_columns: HashMap<String, ()>,
}

/// Pure value/schema admission. The host supplies authoritative uniqueness
/// candidates under its MVCC reservation policy and retains the constraint lock
/// through staging and publication. This evaluator owns no lock or transaction.
pub struct CollectionContractEnforcer<'a> {
    contract: &'a CollectionContract,
}

#[derive(Clone, Copy)]
pub struct DefaultTtlPlan {
    pub ttl_ms: Option<u64>,
}

pub struct ContractRow {
    pub id: crate::storage::entity::EntityId,
    pub fields: std::collections::BTreeMap<String, Value>,
}

impl DefaultTtlPlan {
    pub fn apply(self, metadata: &mut Vec<(String, MetadataValue)>) {
        if let Some(entry) = default_ttl_metadata(self.ttl_ms, metadata) {
            metadata.push(entry);
        }
    }
}

impl<'a> CollectionContractEnforcer<'a> {
    pub fn new(contract: &'a CollectionContract) -> Self {
        Self { contract }
    }

    pub fn normalize_insert_at(
        &self,
        fields: Vec<(String, Value)>,
        now_ms: u64,
    ) -> ContractResult<Vec<(String, Value)>> {
        normalize_row_fields_for_contract_at(
            self.contract,
            &self.contract.name,
            fields,
            NormalizeMode::Insert,
            now_ms,
        )
    }

    pub fn ensure_model(
        &self,
        requested_model: crate::catalog::CollectionModel,
    ) -> ContractResult<()> {
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
        Err(ContractError::InvalidOperation(format!(
            "collection '{}' is declared as '{}' and does not allow '{}' writes",
            self.contract.name,
            collection_model_name(self.contract.declared_model),
            collection_model_name(requested_model)
        )))
    }

    pub fn ensure_vector_dimension(&self, actual_dimension: usize) -> ContractResult<()> {
        let Some(expected_dimension) = self.contract.vector_dimension else {
            return Ok(());
        };
        if expected_dimension == actual_dimension {
            return Ok(());
        }
        Err(ContractError::Query(format!(
            "vector dimension mismatch for collection '{}': expected {expected_dimension}, got {actual_dimension}",
            self.contract.name
        )))
    }

    pub fn normalize_update_at(
        &self,
        fields: Vec<(String, Value)>,
        now_ms: u64,
    ) -> ContractResult<Vec<(String, Value)>> {
        normalize_row_fields_for_contract_at(
            self.contract,
            &self.contract.name,
            fields,
            NormalizeMode::Update,
            now_ms,
        )
    }

    pub fn default_ttl_metadata(
        &self,
        metadata: &[(String, MetadataValue)],
    ) -> Option<(String, MetadataValue)> {
        default_ttl_metadata(self.contract.default_ttl_ms, metadata)
    }

    pub fn default_ttl_plan(&self) -> DefaultTtlPlan {
        DefaultTtlPlan {
            ttl_ms: self.contract.default_ttl_ms,
        }
    }

    pub fn enforce_batch_uniqueness(&self, rows: &[Vec<(String, Value)>]) -> ContractResult<()> {
        enforce_row_batch_uniqueness_for_contract(self.contract, &self.contract.name, rows)
    }

    pub fn enforce_uniqueness(
        &self,
        fields: &[(String, Value)],
        existing_rows: &[ContractRow],
        exclude_id: Option<crate::storage::entity::EntityId>,
    ) -> ContractResult<()> {
        enforce_row_uniqueness_for_contract(
            self.contract,
            &self.contract.name,
            fields,
            existing_rows,
            exclude_id,
        )
    }

    pub fn row_update_plan(&self) -> ContractResult<RowUpdateContractPlan> {
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

#[derive(Clone)]
pub struct UniquenessRule {
    pub name: String,
    pub columns: Vec<String>,
    pub primary_key: bool,
}

pub struct UniquenessConflict {
    pub rule: UniquenessRule,
    pub entity_id: crate::storage::entity::EntityId,
}

#[derive(Copy, Clone, PartialEq, Eq)]
pub enum NormalizeMode {
    /// First write for this row. Timestamps auto-filled from now on
    /// both `created_at` and `updated_at`; user attempts to set
    /// either column are rejected.
    Insert,
    /// Update/patch path. `created_at` is preserved from the existing
    /// row (immutable after insert); `updated_at` is bumped to now.
    /// User attempts to set either via the patch are rejected.
    Update,
}

pub fn contract_enforces_model(contract: &CollectionContract) -> bool {
    !matches!(
        (&contract.origin, &contract.schema_mode),
        (
            super::ContractOrigin::Implicit,
            crate::catalog::SchemaMode::Dynamic,
        )
    )
}

pub fn collection_model_allows(
    declared_model: crate::catalog::CollectionModel,
    requested_model: crate::catalog::CollectionModel,
) -> bool {
    declared_model == requested_model || declared_model == crate::catalog::CollectionModel::Mixed
}

pub fn collection_model_name(model: crate::catalog::CollectionModel) -> &'static str {
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

pub fn normalize_row_fields_for_contract_at(
    contract: &CollectionContract,
    collection: &str,
    fields: Vec<(String, Value)>,
    mode: NormalizeMode,
    now_ms: u64,
) -> ContractResult<Vec<(String, Value)>> {
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
                return Err(ContractError::Query(format!(
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

    let resolved_columns = resolved_contract_columns(contract)?;
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
        return Err(ContractError::Query(format!(
            "collection '{}' is strict and does not allow undeclared fields: {}",
            collection,
            unknown_fields.join(", ")
        )));
    }
    let mut normalized = Vec::new();
    let generated_names: std::collections::BTreeSet<&str> = contract
        .declared_columns
        .iter()
        .filter(|column| column.generated.is_some())
        .map(|column| column.name.as_str())
        .collect();

    for column in &resolved_columns {
        if generated_names.contains(column.name.as_str()) {
            // A derived value is never trusted, including one carried forward by UPDATE.
            provided.remove(&column.name);
            continue;
        }
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
                    return Err(ContractError::Query(format!(
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

    expressions::apply_contract_expressions(contract, &resolved_columns, &mut normalized)?;
    Ok(normalized)
}

fn enforce_row_uniqueness_for_contract(
    contract: &CollectionContract,
    collection: &str,
    fields: &[(String, Value)],
    existing_rows: &[ContractRow],
    exclude_id: Option<crate::storage::entity::EntityId>,
) -> ContractResult<()> {
    if let Some(conflict) = find_row_uniqueness_conflict_for_contract(
        contract,
        collection,
        fields,
        existing_rows,
        None,
        exclude_id,
    )? {
        return Err(row_uniqueness_error(&conflict.rule, collection));
    }
    Ok(())
}

pub fn find_row_uniqueness_conflict_for_contract(
    contract: &CollectionContract,
    collection: &str,
    fields: &[(String, Value)],
    existing_rows: &[ContractRow],
    target: Option<&[String]>,
    exclude_id: Option<crate::storage::entity::EntityId>,
) -> ContractResult<Option<UniquenessConflict>> {
    if !matches!(
        contract.declared_model,
        crate::catalog::CollectionModel::Table | crate::catalog::CollectionModel::Mixed
    ) {
        return Ok(None);
    }

    let mut rules = resolved_uniqueness_rules(contract);
    if rules.is_empty() {
        return Ok(None);
    }
    if let Some(target) = target {
        rules.retain(|rule| uniqueness_columns_match(&rule.columns, target));
        if rules.is_empty() {
            return Err(ContractError::Query(format!(
                "no unique or primary-key constraint on collection '{}' matches ON CONFLICT ({})",
                collection,
                target.join(", ")
            )));
        }
    }

    let input_fields: std::collections::BTreeMap<String, Value> = fields.iter().cloned().collect();

    for rule in &rules {
        let mut expected_signatures = Vec::new();
        let mut skip_rule = false;

        for column in &rule.columns {
            match input_fields.get(column) {
                Some(Value::Null) | None if rule.primary_key => {
                    return Err(ContractError::Query(format!(
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
                return Ok(Some(UniquenessConflict {
                    rule: rule.clone(),
                    entity_id: existing.id,
                }));
            }
        }
    }

    Ok(None)
}

pub fn uniqueness_columns_match(rule_columns: &[String], target_columns: &[String]) -> bool {
    rule_columns.len() == target_columns.len()
        && rule_columns.iter().all(|rule_column| {
            target_columns
                .iter()
                .any(|target_column| target_column.eq_ignore_ascii_case(rule_column))
        })
}

pub fn row_uniqueness_error(rule: &UniquenessRule, collection: &str) -> ContractError {
    let qualifier = if rule.primary_key {
        "primary key"
    } else {
        "unique constraint"
    };
    ContractError::Query(format!(
        "{} '{}' violated on collection '{}' for columns [{}]",
        qualifier,
        rule.name,
        collection,
        rule.columns.join(", ")
    ))
}

pub fn enforce_row_batch_uniqueness_for_contract(
    contract: &CollectionContract,
    collection: &str,
    rows: &[Vec<(String, Value)>],
) -> ContractResult<()> {
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
                        return Err(ContractError::Query(format!(
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
                return Err(ContractError::Query(format!(
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

pub fn build_row_update_contract_plan_for_contract(
    contract: &CollectionContract,
) -> ContractResult<RowUpdateContractPlan> {
    let declared_rules = if contract.declared_model == crate::catalog::CollectionModel::Table
        && !(contract.declared_columns.is_empty()
            && contract
                .table_def
                .as_ref()
                .map(|table| table.columns.is_empty())
                .unwrap_or(true))
    {
        resolved_contract_columns(contract)?
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
        resolved_uniqueness_rules(contract)
            .into_iter()
            .flat_map(|rule| rule.columns.into_iter())
            .map(|column| (column, ()))
            .collect()
    } else {
        HashMap::new()
    };

    Ok(RowUpdateContractPlan {
        has_expressions: has_contract_expressions(contract),
        timestamps_enabled: contract.timestamps_enabled,
        strict_schema: matches!(contract.schema_mode, crate::catalog::SchemaMode::Strict),
        declared_rules,
        unique_columns,
    })
}

pub fn normalize_row_update_assignment_with_plan(
    collection: &str,
    column: &str,
    value: Value,
    row_contract_plan: Option<&RowUpdateContractPlan>,
) -> ContractResult<Value> {
    let Some(plan) = row_contract_plan else {
        return Ok(value);
    };

    if plan.timestamps_enabled && (column == "created_at" || column == "updated_at") {
        return Err(ContractError::Query(format!(
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
        Err(ContractError::Query(format!(
            "collection '{}' is strict and does not allow undeclared fields: {}",
            collection, column
        )))
    } else {
        Ok(value)
    }
}

pub fn normalize_row_update_value_for_rule(
    collection: &str,
    value: Value,
    row_rule: Option<&RowUpdateColumnRule>,
) -> ContractResult<Value> {
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

pub fn resolved_uniqueness_rules(contract: &CollectionContract) -> Vec<UniquenessRule> {
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
                reddb_types::ConstraintType::PrimaryKey
            ) && !constraint.columns.is_empty()
            {
                rules.push(UniquenessRule {
                    name: constraint.name.clone(),
                    columns: constraint.columns.clone(),
                    primary_key: true,
                });
            } else if matches!(
                constraint.constraint_type,
                reddb_types::ConstraintType::Unique
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

pub fn value_signature(value: &Value) -> String {
    format!("{value:?}")
}

pub fn normalize_declared_value(
    context: &str,
    name: &str,
    sql_type: &reddb_types::SqlTypeName,
    value: Value,
) -> ContractResult<Value> {
    let data_type = DataType::from_sql_type_name(sql_type)
        .ok_or_else(|| ContractError::Query(format!("unknown declared type '{sql_type}'")))?;
    let rule = ResolvedColumnRule {
        name: name.to_string(),
        data_type,
        data_type_name: sql_type.to_string(),
        not_null: false,
        default: None,
        enum_variants: sql_type.enum_variants().unwrap_or_default(),
    };
    normalize_contract_value(context, &rule, value)
}

fn normalize_contract_value(
    collection: &str,
    column: &ResolvedColumnRule,
    value: Value,
) -> ContractResult<Value> {
    if matches!(value, Value::Null) {
        if column.not_null {
            return Err(ContractError::Query(format!(
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
        return Err(ContractError::Query(format!(
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
) -> ContractResult<Value> {
    let target = column.data_type;
    match target {
        DataType::Blob => Ok(Value::Blob(raw.as_bytes().to_vec())),
        // ADR 0067 (#1721): a JSON value is written as an inline strict-JSON
        // literal — a bare string is not silently wrapped as JSON. Wrap a
        // runtime string with `JSON_PARSE(<expr>)`. (An inline literal already
        // arrives as `Value::Json` and never reaches this coercion.)
        DataType::Json => Err(ContractError::Query(format!(
            "column '{column_name}' in collection '{collection}' requires an inline strict-JSON \
             literal (e.g. `{{\"k\": \"v\"}}`) or `JSON_PARSE(<expr>)`; a bare string is not \
             coerced to JSON (ADR 0067)"
        ))),
        DataType::Timestamp => raw.parse::<i64>().map(Value::Timestamp).map_err(|err| {
            ContractError::Query(format!(
                "failed to coerce column '{}' in collection '{}' to '{}': {}",
                column_name, collection, column.data_type_name, err
            ))
        }),
        DataType::Duration => raw.parse::<i64>().map(Value::Duration).map_err(|err| {
            ContractError::Query(format!(
                "failed to coerce column '{}' in collection '{}' to '{}': {}",
                column_name, collection, column.data_type_name, err
            ))
        }),
        DataType::Vector | DataType::Array => Err(ContractError::Query(format!(
            "column '{}' in collection '{}' requires '{}' and only typed values are accepted for this type",
            column_name, collection, column.data_type_name
        ))),
        _ => coerce_schema_value(raw, target, Some(column.enum_variants.as_slice())).map_err(
            |err| {
                ContractError::Query(format!(
                    "failed to coerce column '{}' in collection '{}' to '{}': {}",
                    column_name, collection, column.data_type_name, err
                ))
            },
        ),
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
