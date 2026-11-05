//! Runtime contract mappings; reddb-file owns persisted JSON and table bytes.
use super::*;
use reddb_types::json::Value as JsonValue;
use std::io;

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}
fn file_json_to_core_json(json: String) -> JsonValue {
    crate::serde_json::from_str(&json).expect("reddb-file emitted JSON the runtime can parse")
}

pub fn collection_contract_to_json(contract: &CollectionContract) -> JsonValue {
    file_json_to_core_json(
        reddb_file::encode_physical_collection_contract_json(&collection_contract_to_persisted(
            contract,
        ))
        .expect("reddb-file must encode physical collection contract JSON"),
    )
}

pub fn collection_contract_to_persisted(
    contract: &CollectionContract,
) -> reddb_file::PhysicalCollectionContract {
    reddb_file::PhysicalCollectionContract {
        name: contract.name.clone(),
        declared_model: collection_model_as_str(contract.declared_model).to_string(),
        schema_mode: schema_mode_as_str(contract.schema_mode).to_string(),
        origin: contract.origin.as_str().to_string(),
        version: contract.version,
        created_at_unix_ms: contract.created_at_unix_ms,
        updated_at_unix_ms: contract.updated_at_unix_ms,
        default_ttl_ms: contract.default_ttl_ms,
        vector_dimension: contract.vector_dimension,
        vector_metric: contract
            .vector_metric
            .map(distance_metric_as_str)
            .map(str::to_string),
        context_index_fields: contract.context_index_fields.clone(),
        declared_columns: contract
            .declared_columns
            .iter()
            .map(declared_column_contract_to_persisted)
            .collect(),
        table_def_hex: contract
            .table_def
            .as_ref()
            .map(|table_def| hex::encode(reddb_file::encode_table_def(table_def))),
        timestamps_enabled: contract.timestamps_enabled,
        context_index_enabled: contract.context_index_enabled,
        metrics_raw_retention_ms: contract.metrics_raw_retention_ms,
        metrics_rollup_policies: contract.metrics_rollup_policies.clone(),
        metrics_tenant_identity: contract.metrics_tenant_identity.clone(),
        metrics_namespace: contract.metrics_namespace.clone(),
        append_only: contract.append_only,
        subscriptions: contract
            .subscriptions
            .iter()
            .map(subscription_descriptor_to_persisted)
            .collect(),
        analytics_config: contract
            .analytics_config
            .iter()
            .map(analytics_view_descriptor_to_persisted)
            .collect(),
        session_key: contract.session_key.clone(),
        session_gap_ms: contract.session_gap_ms,
        retention_duration_ms: contract.retention_duration_ms,
        analytical_storage: contract
            .analytical_storage
            .as_ref()
            .map(analytical_storage_to_persisted),
        ai_policy: contract.ai_policy.as_ref().map(ai_policy_to_persisted),
    }
}

