use crate::api::{RedDBError, RedDBResult};
use crate::application::entity::CreateKvInput;
use crate::application::ports::RuntimeEntityPort;
use crate::storage::query::ast::KvQuery;
use crate::storage::query::modes::QueryMode;
use crate::storage::query::unified::{UnifiedRecord, UnifiedResult};
use crate::storage::schema::Value;
use crate::storage::unified::MetadataValue;
use crate::storage::{EntityData, EntityId};

use std::sync::Arc;

use super::{RedDBRuntime, RuntimeQueryResult};

pub const DEFAULT_KV_COLLECTION: &str = "kv_default";

pub struct KvAtomicOps<'a> {
    runtime: &'a RedDBRuntime,
}

#[derive(Debug, Clone)]
struct KvTarget {
    collection: String,
    key: String,
}

#[derive(Debug, Clone)]
struct KvEntry {
    id: EntityId,
    value: Value,
}

impl<'a> KvAtomicOps<'a> {
    pub fn new(runtime: &'a RedDBRuntime) -> Self {
        Self { runtime }
    }

    pub fn execute(&self, raw_query: &str, query: &KvQuery) -> RedDBResult<RuntimeQueryResult> {
        match query {
            KvQuery::Put {
                key,
                value,
                ttl_ms,
                if_not_exists,
            } => self.set(raw_query, key, value.clone(), *ttl_ms, *if_not_exists),
            KvQuery::Get { key } => self.get(raw_query, key),
            KvQuery::Delete { key } => self.delete(raw_query, key),
            KvQuery::Cas {
                key,
                expected,
                new_value,
                ttl_ms,
            } => self.compare_and_set(raw_query, key, expected, new_value.clone(), *ttl_ms),
            KvQuery::Incr { key, by, ttl_ms } => self.incr_query(raw_query, key, *by, *ttl_ms),
        }
    }

    pub fn set(
        &self,
        raw_query: &str,
        key: &str,
        value: Value,
        ttl_ms: Option<u64>,
        if_not_exists: bool,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.runtime
            .check_write(crate::runtime::write_gate::WriteKind::Dml)?;
        let target = self.resolve_target(key, true);

        if let Some(existing) = self.find_live_entry(&target)? {
            if if_not_exists {
                return Ok(RuntimeQueryResult::dml_result(
                    raw_query.to_string(),
                    0,
                    "put",
                    "runtime-kv",
                ));
            }
            self.delete_entity(&target.collection, existing.id)?;
        }

        let mut metadata = Vec::new();
        if let Some(ms) = ttl_ms {
            metadata.push((
                "_ttl_ms".to_string(),
                if ms <= i64::MAX as u64 {
                    MetadataValue::Int(ms as i64)
                } else {
                    MetadataValue::Timestamp(ms)
                },
            ));
        }

        self.runtime.create_kv(CreateKvInput {
            collection: target.collection.clone(),
            key: target.key.clone(),
            value,
            metadata,
        })?;
        self.runtime
            .kv_clear_deleted(&target.collection, &target.key);
        self.runtime.note_table_write(&target.collection);

        Ok(RuntimeQueryResult::dml_result(
            raw_query.to_string(),
            1,
            "put",
            "runtime-kv",
        ))
    }

    pub fn get(&self, raw_query: &str, key: &str) -> RedDBResult<RuntimeQueryResult> {
        let target = self.resolve_target(key, false);
        let mut result = UnifiedResult::with_columns(vec!["key".to_string(), "value".to_string()]);
        if let Some(entry) = self.find_live_entry(&target)? {
            let mut record = UnifiedRecord::new();
            record.set("key", Value::text(target.key));
            record.set("value", entry.value);
            result.push(record);
        }
        Ok(RuntimeQueryResult {
            query: raw_query.to_string(),
            mode: QueryMode::Sql,
            statement: "get",
            engine: "runtime-kv",
            affected_rows: 0,
            statement_type: "select",
            result,
        })
    }

