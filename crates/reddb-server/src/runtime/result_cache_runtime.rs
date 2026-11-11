use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};

thread_local! {
    static DEFERRED_CACHE_INVALIDATION: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

use super::{RedDBRuntime, RuntimeQueryResult, RuntimeResultCacheEntry};

const RESULT_CACHE_BLOB_NAMESPACE: &str = "runtime.result_cache";
const RESULT_CACHE_TTL_SECS: u64 = 30;
const RESULT_CACHE_MAX_ENTRIES: usize = 1000;
const RESULT_CACHE_ENABLED_KEY: &str = "runtime.result_cache.enabled";
const RESULT_CACHE_TTL_KEY: &str = "runtime.result_cache.ttl_seconds";
const RESULT_CACHE_CAPACITY_KEY: &str = "runtime.result_cache.capacity_entries";
const RESULT_CACHE_PAYLOAD_MAGIC: &[u8; 8] = b"RDRC0002";
const RESULT_CACHE_WRITE_QUIET_PERIOD: std::time::Duration = std::time::Duration::from_secs(1);

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
    if entry.scopes.iter().any(|scope| {
        reddb_engine::storage::system_collection_names::canonical_or_self(scope).starts_with("red.")
    }) || result.result.stats.vector.is_some()
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
    out.extend_from_slice(&entry.data_epoch.to_le_bytes());
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

fn decode_result_cache_payload(
    mut input: &[u8],
) -> Option<(RuntimeQueryResult, HashSet<String>, u64)> {
    if input.len() < RESULT_CACHE_PAYLOAD_MAGIC.len()
        || &input[..RESULT_CACHE_PAYLOAD_MAGIC.len()] != RESULT_CACHE_PAYLOAD_MAGIC
    {
        return None;
    }
    input = &input[RESULT_CACHE_PAYLOAD_MAGIC.len()..];
    let data_epoch = read_u64(&mut input)?;

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
        data_epoch,
    ))
}

impl RedDBRuntime {
    fn invalidate_result_cache_during_wal_capture(&self) {
        DEFERRED_CACHE_INVALIDATION.with(|pending| pending.set(true));
        let store = self.inner.db.store();
        let _publication = store.vault_publication_lock.lock();
        let _promotion = self.inner.result_blob_promotion.lock();
        let mut entries = self.inner.result_blob_entries.write();
        entries.0.clear();
        entries.1.clear();
        entries.2 = true;
        entries.3 = entries.3.wrapping_add(1);
        entries.4.clear();
        entries.5 = None;
        let mut ask = self.inner.ask_answer_cache_entries.write();
        ask.0.clear();
        ask.1.clear();
        self.inner
            .result_blob_cache
            .invalidate_namespace_in_memory(RESULT_CACHE_BLOB_NAMESPACE);
        self.inner
            .result_blob_cache
            .invalidate_namespace_in_memory(super::ASK_ANSWER_CACHE_NAMESPACE);
    }