pub fn collection_contract_from_persisted(
    contract: reddb_file::PhysicalCollectionContract,
) -> io::Result<CollectionContract> {
    let table_def = match contract.table_def_hex {
        Some(encoded) => {
            let bytes = hex::decode(encoded).map_err(|err| {
                invalid_data(format!("invalid collection contract table_def hex: {err}"))
            })?;
            Some(reddb_file::decode_table_def(&bytes).map_err(|err| {
                invalid_data(format!(
                    "invalid collection contract table_def payload: {err}"
                ))
            })?)
        }
        None => None,
    };

    let decoded = CollectionContract {
        name: contract.name,
        declared_model: collection_model_from_str(&contract.declared_model)?,
        schema_mode: schema_mode_from_str(&contract.schema_mode)?,
        origin: contract_origin_from_str(&contract.origin)?,
        version: contract.version,
        created_at_unix_ms: contract.created_at_unix_ms,
        updated_at_unix_ms: contract.updated_at_unix_ms,
        default_ttl_ms: contract.default_ttl_ms,
        vector_dimension: contract.vector_dimension,
        vector_metric: contract
            .vector_metric
            .as_deref()
            .map(distance_metric_from_str)
            .transpose()?,
        context_index_fields: contract.context_index_fields,
        declared_columns: contract
            .declared_columns
            .into_iter()
            .map(declared_column_contract_from_persisted)
            .collect::<io::Result<Vec<_>>>()?,
        table_def,
        timestamps_enabled: contract.timestamps_enabled,
        context_index_enabled: contract.context_index_enabled,
        metrics_raw_retention_ms: contract.metrics_raw_retention_ms,
        metrics_rollup_policies: contract.metrics_rollup_policies,
        metrics_tenant_identity: contract.metrics_tenant_identity,
        metrics_namespace: contract.metrics_namespace,
        append_only: contract.append_only,
        subscriptions: contract
            .subscriptions
            .into_iter()
            .map(subscription_descriptor_from_persisted)
            .collect::<io::Result<Vec<_>>>()?,
        analytics_config: contract
            .analytics_config
            .into_iter()
            .map(analytics_view_descriptor_from_persisted)
            .collect::<io::Result<Vec<_>>>()?,
        session_key: contract.session_key,
        session_gap_ms: contract.session_gap_ms,
        retention_duration_ms: contract.retention_duration_ms,
        analytical_storage: contract
            .analytical_storage
            .map(analytical_storage_from_persisted),
        ai_policy: contract.ai_policy.map(ai_policy_from_persisted),
    };
    if super::schema::has_contract_expressions(&decoded) {
        super::schema::validate_contract_expressions(&decoded)
            .map_err(|error| invalid_data(error.to_string()))?;
    }
    Ok(decoded)
}

pub fn analytical_storage_to_json(cfg: &crate::catalog::AnalyticalStorageConfig) -> JsonValue {
    file_json_to_core_json(
        reddb_file::encode_physical_analytical_storage_json(&analytical_storage_to_persisted(cfg))
            .expect("reddb-file must encode physical analytical storage JSON"),
    )
}

pub fn analytical_storage_from_json(
    value: &JsonValue,
) -> io::Result<crate::catalog::AnalyticalStorageConfig> {
    let persisted = reddb_file::decode_physical_analytical_storage_json(&value.to_string_compact())
        .map_err(|err| invalid_data(format!("decode physical analytical storage: {err}")))?;
    Ok(analytical_storage_from_persisted(persisted))
}

pub fn analytical_storage_to_persisted(
    cfg: &crate::catalog::AnalyticalStorageConfig,
) -> reddb_file::PhysicalAnalyticalStorageConfig {
    reddb_file::PhysicalAnalyticalStorageConfig {
        columnar: cfg.columnar,
        time_key: cfg.time_key.clone(),
        order_by_key: cfg.order_by_key.clone(),
    }
}

pub fn analytical_storage_from_persisted(
    cfg: reddb_file::PhysicalAnalyticalStorageConfig,
) -> crate::catalog::AnalyticalStorageConfig {
    crate::catalog::AnalyticalStorageConfig {
        columnar: cfg.columnar,
        time_key: cfg.time_key,
        order_by_key: cfg.order_by_key,
    }
}

pub fn ai_policy_to_persisted(policy: &crate::catalog::AiPolicy) -> reddb_file::PhysicalAiPolicy {
    reddb_file::PhysicalAiPolicy {
        embed: policy
            .embed
            .as_ref()
            .map(|embed| reddb_file::PhysicalAiEmbedPolicy {
                fields: embed.fields.clone(),
                provider: embed.provider.clone(),
                model: embed.model.clone(),
            }),
        moderate: policy
            .moderate
            .as_ref()
            .map(|moderate| reddb_file::PhysicalAiModeratePolicy {
                fields: moderate.fields.clone(),
                provider: moderate.provider.clone(),
                model: moderate.model.clone(),
                sync_gate: moderate.sync_gate,
                degraded_mode: moderate.degraded_mode.as_str().to_string(),
                reject_action: moderate.reject_action.as_str().to_string(),
                hard_delete_on_reject: moderate.hard_delete_on_reject,
            }),
        vision: policy
            .vision
            .as_ref()
            .map(|vision| reddb_file::PhysicalAiVisionPolicy {
                image_field: vision.image_field.clone(),
                output_kinds: vision.output_kinds.clone(),
                provider: vision.provider.clone(),
                model: vision.model.clone(),
            }),
    }
}

