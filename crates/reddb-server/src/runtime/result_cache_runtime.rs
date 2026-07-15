use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};

use super::{RedDBRuntime, RuntimeQueryResult, RuntimeResultCacheEntry};

const RESULT_CACHE_BACKEND_KEY: &str = "runtime.result_cache.backend";
const RESULT_CACHE_DEFAULT_BACKEND: &str = "legacy";
const RESULT_CACHE_BLOB_NAMESPACE: &str = "runtime.result_cache";
const RESULT_CACHE_EMBEDDED_COLLECTION: &str = "red_internal_result_cache";
const RESULT_CACHE_TTL_SECS: u64 = 30;
const RESULT_CACHE_MAX_ENTRIES: usize = 1000;
const RESULT_CACHE_ENABLED_KEY: &str = "runtime.result_cache.enabled";
const RESULT_CACHE_TTL_KEY: &str = "runtime.result_cache.ttl_seconds";
const RESULT_CACHE_CAPACITY_KEY: &str = "runtime.result_cache.capacity_entries";
const RESULT_CACHE_PAYLOAD_MAGIC: &[u8; 8] = b"RDRC0001";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RuntimeResultCacheBackend {
    Legacy,
    BlobCache,
    Shadow,
}

fn trim_result_cache(
    map: &mut HashMap<String, RuntimeResultCacheEntry>,
    order: &mut VecDeque<String>,
    max_entries: usize,
) -> u64 {
    let mut evicted = 0u64;
    while map.len() > max_entries {
        if let Some(oldest) = order.pop_front() {
            if map.remove(&oldest).is_some() {
                evicted += 1;
            }
        } else {
            break;
        }
    }
    evicted
}

fn result_cache_fingerprint(result: &RuntimeQueryResult) -> String {
    format!(
        "{:?}|{}|{}|{}|{}|{:?}",
        result.result,
        result.query,
        result.statement,
        result.engine,
        result.affected_rows,
        result.statement_type
    )
}

fn mode_to_byte(mode: crate::storage::query::modes::QueryMode) -> u8 {
    match mode {
        crate::storage::query::modes::QueryMode::Sql => 0,
        crate::storage::query::modes::QueryMode::Gremlin => 1,
        crate::storage::query::modes::QueryMode::Cypher => 2,
        crate::storage::query::modes::QueryMode::Sparql => 3,
        crate::storage::query::modes::QueryMode::Path => 4,
        crate::storage::query::modes::QueryMode::Natural => 5,
        crate::storage::query::modes::QueryMode::Unknown => 255,
    }
}

fn mode_from_byte(byte: u8) -> Option<crate::storage::query::modes::QueryMode> {
    match byte {
        0 => Some(crate::storage::query::modes::QueryMode::Sql),
        1 => Some(crate::storage::query::modes::QueryMode::Gremlin),
        2 => Some(crate::storage::query::modes::QueryMode::Cypher),
        3 => Some(crate::storage::query::modes::QueryMode::Sparql),
        4 => Some(crate::storage::query::modes::QueryMode::Path),
        5 => Some(crate::storage::query::modes::QueryMode::Natural),
        255 => Some(crate::storage::query::modes::QueryMode::Unknown),
        _ => None,
    }
}

fn result_cache_static_str(value: &str) -> Option<&'static str> {
    match value {
        "select" => Some("select"),
        "materialized-graph" => Some("materialized-graph"),
        "runtime-red-schema" => Some("runtime-red-schema"),
        "runtime-fdw" => Some("runtime-fdw"),
        "runtime-table-rls" => Some("runtime-table-rls"),
        "runtime-table" => Some("runtime-table"),
        "runtime-join-rls" => Some("runtime-join-rls"),
        "runtime-join" => Some("runtime-join"),
        "runtime-vector" => Some("runtime-vector"),
        "runtime-hybrid" => Some("runtime-hybrid"),
        "runtime-secret" => Some("runtime-secret"),
        "runtime-config" => Some("runtime-config"),
        "runtime-tenant" => Some("runtime-tenant"),
        "runtime-explain" => Some("runtime-explain"),
        "runtime-tree" => Some("runtime-tree"),
        "runtime-kv" => Some("runtime-kv"),
        "runtime-queue" => Some("runtime-queue"),
        _ => None,
    }
}

fn write_u32(out: &mut Vec<u8>, value: usize) -> Option<()> {
    let value = u32::try_from(value).ok()?;
    out.extend_from_slice(&value.to_le_bytes());
    Some(())
}

