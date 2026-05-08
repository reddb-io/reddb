use crate::api::{RedDBError, RedDBResult};
use crate::application::entity::CreateKvInput;
use crate::application::ports::RuntimeEntityPort;
use crate::storage::query::ast::KvQuery;
use crate::storage::query::modes::QueryMode;
use crate::storage::query::unified::{UnifiedRecord, UnifiedResult};
use crate::storage::schema::Value;
use crate::storage::unified::MetadataValue;
use crate::storage::{EntityData, EntityId};

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
            key: target.key,
            value,
            metadata,
        })?;
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
        let store = self.runtime.db().store();
        let Some(manager) = store.get_collection(&target.collection) else {
            return Ok(None);
        };

        for entity in manager.query_all(|_| true) {
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
        let deleted = self
            .runtime
            .db()
            .store()
            .delete_batch(collection, &[id])
            .map_err(|err| RedDBError::Internal(err.to_string()))?;
        if !deleted.is_empty() {
            self.runtime.db().store().context_index().remove_entity(id);
            self.runtime.cdc_emit(
                crate::replication::cdc::ChangeOperation::Delete,
                collection,
                id.raw(),
                "entity",
            );
        }
        Ok(())
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
        rt.execute_query("CREATE TABLE sessions (key TEXT, value TEXT)")
            .unwrap();

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
}