pub fn ai_policy_from_persisted(policy: reddb_file::PhysicalAiPolicy) -> crate::catalog::AiPolicy {
    use crate::catalog::{ModerateDegradedMode, ModerateRejectAction};
    crate::catalog::AiPolicy {
        embed: policy.embed.map(|embed| crate::catalog::EmbedPolicy {
            fields: embed.fields,
            provider: embed.provider,
            model: embed.model,
        }),
        moderate: policy
            .moderate
            .map(|moderate| crate::catalog::ModeratePolicy {
                fields: moderate.fields,
                provider: moderate.provider,
                model: moderate.model,
                sync_gate: moderate.sync_gate,
                degraded_mode: ModerateDegradedMode::from_str(&moderate.degraded_mode)
                    .unwrap_or_default(),
                reject_action: ModerateRejectAction::from_str(&moderate.reject_action)
                    .unwrap_or_default(),
                hard_delete_on_reject: moderate.hard_delete_on_reject,
            }),
        vision: policy.vision.map(|vision| crate::catalog::VisionPolicy {
            image_field: vision.image_field,
            output_kinds: vision.output_kinds,
            provider: vision.provider,
            model: vision.model,
        }),
    }
}

pub fn collection_contract_from_json(value: &JsonValue) -> io::Result<CollectionContract> {
    let decode_error = |message: String| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            CollectionContractDecodeError(message),
        )
    };
    let contract = reddb_file::decode_physical_collection_contract_json(&value.to_string_compact())
        .map_err(|error| decode_error(error.to_string()))?;
    collection_contract_from_persisted(contract).map_err(|error| decode_error(error.to_string()))
}

pub fn subscription_descriptor_to_json(
    subscription: &crate::catalog::SubscriptionDescriptor,
) -> JsonValue {
    file_json_to_core_json(
        reddb_file::encode_physical_subscription_descriptor_json(
            &subscription_descriptor_to_persisted(subscription),
        )
        .expect("reddb-file must encode physical subscription descriptor JSON"),
    )
}

pub fn subscription_descriptor_to_persisted(
    subscription: &crate::catalog::SubscriptionDescriptor,
) -> reddb_file::PhysicalSubscriptionDescriptor {
    reddb_file::PhysicalSubscriptionDescriptor {
        name: subscription.name.clone(),
        source: subscription.source.clone(),
        target_queue: subscription.target_queue.clone(),
        ops_filter: subscription
            .ops_filter
            .iter()
            .map(|op| op.as_str().to_string())
            .collect(),
        where_filter: subscription.where_filter.clone(),
        redact_fields: subscription.redact_fields.clone(),
        enabled: subscription.enabled,
        all_tenants: subscription.all_tenants,
    }
}

pub fn subscription_descriptor_from_persisted(
    subscription: reddb_file::PhysicalSubscriptionDescriptor,
) -> io::Result<crate::catalog::SubscriptionDescriptor> {
    let ops_filter = subscription
        .ops_filter
        .iter()
        .map(|op| {
            crate::catalog::SubscriptionOperation::from_str(op).ok_or_else(|| {
                invalid_data(format!(
                    "unsupported subscription operation in catalog: {op}"
                ))
            })
        })
        .collect::<io::Result<Vec<_>>>()?;

    Ok(crate::catalog::SubscriptionDescriptor {
        name: subscription.name,
        source: subscription.source,
        target_queue: subscription.target_queue,
        ops_filter,
        where_filter: subscription.where_filter,
        redact_fields: subscription.redact_fields,
        enabled: subscription.enabled,
        all_tenants: subscription.all_tenants,
    })
}