    pub fn delete(&self, raw_query: &str, key: &str) -> RedDBResult<RuntimeQueryResult> {
        self.runtime
            .check_write(crate::runtime::write_gate::WriteKind::Dml)?;
        let target = self.resolve_target(key, false);
        let affected = if let Some(entry) = self.find_live_entry(&target)? {
            self.delete_entity(&target.collection, entry.id)?;
            self.runtime
                .kv_mark_deleted(&target.collection, &target.key);
            self.runtime.note_table_write(&target.collection);
            1
        } else {
            0
        };
        Ok(RuntimeQueryResult::dml_result(
            raw_query.to_string(),
            affected,
            "delete",
            "runtime-kv",
        ))
    }

    pub fn compare_and_set(
        &self,
        raw_query: &str,
        key: &str,
        expected: &Value,
        new_value: Value,
        ttl_ms: Option<u64>,
    ) -> RedDBResult<RuntimeQueryResult> {
        self.runtime
            .check_write(crate::runtime::write_gate::WriteKind::Dml)?;
        let target = self.resolve_target(key, true);
        let lock = self.runtime.kv_atomic_lock(&target.collection, &target.key);
        let _guard = lock.lock();

        let existing = self.find_live_entry(&target)?;
        let observed = existing
            .as_ref()
            .map(|entry| entry.value.clone())
            .unwrap_or(Value::Null);

        let ok = values_typed_equal(&observed, expected);
        let current = if ok {
            if let Some(entry) = existing {
                self.delete_entity(&target.collection, entry.id)?;
            }
            self.runtime.create_kv(CreateKvInput {
                collection: target.collection.clone(),
                key: target.key.clone(),
                value: new_value.clone(),
                metadata: ttl_metadata_fields(ttl_ms),
            })?;
            self.runtime
                .kv_clear_deleted(&target.collection, &target.key);
            self.runtime.note_table_write(&target.collection);
            new_value
        } else {
            observed
        };

        Ok(cas_result(raw_query, ok, current))
    }

    pub fn incr(
        &self,
        collection: &str,
        key: &str,
        by: i64,
        ttl_ms: Option<u64>,
    ) -> RedDBResult<i64> {
        self.runtime
            .check_write(crate::runtime::write_gate::WriteKind::Dml)?;
        let lock = self.runtime.kv_atomic_lock(collection, key);
        let _guard = lock.lock();
        let target = KvTarget {
            collection: collection.to_string(),
            key: key.to_string(),
        };

        match self.find_live_entry(&target)? {
            Some(entry) => {
                let current = kv_counter_i64(&entry.value)?;
                let next = current
                    .checked_add(by)
                    .ok_or_else(|| RedDBError::Query("INCR/DECR counter overflow".to_string()))?;
                self.update_counter_value(&target, entry.id, next, ttl_ms)?;
                Ok(next)
            }
            None => {
                self.runtime.create_kv(CreateKvInput {
                    collection: collection.to_string(),
                    key: key.to_string(),
                    value: Value::Integer(by),
                    metadata: ttl_metadata_fields(ttl_ms),
                })?;
                self.runtime.kv_clear_deleted(collection, key);
                self.runtime.note_table_write(collection);
                Ok(by)
            }
        }
    }

    pub fn decr(
        &self,
        collection: &str,
        key: &str,
        by: i64,
        ttl_ms: Option<u64>,
    ) -> RedDBResult<i64> {
        let delta = by
            .checked_neg()
            .ok_or_else(|| RedDBError::Query("DECR BY value overflows i64".to_string()))?;
        self.incr(collection, key, delta, ttl_ms)
    }

