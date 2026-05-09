//! KV DSL command execution and KvAtomicOps module.
//!
//! Handles `KV PUT key = value [EXPIRE n] [IF NOT EXISTS]`,
//! `KV GET key`, and `KV DELETE key`.

use crate::application::ports::RuntimeEntityPort;
use crate::storage::unified::{Metadata, MetadataValue};

use super::*;

/// Default collection name for bare-key KV operations.
pub const KV_DEFAULT_COLLECTION: &str = "kv_default";

fn vault_master_key_ref(collection: &str) -> String {
    format!("red.vault.{collection}.master_key")
}

fn keyed_model_name(model: crate::catalog::CollectionModel) -> &'static str {
    match model {
        crate::catalog::CollectionModel::Kv => "kv",
        crate::catalog::CollectionModel::Vault => "vault",
        crate::catalog::CollectionModel::Config => "config",
        _ => "collection",
    }
}

/// Atomic KV operations interface — the seam that transports and drivers depend on.
///
/// All three verbs delegate to the runtime's existing `create_kv` / `get_kv` /
/// `delete_kv` plumbing; this struct adds the auto-create and upsert logic.
pub struct KvAtomicOps<'a> {
    runtime: &'a RedDBRuntime,
}

impl<'a> KvAtomicOps<'a> {
    pub fn new(runtime: &'a RedDBRuntime) -> Self {
        Self { runtime }
    }

    /// Insert or update a KV entry. Auto-creates the collection when needed.
    ///
    /// Insert or update a KV entry. Returns `(created: bool, id: EntityId)`.
    pub fn set(
        &self,
        model: crate::catalog::CollectionModel,
        collection: &str,
        key: &str,
        value: crate::storage::schema::Value,
        ttl_ms: Option<u64>,
        if_not_exists: bool,
    ) -> RedDBResult<(bool, crate::storage::EntityId)> {
        self.ensure_keyed_collection(model, collection)?;

        let existing = self.get_entry(model, collection, key)?;
        let was_present = existing.is_some();

        if if_not_exists && was_present {
            let (_, id) = existing.unwrap();
            return Ok((false, id));
        }

        // Delete old entry so we can create fresh (handles TTL refresh).
        if was_present {
            self.delete(model, collection, key)?;
        }

        let meta_vec: Vec<(String, MetadataValue)> = ttl_metadata(ttl_ms)
            .map(|m| m.fields.into_iter().collect())
            .unwrap_or_default();

        let output = self
            .runtime
            .create_kv(crate::application::entity::CreateKvInput {
                collection: collection.to_string(),
                key: key.to_string(),
                value,
                metadata: meta_vec,
            })?;

        Ok((!was_present, output.id))
    }

    /// Retrieve a KV value by key. Returns `None` when not found.
    pub fn get(
        &self,
        model: crate::catalog::CollectionModel,
        collection: &str,
        key: &str,
    ) -> RedDBResult<Option<crate::storage::schema::Value>> {
        let result = self.get_entry(model, collection, key)?;
        Ok(result.map(|(v, _)| v))
    }

    /// Delete a KV entry. Returns `true` if the key existed.
    pub fn delete(
        &self,
        model: crate::catalog::CollectionModel,
        collection: &str,
        key: &str,
    ) -> RedDBResult<bool> {
        self.ensure_declared_model(model, collection)?;
        let found = self.get_entry(model, collection, key)?;
        if let Some((_, id)) = found {
            let store = self.runtime.inner.db.store();
            let deleted = store
                .delete(collection, id)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
            if deleted {
                store.context_index().remove_entity(id);
            }
            Ok(deleted)
        } else {
            Ok(false)
        }
    }