fn write_string(out: &mut Vec<u8>, value: &str) -> Option<()> {
    write_u32(out, value.len())?;
    out.extend_from_slice(value.as_bytes());
    Some(())
}

fn write_bytes(out: &mut Vec<u8>, value: &[u8]) -> Option<()> {
    write_u32(out, value.len())?;
    out.extend_from_slice(value);
    Some(())
}

fn read_u8(input: &mut &[u8]) -> Option<u8> {
    let (&value, rest) = input.split_first()?;
    *input = rest;
    Some(value)
}

fn read_u32(input: &mut &[u8]) -> Option<usize> {
    if input.len() < 4 {
        return None;
    }
    let value = u32::from_le_bytes(input[..4].try_into().ok()?) as usize;
    *input = &input[4..];
    Some(value)
}

fn read_u64(input: &mut &[u8]) -> Option<u64> {
    if input.len() < 8 {
        return None;
    }
    let value = u64::from_le_bytes(input[..8].try_into().ok()?);
    *input = &input[8..];
    Some(value)
}

fn read_string(input: &mut &[u8]) -> Option<String> {
    let len = read_u32(input)?;
    if input.len() < len {
        return None;
    }
    let value = String::from_utf8(input[..len].to_vec()).ok()?;
    *input = &input[len..];
    Some(value)
}

fn read_bytes<'a>(input: &mut &'a [u8]) -> Option<&'a [u8]> {
    let len = read_u32(input)?;
    if input.len() < len {
        return None;
    }
    let value = &input[..len];
    *input = &input[len..];
    Some(value)
}

fn encode_result_cache_payload(entry: &RuntimeResultCacheEntry) -> Option<Vec<u8>> {
    let result = &entry.result;
    if result.result.pre_serialized_json.is_some()
        || result_cache_static_str(result.statement).is_none()
        || result_cache_static_str(result.engine).is_none()
        || result_cache_static_str(result.statement_type).is_none()
        || result.result.records.iter().any(|record| {
            !record.nodes.is_empty()
                || !record.edges.is_empty()
                || !record.paths.is_empty()
                || !record.vector_results.is_empty()
        })
    {
        return None;
    }

    let mut out = Vec::new();
    out.extend_from_slice(RESULT_CACHE_PAYLOAD_MAGIC);
    write_string(&mut out, &result.query)?;
    out.push(mode_to_byte(result.mode));
    write_string(&mut out, result.statement)?;
    write_string(&mut out, result.engine)?;
    out.extend_from_slice(&result.affected_rows.to_le_bytes());
    write_string(&mut out, result.statement_type)?;

    write_u32(&mut out, result.result.columns.len())?;
    for column in &result.result.columns {
        write_string(&mut out, column)?;
    }
    out.extend_from_slice(&result.result.stats.nodes_scanned.to_le_bytes());
    out.extend_from_slice(&result.result.stats.edges_scanned.to_le_bytes());
    out.extend_from_slice(&result.result.stats.rows_scanned.to_le_bytes());
    out.extend_from_slice(&result.result.stats.segments_total.to_le_bytes());
    out.extend_from_slice(&result.result.stats.segments_scanned.to_le_bytes());
    out.extend_from_slice(&result.result.stats.segments_pruned.to_le_bytes());
    out.extend_from_slice(&result.result.stats.exec_time_us.to_le_bytes());

    write_u32(&mut out, result.result.records.len())?;
    for record in &result.result.records {
        let fields = record.iter_fields().collect::<Vec<_>>();
        write_u32(&mut out, fields.len())?;
        for (name, value) in fields {
            write_string(&mut out, name)?;
            let mut encoded = Vec::new();
            crate::storage::schema::value_codec::encode(value, &mut encoded);
            write_bytes(&mut out, &encoded)?;
        }
    }

    write_u32(&mut out, entry.scopes.len())?;
    for scope in &entry.scopes {
        write_string(&mut out, scope)?;
    }
    Some(out)
}

