//! Runtime config / secret / KV resolution.
//!
//! Extracted verbatim from `impl_core.rs` (impl_core slice 7/10, issue #1628).
//! Houses the config-tier readers, secret encrypt/decrypt pipeline, and vault
//! KV accessors:
//!
//! - **Free helpers** — `seed_storage_deploy_config`, `show_secrets_allows_key`,
//!   `secret_sql_value_to_string`, `insert_config_json_path`,
//!   `insert_config_json_segments`, `show_config_json_result`.
//! - **Methods** — `vault_kv_get`, `vault_kv_try_set`, `secret_aes_key`,
//!   `config_bool`, `binary_document_body_enabled`, `config_u64`, `config_f64`,
//!   `config_string`, `secret_auto_encrypt`.
use super::*;

pub(crate) fn seed_storage_deploy_config(
    store: &crate::storage::UnifiedStore,
    selection: crate::storage::StorageProfileSelection,
) {
    store.set_config_tree(
        "storage.deploy",
        &crate::json!({
            "profile": selection.deploy_profile.as_str(),
            "packaging": selection.packaging.as_str(),
            "preset": selection.preset_name(),
            "replica_count": selection.replica_count,
            "managed_backup": selection.managed_backup,
            "wal_retention": selection.wal_retention,
        }),
    );
}

pub(crate) fn show_secrets_allows_key(key: &str) -> bool {
    ![
        "red.secret.",
        "red.config.",
        "red.iam.",
        "red.acl.",
        "red.vault.",
    ]
    .iter()
    .any(|prefix| key.starts_with(prefix))
}