    fn incr_query(
        &self,
        raw_query: &str,
        key: &str,
        by: i64,
        ttl_ms: Option<u64>,
    ) -> RedDBResult<RuntimeQueryResult> {
        let target = self.resolve_target(key, true);
        let value = self.incr(&target.collection, &target.key, by, ttl_ms)?;
        let mut result = UnifiedResult::with_columns(vec!["key".to_string(), "value".to_string()]);
        let mut record = UnifiedRecord::new();
        record.set("key", Value::text(target.key));
        record.set("value", Value::Integer(value));
        result.push(record);
        Ok(RuntimeQueryResult {
            query: raw_query.to_string(),
            mode: QueryMode::Sql,
            statement: "incr",
            engine: "runtime-kv",
            affected_rows: 1,
            statement_type: "update",
            result,
        })
    }

    fn resolve_target(&self, key: &str, create_default: bool) -> KvTarget {
        let store = self.runtime.db().store();
        if let Some((collection, rest)) = key.split_once('.') {
            if !collection.is_empty()
                && !rest.is_empty()
                && store.get_collection(collection).is_some()
            {
                return KvTarget {
                    collection: collection.to_string(),
                    key: rest.to_string(),
                };
            }
        }
        if create_default {
            let _ = store.get_or_create_collection(DEFAULT_KV_COLLECTION);
        }
        KvTarget {
            collection: DEFAULT_KV_COLLECTION.to_string(),
            key: key.to_string(),
        }
    }

    fn find_live_entry(&self, target: &KvTarget) -> RedDBResult<Option<KvEntry>> {
        if self.runtime.kv_is_deleted(&target.collection, &target.key) {
            return Ok(None);
        }
        let store = self.runtime.db().store();
        let Some(manager) = store.get_collection(&target.collection) else {
            return Ok(None);
        };

        for entity in manager.query_all(|_| true) {
            if manager.get(entity.id).is_none() {
                continue;
            }
            let EntityData::Row(row) = &entity.data else {
                continue;
            };
            let Some(Value::Text(key)) = row.get_field("key") else {
                continue;
            };
            if key.as_ref() != target.key {
                continue;
            }
            if self.is_expired(&target.collection, &entity) {
                self.delete_entity(&target.collection, entity.id)?;
                continue;
            }
            let value = row.get_field("value").cloned().unwrap_or(Value::Null);
            return Ok(Some(KvEntry {
                id: entity.id,
                value,
            }));
        }
        Ok(None)
    }

    fn is_expired(&self, collection: &str, entity: &crate::storage::UnifiedEntity) -> bool {
        let Some(metadata) = self
            .runtime
            .db()
            .store()
            .get_metadata(collection, entity.id)
        else {
            return false;
        };
        let now_ms = current_unix_ms();
        if let Some(expires_at) = metadata.get("_expires_at").and_then(metadata_u64) {
            return expires_at <= now_ms;
        }
        let Some(ttl_ms) = metadata.get("_ttl_ms").and_then(metadata_u64).or_else(|| {
            metadata
                .get("_ttl")
                .and_then(metadata_u64)
                .and_then(|s| s.checked_mul(1000))
        }) else {
            return false;
        };
        entity
            .created_at
            .saturating_mul(1000)
            .saturating_add(ttl_ms)
            <= now_ms
    }

    fn delete_entity(&self, collection: &str, id: EntityId) -> RedDBResult<()> {
        let store = self.runtime.db().store();
        if let Some(manager) = store.get_collection(collection) {
            if let Some(mut entity) = manager.get(id) {
                if let EntityData::Row(row) = &mut entity.data {
                    if let Some(named) = row.named.as_mut() {
                        named.insert(
                            "key".to_string(),
                            Value::text(format!("__reddb_deleted_kv_{}", id.raw())),
                        );
                    }
                }
                let _ = manager.update(entity);
            }
        }
        let deleted = store
            .delete(collection, id)
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        if deleted {
            store.context_index().remove_entity(id);
            self.runtime.cdc_emit(
                crate::replication::cdc::ChangeOperation::Delete,
                collection,
                id.raw(),
                "entity",
            );
        }
        Ok(())
    }

