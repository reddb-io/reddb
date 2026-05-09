//! Stable Config keyed command execution.

use std::sync::Arc;

use crate::catalog::{CollectionModel, SchemaMode};
use crate::physical::{CollectionContract, ContractOrigin};
use crate::storage::query::ast::ConfigValueType;
use crate::storage::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};

use super::*;

const CONFIG_HISTORY_LIMIT: usize = 16;

#[derive(Clone)]
struct ConfigVersion {
    id: EntityId,
    version: i64,
    value: Value,
    tombstone: bool,
    created_at_ms: i64,
    op: String,
    value_type: Option<ConfigValueType>,
    schema_version: Option<i64>,
}

impl RedDBRuntime {
    pub fn execute_config_command(
        &self,
        raw_query: &str,
        cmd: &crate::storage::query::ast::ConfigCommand,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::storage::query::ast::ConfigCommand;

        match cmd {
            ConfigCommand::Put {
                collection,
                key,
                value,
                value_type,
            } => self.config_write_result(
                raw_query,
                collection,
                key,
                value.clone(),
                *value_type,
                "put",
            ),
            ConfigCommand::Rotate {
                collection,
                key,
                value,
                value_type,
            } => self.config_write_result(
                raw_query,
                collection,
                key,
                value.clone(),
                *value_type,
                "rotate",
            ),
            ConfigCommand::Get { collection, key } => {
                self.config_get_result(raw_query, collection, key)
            }
            ConfigCommand::Delete { collection, key } => {
                self.config_delete_result(raw_query, collection, key)
            }
            ConfigCommand::History { collection, key } => {
                self.config_history_result(raw_query, collection, key)
            }
            ConfigCommand::InvalidVolatileOperation { operation, .. } => {
                Err(invalid_config_volatility(operation))
            }
        }
    }

    pub(crate) fn validate_config_command_before_auth(
        &self,
        cmd: &crate::storage::query::ast::ConfigCommand,
    ) -> RedDBResult<()> {
        use crate::storage::query::ast::ConfigCommand;
        match cmd {
            ConfigCommand::InvalidVolatileOperation { operation, .. } => {
                Err(invalid_config_volatility(operation))
            }
            ConfigCommand::Put { collection, .. }
            | ConfigCommand::Get { collection, .. }
            | ConfigCommand::Rotate { collection, .. }
            | ConfigCommand::Delete { collection, .. }
            | ConfigCommand::History { collection, .. } => {
                let snapshot = self.inner.db.catalog_model_snapshot();
                let Some(actual_model) = snapshot
                    .collections
                    .iter()
                    .find(|c| c.name == *collection)
                    .map(|c| c.declared_model.unwrap_or(c.model))
                else {
                    return Ok(());
                };
                crate::runtime::ddl::polymorphic_resolver::ensure_model_match(
                    CollectionModel::Config,
                    actual_model,
                )
            }
        }
    }