    /// Atomically increment (or decrement) a counter key. Returns the new value.
    ///
    /// - Missing key initialises at `by` (Redis-compat).
    /// - Non-integer value returns an error before any mutation.
    pub fn incr(
        &self,
        model: crate::catalog::CollectionModel,
        collection: &str,
        key: &str,
        by: i64,
        ttl_ms: Option<u64>,
    ) -> RedDBResult<i64> {
        if model == crate::catalog::CollectionModel::Vault {
            return Err(RedDBError::InvalidOperation(
                "VAULT INCR is not supported for sealed secrets".to_string(),
            ));
        }
        self.ensure_kv_collection(collection)?;

        let current: i64 = match self.runtime.get_kv(collection, key)? {
            None => 0,
            Some((crate::storage::schema::Value::Integer(n), _)) => n,
            Some((crate::storage::schema::Value::Float(f), _)) => f as i64,
            Some((other, _)) => {
                return Err(RedDBError::Internal(format!(
                    "INCR on non-integer value: {:?}",
                    other
                )));
            }
        };

        let next = current
            .checked_add(by)
            .ok_or_else(|| RedDBError::Internal(format!("INCR overflow: {current} + {by}")))?;

        // Delete then re-create so TTL is refreshed.
        if self.runtime.get_kv(collection, key)?.is_some() {
            self.runtime.delete_kv(collection, key)?;
        }

        let meta_vec: Vec<(String, crate::storage::unified::MetadataValue)> = ttl_metadata(ttl_ms)
            .map(|m| m.fields.into_iter().collect())
            .unwrap_or_default();

        self.runtime
            .create_kv(crate::application::entity::CreateKvInput {
                collection: collection.to_string(),
                key: key.to_string(),
                value: crate::storage::schema::Value::Integer(next),
                metadata: meta_vec,
            })?;

        Ok(next)
    }

    /// Compare-and-set: atomically swap `key` from `expected` to `new_value`.
    ///
    /// Returns `(ok, current)`:
    /// - `ok = true`  → swap applied; `current` is the value *before* the swap.
    /// - `ok = false` → swap skipped; `current` holds the actual current value.
    ///
    /// `expected = None` means the caller expects the key to be *absent* (create-if-absent).
    pub fn cas(
        &self,
        model: crate::catalog::CollectionModel,
        collection: &str,
        key: &str,
        expected: Option<&crate::storage::schema::Value>,
        new_value: crate::storage::schema::Value,
        ttl_ms: Option<u64>,
    ) -> RedDBResult<(bool, Option<crate::storage::schema::Value>)> {
        if model == crate::catalog::CollectionModel::Vault {
            return Err(RedDBError::InvalidOperation(
                "VAULT CAS is not supported for sealed secrets".to_string(),
            ));
        }
        self.ensure_kv_collection(collection)?;

        let current = self.runtime.get_kv(collection, key)?.map(|(v, _)| v);

        let matches = match (&current, expected) {
            (None, None) => true,
            (Some(cur), Some(exp)) => cur == exp,
            _ => false,
        };

        if !matches {
            return Ok((false, current));
        }

        // Swap: delete old entry (if present), write new one.
        if current.is_some() {
            self.runtime.delete_kv(collection, key)?;
        }

        let meta_vec: Vec<(String, crate::storage::unified::MetadataValue)> = ttl_metadata(ttl_ms)
            .map(|m| m.fields.into_iter().collect())
            .unwrap_or_default();

        self.runtime
            .create_kv(crate::application::entity::CreateKvInput {
                collection: collection.to_string(),
                key: key.to_string(),
                value: new_value,
                metadata: meta_vec,
            })?;

        Ok((true, current))
    }

    /// Auto-create a KV collection if it does not exist yet.
    fn ensure_kv_collection(&self, collection: &str) -> RedDBResult<()> {
        self.ensure_keyed_collection(crate::catalog::CollectionModel::Kv, collection)
    }