fn decode_result_cache_payload(mut input: &[u8]) -> Option<(RuntimeQueryResult, HashSet<String>)> {
    if input.len() < RESULT_CACHE_PAYLOAD_MAGIC.len()
        || &input[..RESULT_CACHE_PAYLOAD_MAGIC.len()] != RESULT_CACHE_PAYLOAD_MAGIC
    {
        return None;
    }
    input = &input[RESULT_CACHE_PAYLOAD_MAGIC.len()..];

    let query = read_string(&mut input)?;
    let mode = mode_from_byte(read_u8(&mut input)?)?;
    let statement = result_cache_static_str(&read_string(&mut input)?)?;
    let engine = result_cache_static_str(&read_string(&mut input)?)?;
    let affected_rows = read_u64(&mut input)?;
    let statement_type = result_cache_static_str(&read_string(&mut input)?)?;

    let mut columns = Vec::new();
    for _ in 0..read_u32(&mut input)? {
        columns.push(read_string(&mut input)?);
    }
    let stats = crate::storage::query::unified::QueryStats {
        nodes_scanned: read_u64(&mut input)?,
        edges_scanned: read_u64(&mut input)?,
        rows_scanned: read_u64(&mut input)?,
        segments_total: read_u64(&mut input)?,
        segments_scanned: read_u64(&mut input)?,
        segments_pruned: read_u64(&mut input)?,
        exec_time_us: read_u64(&mut input)?,
    };

    let mut records = Vec::new();
    for _ in 0..read_u32(&mut input)? {
        let mut record = crate::storage::query::unified::UnifiedRecord::new();
        for _ in 0..read_u32(&mut input)? {
            let name = read_string(&mut input)?;
            let bytes = read_bytes(&mut input)?;
            let (value, used) = crate::storage::schema::value_codec::decode(bytes).ok()?;
            if used != bytes.len() {
                return None;
            }
            record.set_owned(name, value);
        }
        records.push(record);
    }

    let mut scopes = HashSet::new();
    for _ in 0..read_u32(&mut input)? {
        scopes.insert(read_string(&mut input)?);
    }
    if !input.is_empty() {
        return None;
    }

    Some((
        RuntimeQueryResult {
            query,
            mode,
            statement,
            engine,
            result: crate::storage::query::unified::UnifiedResult {
                columns,
                records,
                stats,
                pre_serialized_json: None,
            },
            affected_rows,
            statement_type,
            bookmark: None,
            notice: None,
        },
        scopes,
    ))
}

impl RedDBRuntime {
    fn result_cache_backend(&self) -> RuntimeResultCacheBackend {
        match self
            .config_string(RESULT_CACHE_BACKEND_KEY, RESULT_CACHE_DEFAULT_BACKEND)
            .as_str()
        {
            "blob_cache" => RuntimeResultCacheBackend::BlobCache,
            "shadow" => RuntimeResultCacheBackend::Shadow,
            _ => RuntimeResultCacheBackend::Legacy,
        }
    }

    fn result_cache_enabled(&self) -> bool {
        self.config_bool(RESULT_CACHE_ENABLED_KEY, true)
    }

    fn result_cache_ttl_secs(&self) -> u64 {
        self.config_u64(RESULT_CACHE_TTL_KEY, RESULT_CACHE_TTL_SECS)
    }

    fn result_cache_capacity(&self) -> usize {
        self.config_u64(RESULT_CACHE_CAPACITY_KEY, RESULT_CACHE_MAX_ENTRIES as u64) as usize
    }

    pub fn result_cache_metrics(&self) -> (u64, u64, u64) {
        use std::sync::atomic::Ordering::Relaxed;
        (
            self.inner.result_cache_hits.load(Relaxed),
            self.inner.result_cache_misses.load(Relaxed),
            self.inner.result_cache_evictions.load(Relaxed),
        )
    }

    fn record_result_cache_evictions(&self, evicted: u64) {
        if evicted > 0 {
            self.inner
                .result_cache_evictions
                .fetch_add(evicted, std::sync::atomic::Ordering::Relaxed);
        }
    }

    pub(super) fn get_result_cache_entry(&self, key: &str) -> Option<RuntimeQueryResult> {
        if !self.result_cache_enabled() {
            return None;
        }
        let hit = self.get_result_cache_entry_inner(key);
        let counter = if hit.is_some() {
            &self.inner.result_cache_hits
        } else {
            &self.inner.result_cache_misses
        };
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        hit
    }