    pub(super) fn flush_deferred_cache_invalidation(&self) {
        if DEFERRED_CACHE_INVALIDATION.with(|pending| pending.replace(false)) {
            self.invalidate_result_cache();
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

    fn ensure_result_cache_format(&self) -> Option<u64> {
        let store = self.inner.db.store();
        if crate::storage::UnifiedStore::deferred_store_wal_capture_active() {
            return None;
        }
        let _publication = store.vault_publication_lock.lock();
        let _format = self.inner.result_cache_format_lock.lock();
        if store.result_cache_format_fenced() {
            return store.result_cache_read_epoch();
        }
        if self.inner.db.path().is_some() && store.is_read_only() {
            return None;
        }
        {
            let _promotion = self.inner.result_blob_promotion.lock();
            if self
                .inner
                .result_blob_cache
                .try_invalidate_namespace(RESULT_CACHE_BLOB_NAMESPACE)
                .is_err()
            {
                return None;
            }
        }
        store.ensure_result_cache_epoch().ok()?;
        if self.inner.db.path().is_some() {
            if let Some(pager) = store.pager() {
                let version = if store.config().resident_memory_bytes_max.is_some() {
                    reddb_file::file_format::PAGE_RESULT_CACHE_DISK_FILE_VERSION
                } else {
                    reddb_file::file_format::PAGE_RESULT_CACHE_FILE_VERSION
                };
                pager.fence_file_format_version(version).ok()?;
            } else {
                self.checkpoint_local().ok()?;
                #[cfg(test)]
                self.index_store_ref().mutation_test_hook(
                    "",
                    super::index_store::MutationTestPhase::ResultCacheFormatCheckpointed,
                );
            }
        }
        store.seal_result_cache_snapshot().ok()?;
        store.result_cache_read_epoch()
    }

    pub(super) fn result_cache_snapshot(&self) -> (Option<u64>, u64) {
        let epoch = if self.result_cache_enabled() {
            let store = self.inner.db.store();
            if store.result_cache_format_fenced() {
                store.result_cache_read_epoch()
            } else {
                self.ensure_result_cache_format()
            }
        } else {
            Some(0)
        };
        let cache = self.inner.result_blob_entries.read();
        if cache.6 != 0 || !cache.0.is_empty() || epoch.unwrap_or(0) == 0 {
            return (epoch, cache.3);
        }
        drop(cache);
        let mut cache = self.inner.result_blob_entries.write();
        if cache.6 == 0 && cache.0.is_empty() {
            cache.6 = epoch.unwrap_or(0);
        }
        (epoch, cache.3)
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

    pub(super) fn get_result_cache_entry(
        &self,
        key: &str,
        epoch: Option<u64>,
    ) -> Option<RuntimeQueryResult> {
        if !self.result_cache_enabled() {
            return None;
        }
        let hit = self.get_result_cache_entry_inner(key, epoch);
        let counter = if hit.is_some() {
            &self.inner.result_cache_hits
        } else {
            &self.inner.result_cache_misses
        };
        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        hit
    }

    fn get_result_cache_entry_inner(
        &self,
        key: &str,
        epoch: Option<u64>,
    ) -> Option<RuntimeQueryResult> {
        epoch.and_then(|epoch| self.get_blob_result_cache_entry(key, epoch))
    }

    fn result_cache_entry_reusable(&self, entry: &RuntimeResultCacheEntry) -> bool {
        let store = self.inner.db.store();
        if !matches!(
            entry.result.engine,
            "runtime-table" | "runtime-table-rls" | "runtime-join" | "runtime-join-rls"
        ) || entry.scopes.is_empty()
            || entry
                .scopes
                .iter()
                .any(|scope| store.get_collection(scope).is_none())
        {
            return store.result_cache_epoch() == Some(entry.data_epoch);
        }
        store.result_cache_unchanged_since(entry.data_epoch, &entry.scopes)
    }

    fn get_blob_result_cache_entry(&self, key: &str, epoch: u64) -> Option<RuntimeQueryResult> {
        self.get_blob_result_cache_entry_from_blob_cache(key, epoch)
    }

    fn get_blob_result_cache_entry_from_blob_cache(
        &self,
        key: &str,
        epoch: u64,
    ) -> Option<RuntimeQueryResult> {
        {
            let cache = self.inner.result_blob_entries.read();
            if cache.4.contains(key) {
                let entry = cache.0.get(key)?;
                return (self.result_cache_entry_reusable(entry)
                    && entry.cached_at.elapsed().as_secs() < self.result_cache_ttl_secs())
                .then(|| entry.result.clone());
            }
        }
        // Do not retain the result-map lock while L2 may reclaim an expired
        // entry through the store publication fence. Writers enter that fence
        // before invalidating this map; retaining the map lock here would
        // invert the canonical order.
        let revision = self.inner.result_blob_entries.read().3;
        let hit = self
            .inner
            .result_blob_cache
            .get(RESULT_CACHE_BLOB_NAMESPACE, key)?;
        {
            let cache = self.inner.result_blob_entries.read();
            if cache.3 != revision {
                return None;
            }
            if let Some(entry) = cache.0.get(key) {
                return self
                    .result_cache_entry_reusable(entry)
                    .then(|| entry.result.clone());
            }
        }

        let (result, scopes, data_epoch) = decode_result_cache_payload(hit.value())?;
        if data_epoch != epoch || self.inner.db.store().result_cache_epoch() != Some(epoch) {
            return None;
        }
        let mut cache = self.inner.result_blob_entries.write();
        if cache.3 != revision {
            return None;
        }
        let (ref mut map, ref mut order, ref mut untracked, ..) = *cache;
        let new_entry = RuntimeResultCacheEntry {
            result: result.clone(),
            cached_at: std::time::Instant::now(),
            scopes,
            data_epoch,
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
            let (ref map, _, _, _, ref mut volatile_keys, ..) = *cache;
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
        let store = self.inner.db.store();
        let Some(_publication) = store.vault_publication_lock.try_lock() else {
            return;
        };
        let Some(_promotion) = self.inner.result_blob_promotion.try_lock() else {
            return;
        };
        let ttl = self.result_cache_ttl_secs();
        let Some(epoch) = store.result_cache_epoch() else {
            return;
        };
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
                (entry.data_epoch == epoch && entry.cached_at.elapsed().as_secs() < ttl)
                    .then(|| (key.clone(), entry.clone()))
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

    pub(super) fn put_result_cache_entry(
        &self,
        key: &str,
        entry: RuntimeResultCacheEntry,
        epoch: Option<u64>,
        revision: u64,
    ) {
        if !self.result_cache_enabled() {
            return;
        }
        if epoch.is_some() {
            self.put_blob_result_cache_entry(key, entry, revision);
        }
    }

    fn put_blob_result_cache_entry(
        &self,
        key: &str,
        entry: RuntimeResultCacheEntry,
        revision: u64,
    ) {
        let policy = crate::storage::cache::BlobCachePolicy::default()
            .ttl_ms(self.result_cache_ttl_secs() * 1000)
            .priority(200);
        let dependencies = entry.scopes.iter().cloned().collect::<Vec<_>>();
        let bytes = encode_result_cache_payload(&entry)
            .unwrap_or_else(|| result_cache_fingerprint(&entry.result).into_bytes());
        let put = crate::storage::cache::BlobCachePut::new(bytes)
            .with_dependencies(dependencies)
            .with_policy(policy);
        let store = self.inner.db.store();
        let _publication = store.vault_publication_lock.lock();
        let _promotion = self.inner.result_blob_promotion.lock();
        let mut cache = self.inner.result_blob_entries.write();
        if cache.3 != revision
            || self.inner.db.store().result_cache_epoch() != Some(entry.data_epoch)
        {
            return;
        }
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
            let (ref map, _, _, _, ref mut volatile_keys, ..) = *cache;
            volatile_keys.retain(|key| map.contains_key(key));
        }
        drop(cache);
        self.record_result_cache_evictions(evicted);
    }

    pub fn invalidate_result_cache(&self) {
        self.invalidate_result_cache_inner();
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
        self.invalidate_result_cache_inner();
    }

    fn invalidate_result_cache_inner(&self) {
        if crate::storage::UnifiedStore::deferred_store_wal_capture_active() {
            self.invalidate_result_cache_during_wal_capture();
            return;
        }
        let store = self.inner.db.store();
        let _publication = store.vault_publication_lock.lock();
        let _promotion = self.inner.result_blob_promotion.lock();
        let mut blob_entries = self.inner.result_blob_entries.write();
        blob_entries.0.clear();
        blob_entries.1.clear();
        blob_entries.4.clear();
        blob_entries.5 = None;
        if let Some(epoch) = self.inner.db.store().result_cache_epoch() {
            blob_entries.6 = epoch;
        }
        blob_entries.2 = self.invalidate_blob_result_namespace();
        blob_entries.3 = blob_entries.3.wrapping_add(1);
        let mut ask_entries = self.inner.ask_answer_cache_entries.write();
        ask_entries.0.clear();
        ask_entries.1.clear();
        self.inner
            .result_blob_cache
            .invalidate_namespace(super::ASK_ANSWER_CACHE_NAMESPACE);
    }

    pub(crate) fn invalidate_result_cache_for_table(&self, table: &str) {
        #[cfg(test)]
        self.index_store_ref().mutation_test_hook(
            table,
            super::index_store::MutationTestPhase::BeforeResultCacheInvalidation,
        );
        if crate::storage::UnifiedStore::deferred_store_wal_capture_active() {
            self.invalidate_result_cache_during_wal_capture();
            return;
        }
        let store = self.inner.db.store();
        let _publication = store.vault_publication_lock.lock();
        let _promotion = self.inner.result_blob_promotion.lock();
        {
            let mut blob_entries = self.inner.result_blob_entries.write();
            let previous_epoch = blob_entries.6;
            if let Some(epoch) = self.inner.db.store().result_cache_epoch() {
                if previous_epoch.checked_add(1) == Some(epoch) {
                    for entry in blob_entries.0.values_mut() {
                        if entry.data_epoch == previous_epoch && !entry.scopes.contains(table) {
                            entry.data_epoch = epoch;
                        }
                    }
                } else if previous_epoch != epoch && !blob_entries.0.is_empty() {
                    // Multiple or out-of-order writes leave dependencies
                    // uncertain. Recompute without synchronous publication.
                    blob_entries.5 =
                        Some(std::time::Instant::now() + RESULT_CACHE_WRITE_QUIET_PERIOD);
                }
                blob_entries.6 = epoch;
            }
            // A query that started before any write must not publish after it.
            blob_entries.3 = blob_entries.3.wrapping_add(1);
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
            } else if has_match {
                blob_entries
                    .0
                    .retain(|_, entry| !entry.scopes.contains(table));
                let (ref map, ref mut order, _, ref mut revision, ref mut volatile_keys, ..) =
                    *blob_entries;
                order.retain(|key| map.contains_key(key));
                volatile_keys.retain(|key| map.contains_key(key));
                *revision = revision.wrapping_add(1);
            }
            if has_match {
                blob_entries.5 = Some(std::time::Instant::now() + RESULT_CACHE_WRITE_QUIET_PERIOD);
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
    const CHILD_OPERATION: &str = "REDDB_RESULT_CACHE_COMMIT_GAP_TEST_OPERATION";

    #[test]
    fn payload_without_a_data_epoch_is_not_rehydrated() {
        assert!(super::decode_result_cache_payload(b"invalid-payload").is_none());
    }

    #[test]
    fn result_cache_format_waits_for_wal_capture_to_finish() {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
        crate::storage::UnifiedStore::begin_deferred_store_wal_capture();
        let during_capture = runtime.result_cache_snapshot().0;
        let actions = crate::storage::UnifiedStore::take_deferred_store_wal_capture();
        assert!(actions.is_empty());
        assert_eq!(during_capture, None);
        assert!(!runtime.db().store().result_cache_format_fenced());
        assert!(runtime.result_cache_snapshot().0.is_some());
        assert!(runtime.db().store().result_cache_format_fenced());
    }

    #[test]
    fn promotion_gate_does_not_block_hits_and_orders_invalidation() {
        use std::sync::mpsc;
        use std::time::Duration;

        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
        for sql in [
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

        let guard = runtime.inner.result_blob_promotion.lock();
        let invalidator = runtime.clone();
        let (started_tx, started_rx) = mpsc::channel();
        let (done_tx, done_rx) = mpsc::channel();
        let invalidator_thread = std::thread::spawn(move || {
            started_tx.send(()).expect("report start");
            invalidator.invalidate_result_cache_for_table("promotion_gate");
            done_tx.send(()).expect("report invalidation");
        });
        let started = started_rx.recv_timeout(Duration::from_secs(3));
        let completed_early = done_rx.recv_timeout(Duration::from_millis(100));
        drop(guard);
        assert!(started.is_ok(), "invalidation did not start");
        assert!(
            matches!(completed_early, Err(mpsc::RecvTimeoutError::Timeout)),
            "invalidation must wait for an in-flight promotion"
        );
        done_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("invalidation did not finish");
        invalidator_thread.join().expect("invalidator thread");
        let before = runtime.result_cache_metrics();
        assert_eq!(
            runtime
                .execute_query("SELECT id FROM promotion_gate ORDER BY id")
                .expect("read after invalidation")
                .result
                .records
                .len(),
            2
        );
        assert_eq!(
            runtime.result_cache_metrics().1 - before.1,
            1,
            "invalidation must remove the cached result"
        );
    }

    #[test]
    fn durable_cache_publication_and_write_invalidation_do_not_invert_locks() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let directory = tempfile::tempdir().expect("temporary directory");
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(
            directory.path().join("cache-lock-order.rdb"),
        ))
        .expect("runtime");
        for sql in [
            "CREATE TABLE locked_write (id INT)",
            "CREATE TABLE cache_fill (id INT)",
            "INSERT INTO locked_write (id) VALUES (1)",
            "INSERT INTO cache_fill (id) VALUES (1)",
        ] {
            runtime.execute_query(sql).expect(sql);
        }
        runtime.result_cache_snapshot();
        std::thread::sleep(super::RESULT_CACHE_WRITE_QUIET_PERIOD + Duration::from_millis(50));

        let (write_paused_tx, write_paused_rx) = mpsc::channel();
        let (resume_write_tx, resume_write_rx) = mpsc::channel();
        let writer = runtime.clone();
        let (writer_done_tx, writer_done_rx) = mpsc::channel();
        let writer_thread = std::thread::spawn(move || {
            let store = writer.db().store();
            let _publication = store.vault_publication_lock.lock();
            write_paused_tx
                .send(store.vault_publication_lock.is_owned_by_current_thread())
                .expect("report paused writer");
            resume_write_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("resume writer");
            writer.invalidate_result_cache_for_table("locked_write");
            writer_done_tx.send(()).expect("report writer result");
        });
        assert!(
            write_paused_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("writer did not reach invalidation"),
            "write invalidation must still own the durable publication fence"
        );

        let cache_filler = runtime.clone();
        let (cache_done_tx, cache_done_rx) = mpsc::channel();
        let cache_thread = std::thread::spawn(move || {
            let result = cache_filler.execute_query("SELECT id FROM cache_fill ORDER BY id");
            cache_done_tx
                .send(result)
                .expect("report cache publication result");
        });

        // Before the fix, a result-cache fill takes the promotion gate and then
        // waits for the durable publication fence. Resuming the writer makes it
        // wait for that same promotion gate and closes the AB/BA cycle. With the
        // canonical order, the fill waits for the durable fence without holding
        // the promotion gate, so the writer can always finish.
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if runtime.inner.result_blob_promotion.try_lock().is_none() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        resume_write_tx.send(()).expect("resume writer");

        let writer_result = writer_done_rx.recv_timeout(Duration::from_secs(3));
        let cache_result = cache_done_rx.recv_timeout(Duration::from_secs(3));
        assert!(
            writer_result.is_ok() && cache_result.is_ok(),
            "durable cache publication deadlocked with write invalidation"
        );
        writer_result.unwrap();
        cache_result.unwrap().expect("cache publication result");
        writer_thread.join().expect("writer thread");
        cache_thread.join().expect("cache thread");
    }

    #[test]
    fn published_write_cannot_return_an_old_in_memory_result() {
        use std::sync::mpsc;
        use std::time::Duration;

        let directory = tempfile::tempdir().expect("temporary directory");
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(
            directory.path().join("live-gap.rdb"),
        ))
        .expect("runtime");
        for sql in [
            "CREATE TABLE live_gap (id INT)",
            "INSERT INTO live_gap (id) VALUES (1)",
            "SELECT id FROM live_gap ORDER BY id",
        ] {
            runtime.execute_query(sql).expect("prepare cache");
        }
        let (published_tx, published_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let resume_rx = Arc::new(parking_lot::Mutex::new(resume_rx));
        *runtime.index_store_ref().mutation_hook.lock() =
            Some(Arc::new(move |collection, phase| {
                if collection == "live_gap"
                    && phase == super::super::index_store::MutationTestPhase::StoragePublished
                {
                    published_tx.send(()).expect("report publication");
                    resume_rx
                        .lock()
                        .recv_timeout(Duration::from_secs(10))
                        .expect("resume writer");
                }
            }));
        let writer = runtime.clone();
        let writer_thread = std::thread::spawn(move || {
            writer.execute_query("INSERT INTO live_gap (id) VALUES (2)")
        });
        published_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("storage publication");
        let reader = runtime.clone();
        let (read_tx, read_rx) = mpsc::channel();
        let reader_thread = std::thread::spawn(move || {
            let rows = reader
                .execute_query("SELECT id FROM live_gap ORDER BY id")
                .expect("read during publication")
                .result
                .records
                .len();
            read_tx.send(rows).expect("report rows");
        });
        let early = read_rx.recv_timeout(Duration::from_secs(2));
        resume_tx.send(()).expect("resume writer");
        writer_thread.join().expect("writer thread").expect("write");
        let rows = match early {
            Ok(rows) => rows,
            Err(mpsc::RecvTimeoutError::Timeout) => read_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("reader finished"),
            Err(error) => panic!("reader failed: {error}"),
        };
        reader_thread.join().expect("reader thread");
        assert_eq!(
            rows, 2,
            "old cached result was returned after storage publication"
        );
    }

    #[test]
    fn unrelated_pending_write_keeps_a_cached_read_available() {
        use std::sync::mpsc;
        use std::time::Duration;

        let directory = tempfile::tempdir().expect("temporary directory");
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(
            directory.path().join("pending-unrelated.rdb"),
        ))
        .expect("runtime");
        for sql in [
            "CREATE TABLE cached_items (id INT)",
            "CREATE TABLE unrelated_items (id INT)",
            "INSERT INTO cached_items (id) VALUES (1)",
            "INSERT INTO unrelated_items (id) VALUES (1)",
            "SELECT id FROM cached_items ORDER BY id",
            "SELECT id FROM cached_items ORDER BY id",
        ] {
            runtime.execute_query(sql).expect(sql);
        }
        let before = runtime.result_cache_metrics();
        runtime
            .execute_query("SELECT id FROM cached_items ORDER BY id")
            .expect("warm hit");
        assert_eq!(runtime.result_cache_metrics().0 - before.0, 1);
        let (pending_tx, pending_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let resume_rx = Arc::new(parking_lot::Mutex::new(resume_rx));
        runtime
            .db()
            .store()
            .set_result_cache_pending_hook(Some(Arc::new(move || {
                pending_tx.send(()).expect("report pending");
                resume_rx
                    .lock()
                    .recv_timeout(Duration::from_secs(10))
                    .expect("resume");
            })));
        let writer = runtime.clone();
        let writer_thread = std::thread::spawn(move || {
            writer.execute_query("UPDATE unrelated_items SET id = 2 WHERE id = 1")
        });
        pending_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("publication pending");
        let before = runtime.result_cache_metrics();
        let reader = runtime.clone();
        let (read_tx, read_rx) = mpsc::channel();
        let reader_thread = std::thread::spawn(move || {
            read_tx
                .send(reader.execute_query("SELECT id FROM cached_items ORDER BY id"))
                .unwrap();
        });
        let result = read_rx.recv_timeout(Duration::from_secs(3));
        runtime.db().store().set_result_cache_pending_hook(None);
        resume_tx.send(()).expect("resume writer");
        writer_thread.join().unwrap().expect("write");
        reader_thread.join().unwrap();
        assert_eq!(
            result
                .expect("cached read must not wait")
                .expect("read")
                .result
                .records
                .len(),
            1
        );
        assert_eq!(
            runtime.result_cache_metrics().0 - before.0,
            1,
            "unrelated pending write must preserve the hit"
        );
    }

    #[test]
    fn committed_insert_before_cache_invalidation_does_not_recover_stale_result() {
        if let Some(path) = std::env::var_os(CHILD_PATH) {
            let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(path))
                .expect("open child database");
            let operation = std::env::var(CHILD_OPERATION).expect("child operation");
            if operation.starts_with("format_crash:") || operation.starts_with("format_seal:") {
                let store = runtime.db().store();
                store
                    .insert_auto(
                        "format_data",
                        crate::storage::UnifiedEntity::table_row(
                            crate::storage::EntityId::new(0),
                            "format_data",
                            0,
                            vec![reddb_types::Value::Integer(42)],
                        ),
                    )
                    .expect("seed durable data");
                if let Some(point) = operation.strip_prefix("format_crash:") {
                    std::env::set_var("REDDB_EMBEDDED_RDB_CRASH_AT", point);
                } else {
                    let point = operation.strip_prefix("format_seal:").unwrap().to_owned();
                    *runtime.index_store_ref().mutation_hook.lock() = Some(Arc::new(
                        move |_, phase| {
                            if phase == super::super::index_store::MutationTestPhase::ResultCacheFormatCheckpointed {
                            if point == "before" {
                                std::process::exit(86);
                            }
                            std::env::set_var("REDDB_EMBEDDED_RDB_CRASH_AT", &point);
                        }
                        },
                    ));
                }
                runtime
                    .execute_query("SELECT * FROM format_data")
                    .expect("format activation");
                panic!("format crash point was not reached");
            }
            for sql in [
                "CREATE TABLE cache_gap (id INT)",
                "INSERT INTO cache_gap (id) VALUES (1)",
                "SELECT id FROM cache_gap ORDER BY id",
            ] {
                runtime.execute_query(sql).expect("prepare cache");
            }
            if operation == "format_fence" {
                assert!(runtime.db().store().result_cache_format_fenced());
                std::process::exit(86);
            }
            if operation == "transaction" {
                let (key, mut old_entry) = {
                    let cache = runtime.inner.result_blob_entries.read();
                    let (key, entry) = cache.0.iter().next().expect("warm original result");
                    (key.clone(), entry.clone())
                };
                super::super::execution_context::set_current_connection_id(901);
                runtime.execute_query("BEGIN").expect("begin transaction");
                runtime
                    .execute_query("INSERT INTO cache_gap (id) VALUES (2)")
                    .expect("stage insert");
                let reader = runtime.clone();
                std::thread::spawn(move || {
                    super::super::execution_context::set_current_connection_id(902);
                    let read = reader
                        .execute_query("SELECT id FROM cache_gap ORDER BY id")
                        .expect("cache committed view");
                    assert_eq!(
                        read.result.records.len(),
                        1,
                        "uncommitted row must not be visible"
                    );
                    // Fault-inject an old durable result after transaction DML.
                    // Normal queries bypass the cache while any xid is active.
                    old_entry.data_epoch = reader.db().store().result_cache_epoch().expect("epoch");
                    let payload =
                        super::encode_result_cache_payload(&old_entry).expect("encode old result");
                    reader
                        .inner
                        .result_blob_cache
                        .put(
                            super::RESULT_CACHE_BLOB_NAMESPACE,
                            &key,
                            crate::storage::cache::BlobCachePut::new(payload),
                        )
                        .expect("persist old result");
                    reader.checkpoint().expect("persist cached result");
                    let hit = reader
                        .inner
                        .result_blob_cache
                        .get(super::RESULT_CACHE_BLOB_NAMESPACE, &key)
                        .expect("old entry remains");
                    assert!(super::decode_result_cache_payload(hit.value()).is_some());
                })
                .join()
                .expect("pre-commit reader");
            } else {
                runtime.checkpoint().expect("persist cached result");
            }
            let transaction = operation == "transaction";
            *runtime.index_store_ref().mutation_hook.lock() = Some(Arc::new(
                move |collection, phase| {
                    if (transaction && phase == super::super::index_store::MutationTestPhase::TransactionWalCommitted)
                        || (!transaction && collection == "cache_gap"
                            && phase == super::super::index_store::MutationTestPhase::BeforeResultCacheInvalidation)
                    {
                        std::process::exit(86);
                    }
                },
            ));
            if operation.starts_with("wal_") {
                std::env::set_var("REDDB_EMBEDDED_RDB_CRASH_AT", &operation);
            }
            let sql = match operation.as_str() {
                "insert"
                | "wal_after_frame_write"
                | "wal_after_frame_sync"
                | "wal_after_superblock_write" => "INSERT INTO cache_gap (id) VALUES (2)",
                "update" => "UPDATE cache_gap SET id = 2 WHERE id = 1",
                "delete" => "DELETE FROM cache_gap WHERE id = 1",
                "transaction" => "COMMIT",
                _ => panic!("unknown operation"),
            };
            runtime
                .execute_query(sql)
                .expect("hook must terminate before return");
            panic!("storage-published hook was not reached");
        }

        let dir = tempfile::tempdir().expect("temporary directory");
        for (operation, expected) in [
            ("insert", Some(vec![1, 2])),
            ("update", Some(vec![2])),
            ("delete", Some(vec![])),
            ("transaction", Some(vec![1, 2])),
            ("wal_after_frame_write", None),
            ("wal_after_frame_sync", None),
            ("wal_after_superblock_write", None),
        ] {
            let path = dir.path().join(format!("cache-gap-{operation}.rdb"));
            let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
                .args([
                    "--exact",
                    "runtime::result_cache_runtime::crash_tests::committed_insert_before_cache_invalidation_does_not_recover_stale_result",
                    "--nocapture",
                ])
                .env(CHILD_PATH, &path)
                .env(CHILD_OPERATION, operation)
                .output()
                .expect("run child");
            assert_eq!(
                output.status.code(),
                Some(if operation.starts_with("wal_") {
                    173
                } else {
                    86
                }),
                "{operation} child did not reach crash hook: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let runtime =
                RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen");
            let before = runtime.result_cache_metrics();
            let cached = runtime
                .execute_query("SELECT id FROM cache_gap ORDER BY id")
                .expect("cached read");
            let after = runtime.result_cache_metrics();
            let values = |result: &crate::runtime::RuntimeQueryResult| {
                result
                    .result
                    .records
                    .iter()
                    .map(|record| match record.get("id") {
                        Some(reddb_types::Value::Integer(value)) => *value,
                        other => panic!("expected id, got {other:?}"),
                    })
                    .collect::<Vec<_>>()
            };
            runtime
                .execute_query("SET CONFIG runtime.result_cache.enabled = false")
                .expect("disable cache");
            let actual = runtime
                .execute_query("SELECT id FROM cache_gap ORDER BY id")
                .expect("authoritative read");
            let actual = values(&actual);
            if let Some(expected) = expected {
                assert_eq!(actual, expected, "{operation} committed data must survive");
            } else {
                assert!(
                    actual == vec![1] || actual == vec![1, 2],
                    "{operation} recovered a partial write"
                );
            }
            assert_eq!(
                values(&cached),
                actual,
                "{operation} recovered a stale result"
            );
            if actual != vec![1] {
                assert_eq!(
                    after.1 - before.1,
                    1,
                    "{operation} must reject the old L2 entry"
                );
            }
        }
    }

    #[test]
    fn result_cache_format_fences_both_embedded_superblocks() {
        use std::io::{Seek, SeekFrom, Write};
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("format-fence.rdb");
        let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "runtime::result_cache_runtime::crash_tests::committed_insert_before_cache_invalidation_does_not_recover_stale_result",
                "--nocapture",
            ])
            .env(CHILD_PATH, &path)
            .env(CHILD_OPERATION, "format_fence")
            .status()
            .expect("run child");
        assert_eq!(status.code(), Some(86));
        for index in 0..2 {
            let copy = directory.path().join(format!("format-fence-{index}.rdb"));
            std::fs::copy(&path, &copy).expect("copy database");
            let mut file = std::fs::OpenOptions::new().write(true).open(&copy).unwrap();
            file.seek(SeekFrom::Start(
                index * reddb_file::EMBEDDED_RDB_SUPERBLOCK_SIZE + 64,
            ))
            .unwrap();
            file.write_all(&[0xff]).unwrap();
            file.sync_all().unwrap();
            drop(file);
            let open = crate::storage::EmbeddedRdbArtifact::open(&copy).expect("other superblock");
            let snapshot = crate::storage::EmbeddedRdbArtifact::read_snapshot(&open)
                .unwrap()
                .unwrap();
            assert_eq!(
                reddb_file::decode_native_store_header(&snapshot).unwrap(),
                reddb_file::STORE_VERSION_V13
            );
        }
    }

    #[test]
    fn result_cache_format_fence_and_warm_result_survive_restart() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("warm-fenced.rdb");
        {
            let runtime =
                RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime");
            for sql in [
                "CREATE TABLE warm_fence (id INT)",
                "INSERT INTO warm_fence (id) VALUES (1)",
                "SELECT id FROM warm_fence ORDER BY id",
            ] {
                runtime.execute_query(sql).expect(sql);
            }
            assert!(runtime.db().store().result_cache_format_fenced());
        }
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("reopen");
        assert!(
            runtime.db().store().result_cache_format_fenced(),
            "durable fence must load before the first query"
        );
        let before = runtime.result_cache_metrics();
        assert_eq!(
            runtime
                .execute_query("SELECT id FROM warm_fence ORDER BY id")
                .expect("read")
                .result
                .records
                .len(),
            1
        );
        assert_eq!(
            runtime.result_cache_metrics().0 - before.0,
            1,
            "valid result must recover warm"
        );
    }

    #[test]
    fn result_cache_format_checkpoint_survives_interrupted_publication() {
        let directory = tempfile::tempdir().expect("temporary directory");
        for (operation, expected_status) in [
            ("format_crash:snapshot_after_image_write", 173),
            ("format_crash:snapshot_after_image_sync", 173),
            ("format_crash:snapshot_after_manifest_write", 173),
            ("format_crash:snapshot_after_manifest_sync", 173),
            ("format_crash:snapshot_after_superblock_write", 173),
            ("format_seal:before", 86),
            ("format_seal:wal_after_frame_write", 173),
            ("format_seal:wal_after_frame_sync", 173),
            ("format_seal:wal_after_superblock_write", 173),
        ] {
            let path = directory
                .path()
                .join(format!("{}.rdb", operation.replace(':', "-")));
            let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
                .args([
                    "--exact",
                    "runtime::result_cache_runtime::crash_tests::committed_insert_before_cache_invalidation_does_not_recover_stale_result",
                    "--nocapture",
                ])
                .env(CHILD_PATH, &path)
                .env(CHILD_OPERATION, operation)
                .status().expect("run child");
            assert_eq!(status.code(), Some(expected_status), "crash at {operation}");
            let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
                .expect("recover database");
            assert_eq!(
                runtime
                    .db()
                    .store()
                    .get_collection("format_data")
                    .expect("recovered table")
                    .count(),
                1
            );
            runtime
                .execute_query("SELECT * FROM format_data")
                .expect("activate format after recovery");
            assert!(runtime.db().store().result_cache_format_fenced());
        }
    }
}