    fn ensure_keyed_collection(
        &self,
        model: crate::catalog::CollectionModel,
        collection: &str,
    ) -> RedDBResult<()> {
        let store = self.runtime.inner.db.store();
        if store.get_collection(collection).is_some() {
            return self.ensure_declared_model(model, collection);
        }
        if model != crate::catalog::CollectionModel::Kv {
            return Err(RedDBError::NotFound(format!(
                "{} collection '{collection}' does not exist",
                keyed_model_name(model)
            )));
        }
        // Check config gate: red.config.kv.default_collection (default = true).
        let auto_create = self
            .runtime
            .config_bool("red.config.kv.default_collection", true);
        if !auto_create {
            return Err(RedDBError::NotFound(format!(
                "kv collection '{collection}' does not exist and auto-create is disabled \
                 (red.config.kv.default_collection = false)"
            )));
        }
        store
            .create_collection(collection)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.runtime
            .inner
            .db
            .save_collection_contract(kv_collection_contract(collection))
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        Ok(())
    }

    fn get_entry(
        &self,
        model: crate::catalog::CollectionModel,
        collection: &str,
        key: &str,
    ) -> RedDBResult<Option<(crate::storage::schema::Value, crate::storage::EntityId)>> {
        self.ensure_declared_model(model, collection)?;
        let store = self.runtime.inner.db.store();
        let Some(manager) = store.get_collection(collection) else {
            return Ok(None);
        };
        let entities = manager.query_all(|_| true);
        for entity in entities {
            if let crate::storage::EntityData::Row(ref row) = entity.data {
                if let Some(ref named) = row.named {
                    if let Some(crate::storage::schema::Value::Text(ref k)) = named.get("key") {
                        if &**k == key {
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

    fn ensure_declared_model(
        &self,
        model: crate::catalog::CollectionModel,
        collection: &str,
    ) -> RedDBResult<()> {
        let Some(contract) = self.runtime.inner.db.collection_contract(collection) else {
            return Ok(());
        };
        if contract.declared_model == model
            || contract.declared_model == crate::catalog::CollectionModel::Mixed
        {
            return Ok(());
        }
        Err(RedDBError::InvalidOperation(format!(
            "collection '{}' is declared as '{}' and does not allow '{}' operations",
            collection,
            keyed_model_name(contract.declared_model),
            keyed_model_name(model)
        )))
    }
}

impl RedDBRuntime {
    pub(crate) fn seal_vault_value(
        &self,
        collection: &str,
        value: crate::storage::schema::Value,
    ) -> RedDBResult<crate::storage::schema::Value> {
        let key = self.vault_encryption_key(collection)?;
        let plaintext = value.to_bytes();
        let nonce_bytes = crate::auth::store::random_bytes(12);
        let mut nonce = [0u8; 12];
        nonce.copy_from_slice(&nonce_bytes[..12]);
        let aad = format!("reddb.vault.{collection}");
        let ciphertext =
            crate::crypto::aes_gcm::aes256_gcm_encrypt(&key, &nonce, aad.as_bytes(), &plaintext);
        let mut payload = Vec::with_capacity(12 + ciphertext.len());
        payload.extend_from_slice(&nonce);
        payload.extend_from_slice(&ciphertext);
        Ok(crate::storage::schema::Value::Secret(payload))
    }

    fn vault_key_available(&self, collection: &str) -> bool {
        self.vault_encryption_key(collection).is_ok()
    }

    fn vault_encryption_key(&self, collection: &str) -> RedDBResult<[u8; 32]> {
        let auth_store = self.inner.auth_store.read().clone().ok_or_else(|| {
            RedDBError::Query("vault sealed_unavailable: no key provider is configured".to_string())
        })?;
        if !auth_store.is_vault_backed() {
            return Err(RedDBError::Query(
                "vault sealed_unavailable: key provider is sealed".to_string(),
            ));
        }

        if let Some(hex_key) = auth_store.vault_kv_get(&vault_master_key_ref(collection)) {
            return decode_vault_key(&hex_key);
        }
        auth_store.vault_secret_key().ok_or_else(|| {
            RedDBError::Query("vault sealed_unavailable: cluster vault key is missing".to_string())
        })
    }

    /// Dispatch a `KV PUT / GET / DELETE` command.
    pub fn execute_kv_command(
        &self,
        raw_query: &str,
        cmd: &crate::storage::query::ast::KvCommand,
    ) -> RedDBResult<RuntimeQueryResult> {
        use crate::storage::query::ast::KvCommand;

        let ops = KvAtomicOps::new(self);

        match cmd {
            KvCommand::Put {
                model,
                collection,
                key,
                value,
                ttl_ms,
                if_not_exists,
            } => {
                self.check_write(crate::runtime::write_gate::WriteKind::Dml)?;
                let (created, id) = ops.set(
                    *model,
                    collection,
                    key,
                    value.clone(),
                    *ttl_ms,
                    *if_not_exists,
                )?;

                let mut result = UnifiedResult::with_columns(vec![
                    "ok".into(),
                    "collection".into(),
                    "key".into(),
                    "id".into(),
                    "created".into(),
                ]);
                let mut record = UnifiedRecord::new();
                record.set("ok", Value::Boolean(true));
                record.set("collection", Value::text(collection.clone()));
                record.set("key", Value::text(key.clone()));
                record.set("id", Value::Integer(id.raw() as i64));
                record.set("created", Value::Boolean(created));
                result.push(record);

                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: crate::storage::query::modes::QueryMode::Sql,
                    statement: if *model == crate::catalog::CollectionModel::Vault {
                        "vault_put"
                    } else {
                        "kv_put"
                    },
                    engine: if *model == crate::catalog::CollectionModel::Vault {
                        "vault"
                    } else {
                        "kv"
                    },
                    result,
                    affected_rows: 1,
                    statement_type: if created { "insert" } else { "update" },
                })
            }

            KvCommand::Get {
                model,
                collection,
                key,
            } => {
                if *model == crate::catalog::CollectionModel::Vault {
                    let present = ops.get(*model, collection, key)?.is_some();
                    let status = if !self.vault_key_available(collection) {
                        "sealed_unavailable"
                    } else if present {
                        "sealed"
                    } else {
                        "missing"
                    };
                    let mut result = UnifiedResult::with_columns(vec![
                        "collection".into(),
                        "key".into(),
                        "value".into(),
                        "status".into(),
                    ]);
                    let mut record = UnifiedRecord::new();
                    record.set("collection", Value::text(collection.clone()));
                    record.set("key", Value::text(key.clone()));
                    record.set("value", Value::text(if present { "***" } else { "" }));
                    record.set("status", Value::text(status));
                    result.push(record);
                    return Ok(RuntimeQueryResult {
                        query: raw_query.to_string(),
                        mode: crate::storage::query::modes::QueryMode::Sql,
                        statement: "vault_get",
                        engine: "vault",
                        result,
                        affected_rows: 0,
                        statement_type: "select",
                    });
                }

                let value = ops.get(*model, collection, key)?;

                let mut result = UnifiedResult::with_columns(vec![
                    "collection".into(),
                    "key".into(),
                    "value".into(),
                ]);
                let mut record = UnifiedRecord::new();
                record.set("collection", Value::text(collection.clone()));
                record.set("key", Value::text(key.clone()));
                record.set(
                    "value",
                    value.unwrap_or(crate::storage::schema::Value::Null),
                );
                result.push(record);

                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: crate::storage::query::modes::QueryMode::Sql,
                    statement: "kv_get",
                    engine: "kv",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }

            KvCommand::Incr {
                model,
                collection,
                key,
                by,
                ttl_ms,
            } => {
                self.check_write(crate::runtime::write_gate::WriteKind::Dml)?;
                let new_value = ops.incr(*model, collection, key, *by, *ttl_ms)?;

                let mut result = UnifiedResult::with_columns(vec![
                    "ok".into(),
                    "collection".into(),
                    "key".into(),
                    "value".into(),
                ]);
                let mut record = UnifiedRecord::new();
                record.set("ok", Value::Boolean(true));
                record.set("collection", Value::text(collection.clone()));
                record.set("key", Value::text(key.clone()));
                record.set("value", Value::Integer(new_value));
                result.push(record);

                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: crate::storage::query::modes::QueryMode::Sql,
                    statement: "kv_incr",
                    engine: "kv",
                    result,
                    affected_rows: 1,
                    statement_type: "update",
                })
            }

            KvCommand::Cas {
                model,
                collection,
                key,
                expected,
                new_value,
                ttl_ms,
            } => {
                self.check_write(crate::runtime::write_gate::WriteKind::Dml)?;
                let (ok, current) = ops.cas(
                    *model,
                    collection,
                    key,
                    expected.as_ref(),
                    new_value.clone(),
                    *ttl_ms,
                )?;

                let mut result = UnifiedResult::with_columns(vec![
                    "ok".into(),
                    "collection".into(),
                    "key".into(),
                    "current".into(),
                ]);
                let mut record = UnifiedRecord::new();
                record.set("ok", Value::Boolean(ok));
                record.set("collection", Value::text(collection.clone()));
                record.set("key", Value::text(key.clone()));
                record.set(
                    "current",
                    current.unwrap_or(crate::storage::schema::Value::Null),
                );
                result.push(record);

                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: crate::storage::query::modes::QueryMode::Sql,
                    statement: "kv_cas",
                    engine: "kv",
                    result,
                    affected_rows: if ok { 1 } else { 0 },
                    statement_type: "update",
                })
            }

            KvCommand::Delete {
                model,
                collection,
                key,
            } => {
                if *model == crate::catalog::CollectionModel::Vault {
                    return Err(RedDBError::InvalidOperation(
                        "VAULT DELETE is not supported before rotate/history/purge lands"
                            .to_string(),
                    ));
                }
                self.check_write(crate::runtime::write_gate::WriteKind::Dml)?;
                let deleted = ops.delete(*model, collection, key)?;

                let mut result = UnifiedResult::with_columns(vec![
                    "ok".into(),
                    "collection".into(),
                    "key".into(),
                    "deleted".into(),
                ]);
                let mut record = UnifiedRecord::new();
                record.set("ok", Value::Boolean(true));
                record.set("collection", Value::text(collection.clone()));
                record.set("key", Value::text(key.clone()));
                record.set("deleted", Value::Boolean(deleted));
                result.push(record);

                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: crate::storage::query::modes::QueryMode::Sql,
                    statement: "kv_delete",
                    engine: "kv",
                    result,
                    affected_rows: if deleted { 1 } else { 0 },
                    statement_type: "delete",
                })
            }
        }
    }
}