    fn get_result_cache_entry_inner(&self, key: &str) -> Option<RuntimeQueryResult> {
        match self.result_cache_backend() {
            RuntimeResultCacheBackend::Legacy => self.get_legacy_result_cache_entry(key),
            RuntimeResultCacheBackend::BlobCache => self.get_blob_result_cache_entry(key),
            RuntimeResultCacheBackend::Shadow => {
                let legacy = self.get_legacy_result_cache_entry(key);
                let blob = self.get_blob_result_cache_entry(key);
                if let (Some(ref legacy), Some(ref blob)) = (&legacy, &blob) {
                    if result_cache_fingerprint(legacy) != result_cache_fingerprint(blob) {
                        self.inner
                            .result_cache_shadow_divergences
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        tracing::warn!(
                            key,
                            metric = crate::runtime::METRIC_CACHE_SHADOW_DIVERGENCE_TOTAL,
                            "result cache shadow backend diverged from legacy"
                        );
                    }
                }
                legacy
            }
        }
    }

    fn get_legacy_result_cache_entry(&self, key: &str) -> Option<RuntimeQueryResult> {
        let ttl = self.result_cache_ttl_secs();
        let cache = self.inner.result_cache.read();
        cache.0.get(key).and_then(|entry| {
            if entry.cached_at.elapsed().as_secs() < ttl {
                Some(entry.result.clone())
            } else {
                None
            }
        })
    }

    fn get_blob_result_cache_entry(&self, key: &str) -> Option<RuntimeQueryResult> {
        if self.inner.embedded_single_file {
            if let Some(bytes) = self.get_embedded_result_cache_payload(key) {
                let policy = crate::storage::cache::BlobCachePolicy::default()
                    .ttl_ms(self.result_cache_ttl_secs() * 1000)
                    .priority(200);
                let put = crate::storage::cache::BlobCachePut::new(bytes).with_policy(policy);
                if self
                    .inner
                    .result_blob_cache
                    .put(RESULT_CACHE_BLOB_NAMESPACE, key, put)
                    .is_err()
                {
                    return None;
                }
            }
        }
        self.get_blob_result_cache_entry_from_blob_cache(key)
    }

    fn get_blob_result_cache_entry_from_blob_cache(&self, key: &str) -> Option<RuntimeQueryResult> {
        let hit = self
            .inner
            .result_blob_cache
            .get(RESULT_CACHE_BLOB_NAMESPACE, key)?;
        {
            let cache = self.inner.result_blob_entries.read();
            if let Some(entry) = cache.0.get(key) {
                return Some(entry.result.clone());
            }
        }

        let (result, scopes) = decode_result_cache_payload(hit.value())?;
        let mut cache = self.inner.result_blob_entries.write();
        let (ref mut map, ref mut order) = *cache;
        let new_entry = RuntimeResultCacheEntry {
            result: result.clone(),
            cached_at: std::time::Instant::now(),
            scopes,
        };
        // Single hash lookup: drive the LRU-order push_back only on Vacant.
        match map.entry(key.to_string()) {
            Entry::Occupied(mut slot) => {
                slot.insert(new_entry);
            }
            Entry::Vacant(slot) => {
                order.push_back(slot.key().clone());
                slot.insert(new_entry);
            }
        }
        let evicted = trim_result_cache(map, order, self.result_cache_capacity());
        drop(cache);
        self.record_result_cache_evictions(evicted);
        Some(result)
    }

    fn get_embedded_result_cache_payload(&self, key: &str) -> Option<Vec<u8>> {
        let manager = self
            .inner
            .db
            .store()
            .get_collection(RESULT_CACHE_EMBEDDED_COLLECTION)?;
        let mut latest: Option<(u64, Vec<u8>)> = None;
        manager.for_each_entity(|entity| {
            let Some(row) = entity.data.as_row() else {
                return true;
            };
            let namespace_matches = row.get_field("namespace").and_then(|value| match value {
                crate::storage::schema::Value::Text(value) => Some(value.as_ref()),
                _ => None,
            }) == Some(RESULT_CACHE_BLOB_NAMESPACE);
            let key_matches = row.get_field("key").and_then(|value| match value {
                crate::storage::schema::Value::Text(value) => Some(value.as_ref()),
                _ => None,
            }) == Some(key);
            if namespace_matches && key_matches {
                if let Some(crate::storage::schema::Value::Blob(payload)) = row.get_field("payload")
                {
                    let id = entity.id.raw();
                    if latest
                        .as_ref()
                        .is_none_or(|(latest_id, _)| id >= *latest_id)
                    {
                        latest = Some((id, payload.clone()));
                    }
                }
            }
            true
        });
        latest.map(|(_, payload)| payload)
    }

