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
const RESULT_CACHE_WRITE_QUIET_PERIOD: std::time::Duration = std::time::Duration::from_secs(1);

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

fn mode_to_byte(mode: reddb_rql::modes::QueryMode) -> u8 {
    match mode {
        reddb_rql::modes::QueryMode::Sql => 0,
        reddb_rql::modes::QueryMode::Gremlin => 1,
        reddb_rql::modes::QueryMode::Cypher => 2,
        reddb_rql::modes::QueryMode::Sparql => 3,
        reddb_rql::modes::QueryMode::Path => 4,
        reddb_rql::modes::QueryMode::Natural => 5,
        reddb_rql::modes::QueryMode::Unknown => 255,
    }
}

fn mode_from_byte(byte: u8) -> Option<reddb_rql::modes::QueryMode> {
    match byte {
        0 => Some(reddb_rql::modes::QueryMode::Sql),
        1 => Some(reddb_rql::modes::QueryMode::Gremlin),
        2 => Some(reddb_rql::modes::QueryMode::Cypher),
        3 => Some(reddb_rql::modes::QueryMode::Sparql),
        4 => Some(reddb_rql::modes::QueryMode::Path),
        5 => Some(reddb_rql::modes::QueryMode::Natural),
        255 => Some(reddb_rql::modes::QueryMode::Unknown),
        _ => None,
    }
}