fn ttl_metadata(ttl_ms: Option<u64>) -> Option<Metadata> {
    let ttl_ms = ttl_ms?;
    Some(Metadata::with_fields(
        [(
            "_ttl_ms".to_string(),
            if ttl_ms <= i64::MAX as u64 {
                MetadataValue::Int(ttl_ms as i64)
            } else {
                MetadataValue::Timestamp(ttl_ms)
            },
        )]
        .into_iter()
        .collect(),
    ))
}

fn decode_vault_key(hex_key: &str) -> RedDBResult<[u8; 32]> {
    let bytes = hex::decode(hex_key)
        .map_err(|_| RedDBError::Query("vault sealed_unavailable: bad key material".to_string()))?;
    let key: [u8; 32] = bytes.try_into().map_err(|_| {
        RedDBError::Query("vault sealed_unavailable: bad key material length".to_string())
    })?;
    Ok(key)
}

fn kv_collection_contract(name: &str) -> crate::physical::CollectionContract {
    let now = current_unix_ms();
    crate::physical::CollectionContract {
        name: name.to_string(),
        declared_model: crate::catalog::CollectionModel::Kv,
        schema_mode: crate::catalog::SchemaMode::Dynamic,
        origin: crate::physical::ContractOrigin::Implicit,
        version: 1,
        created_at_unix_ms: now,
        updated_at_unix_ms: now,
        default_ttl_ms: None,
        context_index_fields: Vec::new(),
        declared_columns: Vec::new(),
        table_def: None,
        timestamps_enabled: false,
        context_index_enabled: false,
        append_only: false,
        subscriptions: Vec::new(),
    }
}