pub fn subscription_descriptor_from_json(
    value: &JsonValue,
) -> io::Result<crate::catalog::SubscriptionDescriptor> {
    let subscription =
        reddb_file::decode_physical_subscription_descriptor_json(&value.to_string_compact())
            .map_err(|err| {
                invalid_data(format!("decode physical subscription descriptor: {err}"))
            })?;
    subscription_descriptor_from_persisted(subscription)
}

pub fn analytics_view_descriptor_to_json(
    view: &crate::catalog::AnalyticsViewDescriptor,
) -> JsonValue {
    file_json_to_core_json(
        reddb_file::encode_physical_analytics_view_descriptor_json(
            &analytics_view_descriptor_to_persisted(view),
        )
        .expect("reddb-file must encode physical analytics view descriptor JSON"),
    )
}

pub fn analytics_view_descriptor_to_persisted(
    view: &crate::catalog::AnalyticsViewDescriptor,
) -> reddb_file::PhysicalAnalyticsViewDescriptor {
    reddb_file::PhysicalAnalyticsViewDescriptor {
        output: view.output.as_str().to_string(),
        algorithm: view.algorithm.clone(),
        resolution: view.resolution,
        max_iterations: view.max_iterations,
        tolerance: view.tolerance,
    }
}

pub fn analytics_view_descriptor_from_persisted(
    view: reddb_file::PhysicalAnalyticsViewDescriptor,
) -> io::Result<crate::catalog::AnalyticsViewDescriptor> {
    let output = crate::catalog::AnalyticsOutput::from_str(&view.output).ok_or_else(|| {
        invalid_data(format!(
            "analytics_view_descriptor.output has unsupported value: {}",
            view.output
        ))
    })?;
    Ok(crate::catalog::AnalyticsViewDescriptor {
        output,
        algorithm: view.algorithm,
        resolution: view.resolution,
        max_iterations: view.max_iterations,
        tolerance: view.tolerance,
    })
}

pub fn analytics_view_descriptor_from_json(
    value: &JsonValue,
) -> io::Result<crate::catalog::AnalyticsViewDescriptor> {
    let view =
        reddb_file::decode_physical_analytics_view_descriptor_json(&value.to_string_compact())
            .map_err(|err| {
                invalid_data(format!("decode physical analytics view descriptor: {err}"))
            })?;
    analytics_view_descriptor_from_persisted(view)
}

pub fn declared_column_contract_to_json(column: &DeclaredColumnContract) -> JsonValue {
    file_json_to_core_json(
        reddb_file::encode_physical_declared_column_contract_json(
            &declared_column_contract_to_persisted(column),
        )
        .expect("reddb-file must encode physical declared column contract JSON"),
    )
}

pub fn declared_column_contract_to_persisted(
    column: &DeclaredColumnContract,
) -> reddb_file::PhysicalDeclaredColumnContract {
    reddb_file::PhysicalDeclaredColumnContract {
        name: column.name.clone(),
        data_type: column.data_type.clone(),
        sql_type: column.sql_type.as_ref().map(sql_type_name_to_persisted),
        not_null: column.not_null,
        default: column.default.clone(),
        generated: column
            .generated
            .as_ref()
            .map(|expression| expression.source().to_string()),
        check: column
            .check
            .as_ref()
            .map(|expression| expression.source().to_string()),
        compress: column.compress,
        unique: column.unique,
        primary_key: column.primary_key,
        enum_variants: column.enum_variants.clone(),
        array_element: column.array_element.clone(),
        decimal_precision: column.decimal_precision,
    }
}