    pub(super) fn put_result_cache_entry(&self, key: &str, entry: RuntimeResultCacheEntry) {
        if !self.result_cache_enabled() {
            return;
        }
        match self.result_cache_backend() {
            RuntimeResultCacheBackend::Legacy => self.put_legacy_result_cache_entry(key, entry),
            RuntimeResultCacheBackend::BlobCache => self.put_blob_result_cache_entry(key, entry),
            RuntimeResultCacheBackend::Shadow => {
                self.put_legacy_result_cache_entry(key, entry.clone());
                self.put_blob_result_cache_entry(key, entry);
            }
        }
    }

    fn put_legacy_result_cache_entry(&self, key: &str, entry: RuntimeResultCacheEntry) {
        let capacity = self.result_cache_capacity();
        let mut cache = self.inner.result_cache.write();
        let (ref mut map, ref mut order) = *cache;
        // Single hash lookup: drive the LRU-order push_back only on Vacant.
        match map.entry(key.to_string()) {
            Entry::Occupied(mut slot) => {
                slot.insert(entry);
            }
            Entry::Vacant(slot) => {
                order.push_back(slot.key().clone());
                slot.insert(entry);
            }
        }
        let evicted = trim_result_cache(map, order, capacity);
        drop(cache);
        self.record_result_cache_evictions(evicted);
    }

    fn put_blob_result_cache_entry(&self, key: &str, entry: RuntimeResultCacheEntry) {
        let policy = crate::storage::cache::BlobCachePolicy::default()
            .ttl_ms(self.result_cache_ttl_secs() * 1000)
            .priority(200);
        let dependencies = entry.scopes.iter().cloned().collect::<Vec<_>>();
        let bytes = encode_result_cache_payload(&entry)
            .unwrap_or_else(|| result_cache_fingerprint(&entry.result).into_bytes());
        if self.inner.embedded_single_file {
            self.put_embedded_result_cache_payload(key, &bytes, &dependencies);
        }
        let put = crate::storage::cache::BlobCachePut::new(bytes)
            .with_dependencies(dependencies)
            .with_policy(policy);
        if self
            .inner
            .result_blob_cache
            .put(RESULT_CACHE_BLOB_NAMESPACE, key, put)
            .is_err()
        {
            return;
        }

        let capacity = self.result_cache_capacity();
        let mut cache = self.inner.result_blob_entries.write();
        let (ref mut map, ref mut order) = *cache;
        // Single hash lookup: drive the LRU-order push_back only on Vacant.
        match map.entry(key.to_string()) {
            Entry::Occupied(mut slot) => {
                slot.insert(entry);
            }
            Entry::Vacant(slot) => {
                order.push_back(slot.key().clone());
                slot.insert(entry);
            }
        }
        let evicted = trim_result_cache(map, order, capacity);
        drop(cache);
        self.record_result_cache_evictions(evicted);
    }

    fn put_embedded_result_cache_payload(&self, key: &str, bytes: &[u8], scopes: &[String]) {
        let store = self.inner.db.store();
        let _ = store.get_or_create_collection(RESULT_CACHE_EMBEDDED_COLLECTION);
        let entity = crate::storage::UnifiedEntity::new(
            crate::storage::EntityId::new(0),
            crate::storage::EntityKind::TableRow {
                table: std::sync::Arc::from(RESULT_CACHE_EMBEDDED_COLLECTION),
                row_id: 0,
            },
            crate::storage::EntityData::Row(crate::storage::RowData {
                columns: Vec::new(),
                named: Some(HashMap::from([
                    (
                        "namespace".to_string(),
                        crate::storage::schema::Value::text(RESULT_CACHE_BLOB_NAMESPACE),
                    ),
                    (
                        "key".to_string(),
                        crate::storage::schema::Value::text(key.to_string()),
                    ),
                    (
                        "payload".to_string(),
                        crate::storage::schema::Value::Blob(bytes.to_vec()),
                    ),
                    (
                        "scopes".to_string(),
                        crate::storage::schema::Value::text(scopes.join("\n")),
                    ),
                ])),
                schema: None,
            }),
        );
        let _ = store.insert_auto(RESULT_CACHE_EMBEDDED_COLLECTION, entity);
    }