    fn config_write_result(
        &self,
        raw_query: &str,
        collection: &str,
        key: &str,
        value: Value,
        requested_type: Option<ConfigValueType>,
        op: &str,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Dml)?;
        self.ensure_config_collection(collection)?;
        let latest = self.latest_config_version(collection, key)?;
        let version = latest.as_ref().map(|version| version.version).unwrap_or(0) + 1;
        let (value_type, schema_version) = resolve_config_schema(latest.as_ref(), requested_type);
        if let Some(value_type) = value_type {
            validate_config_value_type(&value, value_type)?;
        }
        let id = self.append_config_version(
            collection,
            key,
            value,
            version,
            false,
            op,
            value_type,
            schema_version,
        )?;
        self.prune_config_history(collection, key)?;
        self.invalidate_result_cache();
        Ok(config_write_output(
            raw_query,
            collection,
            key,
            version,
            id,
            value_type,
            schema_version,
            match op {
                "rotate" => "config_rotate",
                _ => "config_put",
            },
            1,
        ))
    }

    fn config_delete_result(
        &self,
        raw_query: &str,
        collection: &str,
        key: &str,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_write(crate::runtime::write_gate::WriteKind::Dml)?;
        self.ensure_config_collection(collection)?;
        let latest = self.latest_config_version(collection, key)?;
        let version = latest.as_ref().map(|version| version.version).unwrap_or(0) + 1;
        let value_type = latest.as_ref().and_then(|version| version.value_type);
        let schema_version = latest.as_ref().and_then(|version| version.schema_version);
        let id = self.append_config_version(
            collection,
            key,
            Value::Null,
            version,
            true,
            "delete",
            value_type,
            schema_version,
        )?;
        self.prune_config_history(collection, key)?;
        self.invalidate_result_cache();
        Ok(config_write_output(
            raw_query,
            collection,
            key,
            version,
            id,
            value_type,
            schema_version,
            "delete",
            1,
        ))
    }

    fn config_get_result(
        &self,
        raw_query: &str,
        collection: &str,
        key: &str,
    ) -> RedDBResult<RuntimeQueryResult> {
        let latest = self.latest_config_version(collection, key)?;
        let mut result = UnifiedResult::with_columns(vec![
            "collection".into(),
            "key".into(),
            "value".into(),
            "version".into(),
            "value_type".into(),
            "schema_version".into(),
            "tags".into(),
            "tombstone".into(),
        ]);
        let mut record = UnifiedRecord::new();
        record.set("collection", Value::text(collection.to_string()));
        record.set("key", Value::text(key.to_string()));
        if let Some(version) = latest {
            record.set("value", version.value);
            record.set("version", Value::Integer(version.version));
            record.set("value_type", config_value_type_value(version.value_type));
            record.set(
                "schema_version",
                version
                    .schema_version
                    .map(Value::Integer)
                    .unwrap_or(Value::Null),
            );
            record.set("tags", Value::Null);
            record.set("tombstone", Value::Boolean(version.tombstone));
        } else {
            record.set("value", Value::Null);
            record.set("version", Value::Null);
            record.set("value_type", Value::Null);
            record.set("schema_version", Value::Null);
            record.set("tags", Value::Null);
            record.set("tombstone", Value::Boolean(false));
        }
        result.push(record);
        Ok(RuntimeQueryResult {
            query: raw_query.to_string(),
            mode: crate::storage::query::modes::QueryMode::Sql,
            statement: "config_get",
            engine: "config",
            result,
            affected_rows: 0,
            statement_type: "select",
        })
    }

    fn config_history_result(
        &self,
        raw_query: &str,
        collection: &str,
        key: &str,
    ) -> RedDBResult<RuntimeQueryResult> {
        let mut versions = self.config_versions(collection, key)?;
        versions.sort_by_key(|v| v.version);
        let mut result = UnifiedResult::with_columns(vec![
            "collection".into(),
            "key".into(),
            "version".into(),
            "value".into(),
            "value_type".into(),
            "schema_version".into(),
            "tags".into(),
            "tombstone".into(),
            "op".into(),
            "created_at_ms".into(),
        ]);
        for version in versions {
            let mut record = UnifiedRecord::new();
            record.set("collection", Value::text(collection.to_string()));
            record.set("key", Value::text(key.to_string()));
            record.set("version", Value::Integer(version.version));
            record.set("value", version.value);
            record.set("value_type", config_value_type_value(version.value_type));
            record.set(
                "schema_version",
                version
                    .schema_version
                    .map(Value::Integer)
                    .unwrap_or(Value::Null),
            );
            record.set("tags", Value::Null);
            record.set("tombstone", Value::Boolean(version.tombstone));
            record.set("op", Value::text(version.op));
            record.set("created_at_ms", Value::Integer(version.created_at_ms));
            result.push(record);
        }
        Ok(RuntimeQueryResult {
            query: raw_query.to_string(),
            mode: crate::storage::query::modes::QueryMode::Sql,
            statement: "config_history",
            engine: "config",
            result,
            affected_rows: 0,
            statement_type: "select",
        })
    }

    fn ensure_config_collection(&self, collection: &str) -> RedDBResult<()> {
        let store = self.inner.db.store();
        if store.get_collection(collection).is_none() {
            store
                .create_collection(collection)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
        }
        if let Some(contract) = self.inner.db.collection_contract(collection) {
            crate::runtime::ddl::polymorphic_resolver::ensure_model_match(
                CollectionModel::Config,
                contract.declared_model,
            )?;
            return Ok(());
        }
        let now = current_unix_ms();
        self.inner
            .db
            .save_collection_contract(CollectionContract {
                name: collection.to_string(),
                declared_model: CollectionModel::Config,
                schema_mode: SchemaMode::Dynamic,
                origin: ContractOrigin::Explicit,
                version: 1,
                created_at_unix_ms: now as u128,
                updated_at_unix_ms: now as u128,
                default_ttl_ms: None,
                context_index_fields: Vec::new(),
                declared_columns: Vec::new(),
                table_def: None,
                timestamps_enabled: false,
                context_index_enabled: false,
                append_only: false,
                subscriptions: Vec::new(),
            })
            .map(|_| ())
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    fn append_config_version(
        &self,
        collection: &str,
        key: &str,
        value: Value,
        version: i64,
        tombstone: bool,
        op: &str,
        value_type: Option<ConfigValueType>,
        schema_version: Option<i64>,
    ) -> RedDBResult<EntityId> {
        let now = current_unix_ms() as i64;
        let fields = vec![
            ("key".to_string(), Value::text(key.to_string())),
            ("value".to_string(), value),
            ("version".to_string(), Value::Integer(version)),
            (
                "value_type".to_string(),
                config_value_type_value(value_type),
            ),
            (
                "schema_version".to_string(),
                schema_version.map(Value::Integer).unwrap_or(Value::Null),
            ),
            ("tombstone".to_string(), Value::Boolean(tombstone)),
            ("op".to_string(), Value::text(op.to_string())),
            ("created_at_ms".to_string(), Value::Integer(now)),
        ];
        let mut row = RowData::new(Vec::new());
        row.named = Some(fields.into_iter().collect());
        let entity = UnifiedEntity::new(
            EntityId::new(0),
            EntityKind::TableRow {
                table: Arc::from(collection),
                row_id: 0,
            },
            EntityData::Row(row),
        );
        self.inner
            .db
            .store()
            .insert(collection, entity)
            .map_err(|err| RedDBError::Internal(err.to_string()))
    }

    fn latest_config_version(
        &self,
        collection: &str,
        key: &str,
    ) -> RedDBResult<Option<ConfigVersion>> {
        Ok(self
            .config_versions(collection, key)?
            .into_iter()
            .max_by_key(|version| version.version))
    }

    fn config_versions(&self, collection: &str, key: &str) -> RedDBResult<Vec<ConfigVersion>> {
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(collection) else {
            return Ok(Vec::new());
        };
        let mut versions = Vec::new();
        for entity in manager.query_all(|_| true) {
            let EntityData::Row(row) = &entity.data else {
                continue;
            };
            if !matches!(row.get_field("key"), Some(Value::Text(value)) if value.as_ref() == key) {
                continue;
            }
            versions.push(ConfigVersion {
                id: entity.id,
                version: value_i64(row.get_field("version")).unwrap_or(0),
                value: row.get_field("value").cloned().unwrap_or(Value::Null),
                tombstone: matches!(row.get_field("tombstone"), Some(Value::Boolean(true))),
                created_at_ms: value_i64(row.get_field("created_at_ms")).unwrap_or(0),
                op: match row.get_field("op") {
                    Some(Value::Text(value)) => value.to_string(),
                    _ => "put".to_string(),
                },
                value_type: row
                    .get_field("value_type")
                    .and_then(config_value_type_from_value),
                schema_version: value_i64(row.get_field("schema_version")),
            });
        }
        Ok(versions)
    }

    fn prune_config_history(&self, collection: &str, key: &str) -> RedDBResult<()> {
        let mut versions = self.config_versions(collection, key)?;
        if versions.len() <= CONFIG_HISTORY_LIMIT {
            return Ok(());
        }
        versions.sort_by_key(|version| version.version);
        let drop_count = versions.len() - CONFIG_HISTORY_LIMIT;
        let store = self.inner.db.store();
        for version in versions.into_iter().take(drop_count) {
            store
                .delete(collection, version.id)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
        }
        Ok(())
    }
}