    fn update_counter_value(
        &self,
        target: &KvTarget,
        id: EntityId,
        next: i64,
        ttl_ms: Option<u64>,
    ) -> RedDBResult<()> {
        let store = self.runtime.db().store();
        let manager = store.get_collection(&target.collection).ok_or_else(|| {
            RedDBError::NotFound(format!("collection not found: {}", target.collection))
        })?;
        let mut entity = manager.get(id).ok_or_else(|| {
            RedDBError::NotFound(format!(
                "KV key disappeared during atomic update: {}",
                target.key
            ))
        })?;

        let EntityData::Row(row) = &mut entity.data else {
            return Err(RedDBError::Query(format!(
                "KV key {} is not backed by a table row",
                target.key
            )));
        };
        let named = row.named.as_mut().ok_or_else(|| {
            RedDBError::Query(format!("KV key {} has no named fields", target.key))
        })?;
        named.insert("value".to_string(), Value::Integer(next));
        entity.updated_at = current_unix_secs();

        manager
            .update(entity.clone())
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        if let Some(ttl_ms) = ttl_ms {
            let mut metadata = store
                .get_metadata(&target.collection, id)
                .unwrap_or_default();
            metadata.set("_ttl_ms", ttl_metadata_value(ttl_ms));
            manager
                .set_metadata(id, metadata)
                .map_err(|err| RedDBError::Internal(err.to_string()))?;
        }
        store
            .persist_entities_to_pager(&target.collection, std::slice::from_ref(&entity))
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        self.runtime.note_table_write(&target.collection);
        self.runtime.cdc_emit(
            crate::replication::cdc::ChangeOperation::Update,
            &target.collection,
            id.raw(),
            "kv",
        );
        Ok(())
    }
}

impl RedDBRuntime {
    pub(crate) fn kv_atomic_lock(
        &self,
        collection: &str,
        key: &str,
    ) -> Arc<parking_lot::Mutex<()>> {
        let map_key = (collection.to_string(), key.to_string());
        if let Some(lock) = self.inner.kv_atomic_locks.read().get(&map_key).cloned() {
            return lock;
        }

        let mut locks = self.inner.kv_atomic_locks.write();
        locks
            .entry(map_key)
            .or_insert_with(|| Arc::new(parking_lot::Mutex::new(())))
            .clone()
    }

    fn kv_mark_deleted(&self, collection: &str, key: &str) {
        self.inner
            .kv_deleted_keys
            .write()
            .insert((collection.to_string(), key.to_string()));
    }

    fn kv_clear_deleted(&self, collection: &str, key: &str) {
        self.inner
            .kv_deleted_keys
            .write()
            .remove(&(collection.to_string(), key.to_string()));
    }

    fn kv_is_deleted(&self, collection: &str, key: &str) -> bool {
        self.inner
            .kv_deleted_keys
            .read()
            .contains(&(collection.to_string(), key.to_string()))
    }
}

fn kv_counter_i64(value: &Value) -> RedDBResult<i64> {
    match value {
        Value::Integer(v) => Ok(*v),
        Value::UnsignedInteger(v) if *v <= i64::MAX as u64 => Ok(*v as i64),
        _ => Err(RedDBError::Query(
            "INCR/DECR requires the existing KV value to be an integer".to_string(),
        )),
    }
}

fn cas_result(raw_query: &str, ok: bool, current: Value) -> RuntimeQueryResult {
    let mut result = UnifiedResult::with_columns(vec!["ok".to_string(), "current".to_string()]);
    let mut record = UnifiedRecord::new();
    record.set("ok", Value::Boolean(ok));
    record.set("current", current);
    result.push(record);
    RuntimeQueryResult {
        query: raw_query.to_string(),
        mode: QueryMode::Sql,
        statement: "cas",
        engine: "runtime-kv",
        affected_rows: u64::from(ok),
        statement_type: "update",
        result,
    }
}

fn values_typed_equal(left: &Value, right: &Value) -> bool {
    std::mem::discriminant(left) == std::mem::discriminant(right) && left == right
}