fn current_unix_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

#[cfg(test)]
mod tests {
    use crate::api::RedDBOptions;
    use crate::catalog::CollectionModel;
    use crate::runtime::RedDBRuntime;

    fn rt() -> RedDBRuntime {
        RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("in-memory runtime")
    }

    #[test]
    fn incr_missing_key_initialises_at_by() {
        let r = rt();
        let ops = super::KvAtomicOps::new(&r);
        let v = ops
            .incr(CollectionModel::Kv, "kv_default", "missing", 5, None)
            .unwrap();
        assert_eq!(v, 5);
    }

    #[test]
    fn incr_existing_integer_accumulates() {
        let r = rt();
        let ops = super::KvAtomicOps::new(&r);
        ops.incr(CollectionModel::Kv, "kv_default", "ctr", 1, None)
            .unwrap();
        ops.incr(CollectionModel::Kv, "kv_default", "ctr", 1, None)
            .unwrap();
        let v = ops
            .incr(CollectionModel::Kv, "kv_default", "ctr", 1, None)
            .unwrap();
        assert_eq!(v, 3);
    }

    #[test]
    fn decr_via_negative_by() {
        let r = rt();
        let ops = super::KvAtomicOps::new(&r);
        ops.incr(CollectionModel::Kv, "kv_default", "stock", 10, None)
            .unwrap();
        let v = ops
            .incr(CollectionModel::Kv, "kv_default", "stock", -3, None)
            .unwrap();
        assert_eq!(v, 7);
    }