fn config_write_output(
    raw_query: &str,
    collection: &str,
    key: &str,
    version: i64,
    id: EntityId,
    value_type: Option<ConfigValueType>,
    schema_version: Option<i64>,
    statement: &'static str,
    affected_rows: u64,
) -> RuntimeQueryResult {
    let mut result = UnifiedResult::with_columns(vec![
        "ok".into(),
        "collection".into(),
        "key".into(),
        "version".into(),
        "value_type".into(),
        "schema_version".into(),
        "tags".into(),
        "id".into(),
    ]);
    let mut record = UnifiedRecord::new();
    record.set("ok", Value::Boolean(true));
    record.set("collection", Value::text(collection.to_string()));
    record.set("key", Value::text(key.to_string()));
    record.set("version", Value::Integer(version));
    record.set("value_type", config_value_type_value(value_type));
    record.set(
        "schema_version",
        schema_version.map(Value::Integer).unwrap_or(Value::Null),
    );
    record.set("tags", Value::Null);
    record.set("id", Value::Integer(id.raw() as i64));
    result.push(record);
    RuntimeQueryResult {
        query: raw_query.to_string(),
        mode: crate::storage::query::modes::QueryMode::Sql,
        statement,
        engine: "config",
        result,
        affected_rows,
        statement_type: if statement == "delete" {
            "delete"
        } else {
            "update"
        },
    }
}

