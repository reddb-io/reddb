//! KV DSL command execution and KvAtomicOps module.
//!
//! Handles `KV PUT key = value [EXPIRE n] [TAGS [...]] [IF NOT EXISTS]`,
//! `KV GET key`, and `KV DELETE key`.

use crate::application::ports::RuntimeEntityPort;
use crate::storage::unified::{Metadata, MetadataValue};

use super::impl_core::{current_auth_identity, current_connection_id, current_tenant};
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

#[derive(Debug, Clone)]
struct VaultEntry {
    id: crate::storage::EntityId,
    key: String,
    value: crate::storage::schema::Value,
    metadata: Metadata,
    created_at: u64,
    updated_at: u64,
    sequence_id: u64,
    version: i64,
    tombstone: bool,
    op: String,
}

impl super::keyed_spine::KeyedVersion for VaultEntry {
    fn key(&self) -> &str {
        &self.key
    }

    fn version(&self) -> i64 {
        self.version
    }
}

impl VaultEntry {
    fn from_keyed_row(
        version: super::keyed_spine::KeyedRowVersion,
        metadata: Metadata,
        created_at: u64,
        updated_at: u64,
        sequence_id: u64,
    ) -> Self {
        Self {
            id: version.id,
            key: version.key,
            value: version.value,
            metadata,
            created_at,
            updated_at,
            sequence_id,
            version: version.version,
            tombstone: version.tombstone,
            op: version.op,
        }
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
        self.set_with_tags_for_model(model, collection, key, value, ttl_ms, &[], if_not_exists)
    }

    pub fn set_with_tags(
        &self,
        collection: &str,
        key: &str,
        value: crate::storage::schema::Value,
        ttl_ms: Option<u64>,
        tags: &[String],
        if_not_exists: bool,
    ) -> RedDBResult<(bool, crate::storage::EntityId)> {
        self.set_with_tags_for_model(
            crate::catalog::CollectionModel::Kv,
            collection,
            key,
            value,
            ttl_ms,
            tags,
            if_not_exists,
        )
    }

    fn set_with_tags_for_model(
        &self,
        model: crate::catalog::CollectionModel,
        collection: &str,
        key: &str,
        value: crate::storage::schema::Value,
        ttl_ms: Option<u64>,
        tags: &[String],
        if_not_exists: bool,
    ) -> RedDBResult<(bool, crate::storage::EntityId)> {
        self.set_with_tags_and_metadata_for_model(
            model,
            collection,
            key,
            value,
            ttl_ms,
            tags,
            if_not_exists,
            Vec::new(),
        )
    }

    pub fn set_with_tags_and_metadata(
        &self,
        collection: &str,
        key: &str,
        value: crate::storage::schema::Value,
        ttl_ms: Option<u64>,
        tags: &[String],
        if_not_exists: bool,
        metadata: Vec<(String, MetadataValue)>,
    ) -> RedDBResult<(bool, crate::storage::EntityId)> {
        self.set_with_tags_and_metadata_for_model(
            crate::catalog::CollectionModel::Kv,
            collection,
            key,
            value,
            ttl_ms,
            tags,
            if_not_exists,
            metadata,
        )
    }