pub fn declared_column_contract_from_persisted(
    column: reddb_file::PhysicalDeclaredColumnContract,
) -> io::Result<DeclaredColumnContract> {
    let sql_type = column
        .sql_type
        .map(sql_type_name_from_persisted)
        .or_else(|| Some(reddb_types::SqlTypeName::parse_declared(&column.data_type)));
    Ok(DeclaredColumnContract {
        name: column.name,
        data_type: column.data_type,
        sql_type,
        not_null: column.not_null,
        default: column.default,
        generated: column
            .generated
            .map(reddb_rql::schema_expression::SchemaExpression::parse)
            .transpose()
            .map_err(invalid_data)?,
        check: column
            .check
            .map(reddb_rql::schema_expression::SchemaExpression::parse)
            .transpose()
            .map_err(invalid_data)?,
        compress: column.compress,
        unique: column.unique,
        primary_key: column.primary_key,
        enum_variants: column.enum_variants,
        array_element: column.array_element,
        decimal_precision: column.decimal_precision,
    })
}

pub fn declared_column_contract_from_json(value: &JsonValue) -> io::Result<DeclaredColumnContract> {
    let column =
        reddb_file::decode_physical_declared_column_contract_json(&value.to_string_compact())
            .map_err(|err| {
                invalid_data(format!("decode physical declared column contract: {err}"))
            })?;
    declared_column_contract_from_persisted(column)
}

pub fn sql_type_name_to_persisted(
    sql_type: &reddb_types::SqlTypeName,
) -> reddb_file::PhysicalSqlTypeName {
    reddb_file::PhysicalSqlTypeName {
        name: sql_type.name.clone(),
        modifiers: sql_type
            .modifiers
            .iter()
            .map(type_modifier_to_persisted)
            .collect(),
    }
}

pub fn sql_type_name_from_persisted(
    sql_type: reddb_file::PhysicalSqlTypeName,
) -> reddb_types::SqlTypeName {
    reddb_types::SqlTypeName {
        name: sql_type.name,
        modifiers: sql_type
            .modifiers
            .into_iter()
            .map(type_modifier_from_persisted)
            .collect(),
    }
}

pub fn type_modifier_to_persisted(
    modifier: &reddb_types::TypeModifier,
) -> reddb_file::PhysicalTypeModifier {
    match modifier {
        reddb_types::TypeModifier::Number(value) => {
            reddb_file::PhysicalTypeModifier::Number(*value)
        }
        reddb_types::TypeModifier::Ident(value) => {
            reddb_file::PhysicalTypeModifier::Ident(value.clone())
        }
        reddb_types::TypeModifier::StringLiteral(value) => {
            reddb_file::PhysicalTypeModifier::StringLiteral(value.clone())
        }
        reddb_types::TypeModifier::Type(value) => {
            reddb_file::PhysicalTypeModifier::Type(Box::new(sql_type_name_to_persisted(value)))
        }
    }
}

pub fn type_modifier_from_persisted(
    modifier: reddb_file::PhysicalTypeModifier,
) -> reddb_types::TypeModifier {
    match modifier {
        reddb_file::PhysicalTypeModifier::Number(value) => reddb_types::TypeModifier::Number(value),
        reddb_file::PhysicalTypeModifier::Ident(value) => reddb_types::TypeModifier::Ident(value),
        reddb_file::PhysicalTypeModifier::StringLiteral(value) => {
            reddb_types::TypeModifier::StringLiteral(value)
        }
        reddb_file::PhysicalTypeModifier::Type(value) => {
            reddb_types::TypeModifier::Type(Box::new(sql_type_name_from_persisted(*value)))
        }
    }
}

pub fn collection_model_as_str(model: crate::catalog::CollectionModel) -> &'static str {
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