fn ttl_metadata_fields(ttl_ms: Option<u64>) -> Vec<(String, MetadataValue)> {
    ttl_ms
        .map(|ttl| vec![("_ttl_ms".to_string(), ttl_metadata_value(ttl))])
        .unwrap_or_default()
}

fn ttl_metadata_value(ttl_ms: u64) -> MetadataValue {
    if ttl_ms <= i64::MAX as u64 {
        MetadataValue::Int(ttl_ms as i64)
    } else {
        MetadataValue::Timestamp(ttl_ms)
    }
}

fn metadata_u64(value: &MetadataValue) -> Option<u64> {
    match value {
        MetadataValue::Int(v) if *v >= 0 => Some(*v as u64),
        MetadataValue::Timestamp(v) => Some(*v),
        MetadataValue::Float(v) if v.is_finite() && *v >= 0.0 && v.fract().abs() < f64::EPSILON => {
            Some(v.trunc() as u64)
        }
        MetadataValue::String(v) => v.parse::<u64>().ok(),
        _ => None,
    }
}

fn current_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn current_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use crate::storage::schema::Value;
    use crate::{RedDBOptions, RedDBRuntime};

    #[test]
    fn put_get_delete_use_kv_default() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();

        let put = rt.execute_query("PUT session = 'abc'").unwrap();
        assert_eq!(put.affected_rows, 1);
        assert!(rt
            .db()
            .store()
            .get_collection(super::DEFAULT_KV_COLLECTION)
            .is_some());

        let got = rt.execute_query("GET session").unwrap();
        assert_eq!(got.result.records.len(), 1);
        assert_eq!(
            got.result.records[0].get("value"),
            Some(&Value::text("abc"))
        );

        let deleted = rt.execute_query("DELETE session").unwrap();
        assert_eq!(deleted.affected_rows, 1);
        let missing = rt.execute_query("GET session").unwrap();
        assert!(missing.result.records.is_empty());
    }

    #[test]
    fn put_if_not_exists_does_not_overwrite_existing_key() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();

        rt.execute_query("PUT feature = 'old'").unwrap();
        let skipped = rt
            .execute_query("PUT feature = 'new' IF NOT EXISTS")
            .unwrap();
        assert_eq!(skipped.affected_rows, 0);

        let got = rt.execute_query("GET feature").unwrap();
        assert_eq!(
            got.result.records[0].get("value"),
            Some(&Value::text("old"))
        );
    }

    #[test]
    fn dotted_key_routes_to_existing_collection() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.db().store().get_or_create_collection("sessions");

        rt.execute_query("PUT sessions.42 = 'abc'").unwrap();

        let got = rt.execute_query("GET sessions.42").unwrap();
        assert_eq!(got.result.records.len(), 1);
        assert_eq!(got.result.records[0].get("key"), Some(&Value::text("42")));
        assert_eq!(
            got.result.records[0].get("value"),
            Some(&Value::text("abc"))
        );

        let default_lookup = rt.execute_query("GET 42").unwrap();
        assert!(default_lookup.result.records.is_empty());
    }

    #[test]
    fn get_skips_expired_put_ttl() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();

        rt.execute_query("PUT short = 'gone' EXPIRE 1 ms").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));

        let got = rt.execute_query("GET short").unwrap();
        assert!(got.result.records.is_empty());
    }

    #[test]
    fn incr_decr_initialize_and_return_post_update_value() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();

        let first = rt.execute_query("INCR visits BY 5").unwrap();
        assert_eq!(
            first.result.records[0].get("value"),
            Some(&Value::Integer(5))
        );

        let second = rt.execute_query("DECR visits BY 2").unwrap();
        assert_eq!(
            second.result.records[0].get("value"),
            Some(&Value::Integer(3))
        );

        let got = rt.execute_query("GET visits").unwrap();
        assert_eq!(got.result.records[0].get("value"), Some(&Value::Integer(3)));
    }

    #[test]
    fn incr_errors_on_non_integer_without_mutating() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("PUT visits = 'nope'").unwrap();

        let err = rt.execute_query("INCR visits").unwrap_err();
        assert!(err.to_string().contains("existing KV value"));

        let got = rt.execute_query("GET visits").unwrap();
        assert_eq!(
            got.result.records[0].get("value"),
            Some(&Value::text("nope"))
        );
    }

    #[test]
    fn cas_updates_only_when_expected_value_matches() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("PUT feature = 'old'").unwrap();

        let success = rt
            .execute_query("CAS feature EXPECT 'old' SET 'new'")
            .unwrap();
        assert_eq!(success.affected_rows, 1);
        assert_eq!(
            success.result.records[0].get("ok"),
            Some(&Value::Boolean(true))
        );
        assert_eq!(
            success.result.records[0].get("current"),
            Some(&Value::text("new"))
        );

        let failure = rt
            .execute_query("CAS feature EXPECT 'old' SET 'stale'")
            .unwrap();
        assert_eq!(failure.affected_rows, 0);
        assert_eq!(
            failure.result.records[0].get("ok"),
            Some(&Value::Boolean(false))
        );
        assert_eq!(
            failure.result.records[0].get("current"),
            Some(&Value::text("new"))
        );

        let got = rt.execute_query("GET feature").unwrap();
        assert_eq!(
            got.result.records[0].get("value"),
            Some(&Value::text("new"))
        );
    }

    #[test]
    fn cas_expect_null_creates_absent_key() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();

        let created = rt
            .execute_query("CAS session EXPECT NULL SET 'abc'")
            .unwrap();
        assert_eq!(
            created.result.records[0].get("ok"),
            Some(&Value::Boolean(true))
        );
        assert_eq!(
            created.result.records[0].get("current"),
            Some(&Value::text("abc"))
        );

        let got = rt.execute_query("GET session").unwrap();
        assert_eq!(
            got.result.records[0].get("value"),
            Some(&Value::text("abc"))
        );
    }

    #[test]
    fn cas_uses_typed_equality_without_numeric_coercion() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();
        rt.execute_query("PUT typed = 1").unwrap();

        let failure = rt.execute_query("CAS typed EXPECT 1.0 SET 2").unwrap();
        assert_eq!(
            failure.result.records[0].get("ok"),
            Some(&Value::Boolean(false))
        );
        assert_eq!(
            failure.result.records[0].get("current"),
            Some(&Value::Integer(1))
        );
    }

    #[test]
    fn cas_ttl_applies_on_success() {
        let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap();

        rt.execute_query("CAS short EXPECT NULL SET 'gone' EXPIRE 1 ms")
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));

        let got = rt.execute_query("GET short").unwrap();
        assert!(got.result.records.is_empty());
    }

    #[test]
    fn concurrent_incr_converges() {
        let rt =
            std::sync::Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap());
        let mut handles = Vec::new();

        for _ in 0..100 {
            let rt = std::sync::Arc::clone(&rt);
            handles.push(std::thread::spawn(move || {
                rt.execute_query("INCR counter").unwrap();
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let got = rt.execute_query("GET counter").unwrap();
        assert_eq!(
            got.result.records[0].get("value"),
            Some(&Value::Integer(100))
        );
    }

    #[test]
    fn concurrent_cas_same_expected_succeeds_once() {
        let rt =
            std::sync::Arc::new(RedDBRuntime::with_options(RedDBOptions::in_memory()).unwrap());
        rt.execute_query("PUT latch = 0").unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(32));
        let mut handles = Vec::new();

        for _ in 0..32 {
            let rt = std::sync::Arc::clone(&rt);
            let barrier = std::sync::Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                barrier.wait();
                let result = rt.execute_query("CAS latch EXPECT 0 SET 1").unwrap();
                result.result.records[0].get("ok") == Some(&Value::Boolean(true))
            }));
        }

        let successes = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .filter(|ok| *ok)
            .count();

        assert_eq!(successes, 1);
        let got = rt.execute_query("GET latch").unwrap();
        assert_eq!(got.result.records[0].get("value"), Some(&Value::Integer(1)));
    }
}