    #[test]
    fn incr_on_string_value_returns_error() {
        let r = rt();
        let ops = super::KvAtomicOps::new(&r);
        ops.set(
            CollectionModel::Kv,
            "kv_default",
            "name",
            crate::storage::schema::Value::text("alice"),
            None,
            false,
        )
        .unwrap();
        let err = ops
            .incr(CollectionModel::Kv, "kv_default", "name", 1, None)
            .unwrap_err();
        assert!(err.to_string().contains("non-integer"));
    }

    // --- CAS tests ---

    #[test]
    fn cas_matching_value_succeeds() {
        let r = rt();
        let ops = super::KvAtomicOps::new(&r);
        ops.set(
            CollectionModel::Kv,
            "kv_default",
            "lock",
            crate::storage::schema::Value::text("free"),
            None,
            false,
        )
        .unwrap();
        let (ok, prev) = ops
            .cas(
                CollectionModel::Kv,
                "kv_default",
                "lock",
                Some(&crate::storage::schema::Value::text("free")),
                crate::storage::schema::Value::text("held"),
                None,
            )
            .unwrap();
        assert!(ok);
        assert_eq!(prev, Some(crate::storage::schema::Value::text("free")));
        // Value actually changed.
        assert_eq!(
            ops.get(CollectionModel::Kv, "kv_default", "lock").unwrap(),
            Some(crate::storage::schema::Value::text("held"))
        );
    }