    fn invalidate_embedded_result_cache(&self) {
        let Some(manager) = self
            .inner
            .db
            .store()
            .get_collection(RESULT_CACHE_EMBEDDED_COLLECTION)
        else {
            return;
        };
        let ids = manager
            .query_all(|_| true)
            .into_iter()
            .map(|entity| entity.id)
            .collect::<Vec<_>>();
        if !ids.is_empty() {
            let _ = self
                .inner
                .db
                .store()
                .delete_batch(RESULT_CACHE_EMBEDDED_COLLECTION, &ids);
        }
    }

    fn invalidate_embedded_result_cache_for_scope(&self, scope: &str) {
        let Some(manager) = self
            .inner
            .db
            .store()
            .get_collection(RESULT_CACHE_EMBEDDED_COLLECTION)
        else {
            return;
        };
        let ids = manager
            .query_all(|entity| {
                entity
                    .data
                    .as_row()
                    .and_then(|row| row.get_field("scopes"))
                    .and_then(|value| match value {
                        crate::storage::schema::Value::Text(value) => Some(value.as_ref()),
                        _ => None,
                    })
                    .is_some_and(|scopes| scopes.lines().any(|entry| entry == scope))
            })
            .into_iter()
            .map(|entity| entity.id)
            .collect::<Vec<_>>();
        if !ids.is_empty() {
            let _ = self
                .inner
                .db
                .store()
                .delete_batch(RESULT_CACHE_EMBEDDED_COLLECTION, &ids);
        }
    }

    pub fn result_cache_shadow_divergences(&self) -> u64 {
        self.inner
            .result_cache_shadow_divergences
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn invalidate_result_cache(&self) {
        self.invalidate_result_cache_process_only();
        if self.inner.embedded_single_file {
            self.invalidate_embedded_result_cache();
        }
    }

    pub(crate) fn invalidate_result_cache_process_only(&self) {
        let mut cache = self.inner.result_cache.write();
        cache.0.clear();
        cache.1.clear();
        let mut blob_entries = self.inner.result_blob_entries.write();
        blob_entries.0.clear();
        blob_entries.1.clear();
        self.inner
            .result_blob_cache
            .invalidate_namespace(RESULT_CACHE_BLOB_NAMESPACE);
        let mut ask_entries = self.inner.ask_answer_cache_entries.write();
        ask_entries.0.clear();
        ask_entries.1.clear();
        self.inner
            .result_blob_cache
            .invalidate_namespace(super::ASK_ANSWER_CACHE_NAMESPACE);
    }

    pub(crate) fn invalidate_result_cache_for_table(&self, table: &str) {
        let legacy_has_match = {
            let cache = self.inner.result_cache.read();
            let (ref map, _) = *cache;
            !map.is_empty() && map.values().any(|entry| entry.scopes.contains(table))
        };
        let blob_has_match = {
            let cache = self.inner.result_blob_entries.read();
            let (ref map, _) = *cache;
            !map.is_empty() && map.values().any(|entry| entry.scopes.contains(table))
        };
        if legacy_has_match {
            let mut cache = self.inner.result_cache.write();
            let (ref mut map, ref mut order) = *cache;
            map.retain(|_, entry| !entry.scopes.contains(table));
            order.retain(|key| map.contains_key(key));
        }

        if matches!(
            self.result_cache_backend(),
            RuntimeResultCacheBackend::BlobCache | RuntimeResultCacheBackend::Shadow
        ) {
            let mut blob_entries = self.inner.result_blob_entries.write();
            let (ref mut blob_map, ref mut blob_order) = *blob_entries;
            blob_map.clear();
            blob_order.clear();
            self.inner
                .result_blob_cache
                .invalidate_namespace(RESULT_CACHE_BLOB_NAMESPACE);
            if self.inner.embedded_single_file {
                self.invalidate_embedded_result_cache();
            }
        } else if blob_has_match {
            let mut blob_entries = self.inner.result_blob_entries.write();
            let (ref mut blob_map, ref mut blob_order) = *blob_entries;
            blob_map.retain(|_, entry| !entry.scopes.contains(table));
            blob_order.retain(|key| blob_map.contains_key(key));
            if self.inner.embedded_single_file {
                self.invalidate_embedded_result_cache_for_scope(table);
            }
        }
        let mut ask_entries = self.inner.ask_answer_cache_entries.write();
        ask_entries.0.clear();
        ask_entries.1.clear();
        self.inner
            .result_blob_cache
            .invalidate_namespace(super::ASK_ANSWER_CACHE_NAMESPACE);
    }
}