    fn set_with_tags_and_metadata_for_model(
        &self,
        model: crate::catalog::CollectionModel,
        collection: &str,
        key: &str,
        value: crate::storage::schema::Value,
        ttl_ms: Option<u64>,
        tags: &[String],
        if_not_exists: bool,
        mut metadata: Vec<(String, MetadataValue)>,
    ) -> RedDBResult<(bool, crate::storage::EntityId)> {
        self.ensure_keyed_collection(model, collection)?;

        if model == crate::catalog::CollectionModel::Vault {
            let latest = self.get_vault_entry(collection, key)?;
            let was_present = latest.as_ref().is_some_and(|entry| !entry.tombstone);
            if if_not_exists && was_present {
                return Ok((false, latest.expect("checked present").id));
            }
            let entry = self.append_vault_version(collection, key, value, "put", false, tags)?;
            self.runtime.record_kv_watch_event(
                if latest.is_some() {
                    crate::replication::cdc::ChangeOperation::Update
                } else {
                    crate::replication::cdc::ChangeOperation::Insert
                },
                collection,
                key,
                entry.id.raw(),
                latest.as_ref().map(vault_entry_metadata_json),
                Some(vault_entry_metadata_json(&entry)),
            );
            return Ok((!was_present, entry.id));
        }

        let existing = self.get_entry(model, collection, key)?;
        let was_present = existing.is_some();

        if if_not_exists && was_present {
            let (_, id) = existing.unwrap();
            self.runtime.inner.kv_stats.incr_puts();
            return Ok((false, id));
        }

        let before = existing
            .as_ref()
            .map(|(value, _)| crate::presentation::entity_json::storage_value_to_json(value));
        let op = if was_present {
            crate::replication::cdc::ChangeOperation::Update
        } else {
            crate::replication::cdc::ChangeOperation::Insert
        };
        let after = Some(crate::presentation::entity_json::storage_value_to_json(
            &value,
        ));

        // Delete old entry so we can create fresh (handles TTL refresh).
        if was_present {
            self.delete(model, collection, key)?;
        }

        if let Some(ttl_metadata) = ttl_metadata(ttl_ms) {
            metadata.extend(ttl_metadata.fields);
        }
        if let Some(tags_metadata) = kv_tags_metadata(tags) {
            metadata.push(tags_metadata);
        }

        let output = self
            .runtime
            .create_kv(crate::application::entity::CreateKvInput {
                collection: collection.to_string(),
                key: key.to_string(),
                value,
                metadata,
            })?;
        if model == crate::catalog::CollectionModel::Kv {
            self.runtime
                .inner
                .kv_tag_index
                .replace(collection, key, output.id, tags);
        }

        if model == crate::catalog::CollectionModel::Kv {
            self.runtime
                .record_kv_watch_event(op, collection, key, output.id.raw(), before, after);
        }

        if model == crate::catalog::CollectionModel::Kv {
            self.runtime.inner.kv_stats.incr_puts();
        }
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
        if model == crate::catalog::CollectionModel::Kv {
            self.runtime.inner.kv_stats.incr_gets();
        }
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
        if let Some((value, id)) = found {
            let store = self.runtime.inner.db.store();
            let deleted = store
                .delete(collection, id)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
            if deleted {
                store.context_index().remove_entity(id);
                if model == crate::catalog::CollectionModel::Kv {
                    self.runtime.inner.kv_tag_index.remove(collection, key);
                    self.runtime.record_kv_watch_event(
                        crate::replication::cdc::ChangeOperation::Delete,
                        collection,
                        key,
                        id.raw(),
                        Some(crate::presentation::entity_json::storage_value_to_json(
                            &value,
                        )),
                        None,
                    );
                    self.runtime.inner.kv_stats.incr_deletes();
                }
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
        let rmw_lock = self.runtime.inner.rmw_locks.lock_for(collection, key);
        let _rmw_guard = rmw_lock.lock();
        self.ensure_kv_collection(collection)?;
        let existing = self.runtime.get_kv(collection, key)?;
        let current: i64 = match existing.as_ref() {
            None => 0,
            Some((crate::storage::schema::Value::Integer(n), _)) => *n,
            Some((crate::storage::schema::Value::Float(f), _)) => *f as i64,
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
        if existing.is_some() {
            self.runtime.delete_kv(collection, key)?;
        }

        let meta_vec: Vec<(String, crate::storage::unified::MetadataValue)> = ttl_metadata(ttl_ms)
            .map(|m| m.fields.into_iter().collect())
            .unwrap_or_default();

        let output = self
            .runtime
            .create_kv(crate::application::entity::CreateKvInput {
                collection: collection.to_string(),
                key: key.to_string(),
                value: crate::storage::schema::Value::Integer(next),
                metadata: meta_vec,
            })?;
        self.runtime
            .inner
            .kv_tag_index
            .replace(collection, key, output.id, &[]);

        self.runtime.record_kv_watch_event(
            if existing.is_some() {
                crate::replication::cdc::ChangeOperation::Update
            } else {
                crate::replication::cdc::ChangeOperation::Insert
            },
            collection,
            key,
            output.id.raw(),
            existing
                .as_ref()
                .map(|(value, _)| crate::presentation::entity_json::storage_value_to_json(value)),
            Some(crate::presentation::entity_json::storage_value_to_json(
                &crate::storage::schema::Value::Integer(next),
            )),
        );

        self.runtime.inner.kv_stats.incr_incrs();
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
        let rmw_lock = self.runtime.inner.rmw_locks.lock_for(collection, key);
        let _rmw_guard = rmw_lock.lock();
        self.ensure_kv_collection(collection)?;
        let current = self.runtime.get_kv(collection, key)?.map(|(v, _)| v);

        let matches = match (&current, expected) {
            (None, None) => true,
            (Some(cur), Some(exp)) => cur == exp,
            _ => false,
        };

        if !matches {
            self.runtime.inner.kv_stats.incr_cas_conflict();
            return Ok((false, current));
        }

        // Swap: delete old entry (if present), write new one.
        if current.is_some() {
            self.runtime.delete_kv(collection, key)?;
        }

        let meta_vec: Vec<(String, crate::storage::unified::MetadataValue)> = ttl_metadata(ttl_ms)
            .map(|m| m.fields.into_iter().collect())
            .unwrap_or_default();

        let output = self
            .runtime
            .create_kv(crate::application::entity::CreateKvInput {
                collection: collection.to_string(),
                key: key.to_string(),
                value: new_value.clone(),
                metadata: meta_vec,
            })?;
        self.runtime
            .inner
            .kv_tag_index
            .replace(collection, key, output.id, &[]);

        self.runtime.record_kv_watch_event(
            if current.is_some() {
                crate::replication::cdc::ChangeOperation::Update
            } else {
                crate::replication::cdc::ChangeOperation::Insert
            },
            collection,
            key,
            output.id.raw(),
            current
                .as_ref()
                .map(crate::presentation::entity_json::storage_value_to_json),
            Some(crate::presentation::entity_json::storage_value_to_json(
                &new_value,
            )),
        );

        self.runtime.inner.kv_stats.incr_cas_success();
        Ok((true, current))
    }

    pub fn invalidate_tags(&self, collection: &str, tags: &[String]) -> RedDBResult<usize> {
        self.runtime
            .check_write(crate::runtime::write_gate::WriteKind::Dml)?;
        self.runtime.check_kv_invalidate_policy(collection)?;
        self.ensure_kv_collection(collection)?;
        let entries = self
            .runtime
            .inner
            .kv_tag_index
            .entries_for_tags(collection, tags);
        if entries.is_empty() {
            return Ok(0);
        }

        let store = self.runtime.inner.db.store();
        let mut removed = 0usize;
        for (key, id) in entries {
            let before = store
                .get(collection, id)
                .and_then(|entity| kv_value_from_entity(&entity));
            let deleted = store
                .delete(collection, id)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
            if deleted {
                store.context_index().remove_entity(id);
                self.runtime.inner.kv_tag_index.remove(collection, &key);
                self.runtime.record_kv_watch_event(
                    crate::replication::cdc::ChangeOperation::Delete,
                    collection,
                    &key,
                    id.raw(),
                    before
                        .as_ref()
                        .map(crate::presentation::entity_json::storage_value_to_json),
                    None,
                );
                removed += 1;
            }
        }
        if removed > 0 {
            self.runtime.inner.kv_stats.incr_deletes();
        }
        Ok(removed)
    }

    pub fn tags_for_key(&self, collection: &str, key: &str) -> Vec<String> {
        self.runtime
            .inner
            .kv_tag_index
            .tags_for_key(collection, key)
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

    fn get_vault_entry(&self, collection: &str, key: &str) -> RedDBResult<Option<VaultEntry>> {
        self.vault_versions(collection, key)
            .map(super::keyed_spine::latest_version)
    }

    fn get_vault_entry_version(
        &self,
        collection: &str,
        key: &str,
        version: i64,
    ) -> RedDBResult<Option<VaultEntry>> {
        Ok(self
            .vault_versions(collection, key)?
            .into_iter()
            .find(|entry| entry.version == version))
    }

    fn vault_versions(&self, collection: &str, key: &str) -> RedDBResult<Vec<VaultEntry>> {
        self.ensure_declared_model(crate::catalog::CollectionModel::Vault, collection)?;
        let store = self.runtime.inner.db.store();
        let Some(manager) = store.get_collection(collection) else {
            return Ok(Vec::new());
        };
        let entities = manager.query_all(|_| true);
        let mut versions = Vec::new();
        for entity in entities {
            let crate::storage::EntityData::Row(ref row) = entity.data else {
                continue;
            };
            let Some(version) =
                super::keyed_spine::row_version(entity.id, row, entity.sequence_id as i64)
            else {
                continue;
            };
            if version.key != key {
                continue;
            }
            let metadata = manager.get_metadata(entity.id).unwrap_or_default();
            versions.push(VaultEntry::from_keyed_row(
                version,
                metadata,
                entity.created_at,
                entity.updated_at,
                entity.sequence_id,
            ));
        }
        Ok(versions)
    }

    fn latest_vault_entries(
        &self,
        collection: &str,
        prefix: Option<&str>,
    ) -> RedDBResult<Vec<VaultEntry>> {
        self.ensure_declared_model(crate::catalog::CollectionModel::Vault, collection)?;
        let store = self.runtime.inner.db.store();
        let Some(manager) = store.get_collection(collection) else {
            return Ok(Vec::new());
        };
        let mut versions = Vec::new();
        for entity in manager.query_all(|_| true) {
            let crate::storage::EntityData::Row(ref row) = entity.data else {
                continue;
            };
            let Some(version) =
                super::keyed_spine::row_version(entity.id, row, entity.sequence_id as i64)
            else {
                continue;
            };
            let metadata = manager.get_metadata(entity.id).unwrap_or_default();
            let entry = VaultEntry::from_keyed_row(
                version,
                metadata,
                entity.created_at,
                entity.updated_at,
                entity.sequence_id,
            );
            versions.push(entry);
        }
        Ok(super::keyed_spine::latest_versions(versions, prefix))
    }

    fn append_vault_version(
        &self,
        collection: &str,
        key: &str,
        value: crate::storage::schema::Value,
        op: &str,
        tombstone: bool,
        tags: &[String],
    ) -> RedDBResult<VaultEntry> {
        self.ensure_declared_model(crate::catalog::CollectionModel::Vault, collection)?;
        let version = self
            .get_vault_entry(collection, key)?
            .map(|entry| entry.version)
            .unwrap_or(0)
            + 1;
        let stored_value = if tombstone {
            crate::storage::schema::Value::Null
        } else {
            self.runtime.seal_vault_value(collection, value)?
        };
        let now = current_unix_ms() as i64;
        let fields = vec![
            (
                "key".to_string(),
                crate::storage::schema::Value::text(key.to_string()),
            ),
            ("value".to_string(), stored_value),
            (
                "version".to_string(),
                crate::storage::schema::Value::Integer(version),
            ),
            (
                "tombstone".to_string(),
                crate::storage::schema::Value::Boolean(tombstone),
            ),
            (
                "op".to_string(),
                crate::storage::schema::Value::text(op.to_string()),
            ),
            (
                "created_at_ms".to_string(),
                crate::storage::schema::Value::Integer(now),
            ),
        ];
        let mut row = crate::storage::RowData::new(Vec::new());
        row.named = Some(fields.into_iter().collect());
        let entity = crate::storage::UnifiedEntity::new(
            crate::storage::EntityId::new(0),
            crate::storage::EntityKind::TableRow {
                table: std::sync::Arc::from(collection),
                row_id: 0,
            },
            crate::storage::EntityData::Row(row),
        );
        let id = self
            .runtime
            .inner
            .db
            .store()
            .insert(collection, entity)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        if !tags.is_empty() {
            self.runtime
                .inner
                .db
                .store()
                .set_metadata(
                    collection,
                    id,
                    Metadata::with_fields(vault_tags_metadata(tags)),
                )
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
            self.runtime
                .inner
                .kv_tag_index
                .replace(collection, key, id, tags);
        }
        self.get_vault_entry_version(collection, key, version)?
            .ok_or_else(|| RedDBError::Internal(format!("vault version {id} was not readable")))
    }

    fn purge_vault_versions(&self, collection: &str, key: &str) -> RedDBResult<usize> {
        self.ensure_declared_model(crate::catalog::CollectionModel::Vault, collection)?;
        let versions = self.vault_versions(collection, key)?;
        let store = self.runtime.inner.db.store();
        let mut purged = 0usize;
        for entry in versions {
            if store
                .delete(collection, entry.id)
                .map_err(|err| RedDBError::Internal(err.to_string()))?
            {
                store.context_index().remove_entity(entry.id);
                purged += 1;
            }
        }
        Ok(purged)
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

    fn unseal_vault_value(
        &self,
        collection: &str,
        sealed: &crate::storage::schema::Value,
    ) -> RedDBResult<crate::storage::schema::Value> {
        let crate::storage::schema::Value::Secret(payload) = sealed else {
            return Err(RedDBError::Query(
                "vault unseal failed: stored value is not sealed".to_string(),
            ));
        };
        if payload.len() < 12 {
            return Err(RedDBError::Query(
                "vault unseal failed: sealed payload is truncated".to_string(),
            ));
        }
        let key = self.vault_encryption_key(collection)?;
        let mut nonce = [0u8; 12];
        nonce.copy_from_slice(&payload[..12]);
        let aad = format!("reddb.vault.{collection}");
        let plaintext = crate::crypto::aes_gcm::aes256_gcm_decrypt(
            &key,
            &nonce,
            aad.as_bytes(),
            &payload[12..],
        )
        .map_err(|_| RedDBError::Query("vault unseal failed: decryption failed".to_string()))?;
        let (value, consumed) =
            crate::storage::schema::Value::from_bytes(&plaintext).map_err(|err| {
                RedDBError::Query(format!("vault unseal failed: bad plaintext value: {err}"))
            })?;
        if consumed != plaintext.len() {
            return Err(RedDBError::Query(
                "vault unseal failed: trailing plaintext bytes".to_string(),
            ));
        }
        Ok(value)
    }

    fn vault_target_resource(collection: &str, key: &str) -> String {
        if collection == "red.vault" {
            return format!("red.vault/{}", key.to_ascii_lowercase());
        }
        format!("{collection}.{key}")
    }

    fn current_vault_actor() -> String {
        current_auth_identity()
            .map(|(principal, _)| principal)
            .unwrap_or_else(|| "anonymous".to_string())
    }

    fn vault_request_id() -> String {
        let conn_id = current_connection_id();
        if conn_id == 0 {
            "embedded".to_string()
        } else {
            format!("conn-{conn_id}")
        }
    }

    fn check_vault_capability(
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
                "IAM authorization is enabled; vault capability check requires an authenticated principal"
                    .to_string(),
            );
        };
        let tenant = current_tenant();
        let principal_id = crate::auth::UserId::from_parts(tenant.as_deref(), &principal);
        let mut resource = crate::auth::policies::ResourceRef::new(
            "vault",
            Self::vault_target_resource(collection, key),
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
                "principal=`{}` action=`{}` resource=`vault:{}` denied by IAM policy",
                principal,
                action,
                Self::vault_target_resource(collection, key)
            ))
        }
    }

    fn check_system_vault_capability(
        &self,
        action: &str,
        collection: &str,
        key: &str,
    ) -> Result<(), String> {
        if collection != "red.vault" {
            return Ok(());
        }
        self.check_vault_capability(action, collection, key)
    }

    fn audit_vault_unseal(
        &self,
        collection: &str,
        key: &str,
        outcome: crate::runtime::audit_log::Outcome,
        reason: &str,
        entry: Option<&VaultEntry>,
    ) {
        let actor = Self::current_vault_actor();
        let request_id = Self::vault_request_id();
        let mut builder = crate::runtime::audit_log::AuditEvent::builder("vault/unseal")
            .principal(actor.clone())
            .source(crate::runtime::audit_log::AuditAuthSource::Password)
            .resource(format!(
                "vault:{}",
                Self::vault_target_resource(collection, key)
            ))
            .outcome(outcome)
            .correlation_id(request_id.clone())
            .fields([
                crate::runtime::audit_log::AuditFieldEscaper::field("actor", actor),
                crate::runtime::audit_log::AuditFieldEscaper::field("collection", collection),
                crate::runtime::audit_log::AuditFieldEscaper::field("key", key),
                crate::runtime::audit_log::AuditFieldEscaper::field(
                    "target",
                    Self::vault_target_resource(collection, key),
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
        if let Some(entry) = entry {
            builder = builder.fields([
                crate::runtime::audit_log::AuditFieldEscaper::field("entity_id", entry.id.raw()),
                crate::runtime::audit_log::AuditFieldEscaper::field(
                    "sequence_id",
                    entry.sequence_id,
                ),
            ]);
        }
        self.audit_log().record_event(builder.build());
    }

    fn audit_vault_lifecycle(
        &self,
        operation: &str,
        collection: &str,
        key: &str,
        outcome: crate::runtime::audit_log::Outcome,
        reason: &str,
        entry: Option<&VaultEntry>,
    ) {
        let actor = Self::current_vault_actor();
        let request_id = Self::vault_request_id();
        let mut builder =
            crate::runtime::audit_log::AuditEvent::builder(format!("vault/{operation}"))
                .principal(actor.clone())
                .source(crate::runtime::audit_log::AuditAuthSource::Password)
                .resource(format!(
                    "vault:{}",
                    Self::vault_target_resource(collection, key)
                ))
                .outcome(outcome)
                .correlation_id(request_id.clone())
                .fields([
                    crate::runtime::audit_log::AuditFieldEscaper::field("actor", actor),
                    crate::runtime::audit_log::AuditFieldEscaper::field("collection", collection),
                    crate::runtime::audit_log::AuditFieldEscaper::field("key", key),
                    crate::runtime::audit_log::AuditFieldEscaper::field(
                        "target",
                        Self::vault_target_resource(collection, key),
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
        if let Some(entry) = entry {
            builder = builder.fields([
                crate::runtime::audit_log::AuditFieldEscaper::field("entity_id", entry.id.raw()),
                crate::runtime::audit_log::AuditFieldEscaper::field("version", entry.version),
                crate::runtime::audit_log::AuditFieldEscaper::field(
                    "sequence_id",
                    entry.sequence_id,
                ),
            ]);
        }
        self.audit_log().record_event(builder.build());
    }

    pub(crate) fn resolve_vault_secret_value(
        &self,
        collection: &str,
        key: &str,
    ) -> RedDBResult<Value> {
        let ops = KvAtomicOps::new(self);
        let entry = ops.get_vault_entry(collection, key)?;
        if let Err(reason) = self.check_vault_capability("vault:unseal", collection, key) {
            self.audit_vault_unseal(
                collection,
                key,
                crate::runtime::audit_log::Outcome::Denied,
                &reason,
                entry.as_ref(),
            );
            return Err(RedDBError::Query(reason));
        }
        let Some(entry) = entry else {
            let reason = "not_found";
            self.audit_vault_unseal(
                collection,
                key,
                crate::runtime::audit_log::Outcome::Denied,
                reason,
                None,
            );
            return Err(RedDBError::NotFound(format!(
                "vault secret '{}.{}' not found",
                collection, key
            )));
        };
        if entry.tombstone {
            let reason = "deleted";
            self.audit_vault_unseal(
                collection,
                key,
                crate::runtime::audit_log::Outcome::Denied,
                reason,
                Some(&entry),
            );
            return Err(RedDBError::NotFound(format!(
                "vault secret '{}.{}' is deleted",
                collection, key
            )));
        }
        match self.unseal_vault_value(collection, &entry.value) {
            Ok(value) => {
                self.audit_vault_unseal(
                    collection,
                    key,
                    crate::runtime::audit_log::Outcome::Success,
                    "ok",
                    Some(&entry),
                );
                Ok(value)
            }
            Err(err) => {
                let reason = err.to_string();
                self.audit_vault_unseal(
                    collection,
                    key,
                    crate::runtime::audit_log::Outcome::Error,
                    &reason,
                    Some(&entry),
                );
                Err(err)
            }
        }
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
                tags,
                if_not_exists,
            } => {
                if *model == crate::catalog::CollectionModel::Vault {
                    self.check_system_vault_capability("vault:write", collection, key)
                        .map_err(RedDBError::Query)?;
                }
                self.check_write(crate::runtime::write_gate::WriteKind::Dml)?;
                let (created, id) = ops.set_with_tags_for_model(
                    *model,
                    collection,
                    key,
                    value.clone(),
                    *ttl_ms,
                    tags,
                    *if_not_exists,
                )?;

                let mut result = UnifiedResult::with_columns(vec![
                    "ok".into(),
                    "collection".into(),
                    "key".into(),
                    "id".into(),
                    "created".into(),
                    "tags".into(),
                ]);
                let mut record = UnifiedRecord::new();
                record.set("ok", Value::Boolean(true));
                record.set("collection", Value::text(collection.clone()));
                record.set("key", Value::text(key.clone()));
                record.set("id", Value::Integer(id.raw() as i64));
                record.set("created", Value::Boolean(created));
                record.set("tags", kv_tags_value(tags));
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
            KvCommand::InvalidateTags { collection, tags } => {
                let invalidated = ops.invalidate_tags(collection, tags)?;

                let mut result = UnifiedResult::with_columns(vec![
                    "ok".into(),
                    "collection".into(),
                    "invalidated".into(),
                    "tags".into(),
                ]);
                let mut record = UnifiedRecord::new();
                record.set("ok", Value::Boolean(true));
                record.set("collection", Value::text(collection.clone()));
                record.set("invalidated", Value::Integer(invalidated as i64));
                record.set("tags", kv_tags_value(tags));
                result.push(record);

                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: crate::storage::query::modes::QueryMode::Sql,
                    statement: "kv_invalidate_tags",
                    engine: "kv",
                    result,
                    affected_rows: invalidated as u64,
                    statement_type: "delete",
                })
            }

            KvCommand::Rotate {
                collection,
                key,
                value,
                tags,
            } => {
                self.check_system_vault_capability("vault:write", collection, key)
                    .map_err(RedDBError::Query)?;
                self.check_write(crate::runtime::write_gate::WriteKind::Dml)?;
                let entry = ops.append_vault_version(
                    collection,
                    key,
                    value.clone(),
                    "rotate",
                    false,
                    tags,
                )?;
                self.record_kv_watch_event(
                    crate::replication::cdc::ChangeOperation::Update,
                    collection,
                    key,
                    entry.id.raw(),
                    None,
                    Some(vault_entry_metadata_json(&entry)),
                );
                self.audit_vault_lifecycle(
                    "rotate",
                    collection,
                    key,
                    crate::runtime::audit_log::Outcome::Success,
                    "ok",
                    Some(&entry),
                );
                Ok(vault_write_result(
                    raw_query,
                    "vault_rotate",
                    "update",
                    collection,
                    key,
                    &entry,
                    1,
                ))
            }

            KvCommand::List {
                model,
                collection,
                prefix,
                limit,
                offset,
            } => {
                if *model != crate::catalog::CollectionModel::Vault {
                    return Err(RedDBError::InvalidOperation(
                        "LIST is not supported through normal KV command execution".to_string(),
                    ));
                }
                let mut entries = ops.latest_vault_entries(collection, prefix.as_deref())?;
                entries.sort_by(|left, right| left.key.cmp(&right.key));
                let mut result = UnifiedResult::with_columns(vec![
                    "collection".into(),
                    "key".into(),
                    "version".into(),
                    "fingerprint".into(),
                    "tags".into(),
                    "created_at".into(),
                    "updated_at".into(),
                    "status".into(),
                    "tombstone".into(),
                    "op".into(),
                ]);
                for entry in entries
                    .into_iter()
                    .filter(|entry| {
                        self.check_vault_capability("vault:read_metadata", collection, &entry.key)
                            .is_ok()
                    })
                    .skip(*offset)
                    .take(limit.unwrap_or(usize::MAX))
                {
                    push_vault_metadata_record(&mut result, collection, &entry.key, &entry);
                }
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: crate::storage::query::modes::QueryMode::Sql,
                    statement: "vault_list",
                    engine: "vault",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }

            KvCommand::History { collection, key } => {
                self.check_vault_capability("vault:read_metadata", collection, key)
                    .map_err(RedDBError::Query)?;
                let versions =
                    super::keyed_spine::history_versions(ops.vault_versions(collection, key)?);
                let result = vault_history_result(collection, key, &versions);
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: crate::storage::query::modes::QueryMode::Sql,
                    statement: "vault_history",
                    engine: "vault",
                    result,
                    affected_rows: 0,
                    statement_type: "select",
                })
            }

            KvCommand::Purge { collection, key } => {
                let entry = ops.get_vault_entry(collection, key)?;
                if let Err(reason) = self.check_vault_capability("vault:purge", collection, key) {
                    self.audit_vault_lifecycle(
                        "purge",
                        collection,
                        key,
                        crate::runtime::audit_log::Outcome::Denied,
                        &reason,
                        entry.as_ref(),
                    );
                    return Err(RedDBError::Query(reason));
                }
                self.check_write(crate::runtime::write_gate::WriteKind::Dml)?;
                let purged = ops.purge_vault_versions(collection, key)?;
                self.audit_vault_lifecycle(
                    "purge",
                    collection,
                    key,
                    crate::runtime::audit_log::Outcome::Success,
                    "ok",
                    entry.as_ref(),
                );
                let mut result = UnifiedResult::with_columns(vec![
                    "ok".into(),
                    "collection".into(),
                    "key".into(),
                    "purged".into(),
                ]);
                let mut record = UnifiedRecord::new();
                record.set("ok", Value::Boolean(true));
                record.set("collection", Value::text(collection.clone()));
                record.set("key", Value::text(key.clone()));
                record.set("purged", Value::Integer(purged as i64));
                result.push(record);
                Ok(RuntimeQueryResult {
                    query: raw_query.to_string(),
                    mode: crate::storage::query::modes::QueryMode::Sql,
                    statement: "vault_purge",
                    engine: "vault",
                    result,
                    affected_rows: purged as u64,
                    statement_type: "delete",
                })
            }

            KvCommand::Get {
                model,
                collection,
                key,
            } => {
                if *model == crate::catalog::CollectionModel::Vault {
                    self.check_vault_capability("vault:read_metadata", collection, key)
                        .map_err(RedDBError::Query)?;
                    let entry = ops.get_vault_entry(collection, key)?;
                    let key_available = self.vault_key_available(collection);
                    let result =
                        vault_metadata_result(collection, key, entry.as_ref(), key_available);
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
                    "tags".into(),
                ]);
                let mut record = UnifiedRecord::new();
                record.set("collection", Value::text(collection.clone()));
                record.set("key", Value::text(key.clone()));
                record.set(
                    "value",
                    value.unwrap_or(crate::storage::schema::Value::Null),
                );
                record.set("tags", kv_tags_value(&ops.tags_for_key(collection, key)));
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
            KvCommand::Watch {
                model,
                collection,
                key,
                prefix,
                from_lsn,
            } => {
                let watch_key = if *prefix {
                    format!("{key}.*")
                } else {
                    key.clone()
                };
                let endpoint = match from_lsn {
                    Some(lsn) => format!(
                        "/collections/{collection}/{}/{watch_key}/watch?since_lsn={lsn}",
                        keyed_model_name(*model)
                    ),
                    None => format!(
                        "/collections/{collection}/{}/{watch_key}/watch",
                        keyed_model_name(*model)
                    ),
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
                record.set("collection", Value::text(collection.clone()));
                record.set("key", Value::text(watch_key));
                record.set("prefix", Value::Boolean(*prefix));
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
                    statement: "kv_watch",
                    engine: keyed_model_name(*model),
                    result,
                    affected_rows: 0,
                    statement_type: "stream",
                })
            }

            KvCommand::Unseal {
                collection,
                key,
                version,
            } => {
                let latest = ops.get_vault_entry(collection, key)?;
                let entry = match version {
                    Some(version) => ops.get_vault_entry_version(collection, key, *version)?,
                    None => latest.clone(),
                };
                let action = match (version, latest.as_ref()) {
                    (Some(requested), Some(latest)) if *requested == latest.version => {
                        "vault:unseal"
                    }
                    (Some(_), _) => "vault:unseal_history",
                    _ => "vault:unseal",
                };
                if let Err(reason) = self.check_vault_capability(action, collection, key) {
                    self.audit_vault_unseal(
                        collection,
                        key,
                        crate::runtime::audit_log::Outcome::Denied,
                        &reason,
                        entry.as_ref(),
                    );
                    return Err(RedDBError::Query(reason));
                }
                let Some(entry) = entry else {
                    let reason = "not_found";
                    self.audit_vault_unseal(
                        collection,
                        key,
                        crate::runtime::audit_log::Outcome::Denied,
                        reason,
                        None,
                    );
                    return Err(RedDBError::NotFound(format!(
                        "vault secret '{}.{}' not found",
                        collection, key
                    )));
                };
                if entry.tombstone {
                    let reason = "deleted";
                    self.audit_vault_unseal(
                        collection,
                        key,
                        crate::runtime::audit_log::Outcome::Denied,
                        reason,
                        Some(&entry),
                    );
                    return Err(RedDBError::NotFound(format!(
                        "vault secret '{}.{}' is deleted",
                        collection, key
                    )));
                }
                match self.unseal_vault_value(collection, &entry.value) {
                    Ok(value) => {
                        self.audit_vault_unseal(
                            collection,
                            key,
                            crate::runtime::audit_log::Outcome::Success,
                            "ok",
                            Some(&entry),
                        );
                        let mut result = UnifiedResult::with_columns(vec![
                            "collection".into(),
                            "key".into(),
                            "value".into(),
                        ]);
                        let mut record = UnifiedRecord::new();
                        record.set("collection", Value::text(collection.clone()));
                        record.set("key", Value::text(key.clone()));
                        record.set("value", value);
                        result.push(record);
                        Ok(RuntimeQueryResult {
                            query: raw_query.to_string(),
                            mode: crate::storage::query::modes::QueryMode::Sql,
                            statement: "vault_unseal",
                            engine: "vault",
                            result,
                            affected_rows: 0,
                            statement_type: "select",
                        })
                    }
                    Err(err) => {
                        let reason = err.to_string();
                        self.audit_vault_unseal(
                            collection,
                            key,
                            crate::runtime::audit_log::Outcome::Error,
                            &reason,
                            Some(&entry),
                        );
                        Err(err)
                    }
                }
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
                    self.check_system_vault_capability("vault:write", collection, key)
                        .map_err(RedDBError::Query)?;
                    self.check_write(crate::runtime::write_gate::WriteKind::Dml)?;
                    let entry = ops.append_vault_version(
                        collection,
                        key,
                        Value::Null,
                        "delete",
                        true,
                        &[],
                    )?;
                    self.record_kv_watch_event(
                        crate::replication::cdc::ChangeOperation::Delete,
                        collection,
                        key,
                        entry.id.raw(),
                        None,
                        Some(vault_entry_metadata_json(&entry)),
                    );
                    self.audit_vault_lifecycle(
                        "delete",
                        collection,
                        key,
                        crate::runtime::audit_log::Outcome::Success,
                        "ok",
                        Some(&entry),
                    );
                    return Ok(vault_write_result(
                        raw_query,
                        "vault_delete",
                        "delete",
                        collection,
                        key,
                        &entry,
                        1,
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

    pub fn vault_watch_events_since(
        &self,
        collection: &str,
        key: &str,
        since_lsn: u64,
        max_count: usize,
    ) -> Vec<crate::replication::cdc::KvWatchEvent> {
        self.kv_watch_events_since(collection, key, since_lsn, max_count)
            .into_iter()
            .filter(|event| {
                self.check_vault_capability("vault:read_metadata", &event.collection, &event.key)
                    .is_ok()
            })
            .map(vault_filter_watch_event)
            .collect()
    }

    pub fn vault_watch_events_since_prefix(
        &self,
        collection: &str,
        prefix: &str,
        since_lsn: u64,
        max_count: usize,
    ) -> Vec<crate::replication::cdc::KvWatchEvent> {
        self.kv_watch_events_since_prefix(collection, prefix, since_lsn, max_count)
            .into_iter()
            .filter(|event| {
                self.check_vault_capability("vault:read_metadata", &event.collection, &event.key)
                    .is_ok()
            })
            .map(vault_filter_watch_event)
            .collect()
    }

    fn check_kv_invalidate_policy(&self, collection: &str) -> RedDBResult<()> {
        let auth_store = match self.inner.auth_store.read().clone() {
            Some(store) => store,
            None => return Ok(()),
        };
        let (username, role) = match crate::runtime::impl_core::current_auth_identity() {
            Some(identity) => identity,
            None => return Ok(()),
        };
        if role < crate::auth::Role::Write {
            return Err(RedDBError::Query(format!(
                "principal=`{username}` role=`{role:?}` cannot invalidate KV tags"
            )));
        }
        if !auth_store.iam_authorization_enabled() {
            return Ok(());
        }

        let tenant = crate::runtime::impl_core::current_tenant();
        let principal = crate::auth::UserId::from_parts(tenant.as_deref(), &username);
        let mut resource =
            crate::auth::policies::ResourceRef::new("kv".to_string(), collection.to_string());
        if let Some(tenant) = tenant.clone() {
            resource = resource.with_tenant(tenant);
        }
        let ctx = crate::auth::policies::EvalContext {
            principal_tenant: tenant.clone(),
            current_tenant: tenant,
            peer_ip: None,
            mfa_present: false,
            now_ms: current_unix_ms(),
            principal_is_admin_role: role == crate::auth::Role::Admin,
        };
        if auth_store.check_policy_authz(&principal, "kv:invalidate", &resource, &ctx) {
            Ok(())
        } else {
            Err(RedDBError::Query(format!(
                "principal=`{username}` action=`kv:invalidate` resource=`kv:{collection}` denied by IAM policy"
            )))
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

fn vault_write_result(
    raw_query: &str,
    statement: &'static str,
    statement_type: &'static str,
    collection: &str,
    key: &str,
    entry: &VaultEntry,
    affected_rows: u64,
) -> RuntimeQueryResult {
    let mut result = UnifiedResult::with_columns(vec![
        "ok".into(),
        "collection".into(),
        "key".into(),
        "version".into(),
        "fingerprint".into(),
        "tombstone".into(),
        "op".into(),
        "id".into(),
    ]);
    let mut record = UnifiedRecord::new();
    record.set("ok", Value::Boolean(true));
    record.set("collection", Value::text(collection.to_string()));
    record.set("key", Value::text(key.to_string()));
    record.set("version", Value::Integer(entry.version));
    if entry.tombstone {
        record.set("fingerprint", Value::Null);
    } else {
        record.set("fingerprint", Value::text(vault_fingerprint(&entry.value)));
    }
    record.set("tombstone", Value::Boolean(entry.tombstone));
    record.set("op", Value::text(entry.op.clone()));
    record.set("id", Value::Integer(entry.id.raw() as i64));
    result.push(record);
    RuntimeQueryResult {
        query: raw_query.to_string(),
        mode: crate::storage::query::modes::QueryMode::Sql,
        statement,
        engine: "vault",
        result,
        affected_rows,
        statement_type,
    }
}

fn vault_history_result(collection: &str, key: &str, versions: &[VaultEntry]) -> UnifiedResult {
    let mut result = UnifiedResult::with_columns(vec![
        "collection".into(),
        "key".into(),
        "version".into(),
        "fingerprint".into(),
        "tags".into(),
        "created_at".into(),
        "updated_at".into(),
        "status".into(),
        "tombstone".into(),
        "op".into(),
    ]);
    for entry in versions {
        push_vault_metadata_record(&mut result, collection, key, entry);
    }
    result
}

fn push_vault_metadata_record(
    result: &mut UnifiedResult,
    collection: &str,
    key: &str,
    entry: &VaultEntry,
) {
    let mut record = UnifiedRecord::new();
    record.set("collection", Value::text(collection.to_string()));
    record.set("key", Value::text(key.to_string()));
    record.set("version", Value::Integer(entry.version));
    if entry.tombstone {
        record.set("fingerprint", Value::Null);
        record.set("status", Value::text("deleted"));
    } else {
        record.set("fingerprint", Value::text(vault_fingerprint(&entry.value)));
        record.set("status", Value::text("sealed"));
    }
    record.set("tags", vault_tags_value(&entry.metadata));
    record.set("created_at", Value::TimestampMs(entry.created_at as i64));
    record.set("updated_at", Value::TimestampMs(entry.updated_at as i64));
    record.set("tombstone", Value::Boolean(entry.tombstone));
    record.set("op", Value::text(entry.op.clone()));
    result.push(record);
}

fn vault_metadata_result(
    collection: &str,
    key: &str,
    entry: Option<&VaultEntry>,
    key_available: bool,
) -> UnifiedResult {
    let mut result = UnifiedResult::with_columns(vec![
        "collection".into(),
        "key".into(),
        "version".into(),
        "fingerprint".into(),
        "tags".into(),
        "created_at".into(),
        "updated_at".into(),
        "value".into(),
        "status".into(),
        "tombstone".into(),
        "op".into(),
    ]);
    let mut record = UnifiedRecord::new();
    record.set("collection", Value::text(collection.to_string()));
    record.set("key", Value::text(key.to_string()));
    match entry {
        Some(entry) => {
            record.set("version", Value::Integer(entry.version));
            if entry.tombstone {
                record.set("fingerprint", Value::Null);
            } else {
                record.set("fingerprint", Value::text(vault_fingerprint(&entry.value)));
            }
            record.set("tags", vault_tags_value(&entry.metadata));
            record.set("created_at", Value::TimestampMs(entry.created_at as i64));
            record.set("updated_at", Value::TimestampMs(entry.updated_at as i64));
            record.set("value", Value::text("***"));
            record.set(
                "status",
                Value::text(if entry.tombstone {
                    "deleted"
                } else if key_available {
                    "sealed"
                } else {
                    "sealed_unavailable"
                }),
            );
            record.set("tombstone", Value::Boolean(entry.tombstone));
            record.set("op", Value::text(entry.op.clone()));
        }
        None => {
            record.set("version", Value::Null);
            record.set("fingerprint", Value::Null);
            record.set("tags", Value::Array(Vec::new()));
            record.set("created_at", Value::Null);
            record.set("updated_at", Value::Null);
            record.set("value", Value::text(""));
            record.set("status", Value::text("missing"));
            record.set("tombstone", Value::Boolean(false));
            record.set("op", Value::Null);
        }
    }
    result.push(record);
    result
}

fn vault_fingerprint(value: &Value) -> String {
    match value {
        Value::Secret(payload) => crate::utils::to_hex(&crate::crypto::sha256::sha256(payload)),
        other => crate::utils::to_hex(&crate::crypto::sha256::sha256(&other.to_bytes())),
    }
}

fn vault_entry_metadata_json(entry: &VaultEntry) -> crate::json::Value {
    let mut object = crate::json::Map::new();
    object.insert(
        "key".to_string(),
        crate::json::Value::String(entry.key.clone()),
    );
    object.insert(
        "version".to_string(),
        crate::json::Value::Number(entry.version as f64),
    );
    object.insert(
        "fingerprint".to_string(),
        if entry.tombstone {
            crate::json::Value::Null
        } else {
            crate::json::Value::String(vault_fingerprint(&entry.value))
        },
    );
    object.insert("tags".to_string(), vault_tags_json(&entry.metadata));
    object.insert(
        "actor".to_string(),
        crate::json::Value::String(RedDBRuntime::current_vault_actor()),
    );
    object.insert(
        "sequence_id".to_string(),
        crate::json::Value::Number(entry.sequence_id as f64),
    );
    object.insert(
        "tombstone".to_string(),
        crate::json::Value::Bool(entry.tombstone),
    );
    object.insert(
        "op".to_string(),
        crate::json::Value::String(entry.op.clone()),
    );
    crate::json::Value::Object(object)
}

fn vault_tags_json(metadata: &Metadata) -> crate::json::Value {
    match vault_tags_value(metadata) {
        Value::Array(values) => crate::json::Value::Array(
            values
                .into_iter()
                .filter_map(|value| match value {
                    Value::Text(tag) => Some(crate::json::Value::String(tag.to_string())),
                    _ => None,
                })
                .collect(),
        ),
        _ => crate::json::Value::Array(Vec::new()),
    }
}

fn vault_tags_metadata(tags: &[String]) -> std::collections::HashMap<String, MetadataValue> {
    [(
        "tags".to_string(),
        MetadataValue::Array(
            tags.iter()
                .map(|tag| MetadataValue::String(tag.clone()))
                .collect(),
        ),
    )]
    .into_iter()
    .collect()
}

fn vault_filter_watch_event(
    mut event: crate::replication::cdc::KvWatchEvent,
) -> crate::replication::cdc::KvWatchEvent {
    event.before = event.before.and_then(vault_metadata_json_only);
    event.after = event.after.and_then(vault_metadata_json_only);
    event
}

fn vault_metadata_json_only(value: crate::json::Value) -> Option<crate::json::Value> {
    let object = value.as_object()?;
    let mut out = crate::json::Map::new();
    for field in [
        "key",
        "version",
        "fingerprint",
        "tags",
        "actor",
        "sequence_id",
        "tombstone",
        "op",
    ] {
        if let Some(value) = object.get(field) {
            out.insert(field.to_string(), value.clone());
        }
    }
    Some(crate::json::Value::Object(out))
}

fn vault_tags_value(metadata: &Metadata) -> Value {
    match metadata.get("tags") {
        Some(MetadataValue::Array(values)) => Value::Array(
            values
                .iter()
                .filter_map(|value| match value {
                    MetadataValue::String(tag) => Some(Value::text(tag.clone())),
                    _ => None,
                })
                .collect(),
        ),
        Some(MetadataValue::String(tag)) if !tag.is_empty() => {
            Value::Array(vec![Value::text(tag.clone())])
        }
        _ => Value::Array(Vec::new()),
    }
}

fn decode_vault_key(hex_key: &str) -> RedDBResult<[u8; 32]> {
    let bytes = hex::decode(hex_key)
        .map_err(|_| RedDBError::Query("vault sealed_unavailable: bad key material".to_string()))?;
    let key: [u8; 32] = bytes.try_into().map_err(|_| {
        RedDBError::Query("vault sealed_unavailable: bad key material length".to_string())
    })?;
    Ok(key)
}

fn kv_tags_metadata(tags: &[String]) -> Option<(String, MetadataValue)> {
    if tags.is_empty() {
        return None;
    }
    let values = tags
        .iter()
        .map(|tag| MetadataValue::String(tag.clone()))
        .collect();
    Some(("_kv_tags".to_string(), MetadataValue::Array(values)))
}

fn kv_tags_value(tags: &[String]) -> Value {
    let json = crate::json::Value::Array(
        tags.iter()
            .map(|tag| crate::json::Value::String(tag.clone()))
            .collect(),
    );
    Value::Json(crate::json::to_vec(&json).unwrap_or_default())
}

fn kv_value_from_entity(entity: &crate::storage::UnifiedEntity) -> Option<Value> {
    if let crate::storage::EntityData::Row(ref row) = entity.data {
        if let Some(ref named) = row.named {
            return named.get("value").cloned();
        }
    }
    None
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
        vector_dimension: None,
        vector_metric: None,
        context_index_fields: Vec::new(),
        declared_columns: Vec::new(),
        table_def: None,
        timestamps_enabled: false,
        context_index_enabled: false,
        metrics_raw_retention_ms: None,
        metrics_tenant_identity: None,
        metrics_namespace: None,
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
    fn kv_runtime_stats_count_public_ops() {
        let r = rt();
        let ops = super::KvAtomicOps::new(&r);

        ops.set(
            CollectionModel::Kv,
            "kv_default",
            "profile",
            crate::storage::schema::Value::text("alice"),
            None,
            false,
        )
        .unwrap();
        ops.get(CollectionModel::Kv, "kv_default", "profile")
            .unwrap();
        ops.delete(CollectionModel::Kv, "kv_default", "profile")
            .unwrap();
        ops.incr(CollectionModel::Kv, "kv_default", "hits", 1, None)
            .unwrap();
        ops.cas(
            CollectionModel::Kv,
            "kv_default",
            "profile",
            None,
            crate::storage::schema::Value::text("created"),
            None,
        )
        .unwrap();
        ops.cas(
            CollectionModel::Kv,
            "kv_default",
            "profile",
            Some(&crate::storage::schema::Value::text("different")),
            crate::storage::schema::Value::text("ignored"),
            None,
        )
        .unwrap();

        let stats = r.stats().kv;
        assert_eq!(stats.puts, 1);
        assert_eq!(stats.gets, 1);
        assert_eq!(stats.deletes, 1);
        assert_eq!(stats.incrs, 1);
        assert_eq!(stats.cas_success, 1);
        assert_eq!(stats.cas_conflict, 1);
    }

    #[test]
    fn kv_invalidate_tags_removes_matching_entries_only() {
        let r = rt();

        r.execute_query("KV PUT sessions.blob = 'payload' TAGS [user:42, org:7]")
            .unwrap();

        let miss = r
            .execute_query("INVALIDATE TAGS [org:99] FROM sessions")
            .unwrap();
        assert_eq!(miss.affected_rows, 0);
        assert!(matches!(
            r.execute_query("KV GET sessions.blob")
                .unwrap()
                .result
                .records[0]
                .get("value"),
            Some(crate::storage::schema::Value::Text(value)) if &**value == "payload"
        ));

        let hit = r
            .execute_query("INVALIDATE TAGS [user:42] FROM sessions")
            .unwrap();
        assert_eq!(hit.affected_rows, 1);
        assert!(matches!(
            r.execute_query("KV GET sessions.blob")
                .unwrap()
                .result
                .records[0]
                .get("value"),
            Some(crate::storage::schema::Value::Null)
        ));
    }

    #[test]
    fn kv_runtime_stats_count_watch_streams_and_events() {
        let r = rt();
        let ops = super::KvAtomicOps::new(&r);
        assert_eq!(r.stats().kv.watch_streams_active, 0);

        {
            let mut stream = r.kv_watch_subscribe("kv_default", "watched", None);
            assert_eq!(r.stats().kv.watch_streams_active, 1);

            ops.set(
                CollectionModel::Kv,
                "kv_default",
                "watched",
                crate::storage::schema::Value::Integer(1),
                None,
                false,
            )
            .unwrap();
            let event = stream.poll_next().expect("watch event");
            assert_eq!(event.key, "watched");
            assert_eq!(r.stats().kv.watch_events_emitted, 1);

            stream.record_drop_count(3);
            assert_eq!(r.stats().kv.watch_drops, 3);
        }

        assert_eq!(r.stats().kv.watch_streams_active, 0);
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
    fn concurrent_incr_single_key_is_atomic() {
        const THREADS: usize = 8;
        const ITERS: usize = 1000;

        let runtime = std::sync::Arc::new(rt());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(THREADS));
        let mut handles = Vec::new();

        for _ in 0..THREADS {
            let runtime = std::sync::Arc::clone(&runtime);
            let barrier = std::sync::Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                let ops = super::KvAtomicOps::new(&runtime);
                barrier.wait();
                for _ in 0..ITERS {
                    ops.incr(CollectionModel::Kv, "kv_default", "counter", 1, None)
                        .unwrap();
                }
            }));
        }

        for handle in handles {
            handle.join().expect("worker should finish");
        }

        let ops = super::KvAtomicOps::new(&runtime);
        assert_eq!(
            ops.get(CollectionModel::Kv, "kv_default", "counter")
                .unwrap(),
            Some(crate::storage::schema::Value::Integer(
                (THREADS * ITERS) as i64
            ))
        );
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
    fn concurrent_cas_allows_one_success_per_round() {
        const THREADS: usize = 8;
        const ROUNDS: usize = 100;

        let runtime = std::sync::Arc::new(rt());
        let ops = super::KvAtomicOps::new(&runtime);
        ops.set(
            CollectionModel::Kv,
            "kv_default",
            "cas_counter",
            crate::storage::schema::Value::Integer(0),
            None,
            false,
        )
        .unwrap();

        let start_round = std::sync::Arc::new(std::sync::Barrier::new(THREADS));
        let finish_round = std::sync::Arc::new(std::sync::Barrier::new(THREADS));
        let successes = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut handles = Vec::new();

        for _ in 0..THREADS {
            let runtime = std::sync::Arc::clone(&runtime);
            let start_round = std::sync::Arc::clone(&start_round);
            let finish_round = std::sync::Arc::clone(&finish_round);
            let successes = std::sync::Arc::clone(&successes);
            handles.push(std::thread::spawn(move || {
                let ops = super::KvAtomicOps::new(&runtime);
                for round in 0..ROUNDS {
                    start_round.wait();
                    let (ok, _) = ops
                        .cas(
                            CollectionModel::Kv,
                            "kv_default",
                            "cas_counter",
                            Some(&crate::storage::schema::Value::Integer(round as i64)),
                            crate::storage::schema::Value::Integer((round + 1) as i64),
                            None,
                        )
                        .unwrap();
                    if ok {
                        successes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                    finish_round.wait();
                }
            }));
        }

        for handle in handles {
            handle.join().expect("worker should finish");
        }

        assert_eq!(successes.load(std::sync::atomic::Ordering::SeqCst), ROUNDS);
        assert_eq!(
            ops.get(CollectionModel::Kv, "kv_default", "cas_counter")
                .unwrap(),
            Some(crate::storage::schema::Value::Integer(ROUNDS as i64))
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

    #[test]
    fn concurrent_self_referential_update_is_atomic() {
        const THREADS: usize = 8;
        const ITERS: usize = 100;

        let runtime = std::sync::Arc::new(rt());
        runtime
            .execute_query("CREATE TABLE counters (id INT, n INT)")
            .unwrap();
        runtime
            .execute_query("INSERT INTO counters (id, n) VALUES (1, 0)")
            .unwrap();

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(THREADS));
        let mut handles = Vec::new();
        for _ in 0..THREADS {
            let runtime = std::sync::Arc::clone(&runtime);
            let barrier = std::sync::Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..ITERS {
                    runtime
                        .execute_query("UPDATE counters SET n = n + 1 WHERE id = 1")
                        .unwrap();
                }
            }));
        }

        for handle in handles {
            handle.join().expect("worker should finish");
        }

        let selected = runtime
            .execute_query("SELECT n FROM counters WHERE id = 1")
            .unwrap();
        assert_eq!(
            selected.result.records[0].get("n"),
            Some(&crate::storage::schema::Value::Integer(
                (THREADS * ITERS) as i64
            ))
        );
    }

    #[test]
    fn watch_stream_delivers_key_events_in_lsn_order() {
        let r = rt();
        let ops = super::KvAtomicOps::new(&r);
        let mut stream = r.kv_watch_subscribe("kv_default", "seq", None);

        ops.set(
            CollectionModel::Kv,
            "kv_default",
            "seq",
            crate::storage::schema::Value::Integer(1),
            None,
            false,
        )
        .unwrap();
        ops.incr(CollectionModel::Kv, "kv_default", "seq", 1, None)
            .unwrap();
        ops.delete(CollectionModel::Kv, "kv_default", "seq")
            .unwrap();
        ops.set(
            CollectionModel::Kv,
            "kv_default",
            "seq",
            crate::storage::schema::Value::Integer(9),
            None,
            false,
        )
        .unwrap();

        let mut events = Vec::new();
        while let Some(event) = stream.poll_next() {
            events.push(event);
            if events.len() == 4 {
                break;
            }
        }

        assert_eq!(events.len(), 4);
        assert_eq!(
            events[0].op,
            crate::replication::cdc::ChangeOperation::Insert
        );
        assert_eq!(
            events[1].op,
            crate::replication::cdc::ChangeOperation::Update
        );
        assert_eq!(
            events[2].op,
            crate::replication::cdc::ChangeOperation::Delete
        );
        assert_eq!(
            events[3].op,
            crate::replication::cdc::ChangeOperation::Insert
        );
        assert!(events.windows(2).all(|pair| pair[0].lsn < pair[1].lsn));
    }

    #[test]
    fn watch_prefix_stream_delivers_matching_events_only() {
        let r = rt();
        let ops = super::KvAtomicOps::new(&r);
        let mut stream = r.kv_watch_subscribe_prefix("kv_default", "acct:", None);

        ops.set(
            CollectionModel::Kv,
            "kv_default",
            "acct:1",
            crate::storage::schema::Value::Integer(1),
            None,
            false,
        )
        .unwrap();
        ops.set(
            CollectionModel::Kv,
            "kv_default",
            "session:1",
            crate::storage::schema::Value::Integer(2),
            None,
            false,
        )
        .unwrap();
        ops.set(
            CollectionModel::Kv,
            "kv_default",
            "acct:2",
            crate::storage::schema::Value::Integer(3),
            None,
            false,
        )
        .unwrap();

        let first = stream.poll_next().expect("first prefix event");
        let second = stream.poll_next().expect("second prefix event");
        assert_eq!(first.key, "acct:1");
        assert_eq!(second.key, "acct:2");
        assert!(stream.poll_next().is_none());
    }

    #[test]
    fn watch_stream_resume_from_lsn_delivers_missed_events_without_duplicates() {
        let r = rt();
        let ops = super::KvAtomicOps::new(&r);
        let mut stream = r.kv_watch_subscribe("kv_default", "resume", None);

        let mut last_seen_lsn = 0;
        for value in 0..5 {
            ops.set(
                CollectionModel::Kv,
                "kv_default",
                "resume",
                crate::storage::schema::Value::Integer(value),
                None,
                false,
            )
            .unwrap();
            last_seen_lsn = stream.poll_next().expect("initial event").lsn;
        }
        drop(stream);

        for value in 5..55 {
            ops.set(
                CollectionModel::Kv,
                "kv_default",
                "resume",
                crate::storage::schema::Value::Integer(value),
                None,
                false,
            )
            .unwrap();
        }

        let mut resumed = r.kv_watch_subscribe("kv_default", "resume", Some(last_seen_lsn));
        let mut lsns = Vec::new();
        while let Some(event) = resumed.poll_next() {
            lsns.push(event.lsn);
            if lsns.len() == 50 {
                break;
            }
        }

        assert_eq!(lsns.len(), 50);
        assert!(lsns.iter().all(|lsn| *lsn > last_seen_lsn));
        assert!(lsns.windows(2).all(|pair| pair[0] < pair[1]));
        assert!(resumed.poll_next().is_none());
    }

    #[test]
    fn watch_stream_slow_consumer_drops_oldest_buffered_events() {
        let r = rt();
        let ops = super::KvAtomicOps::new(&r);
        let mut stream = r.kv_watch_subscribe("kv_default", "slow", None);

        for value in 0..10_000 {
            ops.set(
                CollectionModel::Kv,
                "kv_default",
                "slow",
                crate::storage::schema::Value::Integer(value),
                None,
                false,
            )
            .unwrap();
        }

        let event = stream.poll_next().expect("tail event after drops");
        assert!(event.lsn > 1);
        assert!(event.dropped_event_count > 0);
        assert_eq!(stream.dropped_event_count(), event.dropped_event_count);
        assert_eq!(r.stats().kv.watch_drops, event.dropped_event_count);
    }

    #[test]
    fn watch_stream_idle_timeout_closes_subscription() {
        let r = rt();
        r.execute_query("SET CONFIG red.config.kv.watch.idle_timeout_ms = 1")
            .unwrap();

        let mut stream = r.kv_watch_subscribe("kv_default", "idle", None);
        assert_eq!(r.stats().kv.watch_streams_active, 1);
        std::thread::sleep(std::time::Duration::from_millis(5));

        assert!(stream.poll_next().is_none());
        assert_eq!(r.stats().kv.watch_streams_active, 0);
    }

    #[test]
    fn watch_stream_does_not_emit_rolled_back_put() {
        let r = rt();
        let mut stream = r.kv_watch_subscribe("kv_default", "rollback_key", None);

        r.execute_query("BEGIN").unwrap();
        r.execute_query("KV PUT rollback_key = 'dirty'").unwrap();
        r.execute_query("ROLLBACK").unwrap();

        assert!(stream.poll_next().is_none());
    }
}