fn result_cache_static_str(value: &str) -> Option<&'static str> {
    match value {
        "select" => Some("select"),
        "table" => Some("table"),
        "join" => Some("join"),
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
    // Execution measurements must not survive as a persisted cached execution.
    if result.result.stats.vector.is_some()
        || result.result.pre_serialized_json.is_some()
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
            reddb_types::value_codec::encode(value, &mut encoded);
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
        vector: None,
    };

    let mut records = Vec::new();
    for _ in 0..read_u32(&mut input)? {
        let mut record = crate::storage::query::unified::UnifiedRecord::new();
        for _ in 0..read_u32(&mut input)? {
            let name = read_string(&mut input)?;
            let bytes = read_bytes(&mut input)?;
            let (value, used) = reddb_types::value_codec::decode(bytes).ok()?;
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
        if let Some(result) = self.get_blob_result_cache_entry_from_blob_cache(key) {
            return Some(result);
        }
        // One-way compatibility read for databases written before the generic
        // single-file L2 existed. A hit is promoted into the new backend; new
        // writes never duplicate payloads in this legacy collection.
        if self.inner.embedded_single_file {
            let _promotion = self.inner.result_blob_promotion.lock();
            let mut cache = self.inner.result_blob_entries.write();
            if let Some(bytes) = self.get_embedded_result_cache_payload(key) {
                let policy = crate::storage::cache::BlobCachePolicy::default()
                    .ttl_ms(self.result_cache_ttl_secs() * 1000)
                    .priority(200);
                let put = crate::storage::cache::BlobCachePut::new(bytes).with_policy(policy);
                cache.2 = true;
                cache.3 = cache.3.wrapping_add(1);
                let inserted = self
                    .inner
                    .result_blob_cache
                    .put(RESULT_CACHE_BLOB_NAMESPACE, key, put)
                    .is_ok();
                drop(cache);
                if !inserted {
                    return None;
                }
                return self.get_blob_result_cache_entry_from_blob_cache(key);
            }
        }
        None
    }

    fn get_blob_result_cache_entry_from_blob_cache(&self, key: &str) -> Option<RuntimeQueryResult> {
        {
            let cache = self.inner.result_blob_entries.read();
            if cache.4.contains(key) {
                let entry = cache.0.get(key)?;
                return (entry.cached_at.elapsed().as_secs() < self.result_cache_ttl_secs())
                    .then(|| entry.result.clone());
            }
        }
        let (hit, revision) = {
            let cache = self.inner.result_blob_entries.read();
            let hit = self
                .inner
                .result_blob_cache
                .get(RESULT_CACHE_BLOB_NAMESPACE, key)?;
            if let Some(entry) = cache.0.get(key) {
                return Some(entry.result.clone());
            }
            (hit, cache.3)
        };

        let (result, scopes) = decode_result_cache_payload(hit.value())?;
        let mut cache = self.inner.result_blob_entries.write();
        if cache.3 != revision {
            return None;
        }
        let (ref mut map, ref mut order, ref mut untracked, ..) = *cache;
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
        *untracked |= evicted > 0;
        if evicted > 0 {
            let (ref map, _, _, _, ref mut volatile_keys, _) = *cache;
            volatile_keys.retain(|key| map.contains_key(key));
        }
        drop(cache);
        self.record_result_cache_evictions(evicted);
        Some(result)
    }

    // The cache sweeper calls this after writes have been quiet. A single
    // promotion runs at a time, and it does not hold the result-map lock
    // during L2 I/O. Invalidation and foreground puts take the same gate.
    pub(super) fn promote_volatile_result_cache_entry(&self) {
        let Some(_promotion) = self.inner.result_blob_promotion.try_lock() else {
            return;
        };
        let ttl = self.result_cache_ttl_secs();
        let candidate = {
            let cache = self.inner.result_blob_entries.read();
            if cache
                .5
                .is_none_or(|until| std::time::Instant::now() < until)
            {
                return;
            }
            cache.4.iter().find_map(|key| {
                let entry = cache.0.get(key)?;
                (entry.cached_at.elapsed().as_secs() < ttl).then(|| (key.clone(), entry.clone()))
            })
        };
        let Some((key, entry)) = candidate else {
            return;
        };
        let remaining =
            std::time::Duration::from_secs(ttl).saturating_sub(entry.cached_at.elapsed());
        if remaining.is_zero() {
            return;
        }
        let ttl_ms = u64::try_from(remaining.as_millis())
            .unwrap_or(u64::MAX)
            .max(1);
        let policy = crate::storage::cache::BlobCachePolicy::default()
            .ttl_ms(ttl_ms)
            .priority(200);
        let bytes = encode_result_cache_payload(&entry)
            .unwrap_or_else(|| result_cache_fingerprint(&entry.result).into_bytes());
        let put = crate::storage::cache::BlobCachePut::new(bytes)
            .with_dependencies(entry.scopes.iter().cloned())
            .with_policy(policy);
        let result = self
            .inner
            .result_blob_cache
            .put(RESULT_CACHE_BLOB_NAMESPACE, &key, put);
        let mut cache = self.inner.result_blob_entries.write();
        if result.is_ok()
            && cache.4.contains(&key)
            && cache
                .0
                .get(&key)
                .is_some_and(|current| current.cached_at == entry.cached_at)
        {
            cache.4.remove(&key);
        } else {
            // A failed or evicted publication may still have left L2 state.
            cache.2 = true;
            if result.is_err() {
                cache.5 = Some(std::time::Instant::now() + RESULT_CACHE_WRITE_QUIET_PERIOD);
            }
        }
        cache.3 = cache.3.wrapping_add(1);
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
                reddb_types::Value::Text(value) => Some(value.as_ref()),
                _ => None,
            }) == Some(RESULT_CACHE_BLOB_NAMESPACE);
            let key_matches = row.get_field("key").and_then(|value| match value {
                reddb_types::Value::Text(value) => Some(value.as_ref()),
                _ => None,
            }) == Some(key);
            if namespace_matches && key_matches {
                if let Some(reddb_types::Value::Blob(payload)) = row.get_field("payload") {
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
        let put = crate::storage::cache::BlobCachePut::new(bytes)
            .with_dependencies(dependencies)
            .with_policy(policy);
        let _promotion = self.inner.result_blob_promotion.lock();
        let mut cache = self.inner.result_blob_entries.write();
        let volatile = cache
            .5
            .is_some_and(|until| std::time::Instant::now() < until);
        if !volatile
            && self
                .inner
                .result_blob_cache
                .put(RESULT_CACHE_BLOB_NAMESPACE, key, put)
                .is_err()
        {
            // An unsuccessful L2 publication may have left an entry behind.
            cache.2 = true;
            cache.3 = cache.3.wrapping_add(1);
            return;
        }

        if volatile {
            cache.4.insert(key.to_string());
        } else {
            cache.4.remove(key);
        }
        let capacity = self.result_cache_capacity();
        let (ref mut map, ref mut order, ref mut untracked, ref mut revision, ..) = *cache;
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
        *untracked |= evicted > 0;
        *revision = revision.wrapping_add(1);
        if evicted > 0 {
            let (ref map, _, _, _, ref mut volatile_keys, _) = *cache;
            volatile_keys.retain(|key| map.contains_key(key));
        }
        drop(cache);
        self.record_result_cache_evictions(evicted);
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
                        reddb_types::Value::Text(value) => Some(value.as_ref()),
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
        self.invalidate_result_cache_inner(self.inner.embedded_single_file);
    }

    fn invalidate_blob_result_namespace(&self) -> bool {
        match self
            .inner
            .result_blob_cache
            .try_invalidate_namespace(RESULT_CACHE_BLOB_NAMESPACE)
        {
            Ok(_) => false,
            Err(error) => {
                tracing::error!(?error, "failed to persist result-cache invalidation");
                true
            }
        }
    }

    pub(crate) fn invalidate_result_cache_process_only(&self) {
        self.invalidate_result_cache_inner(false);
    }

    fn invalidate_result_cache_inner(&self, clear_embedded: bool) {
        let _promotion = self.inner.result_blob_promotion.lock();
        let mut cache = self.inner.result_cache.write();
        cache.0.clear();
        cache.1.clear();
        let mut blob_entries = self.inner.result_blob_entries.write();
        blob_entries.0.clear();
        blob_entries.1.clear();
        blob_entries.4.clear();
        blob_entries.5 = None;
        blob_entries.2 = self.invalidate_blob_result_namespace();
        blob_entries.3 = blob_entries.3.wrapping_add(1);
        if clear_embedded {
            self.invalidate_embedded_result_cache();
        }
        let mut ask_entries = self.inner.ask_answer_cache_entries.write();
        ask_entries.0.clear();
        ask_entries.1.clear();
        self.inner
            .result_blob_cache
            .invalidate_namespace(super::ASK_ANSWER_CACHE_NAMESPACE);
    }

    pub(crate) fn invalidate_result_cache_for_table(&self, table: &str) {
        let _promotion = self.inner.result_blob_promotion.lock();
        let legacy_has_match = {
            let cache = self.inner.result_cache.read();
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
            let has_match = blob_entries
                .0
                .values()
                .any(|entry| entry.scopes.contains(table));
            let has_durable_match = blob_entries
                .0
                .iter()
                .any(|(key, entry)| entry.scopes.contains(table) && !blob_entries.4.contains(key));
            if blob_entries.2 || has_durable_match {
                blob_entries.0.clear();
                blob_entries.1.clear();
                blob_entries.4.clear();
                blob_entries.2 = self.invalidate_blob_result_namespace();
                blob_entries.3 = blob_entries.3.wrapping_add(1);
                if self.inner.embedded_single_file {
                    self.invalidate_embedded_result_cache();
                }
            } else if has_match {
                blob_entries
                    .0
                    .retain(|_, entry| !entry.scopes.contains(table));
                let (ref map, ref mut order, _, ref mut revision, ref mut volatile_keys, _) =
                    *blob_entries;
                order.retain(|key| map.contains_key(key));
                volatile_keys.retain(|key| map.contains_key(key));
                *revision = revision.wrapping_add(1);
            }
            if has_match {
                blob_entries.5 = Some(std::time::Instant::now() + RESULT_CACHE_WRITE_QUIET_PERIOD);
            }
        } else {
            let mut blob_entries = self.inner.result_blob_entries.write();
            let (
                ref mut blob_map,
                ref mut blob_order,
                ref mut untracked,
                ref mut revision,
                ref mut volatile_keys,
                _,
            ) = *blob_entries;
            if blob_map.values().any(|entry| entry.scopes.contains(table)) {
                blob_map.retain(|_, entry| !entry.scopes.contains(table));
                blob_order.retain(|key| blob_map.contains_key(key));
                volatile_keys.retain(|key| blob_map.contains_key(key));
                *untracked = true;
                *revision = revision.wrapping_add(1);
                if self.inner.embedded_single_file {
                    self.invalidate_embedded_result_cache_for_scope(table);
                }
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

#[cfg(test)]
mod crash_tests {
    use crate::{RedDBOptions, RedDBRuntime};
    use std::sync::Arc;

    const CHILD_PATH: &str = "REDDB_RESULT_CACHE_COMMIT_GAP_TEST_PATH";

    #[test]
    fn promotion_gate_does_not_block_hits_and_orders_invalidation() {
        use std::sync::mpsc;
        use std::time::Duration;

        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
        for sql in [
            "SET CONFIG runtime.result_cache.backend = 'blob_cache'",
            "CREATE TABLE promotion_gate (id INT)",
            "INSERT INTO promotion_gate (id) VALUES (1)",
            "SELECT id FROM promotion_gate ORDER BY id",
            "INSERT INTO promotion_gate (id) VALUES (2)",
            "SELECT id FROM promotion_gate ORDER BY id",
        ] {
            runtime.execute_query(sql).expect("prepare volatile result");
        }

        let guard = runtime.inner.result_blob_promotion.lock();
        let reader = runtime.clone();
        let (read_tx, read_rx) = mpsc::channel();
        let reader_thread = std::thread::spawn(move || {
            let rows = reader
                .execute_query("SELECT id FROM promotion_gate ORDER BY id")
                .expect("cached read")
                .result
                .records
                .len();
            read_tx.send(rows).expect("report read");
        });
        let read = read_rx.recv_timeout(Duration::from_secs(3));
        drop(guard);
        reader_thread.join().expect("reader thread");
        assert_eq!(read.expect("hit blocked by promotion gate"), 2);

        let (published_tx, published_rx) = mpsc::channel();
        *runtime.index_store_ref().mutation_hook.lock() =
            Some(Arc::new(move |collection, phase| {
                if collection == "promotion_gate"
                    && phase == super::super::index_store::MutationTestPhase::StoragePublished
                {
                    published_tx.send(()).expect("report publication");
                }
            }));
        let guard = runtime.inner.result_blob_promotion.lock();
        let writer = runtime.clone();
        let (done_tx, done_rx) = mpsc::channel();
        let writer_thread = std::thread::spawn(move || {
            let result = writer.execute_query("INSERT INTO promotion_gate (id) VALUES (3)");
            done_tx.send(result.is_ok()).expect("report write");
        });
        let published = published_rx.recv_timeout(Duration::from_secs(3));
        let completed_early = done_rx.recv_timeout(Duration::from_millis(100));
        drop(guard);
        assert!(published.is_ok(), "write did not reach storage publication");
        assert!(
            matches!(completed_early, Err(mpsc::RecvTimeoutError::Timeout)),
            "invalidation must wait for an in-flight promotion"
        );
        assert!(done_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("writer did not finish"));
        writer_thread.join().expect("writer thread");
        assert_eq!(
            runtime
                .execute_query("SELECT id FROM promotion_gate ORDER BY id")
                .expect("read after write")
                .result
                .records
                .len(),
            3
        );
    }

    #[test]
    #[ignore = "crash-gap reproducer; run explicitly until durable invalidation is atomic"]
    fn committed_insert_before_cache_invalidation_does_not_recover_stale_result() {
        if let Some(path) = std::env::var_os(CHILD_PATH) {
            let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(path))
                .expect("open child database");
            for sql in [
                "SET CONFIG runtime.result_cache.backend = 'blob_cache'",
                "CREATE TABLE cache_gap (id INT)",
                "INSERT INTO cache_gap (id) VALUES (1)",
                "SELECT id FROM cache_gap ORDER BY id",
            ] {
                runtime.execute_query(sql).expect("prepare cache");
            }
            runtime.checkpoint().expect("persist cached result");
            *runtime.index_store_ref().mutation_hook.lock() =
                Some(Arc::new(|collection, phase| {
                    if collection == "cache_gap"
                        && phase == super::super::index_store::MutationTestPhase::StoragePublished
                    {
                        std::process::exit(86);
                    }
                }));
            runtime
                .execute_query("INSERT INTO cache_gap (id) VALUES (2)")
                .expect("hook must terminate before return");
            panic!("storage-published hook was not reached");
        }

        let dir = tempfile::tempdir().expect("temporary directory");
        let path = dir.path().join("cache-gap.rdb");
        let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "runtime::result_cache_runtime::crash_tests::committed_insert_before_cache_invalidation_does_not_recover_stale_result",
                "--ignored",
                "--nocapture",
            ])
            .env(CHILD_PATH, &path)
            .output()
            .expect("run child");
        assert_eq!(
            output.status.code(),
            Some(86),
            "child did not reach crash hook: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen");
        let cached = runtime
            .execute_query("SELECT id FROM cache_gap ORDER BY id")
            .expect("cached read");
        runtime
            .execute_query("SET CONFIG runtime.result_cache.enabled = false")
            .expect("disable cache");
        let actual = runtime
            .execute_query("SELECT id FROM cache_gap ORDER BY id")
            .expect("authoritative read");
        assert_eq!(
            actual.result.records.len(),
            2,
            "committed insert must have survived"
        );
        assert_eq!(
            cached.result.records.len(),
            actual.result.records.len(),
            "recovered L2 result must not be stale"
        );
    }
}