pub(crate) fn secret_sql_value_to_string(value: &Value) -> RedDBResult<String> {
    match value {
        Value::Text(s) => Ok(s.to_string()),
        Value::Integer(n) => Ok(n.to_string()),
        Value::UnsignedInteger(n) => Ok(n.to_string()),
        Value::Float(n) => Ok(n.to_string()),
        Value::Boolean(b) => Ok(b.to_string()),
        Value::Null => Err(RedDBError::Query(
            "SET SECRET key = NULL deletes the secret; use DELETE SECRET for explicit deletes"
                .to_string(),
        )),
        Value::Password(_) | Value::Secret(_) => Err(RedDBError::Query(
            "SET SECRET accepts plain scalar literals; PASSWORD() and SECRET() are for typed columns"
                .to_string(),
        )),
        _ => Err(RedDBError::Query(format!(
            "SET SECRET does not support value type {:?} yet",
            value.data_type()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn show_secrets_allows_only_user_managed_keys() {
        assert!(!show_secrets_allows_key("red.secret.aes_key"));
        assert!(!show_secrets_allows_key(
            "red.secret.ai.providers.anthropic.tokens.default"
        ));
        assert!(!show_secrets_allows_key("red.config.ai.default.provider"));
        assert!(show_secrets_allows_key("acme.key"));
    }

    #[test]
    fn typed_config_readers_preserve_cold_values_and_live_updates() {
        let directory = tempfile::tempdir().expect("config fixture");
        let runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(directory.path().join("config.rdb"))
                .with_memory_budget(32 * 1024 * 1024)
                .with_bounded_disk_residency(true),
        )
        .expect("runtime");
        let store = runtime.db().store();
        store
            .try_set_config_tree(
                "probe",
                &crate::json!({"boolean": false, "integer": 42, "float": 1.5, "string": "saved"}),
            )
            .expect("persist configuration");
        store
            .try_set_config_tree("red.config.queue.max_wait_ms", &crate::json!(1))
            .expect("queue wait limit");
        store.persist().expect("checkpoint");
        let manager = store
            .get_collection("red_config")
            .expect("config collection");
        assert!(manager.has_backing(), "fixture must read persisted rows");
        assert!(
            manager.query_all(|_| true).is_empty(),
            "fixture must be cold"
        );
        assert_eq!(
            store.try_get_config("probe.integer").unwrap(),
            Some(Value::Integer(42))
        );
        assert!(!runtime.config_bool("probe.boolean", true));
        assert_eq!(runtime.config_u64("probe.integer", 9), 42);
        assert_eq!(runtime.config_f64("probe.float", 9.0), 1.5);
        assert_eq!(runtime.config_string("probe.string", "default"), "saved");
        assert!(runtime.redwire_queue_wait_cap_check(5).is_err());
        store
            .try_set_config_tree("probe.integer", &crate::json!(73))
            .expect("live replacement");
        assert_eq!(runtime.config_u64("probe.integer", 9), 73);
        assert_eq!(runtime.config_u64("probe.missing", 9), 9);
    }

    #[test]
    fn boot_defaults_and_overlay_preserve_cold_operator_configuration() {
        let directory = tempfile::tempdir().expect("config fixture");
        let runtime = RedDBRuntime::with_options(
            RedDBOptions::persistent(directory.path().join("config.rdb"))
                .with_memory_budget(32 * 1024 * 1024)
                .with_bounded_disk_residency(true),
        )
        .expect("runtime");
        let store = runtime.db().store();
        store
            .try_set_config_tree("concurrency.locking.enabled", &crate::json!(false))
            .expect("operator configuration");
        store.persist().expect("checkpoint");
        super::super::config_matrix::heal_critical_keys(store.as_ref());
        assert_eq!(
            store.try_get_config("concurrency.locking.enabled").unwrap(),
            Some(Value::Boolean(false))
        );
        let overlay = directory.path().join("overlay.json");
        std::fs::write(&overlay, r#"{"concurrency":{"locking":{"enabled":true}}}"#)
            .expect("overlay fixture");
        assert_eq!(
            super::super::config_overlay::apply_config_file(
                store.as_ref(),
                overlay.to_str().unwrap()
            ),
            0
        );
        assert_eq!(
            store.try_get_config("concurrency.locking.enabled").unwrap(),
            Some(Value::Boolean(false))
        );
    }
}

pub(crate) fn insert_config_json_path(
    root: &mut crate::serde_json::Value,
    path: &str,
    value: crate::serde_json::Value,
) {
    let segments: Vec<&str> = path
        .split('.')
        .filter(|segment| !segment.is_empty())
        .collect();
    insert_config_json_segments(root, &segments, value);
}

fn insert_config_json_segments(
    root: &mut crate::serde_json::Value,
    segments: &[&str],
    value: crate::serde_json::Value,
) {
    if segments.is_empty() {
        *root = value;
        return;
    }

    if !matches!(root, crate::serde_json::Value::Object(_)) {
        *root = crate::serde_json::Value::Object(crate::serde_json::Map::new());
    }

    let crate::serde_json::Value::Object(map) = root else {
        return;
    };
    if segments.len() == 1 {
        map.insert(segments[0].to_string(), value);
        return;
    }
    let entry = map
        .entry(segments[0].to_string())
        .or_insert_with(|| crate::serde_json::Value::Object(crate::serde_json::Map::new()));
    insert_config_json_segments(entry, &segments[1..], value);
}

pub(crate) fn show_config_json_result(
    query: &str,
    mode: reddb_rql::modes::QueryMode,
    prefix: &Option<String>,
    value: crate::serde_json::Value,
) -> RuntimeQueryResult {
    let mut result = UnifiedResult::with_columns(vec!["key".into(), "value".into()]);
    let mut record = UnifiedRecord::new();
    record.set(
        "key",
        prefix
            .as_ref()
            .map(|key| Value::text(key.clone()))
            .unwrap_or(Value::Null),
    );
    record.set("value", Value::Json(value.to_string_compact().into_bytes()));
    result.push(record);
    RuntimeQueryResult {
        query: query.to_string(),
        mode,
        statement: "show_config_json",
        engine: "runtime-config",
        result,
        affected_rows: 0,
        statement_type: "select",
        bookmark: None,
        notice: None,
    }
}

impl RedDBRuntime {
    pub(crate) fn set_internal_provider_secret(
        &self,
        key: &str,
        value: Option<String>,
    ) -> RedDBResult<()> {
        self.reject_vault_transaction()?;
        if super::execution_context::current_tenant().is_some() {
            return Err(RedDBError::Query(
                "system provider credentials are platform-scoped".into(),
            ));
        }
        let auth =
            self.inner.auth_store.read().clone().ok_or_else(|| {
                RedDBError::Query("secret storage requires an enabled vault".into())
            })?;
        self.check_secret_write_privilege(&auth, key)?;
        match value {
            Some(value) => auth.vault_kv_try_set(key.into(), value),
            None => auth.vault_kv_try_delete(key).map(|_| ()),
        }
        .map_err(|err| RedDBError::Query(err.to_string()))?;
        self.invalidate_result_cache();
        Ok(())
    }

    /// Read a vault KV secret from the configured AuthStore, if present.
    pub fn vault_kv_get(&self, key: &str) -> Option<String> {
        self.inner
            .auth_store
            .read()
            .as_ref()
            .and_then(|store| store.vault_kv_get(key))
    }

    /// Write a vault KV secret and fail if the encrypted vault write is
    /// unavailable or cannot be made durable.
    pub fn vault_kv_try_set(&self, key: String, value: String) -> RedDBResult<()> {
        let store = self.inner.auth_store.read().clone().ok_or_else(|| {
            RedDBError::Query("secret storage requires an enabled, unsealed vault".to_string())
        })?;
        store
            .vault_kv_try_set(key, value)
            .map_err(|err| RedDBError::Query(err.to_string()))
    }

    /// Returns the vault AES key (`red.secret.aes_key`) if an auth
    /// store is wired and a key has been generated. Used by the
    /// `Value::Secret` encrypt/decrypt pipeline.
    pub(crate) fn secret_aes_key(&self) -> Option<[u8; 32]> {
        let guard = self.inner.auth_store.read();
        guard.as_ref().and_then(|s| s.vault_secret_key())
    }

    /// Resolve a boolean flag from `red_config`. Defaults to `default`
    /// when the key is missing or not coercible. If the same key has
    /// been written multiple times (SET CONFIG appends new rows), the
    /// most recent entity wins. Env-var overrides
    /// (`REDDB_<UP_DOTTED>`) take highest precedence.
    pub(crate) fn config_bool(&self, key: &str, default: bool) -> bool {
        if let Some(raw) = self.inner.env_config_overrides.get(key) {
            if let Some(reddb_types::Value::Boolean(b)) =
                crate::runtime::config_overlay::coerce_env_value(key, raw)
            {
                return b;
            }
        }
        match self.inner.db.store().get_config(key) {
            Some(Value::Boolean(value)) => value,
            Some(Value::Text(value)) => matches!(value.as_ref(), "true" | "TRUE" | "True" | "1"),
            Some(Value::Integer(value)) => value != 0,
            _ => default,
        }
    }

    /// Whether DOCUMENT writes should store the body as the native binary
    /// container (PRD-1398, ADR-0063). On by default after the production
    /// cutover. Reads decode the container transparently regardless of this
    /// flag.
    pub(crate) fn binary_document_body_enabled(&self) -> bool {
        self.config_bool("storage.binary_document_body", true)
    }

    pub(crate) fn config_u64(&self, key: &str, default: u64) -> u64 {
        if let Some(raw) = self.inner.env_config_overrides.get(key) {
            if let Some(Value::UnsignedInteger(value)) =
                crate::runtime::config_overlay::coerce_env_value(key, raw)
            {
                return value;
            }
        }
        match self.inner.db.store().get_config(key) {
            Some(Value::Integer(value)) => value as u64,
            Some(Value::UnsignedInteger(value)) => value,
            Some(Value::Text(value)) => value.parse::<u64>().unwrap_or(default),
            _ => default,
        }
    }

    pub(crate) fn config_f64(&self, key: &str, default: f64) -> f64 {
        if let Some(raw) = self.inner.env_config_overrides.get(key) {
            if let Ok(value) = raw.parse::<f64>() {
                return value;
            }
        }
        match self.inner.db.store().get_config(key) {
            Some(Value::Float(value)) => value,
            Some(Value::Integer(value)) => value as f64,
            Some(Value::UnsignedInteger(value)) => value as f64,
            Some(Value::Text(value)) => value.parse::<f64>().unwrap_or(default),
            _ => default,
        }
    }

    pub(crate) fn config_string(&self, key: &str, default: &str) -> String {
        if let Some(raw) = self.inner.env_config_overrides.get(key) {
            return raw.clone();
        }
        match self.inner.db.store().get_config(key) {
            Some(Value::Text(value)) => value.to_string(),
            _ => default.to_string(),
        }
    }

    /// Whether `SECRET('...')` literals should be encrypted with the
    /// vault AES key on INSERT. Default `true`.
    pub(crate) fn secret_auto_encrypt(&self) -> bool {
        self.config_bool("red.config.secret.auto_encrypt", true)
    }
}