pub fn collection_model_from_str(value: &str) -> io::Result<crate::catalog::CollectionModel> {
    match value {
        "table" => Ok(crate::catalog::CollectionModel::Table),
        "document" => Ok(crate::catalog::CollectionModel::Document),
        "graph" => Ok(crate::catalog::CollectionModel::Graph),
        "vector" => Ok(crate::catalog::CollectionModel::Vector),
        "hll" => Ok(crate::catalog::CollectionModel::Hll),
        "sketch" => Ok(crate::catalog::CollectionModel::Sketch),
        "filter" => Ok(crate::catalog::CollectionModel::Filter),
        "kv" => Ok(crate::catalog::CollectionModel::Kv),
        "config" => Ok(crate::catalog::CollectionModel::Config),
        "vault" => Ok(crate::catalog::CollectionModel::Vault),
        "mixed" => Ok(crate::catalog::CollectionModel::Mixed),
        "timeseries" => Ok(crate::catalog::CollectionModel::TimeSeries),
        "queue" => Ok(crate::catalog::CollectionModel::Queue),
        "metrics" => Ok(crate::catalog::CollectionModel::Metrics),
        other => Err(invalid_data(format!(
            "unsupported collection contract model: {other}"
        ))),
    }
}

pub fn distance_metric_as_str(metric: reddb_types::distance::DistanceMetric) -> &'static str {
    match metric {
        reddb_types::distance::DistanceMetric::L2 => "l2",
        reddb_types::distance::DistanceMetric::Cosine => "cosine",
        reddb_types::distance::DistanceMetric::InnerProduct => "inner_product",
    }
}

pub fn distance_metric_from_str(value: &str) -> io::Result<reddb_types::distance::DistanceMetric> {
    match value {
        "l2" | "L2" => Ok(reddb_types::distance::DistanceMetric::L2),
        "cosine" | "COSINE" => Ok(reddb_types::distance::DistanceMetric::Cosine),
        "inner_product" | "INNER_PRODUCT" => {
            Ok(reddb_types::distance::DistanceMetric::InnerProduct)
        }
        other => Err(invalid_data(format!(
            "unsupported collection contract vector_metric: {other}"
        ))),
    }
}

pub fn schema_mode_as_str(mode: crate::catalog::SchemaMode) -> &'static str {
    match mode {
        crate::catalog::SchemaMode::Strict => "strict",
        crate::catalog::SchemaMode::SemiStructured => "semi_structured",
        crate::catalog::SchemaMode::Dynamic => "dynamic",
    }
}

pub fn schema_mode_from_str(value: &str) -> io::Result<crate::catalog::SchemaMode> {
    match value {
        "strict" => Ok(crate::catalog::SchemaMode::Strict),
        "semi_structured" => Ok(crate::catalog::SchemaMode::SemiStructured),
        "dynamic" => Ok(crate::catalog::SchemaMode::Dynamic),
        other => Err(invalid_data(format!(
            "unsupported collection contract schema mode: {other}"
        ))),
    }
}

pub fn contract_origin_from_str(value: &str) -> io::Result<ContractOrigin> {
    match value {
        "explicit" => Ok(ContractOrigin::Explicit),
        "implicit" => Ok(ContractOrigin::Implicit),
        "migrated" => Ok(ContractOrigin::Migrated),
        other => Err(invalid_data(format!(
            "unsupported collection contract origin: {other}"
        ))),
    }
}

/// Serialize a set of collection contracts to a self-describing JSON-array
/// blob. Used to carry contracts through the single-file binary dump (store
/// format V10+) so each collection's `declared_model` survives a restart.
pub fn serialize_collection_contracts(contracts: &[CollectionContract]) -> Vec<u8> {
    let array = JsonValue::Array(contracts.iter().map(collection_contract_to_json).collect());
    array.to_string_compact().into_bytes()
}

/// Inverse of [`serialize_collection_contracts`].
pub fn deserialize_collection_contracts(bytes: &[u8]) -> io::Result<Vec<CollectionContract>> {
    let text = std::str::from_utf8(bytes)
        .map_err(|err| invalid_data(format!("collection contract blob is not utf8: {err}")))?;
    let value = crate::json::from_str::<JsonValue>(text)
        .map_err(|err| invalid_data(format!("invalid collection contract blob: {err}")))?;
    match value {
        JsonValue::Array(items) => items.iter().map(collection_contract_from_json).collect(),
        _ => Err(invalid_data("collection contract blob is not a JSON array")),
    }
}
