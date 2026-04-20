//! Append-only log collection backed by UnifiedStore.

use std::collections::HashMap;
use std::sync::Arc;

use super::id::{LogId, LogIdGenerator};
use crate::storage::schema::Value;
use crate::storage::unified::entity::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};
use crate::storage::unified::store::UnifiedStore;

/// Retention policy for log collections.
#[derive(Debug, Clone, Default)]
pub enum LogRetention {
    /// Keep entries for N days, then auto-delete.
    Days(u64),
    /// Keep at most N entries (oldest evicted first).
    MaxEntries(u64),
    /// Keep total size under N bytes (oldest evicted first).
    MaxBytes(u64),
    /// Keep forever (no automatic cleanup).
    #[default]
    Forever,
}

/// Configuration for a log collection.
#[derive(Debug, Clone)]
pub struct LogCollectionConfig {
    pub name: String,
    pub columns: Vec<String>,
    pub retention: LogRetention,
    pub batch_size: usize,
}

impl LogCollectionConfig {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            columns: Vec::new(),
            retention: LogRetention::Forever,
            batch_size: 64,
        }
    }
}

/// A single log entry with timestamp-based ID and user fields.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub id: LogId,
    pub fields: HashMap<String, Value>,
}

/// Append-only log collection.
pub struct LogCollection {
    config: LogCollectionConfig,
    id_gen: LogIdGenerator,
    store: Arc<UnifiedStore>,
    write_buffer: std::sync::Mutex<Vec<UnifiedEntity>>,
}

