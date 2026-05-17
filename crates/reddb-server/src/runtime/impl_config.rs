//! Stable Config keyed command execution.

use std::sync::Arc;

use crate::catalog::{CollectionModel, SchemaMode};
use crate::physical::{CollectionContract, ContractOrigin};
use crate::storage::query::ast::ConfigValueType;
use crate::storage::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};

use super::impl_core::{current_auth_identity, current_connection_id, current_tenant};
use super::*;

const CONFIG_HISTORY_LIMIT: usize = 16;

#[derive(Clone)]
struct ConfigVersion {
    id: EntityId,
    key: String,
    version: i64,
    value: Value,
    tombstone: bool,
    created_at_ms: i64,
    op: String,
    value_type: Option<ConfigValueType>,
    schema_version: Option<i64>,
    tags: Vec<String>,
}

impl super::keyed_spine::KeyedVersion for ConfigVersion {
    fn key(&self) -> &str {
        &self.key
    }

    fn version(&self) -> i64 {
        self.version
    }
}

impl ConfigVersion {
    fn from_keyed_row(version: super::keyed_spine::KeyedRowVersion, row: &RowData) -> Self {
        Self {
            id: version.id,
            key: version.key,
            version: version.version,
            value: version.value,
            tombstone: version.tombstone,
            created_at_ms: version.created_at_ms,
            op: version.op,
            value_type: row
                .get_field("value_type")
                .and_then(config_value_type_from_value),
            schema_version: super::keyed_spine::value_i64(row.get_field("schema_version")),
            tags: config_tags_from_value(row.get_field("tags")),
        }
    }
}