fn invalid_config_volatility(operation: &str) -> RedDBError {
    RedDBError::InvalidOperation(format!(
        "CONFIG does not support KV-only volatility operation {operation}"
    ))
}

fn resolve_config_schema(
    latest: Option<&ConfigVersion>,
    requested_type: Option<ConfigValueType>,
) -> (Option<ConfigValueType>, Option<i64>) {
    let previous_type = latest.and_then(|version| version.value_type);
    let previous_schema_version = latest.and_then(|version| version.schema_version);
    match requested_type {
        Some(value_type) if Some(value_type) != previous_type => (
            Some(value_type),
            Some(previous_schema_version.unwrap_or(0) + 1),
        ),
        Some(value_type) => (Some(value_type), previous_schema_version.or(Some(1))),
        None => (previous_type, previous_schema_version),
    }
}

fn validate_config_value_type(value: &Value, value_type: ConfigValueType) -> RedDBResult<()> {
    let valid = match value_type {
        ConfigValueType::Bool => matches!(value, Value::Boolean(_)),
        ConfigValueType::Int => matches!(
            value,
            Value::Integer(_) | Value::UnsignedInteger(_) | Value::BigInt(_)
        ),
        ConfigValueType::String => matches!(value, Value::Text(_)),
        ConfigValueType::Url => validate_config_url(value),
        ConfigValueType::Object => validate_config_json_shape(value, true),
        ConfigValueType::Array => {
            matches!(value, Value::Array(_) | Value::Vector(_))
                || validate_config_json_shape(value, false)
        }
    };
    if valid {
        Ok(())
    } else {
        Err(RedDBError::InvalidConfig(format!(
            "CONFIG value type mismatch: expected {}, got {}",
            value_type.as_str(),
            config_actual_value_type(value),
        )))
    }
}

fn validate_config_url(value: &Value) -> bool {
    let url = match value {
        Value::Url(value) => value.as_str(),
        Value::Text(value) => value.as_ref(),
        _ => return false,
    };
    url.starts_with("http://") || url.starts_with("https://") || url.starts_with("ftp://")
}

fn validate_config_json_shape(value: &Value, object: bool) -> bool {
    let Value::Json(bytes) = value else {
        return false;
    };
    let Ok(json) = crate::json::from_slice::<crate::json::Value>(bytes) else {
        return false;
    };
    matches!(
        (object, json),
        (true, crate::json::Value::Object(_)) | (false, crate::json::Value::Array(_))
    )
}

fn config_actual_value_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Boolean(_) => "bool",
        Value::Integer(_) | Value::UnsignedInteger(_) | Value::BigInt(_) => "int",
        Value::Text(_) => "string",
        Value::Url(_) => "url",
        Value::Json(bytes) => match crate::json::from_slice::<crate::json::Value>(bytes) {
            Ok(crate::json::Value::Object(_)) => "object",
            Ok(crate::json::Value::Array(_)) => "array",
            _ => "json",
        },
        Value::Array(_) | Value::Vector(_) => "array",
        _ => "other",
    }
}

fn config_value_type_value(value_type: Option<ConfigValueType>) -> Value {
    value_type
        .map(|value_type| Value::text(value_type.as_str()))
        .unwrap_or(Value::Null)
}

fn config_value_type_from_value(value: &Value) -> Option<ConfigValueType> {
    match value {
        Value::Text(value) => ConfigValueType::parse(value.as_ref()),
        _ => None,
    }
}

fn value_i64(value: Option<&Value>) -> Option<i64> {
    match value {
        Some(Value::Integer(value)) => Some(*value),
        Some(Value::UnsignedInteger(value)) => i64::try_from(*value).ok(),
        Some(Value::Timestamp(value)) => Some(*value),
        Some(Value::Duration(value)) => Some(*value),
        _ => None,
    }
}

fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