    #[test]
    fn cas_mismatching_value_fails() {
        let r = rt();
        let ops = super::KvAtomicOps::new(&r);
        ops.set(
            CollectionModel::Kv,
            "kv_default",
            "lock",
            crate::storage::schema::Value::text("free"),
            None,
            false,
        )
        .unwrap();
        let (ok, current) = ops
            .cas(
                CollectionModel::Kv,
                "kv_default",
                "lock",
                Some(&crate::storage::schema::Value::text("held")),
                crate::storage::schema::Value::text("worker-7"),
                None,
            )
            .unwrap();
        assert!(!ok);
        assert_eq!(current, Some(crate::storage::schema::Value::text("free")));
        // Value unchanged.
        assert_eq!(
            ops.get(CollectionModel::Kv, "kv_default", "lock").unwrap(),
            Some(crate::storage::schema::Value::text("free"))
        );
    }

    #[test]
    fn cas_expect_null_on_missing_key_creates() {
        let r = rt();
        let ops = super::KvAtomicOps::new(&r);
        let (ok, prev) = ops
            .cas(
                CollectionModel::Kv,
                "kv_default",
                "new_key",
                None,
                crate::storage::schema::Value::text("created"),
                None,
            )
            .unwrap();
        assert!(ok);
        assert_eq!(prev, None);
        assert_eq!(
            ops.get(CollectionModel::Kv, "kv_default", "new_key")
                .unwrap(),
            Some(crate::storage::schema::Value::text("created"))
        );
    }

    #[test]
    fn cas_expect_null_on_existing_key_fails() {
        let r = rt();
        let ops = super::KvAtomicOps::new(&r);
        ops.set(
            CollectionModel::Kv,
            "kv_default",
            "taken",
            crate::storage::schema::Value::text("worker-1"),
            None,
            false,
        )
        .unwrap();
        let (ok, current) = ops
            .cas(
                CollectionModel::Kv,
                "kv_default",
                "taken",
                None,
                crate::storage::schema::Value::text("worker-2"),
                None,
            )
            .unwrap();
        assert!(!ok);
        assert_eq!(
            current,
            Some(crate::storage::schema::Value::text("worker-1"))
        );
    }

    #[test]
    fn cas_via_sql_roundtrip() {
        let r = rt();
        // Seed value.
        r.execute_query("KV PUT lock = 'free'").unwrap();
        // CAS: free → held — should succeed.
        let res = r
            .execute_query("KV CAS lock EXPECT 'free' SET 'held'")
            .unwrap();
        let row = &res.result.records[0];
        assert_eq!(
            row.get("ok"),
            Some(&crate::storage::schema::Value::Boolean(true))
        );
        // CAS: free → held again — should fail (value is now 'held').
        let res2 = r
            .execute_query("KV CAS lock EXPECT 'free' SET 'held'")
            .unwrap();
        let row2 = &res2.result.records[0];
        assert_eq!(
            row2.get("ok"),
            Some(&crate::storage::schema::Value::Boolean(false))
        );
    }

    #[test]
    fn cas_expect_null_via_sql() {
        let r = rt();
        let res = r
            .execute_query("KV CAS singleton EXPECT NULL SET 'first'")
            .unwrap();
        let row = &res.result.records[0];
        assert_eq!(
            row.get("ok"),
            Some(&crate::storage::schema::Value::Boolean(true))
        );
        // Second call must fail.
        let res2 = r
            .execute_query("KV CAS singleton EXPECT NULL SET 'second'")
            .unwrap();
        let row2 = &res2.result.records[0];
        assert_eq!(
            row2.get("ok"),
            Some(&crate::storage::schema::Value::Boolean(false))
        );
    }

    #[test]
    fn incr_via_sql_roundtrip() {
        let r = rt();
        let res = r.execute_query("KV INCR hits").unwrap();
        let row = &res.result.records[0];
        assert_eq!(
            row.get("value"),
            Some(&crate::storage::schema::Value::Integer(1))
        );
        let res2 = r.execute_query("KV INCR hits BY 4").unwrap();
        let row2 = &res2.result.records[0];
        assert_eq!(
            row2.get("value"),
            Some(&crate::storage::schema::Value::Integer(5))
        );
    }
}