impl LogCollection {
    pub fn new(store: Arc<UnifiedStore>, config: LogCollectionConfig) -> Self {
        let _ = store.get_or_create_collection(&config.name);

        // Restore ID generator from highest existing entry
        let id_gen = LogIdGenerator::new();
        if let Some(manager) = store.get_collection(&config.name) {
            let mut max_id = 0u64;
            manager.for_each_entity(|entity| {
                if let Some(row) = entity.data.as_row() {
                    if let Some(Value::UnsignedInteger(id)) = row.get_field("id") {
                        if *id > max_id {
                            max_id = *id;
                        }
                    }
                }
                true
            });
            if max_id > 0 {
                id_gen.restore(max_id);
            }
        }

        Self {
            config,
            id_gen,
            store,
            write_buffer: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Append a single log entry. Returns the assigned ID.
    pub fn append(&self, fields: HashMap<String, Value>) -> LogId {
        let id = self.id_gen.next();

        let mut named = HashMap::with_capacity(fields.len() + 1);
        named.insert("id".to_string(), Value::UnsignedInteger(id.raw()));
        for (k, v) in fields {
            named.insert(k, v);
        }

        let entity = UnifiedEntity::new(
            EntityId::new(0),
            EntityKind::TableRow {
                table: Arc::from(self.config.name.as_str()),
                row_id: 0,
            },
            EntityData::Row(RowData {
                columns: Vec::new(),
                named: Some(named),
                schema: None,
            }),
        );

        let batch_size = self.config.batch_size;
        let should_flush = {
            let mut buf = self.write_buffer.lock().unwrap_or_else(|e| e.into_inner());
            buf.push(entity);
            buf.len() >= batch_size
        };

        if should_flush {
            self.flush_buffer();
        }

        id
    }

    /// Append a log entry from (key, value) pairs.
    pub fn append_fields(&self, fields: Vec<(&str, Value)>) -> LogId {
        let map: HashMap<String, Value> = fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        self.append(map)
    }

    /// Flush the write buffer to storage.
    pub fn flush_buffer(&self) {
        let entities = {
            let mut buf = self.write_buffer.lock().unwrap_or_else(|e| e.into_inner());
            std::mem::take(&mut *buf)
        };

        if entities.is_empty() {
            return;
        }

        for entity in entities {
            let _ = self.store.insert_auto(&self.config.name, entity);
        }
    }

    /// Query recent entries (newest first).
    pub fn recent(&self, limit: usize) -> Vec<LogEntry> {
        self.flush_buffer();

        let manager = match self.store.get_collection(&self.config.name) {
            Some(m) => m,
            None => return Vec::new(),
        };

        // Phase 1: collect top-k log IDs + entity IDs using a bounded min-heap.
        // Only stores (log_id, entity_id) — no field cloning until phase 2.
        use std::cmp::Reverse;
        use std::collections::BinaryHeap;

        let mut heap: BinaryHeap<Reverse<(u64, crate::storage::unified::entity::EntityId)>> =
            BinaryHeap::with_capacity(limit + 1);

        manager.for_each_entity(|entity| {
            if let Some(row) = entity.data.as_row() {
                let id_val = row
                    .get_field("id")
                    .and_then(|v| match v {
                        Value::UnsignedInteger(n) => Some(*n),
                        _ => None,
                    })
                    .unwrap_or(0);

                if heap.len() < limit {
                    heap.push(Reverse((id_val, entity.id)));
                } else if let Some(&Reverse((min_id, _))) = heap.peek() {
                    if id_val > min_id {
                        heap.pop();
                        heap.push(Reverse((id_val, entity.id)));
                    }
                }
            }
            true
        });

        // Phase 2: fetch full entities only for top-k (avoids cloning all fields)
        let mut top_ids: Vec<(u64, crate::storage::unified::entity::EntityId)> = heap
            .into_vec()
            .into_iter()
            .map(|Reverse(pair)| pair)
            .collect();
        top_ids.sort_by(|a, b| b.0.cmp(&a.0)); // newest first

        top_ids
            .into_iter()
            .filter_map(|(log_id, entity_id)| {
                let entity = manager.get(entity_id)?;
                let row = entity.data.as_row()?;
                let mut fields = HashMap::new();
                for (key, value) in row.iter_fields() {
                    if key != "id" {
                        fields.insert(key.to_string(), value.clone());
                    }
                }
                Some(LogEntry {
                    id: LogId(log_id),
                    fields,
                })
            })
            .collect()
    }

    /// Query entries within a time range (by ID boundaries).
    pub fn range(&self, from_id: LogId, to_id: LogId, limit: usize) -> Vec<LogEntry> {
        self.flush_buffer();

        let manager = match self.store.get_collection(&self.config.name) {
            Some(m) => m,
            None => return Vec::new(),
        };

        let mut entries = Vec::new();
        manager.for_each_entity(|entity| {
            if let Some(row) = entity.data.as_row() {
                let id_val = row
                    .get_field("id")
                    .and_then(|v| match v {
                        Value::UnsignedInteger(n) => Some(*n),
                        _ => None,
                    })
                    .unwrap_or(0);

                if id_val >= from_id.raw() && id_val <= to_id.raw() {
                    let mut fields = HashMap::new();
                    for (key, value) in row.iter_fields() {
                        if key != "id" {
                            fields.insert(key.to_string(), value.clone());
                        }
                    }
                    entries.push(LogEntry {
                        id: LogId(id_val),
                        fields,
                    });
                }
            }
            true
        });

        entries.sort_by(|a, b| a.id.cmp(&b.id));
        entries.truncate(limit);
        entries
    }

    /// Apply retention policy: delete entries older than the threshold.
    pub fn apply_retention(&self) -> u64 {
        match &self.config.retention {
            LogRetention::Forever => 0,
            LogRetention::Days(days) => {
                let cutoff_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64
                    - days * 86_400_000;
                let cutoff_id = LogId::from_ms(cutoff_ms);
                self.delete_before(cutoff_id)
            }
            LogRetention::MaxEntries(max) => {
                let manager = match self.store.get_collection(&self.config.name) {
                    Some(m) => m,
                    None => return 0,
                };

                let mut ids: Vec<(u64, EntityId)> = Vec::new();
                manager.for_each_entity(|entity| {
                    if let Some(row) = entity.data.as_row() {
                        if let Some(Value::UnsignedInteger(log_id)) = row.get_field("id") {
                            ids.push((*log_id, entity.id));
                        }
                    }
                    true
                });

                if ids.len() as u64 <= *max {
                    return 0;
                }

                ids.sort_by_key(|(log_id, _)| *log_id);
                let to_delete = ids.len() as u64 - max;
                let mut deleted = 0u64;
                for (_, entity_id) in ids.iter().take(to_delete as usize) {
                    if self
                        .store
                        .delete(&self.config.name, *entity_id)
                        .unwrap_or(false)
                    {
                        deleted += 1;
                    }
                }
                deleted
            }
            LogRetention::MaxBytes(max_bytes) => {
                let manager = match self.store.get_collection(&self.config.name) {
                    Some(m) => m,
                    None => return 0,
                };

                // Collect (log_id, entity_id, approx_size) sorted by time
                let mut entries: Vec<(u64, EntityId, u64)> = Vec::new();
                manager.for_each_entity(|entity| {
                    if let Some(row) = entity.data.as_row() {
                        let log_id = row
                            .get_field("id")
                            .and_then(|v| match v {
                                Value::UnsignedInteger(n) => Some(*n),
                                _ => None,
                            })
                            .unwrap_or(0);

                        // Approximate entry size: 8 bytes per field + value sizes
                        let mut size = 8u64; // id field
                        for (key, value) in row.iter_fields() {
                            size += key.len() as u64 + estimate_value_size(value);
                        }
                        entries.push((log_id, entity.id, size));
                    }
                    true
                });

                entries.sort_by_key(|(log_id, _, _)| *log_id);

                let total_size: u64 = entries.iter().map(|(_, _, s)| s).sum();
                if total_size <= *max_bytes {
                    return 0;
                }

                // Delete oldest entries until under budget
                let mut to_free = total_size - max_bytes;
                let mut deleted = 0u64;
                for (_, entity_id, size) in &entries {
                    if to_free == 0 {
                        break;
                    }
                    if self
                        .store
                        .delete(&self.config.name, *entity_id)
                        .unwrap_or(false)
                    {
                        deleted += 1;
                        to_free = to_free.saturating_sub(*size);
                    }
                }
                deleted
            }
        }
    }

    fn delete_before(&self, cutoff: LogId) -> u64 {
        let manager = match self.store.get_collection(&self.config.name) {
            Some(m) => m,
            None => return 0,
        };

        let mut to_delete = Vec::new();
        manager.for_each_entity(|entity| {
            if let Some(row) = entity.data.as_row() {
                if let Some(Value::UnsignedInteger(log_id)) = row.get_field("id") {
                    if *log_id < cutoff.raw() {
                        to_delete.push(entity.id);
                    }
                }
            }
            true
        });

        let mut deleted = 0u64;
        for entity_id in to_delete {
            if self
                .store
                .delete(&self.config.name, entity_id)
                .unwrap_or(false)
            {
                deleted += 1;
            }
        }
        deleted
    }

    /// Total number of entries.
    pub fn len(&self) -> usize {
        self.flush_buffer();
        self.store
            .get_collection(&self.config.name)
            .map(|m| m.stats().total_entities)
            .unwrap_or(0)
    }

    /// Config reference.
    pub fn config(&self) -> &LogCollectionConfig {
        &self.config
    }
}

fn estimate_value_size(value: &Value) -> u64 {
    match value {
        Value::Null => 1,
        Value::Boolean(_) => 1,
        Value::Integer(_) | Value::UnsignedInteger(_) | Value::Float(_) => 8,
        Value::Text(s) => s.len() as u64,
        Value::Blob(b) => b.len() as u64,
        Value::Vector(v) => v.len() as u64 * 4,
        Value::Array(a) => a.iter().map(estimate_value_size).sum::<u64>() + 8,
        _ => 16, // conservative default for other types
    }
}

impl Drop for LogCollection {
    fn drop(&mut self) {
        self.flush_buffer();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_store() -> Arc<UnifiedStore> {
        Arc::new(UnifiedStore::new())
    }

    #[test]
    fn test_append_and_query() {
        let store = test_store();
        let log = LogCollection::new(store, LogCollectionConfig::new("test_log"));

        let id1 = log.append_fields(vec![
            ("level", Value::text("info".into())),
            ("message", Value::text("hello".into())),
        ]);
        let id2 = log.append_fields(vec![
            ("level", Value::text("error".into())),
            ("message", Value::text("oops".into())),
        ]);

        assert!(id2.raw() > id1.raw());

        let recent = log.recent(10);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].id, id2); // newest first
        assert_eq!(recent[1].id, id1);
    }

    #[test]
    fn test_retention_max_entries() {
        let store = test_store();
        let mut config = LogCollectionConfig::new("retention_test");
        config.retention = LogRetention::MaxEntries(3);
        config.batch_size = 1;

        let log = LogCollection::new(store, config);

        for i in 0..5 {
            log.append_fields(vec![("seq", Value::Integer(i))]);
        }

        assert_eq!(log.len(), 5);
        let deleted = log.apply_retention();
        assert_eq!(deleted, 2);
        assert_eq!(log.len(), 3);
    }

    #[test]
    fn test_retention_max_bytes() {
        let store = test_store();
        let mut config = LogCollectionConfig::new("bytes_retention_test");
        config.retention = LogRetention::MaxBytes(200);
        config.batch_size = 1;

        let log = LogCollection::new(store, config);

        // Insert entries with known sizes (~30-50 bytes each)
        for i in 0..10 {
            log.append_fields(vec![("msg", Value::text(format!("entry-{}", i)))]);
        }

        let before = log.len();
        assert_eq!(before, 10);

        let deleted = log.apply_retention();
        assert!(
            deleted > 0,
            "should delete some entries to fit under 200 bytes"
        );
        assert!(log.len() < 10, "should have fewer entries after retention");
    }

    #[test]
    fn test_batch_buffering() {
        let store = test_store();
        let mut config = LogCollectionConfig::new("batch_test");
        config.batch_size = 4;

        let log = LogCollection::new(store.clone(), config);

        // Insert 3 — should stay in buffer (batch_size = 4)
        for _ in 0..3 {
            log.append_fields(vec![("msg", Value::text("buffered".into()))]);
        }

        // Buffer not flushed yet — store might be empty
        // But recent() flushes first
        let entries = log.recent(10);
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn test_id_is_time_ordered() {
        let store = test_store();
        let log = LogCollection::new(store, LogCollectionConfig::new("time_test"));

        let ids: Vec<LogId> = (0..100)
            .map(|i| log.append_fields(vec![("i", Value::Integer(i))]))
            .collect();

        for i in 1..ids.len() {
            assert!(ids[i].raw() > ids[i - 1].raw());
            assert!(ids[i].timestamp_us() >= ids[i - 1].timestamp_us());
        }
    }
}