struct ConfigSecretRef {
    collection: String,
    key: String,
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
                tags,
            } => self.config_write_result(
                raw_query,
                collection,
                key,
                value.clone(),
                *value_type,
                tags,
                "put",
            ),
            ConfigCommand::Rotate {
                collection,
                key,
                value,
                value_type,
                tags,
            } => self.config_write_result(
                raw_query,
                collection,
                key,
                value.clone(),
                *value_type,
                tags,
                "rotate",
            ),
            ConfigCommand::Get { collection, key } => {
                self.config_get_result(raw_query, collection, key)
            }
            ConfigCommand::Resolve { collection, key } => {
                self.config_resolve_result(raw_query, collection, key)
            }
            ConfigCommand::Delete { collection, key } => {
                self.config_delete_result(raw_query, collection, key)
            }
            ConfigCommand::History { collection, key } => {
                self.config_history_result(raw_query, collection, key)
            }
            ConfigCommand::List {
                collection,
                prefix,
                limit,
                offset,
            } => self.config_list_result(raw_query, collection, prefix.as_deref(), *limit, *offset),
            ConfigCommand::Watch {
                collection,
                key,
                prefix,
                from_lsn,
            } => self.config_watch_result(raw_query, collection, key, *prefix, *from_lsn),
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
            | ConfigCommand::Resolve { collection, .. }
            | ConfigCommand::Rotate { collection, .. }
            | ConfigCommand::Delete { collection, .. }
            | ConfigCommand::History { collection, .. }
            | ConfigCommand::List { collection, .. }
            | ConfigCommand::Watch { collection, .. } => {
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

    fn config_resolve_result(
        &self,
        raw_query: &str,
        collection: &str,
        key: &str,
    ) -> RedDBResult<RuntimeQueryResult> {
        let latest = self.latest_config_version(collection, key)?;
        if let Err(reason) = self.check_config_capability("config:read", collection, key) {
            self.audit_config_resolve(
                collection,
                key,
                None,
                crate::runtime::audit_log::Outcome::Denied,
                &reason,
            );
            return Err(RedDBError::Query(reason));
        }

        let Some(version) = latest else {
            let reason = "not_found";
            self.audit_config_resolve(
                collection,
                key,
                None,
                crate::runtime::audit_log::Outcome::Denied,
                reason,
            );
            return Err(RedDBError::NotFound(format!(
                "config '{}.{}' not found",
                collection, key
            )));
        };
        if version.tombstone {
            let reason = "deleted";
            self.audit_config_resolve(
                collection,
                key,
                None,
                crate::runtime::audit_log::Outcome::Denied,
                reason,
            );
            return Err(RedDBError::NotFound(format!(
                "config '{}.{}' is deleted",
                collection, key
            )));
        }

        let secret_ref = parse_config_secret_ref(&version.value).inspect_err(|err| {
            self.audit_config_resolve(
                collection,
                key,
                None,
                crate::runtime::audit_log::Outcome::Error,
                &err.to_string(),
            );
        })?;

        match self.resolve_vault_secret_value(&secret_ref.collection, &secret_ref.key) {
            Ok(value) => {
                self.audit_config_resolve(
                    collection,
                    key,
                    Some(&secret_ref),
                    crate::runtime::audit_log::Outcome::Success,
                    "ok",
                );
                let mut result = UnifiedResult::with_columns(vec![
                    "collection".into(),
                    "key".into(),
                    "value".into(),
                    "resolved_store".into(),
                    "resolved_collection".into(),
                    "resolved_key".into(),
                ]);
                let mut record = UnifiedRecord::new();
                record.set("collection", Value::text(collection.to_string()));
                record.set("key", Value::text(key.to_string()));
                record.set("value", value);
                record.set("resolved_store", Value::text("vault"));
                record.set("resolved_collection", Value::text(secret_ref.collection));
                record.set("resolved_key", Value::text(secret_ref.key));
                result.push(record);
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: crate::storage::query::modes::QueryMode::Sql,
                    statement: "config_resolve",
                    engine: "config",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }
            Err(err) => {
                let reason = err.to_string();
                let outcome = if reason.contains("denied") {
                    crate::runtime::audit_log::Outcome::Denied
                } else {
                    crate::runtime::audit_log::Outcome::Error
                };
                self.audit_config_resolve(collection, key, Some(&secret_ref), outcome, &reason);
                Err(err)
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
        tags: &[String],
        op: &str,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.check_system_config_capability("config:write", collection, key)
            .map_err(RedDBError::Query)?;
        self.check_write(crate::runtime::write_gate::WriteKind::Dml)?;
        self.ensure_config_collection(collection)?;
        let latest = self.latest_config_version(collection, key)?;
        let version = latest.as_ref().map(|version| version.version).unwrap_or(0) + 1;
        let (value_type, schema_version) = resolve_config_schema(latest.as_ref(), requested_type);
        if let Some(value_type) = value_type {
            validate_config_value_type(&value, value_type)?;
        }
        let before = latest.as_ref().and_then(|version| {
            if version.tombstone {
                None
            } else {
                Some(crate::presentation::entity_json::storage_value_to_json(
                    &version.value,
                ))
            }
        });
        let after = Some(crate::presentation::entity_json::storage_value_to_json(
            &value,
        ));
        let change_op = if latest.is_some() {
            crate::replication::cdc::ChangeOperation::Update
        } else {
            crate::replication::cdc::ChangeOperation::Insert
        };
        let id = self.append_config_version(
            collection,
            key,
            value,
            version,
            false,
            op,
            value_type,
            schema_version,
            tags,
        )?;
        self.record_kv_watch_event(change_op, collection, key, id.raw(), before, after);
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
            tags,
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
        self.check_system_config_capability("config:write", collection, key)
            .map_err(RedDBError::Query)?;
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
            &[],
        )?;
        if let Some(before) = latest.as_ref().and_then(|version| {
            if version.tombstone {
                None
            } else {
                Some(crate::presentation::entity_json::storage_value_to_json(
                    &version.value,
                ))
            }
        }) {
            self.record_kv_watch_event(
                crate::replication::cdc::ChangeOperation::Delete,
                collection,
                key,
                id.raw(),
                Some(before),
                None,
            );
        }
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
            &[],
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
        self.check_system_config_capability("config:read", collection, key)
            .map_err(RedDBError::Query)?;
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
            record.set("tags", config_tags_value(&version.tags));
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
        self.check_system_config_capability("config:read", collection, key)
            .map_err(RedDBError::Query)?;
        let versions = super::keyed_spine::history_versions(self.config_versions(collection, key)?);
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

    fn config_list_result(
        &self,
        raw_query: &str,
        collection: &str,
        prefix: Option<&str>,
        limit: Option<usize>,
        offset: usize,
    ) -> RedDBResult<RuntimeQueryResult> {
        let mut versions = self.latest_config_versions(collection, prefix)?;
        versions.sort_by(|left, right| left.key.cmp(&right.key));
        let mut result = UnifiedResult::with_columns(vec![
            "collection".into(),
            "key".into(),
            "value".into(),
            "version".into(),
            "value_type".into(),
            "schema_version".into(),
            "tags".into(),
            "tombstone".into(),
            "op".into(),
            "created_at_ms".into(),
        ]);
        for version in versions
            .into_iter()
            .filter(|version| {
                self.check_config_capability("config:read", collection, &version.key)
                    .is_ok()
            })
            .skip(offset)
            .take(limit.unwrap_or(usize::MAX))
        {
            let mut record = UnifiedRecord::new();
            record.set("collection", Value::text(collection.to_string()));
            record.set("key", Value::text(version.key));
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
            record.set("tags", config_tags_value(&version.tags));
            record.set("tombstone", Value::Boolean(version.tombstone));
            record.set("op", Value::text(version.op));
            record.set("created_at_ms", Value::Integer(version.created_at_ms));
            result.push(record);
        }
        Ok(RuntimeQueryResult {
            query: raw_query.to_string(),
            mode: crate::storage::query::modes::QueryMode::Sql,
            statement: "config_list",
            engine: "config",
            result,
            affected_rows: 0,
            statement_type: "select",
        })
    }

    fn config_watch_result(
        &self,
        raw_query: &str,
        collection: &str,
        key: &str,
        prefix: bool,
        from_lsn: Option<u64>,
    ) -> RedDBResult<RuntimeQueryResult> {
        let watch_key = if prefix {
            format!("{key}.*")
        } else {
            key.to_string()
        };
        let endpoint = match from_lsn {
            Some(lsn) => {
                format!("/collections/{collection}/config/{watch_key}/watch?since_lsn={lsn}")
            }
            None => format!("/collections/{collection}/config/{watch_key}/watch"),
        };
        let mut result = UnifiedResult::with_columns(vec![
            "collection".into(),
            "key".into(),
            "prefix".into(),
            "from_lsn".into(),
            "watch_url".into(),
            "streaming".into(),
        ]);
        let mut record = UnifiedRecord::new();
        record.set("collection", Value::text(collection.to_string()));
        record.set("key", Value::text(watch_key));
        record.set("prefix", Value::Boolean(prefix));
        record.set(
            "from_lsn",
            from_lsn
                .map(Value::UnsignedInteger)
                .unwrap_or(crate::storage::schema::Value::Null),
        );
        record.set("watch_url", Value::text(endpoint));
        record.set("streaming", Value::Boolean(true));
        result.push(record);
        Ok(RuntimeQueryResult {
            query: raw_query.to_string(),
            mode: crate::storage::query::modes::QueryMode::Sql,
            statement: "config_watch",
            engine: "config",
            result,
            affected_rows: 0,
            statement_type: "stream",
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
                vector_dimension: None,
                vector_metric: None,
                context_index_fields: Vec::new(),
                declared_columns: Vec::new(),
                table_def: None,
                timestamps_enabled: false,
                context_index_enabled: false,
                metrics_raw_retention_ms: None,
                metrics_rollup_policies: Vec::new(),
                metrics_tenant_identity: None,
                metrics_namespace: None,
                append_only: false,
                subscriptions: Vec::new(),
                session_key: None,
                session_gap_ms: None,
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
        tags: &[String],
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
            ("tags".to_string(), config_tags_value(tags)),
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
        Ok(super::keyed_spine::latest_version(
            self.config_versions(collection, key)?,
        ))
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
            let Some(version) = super::keyed_spine::row_version(entity.id, row, 0) else {
                continue;
            };
            if version.key != key {
                continue;
            }
            versions.push(ConfigVersion::from_keyed_row(version, row));
        }
        Ok(versions)
    }

    fn latest_config_versions(
        &self,
        collection: &str,
        prefix: Option<&str>,
    ) -> RedDBResult<Vec<ConfigVersion>> {
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection(collection) else {
            return Ok(Vec::new());
        };
        let mut versions = Vec::new();
        for entity in manager.query_all(|_| true) {
            let EntityData::Row(row) = &entity.data else {
                continue;
            };
            let Some(version) = super::keyed_spine::row_version(entity.id, row, 0) else {
                continue;
            };
            versions.push(ConfigVersion::from_keyed_row(version, row));
        }
        Ok(super::keyed_spine::latest_versions(versions, prefix))
    }

    fn prune_config_history(&self, collection: &str, key: &str) -> RedDBResult<()> {
        let mut versions = self.config_versions(collection, key)?;
        if versions.len() <= CONFIG_HISTORY_LIMIT {
            return Ok(());
        }
        versions = super::keyed_spine::history_versions(versions);
        let drop_count = versions.len() - CONFIG_HISTORY_LIMIT;
        let store = self.inner.db.store();
        for version in versions.into_iter().take(drop_count) {
            store
                .delete(collection, version.id)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
        }
        Ok(())
    }

    fn check_config_capability(
        &self,
        action: &str,
        collection: &str,
        key: &str,
    ) -> Result<(), String> {
        let Some(auth_store) = self.inner.auth_store.read().clone() else {
            return Ok(());
        };
        if !auth_store.iam_authorization_enabled() {
            return Ok(());
        }
        let Some((principal, role)) = current_auth_identity() else {
            return Err(
                "IAM authorization is enabled; config capability check requires an authenticated principal"
                    .to_string(),
            );
        };
        let tenant = current_tenant();
        let principal_id = crate::auth::UserId::from_parts(tenant.as_deref(), &principal);
        let mut resource = crate::auth::policies::ResourceRef::new(
            "config",
            config_target_resource(collection, key),
        );
        if let Some(ref tenant) = tenant {
            resource = resource.with_tenant(tenant.clone());
        }
        let ctx = crate::auth::policies::EvalContext {
            principal_tenant: tenant.clone(),
            current_tenant: tenant,
            peer_ip: None,
            mfa_present: false,
            now_ms: crate::utils::now_unix_millis() as u128,
            principal_is_admin_role: role == crate::auth::Role::Admin,
        };
        if auth_store.check_policy_authz(&principal_id, action, &resource, &ctx) {
            Ok(())
        } else {
            Err(format!(
                "principal=`{}` action=`{}` resource=`config:{}` denied by IAM policy",
                principal,
                action,
                config_target_resource(collection, key)
            ))
        }
    }

    fn check_system_config_capability(
        &self,
        action: &str,
        collection: &str,
        key: &str,
    ) -> Result<(), String> {
        if collection != "red.config" {
            return Ok(());
        }
        self.check_config_capability(action, collection, key)
    }

    pub fn config_watch_events_since(
        &self,
        collection: &str,
        key: &str,
        since_lsn: u64,
        max_count: usize,
    ) -> Vec<crate::replication::cdc::KvWatchEvent> {
        self.kv_watch_events_since(collection, key, since_lsn, max_count)
            .into_iter()
            .map(|event| self.policy_filter_config_watch_event(event))
            .collect()
    }

    pub fn config_watch_events_since_prefix(
        &self,
        collection: &str,
        prefix: &str,
        since_lsn: u64,
        max_count: usize,
    ) -> Vec<crate::replication::cdc::KvWatchEvent> {
        self.kv_watch_events_since_prefix(collection, prefix, since_lsn, max_count)
            .into_iter()
            .map(|event| self.policy_filter_config_watch_event(event))
            .collect()
    }

    fn policy_filter_config_watch_event(
        &self,
        mut event: crate::replication::cdc::KvWatchEvent,
    ) -> crate::replication::cdc::KvWatchEvent {
        if self
            .check_config_capability("config:read", &event.collection, &event.key)
            .is_err()
        {
            event.before = None;
            event.after = None;
        }
        event
    }

    fn audit_config_resolve(
        &self,
        collection: &str,
        key: &str,
        secret_ref: Option<&ConfigSecretRef>,
        outcome: crate::runtime::audit_log::Outcome,
        reason: &str,
    ) {
        let actor = current_auth_identity()
            .map(|(principal, _)| principal)
            .unwrap_or_else(|| "anonymous".to_string());
        let request_id = match current_connection_id() {
            0 => "embedded".to_string(),
            id => format!("conn-{id}"),
        };
        let mut builder = crate::runtime::audit_log::AuditEvent::builder("config/resolve")
            .principal(actor.clone())
            .source(crate::runtime::audit_log::AuditAuthSource::Password)
            .resource(format!(
                "config:{}",
                config_target_resource(collection, key)
            ))
            .outcome(outcome)
            .correlation_id(request_id.clone())
            .fields([
                crate::runtime::audit_log::AuditFieldEscaper::field("actor", actor),
                crate::runtime::audit_log::AuditFieldEscaper::field("collection", collection),
                crate::runtime::audit_log::AuditFieldEscaper::field("key", key),
                crate::runtime::audit_log::AuditFieldEscaper::field(
                    "target",
                    config_target_resource(collection, key),
                ),
                crate::runtime::audit_log::AuditFieldEscaper::field("reason", reason),
                crate::runtime::audit_log::AuditFieldEscaper::field("request_id", request_id),
                crate::runtime::audit_log::AuditFieldEscaper::field(
                    "connection_id",
                    current_connection_id(),
                ),
            ]);
        if let Some(tenant) = current_tenant() {
            builder = builder.tenant(tenant);
        }
        if let Some(secret_ref) = secret_ref {
            builder = builder.fields([
                crate::runtime::audit_log::AuditFieldEscaper::field("resolved_store", "vault"),
                crate::runtime::audit_log::AuditFieldEscaper::field(
                    "resolved_collection",
                    secret_ref.collection.as_str(),
                ),
                crate::runtime::audit_log::AuditFieldEscaper::field(
                    "resolved_key",
                    secret_ref.key.as_str(),
                ),
                crate::runtime::audit_log::AuditFieldEscaper::field(
                    "resolved_target",
                    format!("{}.{}", secret_ref.collection, secret_ref.key),
                ),
            ]);
        }
        self.audit_log().record_event(builder.build());
    }
}

fn parse_config_secret_ref(value: &Value) -> RedDBResult<ConfigSecretRef> {
    let Value::Json(bytes) = value else {
        return Err(RedDBError::InvalidConfig(
            "CONFIG value is not a SecretRef".to_string(),
        ));
    };
    let json = crate::json::from_slice::<crate::json::Value>(bytes).map_err(|err| {
        RedDBError::InvalidConfig(format!("CONFIG SecretRef is malformed: {err}"))
    })?;
    let Some(object) = json.as_object() else {
        return Err(RedDBError::InvalidConfig(
            "CONFIG SecretRef must be an object".to_string(),
        ));
    };
    let get_str = |field: &str| -> RedDBResult<&str> {
        object
            .get(field)
            .and_then(|value| value.as_str())
            .ok_or_else(|| RedDBError::InvalidConfig(format!("CONFIG SecretRef missing {field}")))
    };
    if get_str("type")? != "secret_ref" {
        return Err(RedDBError::InvalidConfig(
            "CONFIG value is not a SecretRef".to_string(),
        ));
    }
    if get_str("store")? != "vault" {
        return Err(RedDBError::InvalidConfig(
            "CONFIG SecretRef store is unsupported".to_string(),
        ));
    }
    Ok(ConfigSecretRef {
        collection: get_str("collection")?.to_string(),
        key: get_str("key")?.to_string(),
    })
}

fn config_target_resource(collection: &str, key: &str) -> String {
    if collection == "red.config" {
        format!("red.config/{}", key.to_ascii_lowercase())
    } else {
        format!("{collection}.{key}")
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
    tags: &[String],
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
    record.set("tags", config_tags_value(tags));
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

fn config_tags_value(tags: &[String]) -> Value {
    if tags.is_empty() {
        return Value::Null;
    }
    Value::Array(tags.iter().map(|tag| Value::text(tag.clone())).collect())
}

fn config_tags_from_value(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(|value| match value {
                Value::Text(tag) => Some(tag.to_string()),
                _ => None,
            })
            .collect(),
        Some(Value::Json(bytes)) => crate::json::from_slice::<crate::json::Value>(bytes)
            .ok()
            .and_then(|value| value.as_array().map(|values| values.to_vec()))
            .map(|values| {
                values
                    .into_iter()
                    .filter_map(|value| value.as_str().map(ToOwned::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
