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
//!   `config_string`, `secret_auto_encrypt`, `secret_auto_decrypt`,
//!   `apply_secret_decryption`.
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
    !key.starts_with("red.secret.") && !key.starts_with("red.config.")
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
            "red.secret.ai.anthropic.default.api_key"
        ));
        assert!(!show_secrets_allows_key("red.config.ai.default.provider"));
        assert!(show_secrets_allows_key("acme.key"));
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
    mode: crate::storage::query::modes::QueryMode,
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
    }
}

impl RedDBRuntime {
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
            if let Some(crate::storage::schema::Value::Boolean(b)) =
                crate::runtime::config_overlay::coerce_env_value(key, raw)
            {
                return b;
            }
        }
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection("red_config") else {
            return default;
        };
        let mut result = default;
        let mut latest_id: u64 = 0;
        manager.for_each_entity(|entity| {
            if let Some(row) = entity.data.as_row() {
                let entry_key = row.get_field("key").and_then(|v| match v {
                    crate::storage::schema::Value::Text(s) => Some(s.as_ref()),
                    _ => None,
                });
                if entry_key == Some(key) {
                    let id = entity.id.raw();
                    if id >= latest_id {
                        latest_id = id;
                        result = match row.get_field("value") {
                            Some(crate::storage::schema::Value::Boolean(b)) => *b,
                            Some(crate::storage::schema::Value::Text(s)) => {
                                matches!(s.as_ref(), "true" | "TRUE" | "True" | "1")
                            }
                            Some(crate::storage::schema::Value::Integer(n)) => *n != 0,
                            _ => default,
                        };
                    }
                }
            }
            true
        });
        result
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
            if let Some(crate::storage::schema::Value::UnsignedInteger(n)) =
                crate::runtime::config_overlay::coerce_env_value(key, raw)
            {
                return n;
            }
        }
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection("red_config") else {
            return default;
        };
        let mut result = default;
        let mut latest_id: u64 = 0;
        manager.for_each_entity(|entity| {
            if let Some(row) = entity.data.as_row() {
                let entry_key = row.get_field("key").and_then(|v| match v {
                    crate::storage::schema::Value::Text(s) => Some(s.as_ref()),
                    _ => None,
                });
                if entry_key == Some(key) {
                    let id = entity.id.raw();
                    if id >= latest_id {
                        latest_id = id;
                        result = match row.get_field("value") {
                            Some(crate::storage::schema::Value::Integer(n)) => *n as u64,
                            Some(crate::storage::schema::Value::UnsignedInteger(n)) => *n,
                            Some(crate::storage::schema::Value::Text(s)) => {
                                s.parse::<u64>().unwrap_or(default)
                            }
                            _ => default,
                        };
                    }
                }
            }
            true
        });
        result
    }

    pub(crate) fn config_f64(&self, key: &str, default: f64) -> f64 {
        if let Some(raw) = self.inner.env_config_overrides.get(key) {
            if let Ok(n) = raw.parse::<f64>() {
                return n;
            }
        }
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection("red_config") else {
            return default;
        };
        let mut result = default;
        let mut latest_id: u64 = 0;
        manager.for_each_entity(|entity| {
            if let Some(row) = entity.data.as_row() {
                let entry_key = row.get_field("key").and_then(|v| match v {
                    crate::storage::schema::Value::Text(s) => Some(s.as_ref()),
                    _ => None,
                });
                if entry_key == Some(key) {
                    let id = entity.id.raw();
                    if id >= latest_id {
                        latest_id = id;
                        result = match row.get_field("value") {
                            Some(crate::storage::schema::Value::Float(n)) => *n,
                            Some(crate::storage::schema::Value::Integer(n)) => *n as f64,
                            Some(crate::storage::schema::Value::UnsignedInteger(n)) => *n as f64,
                            Some(crate::storage::schema::Value::Text(s)) => {
                                s.parse::<f64>().unwrap_or(default)
                            }
                            _ => default,
                        };
                    }
                }
            }
            true
        });
        result
    }

    pub(crate) fn config_string(&self, key: &str, default: &str) -> String {
        if let Some(raw) = self.inner.env_config_overrides.get(key) {
            return raw.clone();
        }
        let store = self.inner.db.store();
        let Some(manager) = store.get_collection("red_config") else {
            return default.to_string();
        };
        let mut result = default.to_string();
        let mut latest_id: u64 = 0;
        manager.for_each_entity(|entity| {
            if let Some(row) = entity.data.as_row() {
                let entry_key = row.get_field("key").and_then(|v| match v {
                    crate::storage::schema::Value::Text(s) => Some(s.as_ref()),
                    _ => None,
                });
                if entry_key == Some(key) {
                    let id = entity.id.raw();
                    if id >= latest_id {
                        latest_id = id;
                        if let Some(crate::storage::schema::Value::Text(value)) =
                            row.get_field("value")
                        {
                            result = value.to_string();
                        }
                    }
                }
            }
            true
        });
        result
    }

    /// Whether `SECRET('...')` literals should be encrypted with the
    /// vault AES key on INSERT. Default `true`.
    pub(crate) fn secret_auto_encrypt(&self) -> bool {
        self.config_bool("red.config.secret.auto_encrypt", true)
    }

    /// Whether `Value::Secret` columns should be decrypted back to
    /// plaintext on SELECT when the vault is unsealed. Default `true`.
    /// Turning this off keeps secrets masked as `***` even while the
    /// vault is open — useful for audit trails or read-only exports.
    pub(crate) fn secret_auto_decrypt(&self) -> bool {
        self.config_bool("red.config.secret.auto_decrypt", true)
    }

    /// Walk every record in `result` and swap `Value::Secret(bytes)`
    /// for the decrypted plaintext when the runtime has the vault
    /// AES key AND `red.config.secret.auto_decrypt = true`. If the
    /// key is missing, the vault is sealed, or auto_decrypt is off,
    /// secrets are left as `Value::Secret` which every formatter
    /// (Display, JSON) already masks as `***`.
    pub(crate) fn apply_secret_decryption(&self, result: &mut RuntimeQueryResult) {
        if !self.secret_auto_decrypt() {
            return;
        }
        let Some(key) = self.secret_aes_key() else {
            return;
        };
        for record in result.result.records.iter_mut() {
            for value in record.values_mut() {
                if let Value::Secret(ref bytes) = value {
                    if let Some(plain) =
                        super::impl_dml_crypto::decrypt_secret_payload(&key, bytes.as_slice())
                    {
                        if let Ok(text) = String::from_utf8(plain) {
                            *value = Value::text(text);
                        }
                    }
                }
            }
        }
    }
}
