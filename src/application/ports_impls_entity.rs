use crate::application::entity::{CreateDocumentInput, CreateKvInput};
use crate::application::ttl_payload::{
    has_internal_ttl_metadata, normalize_ttl_patch_operations, parse_top_level_ttl_metadata_entries,
};
use crate::json::{to_vec as json_to_vec, Value as JsonValue};
use crate::storage::schema::{coerce as coerce_schema_value, DataType, Value};
use crate::storage::unified::MetadataValue;

use super::*;

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
        .filter_map(|(name, _)| (!declared_names.contains(name)).then(|| name.clone()))
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
            Ok(ResolvedColumnRule {
                name: column.name.clone(),
                data_type: parse_declared_data_type(&column.data_type)?,
                data_type_name: column.data_type.clone(),
                not_null: column.not_null,
                default: column.default.clone(),
                enum_variants: column.enum_variants.clone(),
            })
        })
        .collect()
}

fn parse_declared_data_type(value: &str) -> RedDBResult<DataType> {
    let normalized = value.trim().to_ascii_lowercase();
    let data_type = match normalized.as_str() {
        "integer" | "int" => DataType::Integer,
        "unsignedinteger" | "unsigned_integer" | "uint" => DataType::UnsignedInteger,
        "float" | "double" | "real" => DataType::Float,
        "text" | "string" => DataType::Text,
        "blob" => DataType::Blob,
        "boolean" | "bool" => DataType::Boolean,
        "timestamp" => DataType::Timestamp,
        "duration" => DataType::Duration,
        "ipaddr" | "ip" => DataType::IpAddr,
        "macaddr" => DataType::MacAddr,
        "vector" => DataType::Vector,
        "json" => DataType::Json,
        "uuid" => DataType::Uuid,
        "noderef" => DataType::NodeRef,
        "edgeref" => DataType::EdgeRef,
        "vectorref" => DataType::VectorRef,
        "rowref" => DataType::RowRef,
        "color" => DataType::Color,
        "email" => DataType::Email,
        "url" => DataType::Url,
        "phone" => DataType::Phone,
        "semver" => DataType::Semver,
        "cidr" => DataType::Cidr,
        "date" => DataType::Date,
        "time" => DataType::Time,
        "decimal" => DataType::Decimal,
        "enum" => DataType::Enum,
        "array" => DataType::Array,
        "timestampms" | "timestamp_ms" => DataType::TimestampMs,
        "ipv4" => DataType::Ipv4,
        "ipv6" => DataType::Ipv6,
        "subnet" => DataType::Subnet,
        "port" => DataType::Port,
        "latitude" => DataType::Latitude,
        "longitude" => DataType::Longitude,
        "geopoint" | "geo_point" => DataType::GeoPoint,
        "country2" => DataType::Country2,
        "country3" => DataType::Country3,
        "lang2" => DataType::Lang2,
        "lang5" => DataType::Lang5,
        "currency" => DataType::Currency,
        "coloralpha" | "color_alpha" => DataType::ColorAlpha,
        "bigint" | "big_int" => DataType::BigInt,
        "keyref" => DataType::KeyRef,
        "docref" => DataType::DocRef,
        "tableref" => DataType::TableRef,
        "pageref" => DataType::PageRef,
        "secret" => DataType::Secret,
        "password" => DataType::Password,
        other => {
            return Err(crate::RedDBError::Query(format!(
                "unsupported declared data type '{}'",
                other
            )))
        }
    };
    Ok(data_type)
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
        DataType::ColorAlpha => "color_alpha",
        DataType::BigInt => "bigint",
        DataType::KeyRef => "keyref",
        DataType::DocRef => "docref",
        DataType::TableRef => "tableref",
        DataType::PageRef => "pageref",
        DataType::Secret => "secret",
        DataType::Password => "password",
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
        let columns: Vec<(&str, crate::storage::schema::Value)> = fields
            .iter()
            .map(|(key, value)| (key.as_str(), value.clone()))
            .collect();
        let mut builder = db.row(&collection, columns);

        for (key, value) in metadata {
            builder = builder.metadata(key, value);
        }

        for node in node_links {
            builder = builder.link_to_node(node);
        }

        for vector in vector_links {
            builder = builder.link_to_vector(vector);
        }

        let id = builder.save()?;
        refresh_context_index(&db, &collection, id)?;
        self.cdc_emit(
            crate::replication::cdc::ChangeOperation::Insert,
            &collection,
            id.raw(),
            "table",
        );
        Ok(CreateEntityOutput {
            id,
            entity: db.get(id),
        })
    }

    fn create_node(&self, input: CreateNodeInput) -> RedDBResult<CreateEntityOutput> {
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
        refresh_context_index(&db, &input.collection, id)?;
        self.cdc_emit(
            crate::replication::cdc::ChangeOperation::Insert,
            &input.collection,
            id.raw(),
            "graph_node",
        );
        Ok(CreateEntityOutput {
            id,
            entity: db.get(id),
        })
    }

    fn create_edge(&self, input: CreateEdgeInput) -> RedDBResult<CreateEntityOutput> {
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
        refresh_context_index(&db, &input.collection, id)?;
        self.cdc_emit(
            crate::replication::cdc::ChangeOperation::Insert,
            &input.collection,
            id.raw(),
            "graph_edge",
        );
        Ok(CreateEntityOutput {
            id,
            entity: db.get(id),
        })
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
        refresh_context_index(&db, &input.collection, id)?;
        self.cdc_emit(
            crate::replication::cdc::ChangeOperation::Insert,
            &input.collection,
            id.raw(),
            "vector",
        );
        Ok(CreateEntityOutput {
            id,
            entity: db.get(id),
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
        refresh_context_index(&db, &input.collection, id)?;
        self.cdc_emit(
            crate::replication::cdc::ChangeOperation::Insert,
            &input.collection,
            id.raw(),
            "document",
        );
        Ok(CreateEntityOutput {
            id,
            entity: db.get(id),
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
            store.context_index().remove_entity(id);
            store
                .delete(collection, id)
                .map_err(|err| crate::RedDBError::Internal(err.to_string()))?;
            Ok(true)
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
        let operations = normalize_ttl_patch_operations(operations)?;

        let db = self.db();
        let store = db.store();
        let Some(manager) = store.get_collection(&collection) else {
            return Err(crate::RedDBError::NotFound(format!(
                "collection not found: {collection}"
            )));
        };
        let Some(mut entity) = manager.get(id) else {
            return Err(crate::RedDBError::NotFound(format!(
                "entity not found: {}",
                id.raw()
            )));
        };

        let mut patch_metadata = store.get_metadata(&collection, id).unwrap_or_default();
        let mut metadata_changed = false;

        // Contract-aware guard: if this collection auto-manages
        // `created_at`/`updated_at`, reject any patch that targets
        // them directly. The runtime will still bump `updated_at`
        // on its own inside `normalize_row_fields_for_contract_with_mode`.
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
                    let named = row.named.get_or_insert_with(Default::default);
                    for (key, value) in fields {
                        named.insert(key.clone(), json_to_storage_value(value)?);
                    }
                }

                if !metadata_ops.is_empty() {
                    let mut metadata_json = metadata_to_json(&patch_metadata);
                    apply_patch_operations_to_json(&mut metadata_json, &metadata_ops)
                        .map_err(crate::RedDBError::Query)?;
                    patch_metadata = metadata_from_json(&metadata_json)?;
                    metadata_changed = true;
                }

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
                enforce_row_uniqueness(&db, &collection, &normalized_fields, Some(id))?;
                row.named = Some(normalized_fields.into_iter().collect());
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
                    apply_patch_operations_to_storage_map(&mut node.properties, &field_ops)?;
                }

                if let Some(fields) = payload
                    .get("fields")
                    .and_then(crate::json::Value::as_object)
                {
                    for (key, value) in fields {
                        node.properties
                            .insert(key.clone(), json_to_storage_value(value)?);
                    }
                }

                if !metadata_ops.is_empty() {
                    let mut metadata_json = metadata_to_json(&patch_metadata);
                    apply_patch_operations_to_json(&mut metadata_json, &metadata_ops)
                        .map_err(crate::RedDBError::Query)?;
                    patch_metadata = metadata_from_json(&metadata_json)?;
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
                    apply_patch_operations_to_storage_map(&mut edge.properties, &field_ops)?;
                }

                for op in weight_ops {
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
                    for (key, value) in fields {
                        edge.properties
                            .insert(key.clone(), json_to_storage_value(value)?);
                    }
                }

                if !metadata_ops.is_empty() {
                    let mut metadata_json = metadata_to_json(&patch_metadata);
                    apply_patch_operations_to_json(&mut metadata_json, &metadata_ops)
                        .map_err(crate::RedDBError::Query)?;
                    patch_metadata = metadata_from_json(&metadata_json)?;
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
                    apply_patch_operations_to_vector_fields(vector, &field_ops)?;
                }

                if let Some(fields) = payload
                    .get("fields")
                    .and_then(crate::json::Value::as_object)
                {
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
                    let mut metadata_json = metadata_to_json(&patch_metadata);
                    apply_patch_operations_to_json(&mut metadata_json, &metadata_ops)
                        .map_err(crate::RedDBError::Query)?;
                    patch_metadata = metadata_from_json(&metadata_json)?;
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
            for (key, value) in metadata {
                patch_metadata.set(key.clone(), json_to_metadata_value(value)?);
            }
            metadata_changed = true;
        }

        for (key, value) in parse_top_level_ttl_metadata_entries(&payload)? {
            if matches!(value, crate::storage::unified::MetadataValue::Null) {
                patch_metadata.remove(&key);
            } else {
                patch_metadata.set(key, value);
            }
            metadata_changed = true;
        }

        if metadata_changed {
            store
                .set_metadata(&collection, id, patch_metadata)
                .map_err(|err| crate::RedDBError::Query(err.to_string()))?;
        }

        entity.updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        manager
            .update(entity)
            .map_err(|err| crate::RedDBError::Query(err.to_string()))?;
        refresh_context_index(&db, &collection, id)?;
        self.cdc_emit(
            crate::replication::cdc::ChangeOperation::Update,
            &collection,
            id.raw(),
            "entity",
        );

        Ok(CreateEntityOutput {
            id,
            entity: db.get(id),
        })
    }

    fn delete_entity(&self, input: DeleteEntityInput) -> RedDBResult<DeleteEntityOutput> {
        let store = self.db().store();
        store.context_index().remove_entity(input.id);
        self.cdc_emit(
            crate::replication::cdc::ChangeOperation::Delete,
            &input.collection,
            input.id.raw(),
            "entity",
        );
        let deleted = store
            .delete(&input.collection, input.id)
            .map_err(|err| crate::RedDBError::Internal(err.to_string()))?;
        Ok(DeleteEntityOutput {
            deleted,
            id: input.id,
        })
    }
}
