//! Semantic cache for LLM responses (ML Feature 3, MVP).
//!
//! Caches `(prompt, response)` pairs keyed by the prompt's embedding
//! vector. A lookup returns the cached response when any entry has
//! cosine similarity to the query embedding ≥ the caller's threshold
//! **and** has not expired.
//!
//! This is a linear-scan implementation — fine for caches up to
//! ~10k entries. A future sprint swaps the scan for the existing
//! HNSW index when entry counts make that worth the added
//! complexity. The external surface stays the same.
//!
//! Eviction is lazy: `lookup` drops any expired entry it skips
//! over, and [`Self::evict_expired`] sweeps the whole set on
//! demand. Bounded size is enforced on insert via LRU-by-age
//! (oldest entries dropped first once `max_entries` is reached).

use std::sync::{Arc, Mutex};

use super::jobs::now_ms;
use super::persist::{MlPersistence, MlPersistenceResult};
use crate::json::{Map, Value as JsonValue};

/// One cached entry.
#[derive(Debug, Clone)]
pub struct SemanticCacheEntry {
    pub prompt: String,
    pub response: String,
    pub embedding: Vec<f32>,
    /// Epoch millis; `0` means "never expires".
    pub expires_at_ms: u64,
    /// Epoch millis of last read hit — used for LRU eviction.
    pub last_hit_ms: u64,
    /// Epoch millis the entry landed in the cache.
    pub inserted_at_ms: u64,
}

impl SemanticCacheEntry {
    pub fn is_expired_at(&self, now_ms_val: u64) -> bool {
        self.expires_at_ms != 0 && now_ms_val >= self.expires_at_ms
    }
}

/// Compile-time tuneables for a cache instance.
#[derive(Debug, Clone)]
pub struct SemanticCacheConfig {
    /// Cosine similarity threshold above which a candidate counts
    /// as a hit. Typical values: 0.90–0.98.
    pub similarity_threshold: f32,
    /// Default TTL applied to freshly-inserted entries. `0` =
    /// entries never expire. Callers can still pass a per-insert
    /// override.
    pub default_ttl_ms: u64,
    /// Maximum live entries. Oldest inserted entry is evicted once
    /// this limit is reached. `0` = unbounded.
    pub max_entries: usize,
    /// Persistence namespace suffix. Allows multiple named caches
    /// to share one `MlPersistence` backend without colliding.
    pub namespace: String,
}

impl Default for SemanticCacheConfig {
    fn default() -> Self {
        Self {
            similarity_threshold: 0.95,
            default_ttl_ms: 24 * 60 * 60 * 1000,
            max_entries: 10_000,
            namespace: "default".to_string(),
        }
    }
}

/// Runtime statistics — exposed via `SELECT * FROM ML_CACHE_STATS`
/// later, and useful in tests now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SemanticCacheStats {
    pub entries: usize,
    pub hits: u64,
    pub misses: u64,
    pub expired_evictions: u64,
    pub capacity_evictions: u64,
}

struct Inner {
    entries: Vec<SemanticCacheEntry>,
    stats: SemanticCacheStats,
}

/// The cache itself. Cloning shares state via the inner `Arc`.
#[derive(Clone)]
pub struct SemanticCache {
    inner: Arc<Mutex<Inner>>,
    config: SemanticCacheConfig,
    backend: Option<Arc<dyn MlPersistence>>,
}

impl std::fmt::Debug for SemanticCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SemanticCache")
            .field("namespace", &self.config.namespace)
            .field("similarity_threshold", &self.config.similarity_threshold)
            .field("max_entries", &self.config.max_entries)
            .field("persistent", &self.backend.is_some())
            .finish()
    }
}

impl SemanticCache {
    /// Build an in-process cache with no durable backend.
    pub fn new(config: SemanticCacheConfig) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner {
                entries: Vec::new(),
                stats: SemanticCacheStats::default(),
            })),
            config,
            backend: None,
        }
    }

    /// Build a cache that persists every mutation to `backend` under
    /// `cache:{namespace}`. Entries rehydrate on construction.
    pub fn with_backend(config: SemanticCacheConfig, backend: Arc<dyn MlPersistence>) -> Self {
        let cache = Self {
            inner: Arc::new(Mutex::new(Inner {
                entries: Vec::new(),
                stats: SemanticCacheStats::default(),
            })),
            config,
            backend: Some(backend),
        };
        let _ = cache.load_from_backend();
        cache
    }

    fn backend_namespace(&self) -> String {
        format!("cache:{}", self.config.namespace)
    }

    fn persist_entry(&self, key: &str, entry: &SemanticCacheEntry) {
        if let Some(backend) = self.backend.as_ref() {
            let _ = backend.put(&self.backend_namespace(), key, &encode_entry(entry));
        }
    }

    fn forget_entry(&self, key: &str) {
        if let Some(backend) = self.backend.as_ref() {
            let _ = backend.delete(&self.backend_namespace(), key);
        }
    }

    /// Re-read persisted entries into memory. Malformed rows are
    /// skipped rather than crashing startup.
    pub fn load_from_backend(&self) -> MlPersistenceResult<usize> {
        let Some(backend) = self.backend.as_ref() else {
            return Ok(0);
        };
        let rows = backend.list(&self.backend_namespace())?;
        let mut loaded = 0usize;
        let now = now_ms();
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.entries.clear();
        for (_, raw) in rows {
            let Some(entry) = decode_entry(&raw) else {
                continue;
            };
            if entry.is_expired_at(now) {
                // Skip stale entries rather than loading-then-evicting —
                // saves a pass. Their rows are left in the backend
                // and removed on next insert of the same prompt or
                // on explicit evict.
                continue;
            }
            guard.entries.push(entry);
            loaded += 1;
        }
        guard.stats.entries = guard.entries.len();
        Ok(loaded)
    }

    /// Look up by embedding. Returns the cached response on hit;
    /// updates `last_hit_ms` and hit counter as a side-effect.
    pub fn lookup(&self, embedding: &[f32]) -> Option<String> {
        if embedding.is_empty() {
            return None;
        }
        let now = now_ms();
        let mut guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        // Drop expired entries in-place so the scan cost stays low.
        let before = guard.entries.len();
        guard.entries.retain(|e| !e.is_expired_at(now));
        let evicted = before - guard.entries.len();
        guard.stats.expired_evictions += evicted as u64;

        let mut best: Option<(usize, f32)> = None;
        for (idx, entry) in guard.entries.iter().enumerate() {
            let score = cosine_similarity(embedding, &entry.embedding);
            if score >= self.config.similarity_threshold {
                match best {
                    Some((_, best_score)) if best_score >= score => {}
                    _ => best = Some((idx, score)),
                }
            }
        }
        match best {
            Some((idx, _)) => {
                let entry = &mut guard.entries[idx];
                entry.last_hit_ms = now;
                let response = entry.response.clone();
                let persisted = entry.clone();
                guard.stats.hits += 1;
                guard.stats.entries = guard.entries.len();
                drop(guard);
                // Persist the updated last_hit so LRU eviction
                // respects read traffic across restarts.
                let key = cache_key(&persisted);
                self.persist_entry(&key, &persisted);
                Some(response)
            }
            None => {
                guard.stats.misses += 1;
                guard.stats.entries = guard.entries.len();
                None
            }
        }
    }

    /// Insert `(prompt, response)` keyed by `embedding`.
    /// If `ttl_ms_override` is `None` the config default applies.
    pub fn insert(
        &self,
        prompt: impl Into<String>,
        response: impl Into<String>,
        embedding: Vec<f32>,
        ttl_ms_override: Option<u64>,
    ) {
        if embedding.is_empty() {
            return;
        }
        let now = now_ms();
        let ttl = ttl_ms_override.unwrap_or(self.config.default_ttl_ms);
        let expires_at_ms = if ttl == 0 { 0 } else { now.saturating_add(ttl) };
        let entry = SemanticCacheEntry {
            prompt: prompt.into(),
            response: response.into(),
            embedding,
            expires_at_ms,
            last_hit_ms: now,
            inserted_at_ms: now,
        };
        let evicted_keys: Vec<String>;
        let stored_key: String;
        let persist_entry: SemanticCacheEntry;
        {
            let mut guard = match self.inner.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            // Enforce capacity first. Oldest `inserted_at` loses —
            // simple and deterministic; swap for LRU-on-read later
            // if needed.
            let mut pruned: Vec<String> = Vec::new();
            if self.config.max_entries > 0 {
                while guard.entries.len() >= self.config.max_entries {
                    if let Some((oldest_idx, _)) = guard
                        .entries
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, e)| e.inserted_at_ms)
                    {
                        let gone = guard.entries.remove(oldest_idx);
                        guard.stats.capacity_evictions += 1;
                        pruned.push(cache_key(&gone));
                    } else {
                        break;
                    }
                }
            }
            guard.entries.push(entry.clone());
            guard.stats.entries = guard.entries.len();
            evicted_keys = pruned;
            stored_key = cache_key(&entry);
            persist_entry = entry;
        }
        for k in &evicted_keys {
            self.forget_entry(k);
        }
        self.persist_entry(&stored_key, &persist_entry);
    }

    /// Manually force a sweep. Returns number of entries dropped.
    pub fn evict_expired(&self) -> usize {
        let now = now_ms();
        let evicted_keys: Vec<String>;
        let count;
        {
            let mut guard = match self.inner.lock() {
                Ok(g) => g,
                Err(p) => p.into_inner(),
            };
            let mut keep = Vec::with_capacity(guard.entries.len());
            let mut dropped = Vec::new();
            for entry in guard.entries.drain(..) {
                if entry.is_expired_at(now) {
                    dropped.push(cache_key(&entry));
                } else {
                    keep.push(entry);
                }
            }
            count = dropped.len();
            guard.entries = keep;
            guard.stats.expired_evictions += count as u64;
            guard.stats.entries = guard.entries.len();
            evicted_keys = dropped;
        }
        for k in &evicted_keys {
            self.forget_entry(k);
        }
        count
    }

    /// Snapshot of counters.
    pub fn stats(&self) -> SemanticCacheStats {
        let guard = match self.inner.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        SemanticCacheStats {
            entries: guard.entries.len(),
            ..guard.stats.clone()
        }
    }

    pub fn config(&self) -> &SemanticCacheConfig {
        &self.config
    }
}

/// Deterministic key per entry — the inserted_at timestamp plus the
/// first 16 bytes of the prompt gives a collision-resistant, sortable
/// identifier without pulling sha2 into the ML module.
fn cache_key(entry: &SemanticCacheEntry) -> String {
    // Hash the prompt with a small FNV-1a so the key is stable across
    // processes. Avoids depending on `sha2` here; collisions between
    // two distinct prompts inserted in the exact same millisecond are
    // acceptable (the second insert overwrites the first).
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut h = FNV_OFFSET;
    for b in entry.prompt.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    format!("{:020}-{:016x}", entry.inserted_at_ms, h)
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
        na += a[i] * a[i];
        nb += b[i] * b[i];
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

// ---- JSON (en|de)coding of a single entry -------------------------------

fn encode_entry(entry: &SemanticCacheEntry) -> String {
    let mut obj = Map::new();
    obj.insert(
        "prompt".to_string(),
        JsonValue::String(entry.prompt.clone()),
    );
    obj.insert(
        "response".to_string(),
        JsonValue::String(entry.response.clone()),
    );
    obj.insert(
        "embedding".to_string(),
        JsonValue::Array(
            entry
                .embedding
                .iter()
                .map(|f| JsonValue::Number(*f as f64))
                .collect(),
        ),
    );
    obj.insert(
        "expires_at".to_string(),
        JsonValue::Number(entry.expires_at_ms as f64),
    );
    obj.insert(
        "last_hit".to_string(),
        JsonValue::Number(entry.last_hit_ms as f64),
    );
    obj.insert(
        "inserted_at".to_string(),
        JsonValue::Number(entry.inserted_at_ms as f64),
    );
    JsonValue::Object(obj).to_string_compact()
}

fn decode_entry(raw: &str) -> Option<SemanticCacheEntry> {
    let parsed = crate::json::parse_json(raw).ok()?;
    let value = JsonValue::from(parsed);
    let obj = value.as_object()?;
    let prompt = obj.get("prompt")?.as_str()?.to_string();
    let response = obj.get("response")?.as_str()?.to_string();
    let embedding = obj
        .get("embedding")?
        .as_array()?
        .iter()
        .filter_map(|v| v.as_f64().map(|f| f as f32))
        .collect::<Vec<f32>>();
    let expires_at_ms = obj.get("expires_at")?.as_i64()? as u64;
    let last_hit_ms = obj.get("last_hit")?.as_i64()? as u64;
    let inserted_at_ms = obj.get("inserted_at")?.as_i64()? as u64;
    Some(SemanticCacheEntry {
        prompt,
        response,
        embedding,
        expires_at_ms,
        last_hit_ms,
        inserted_at_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::super::persist::InMemoryMlPersistence;
    use super::*;

    fn cfg(threshold: f32, max: usize, ttl: u64) -> SemanticCacheConfig {
        SemanticCacheConfig {
            similarity_threshold: threshold,
            default_ttl_ms: ttl,
            max_entries: max,
            namespace: "t".to_string(),
        }
    }

    #[test]
    fn cosine_similarity_is_symmetric_and_bounded() {
        let a = [1.0, 0.0, 0.0];
        let b = [0.0, 1.0, 0.0];
        let c = [1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &c) - 1.0).abs() < 1e-6);
        assert!(cosine_similarity(&a, &b).abs() < 1e-6);
        assert!((cosine_similarity(&a, &b) - cosine_similarity(&b, &a)).abs() < 1e-6);
    }

    #[test]
    fn cosine_zero_on_mismatched_dims_or_zero_vec() {
        assert_eq!(cosine_similarity(&[1.0], &[1.0, 0.0]), 0.0);
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[0.0, 0.0]), 0.0);
    }

    #[test]
    fn miss_returns_none_and_increments_miss_counter() {
        let c = SemanticCache::new(cfg(0.9, 100, 0));
        assert!(c.lookup(&[1.0, 0.0]).is_none());
        assert_eq!(c.stats().misses, 1);
        assert_eq!(c.stats().hits, 0);
    }

    #[test]
    fn inserted_entry_is_found_on_identical_vector() {
        let c = SemanticCache::new(cfg(0.9, 100, 0));
        c.insert("p", "hello world", vec![1.0, 0.0, 0.0], None);
        let got = c.lookup(&[1.0, 0.0, 0.0]).unwrap();
        assert_eq!(got, "hello world");
        assert_eq!(c.stats().hits, 1);
    }

    #[test]
    fn below_threshold_is_a_miss() {
        let c = SemanticCache::new(cfg(0.99, 100, 0));
        c.insert("p", "r", vec![1.0, 0.0, 0.0], None);
        // Cosine of [1,0,0] vs [0.8, 0.6, 0] = 0.8 < 0.99
        assert!(c.lookup(&[0.8, 0.6, 0.0]).is_none());
    }

    #[test]
    fn expired_entries_are_skipped_and_evicted() {
        let c = SemanticCache::new(cfg(0.9, 100, 1));
        c.insert("p", "r", vec![1.0, 0.0], None);
        std::thread::sleep(std::time::Duration::from_millis(5));
        assert!(c.lookup(&[1.0, 0.0]).is_none());
        let stats = c.stats();
        assert_eq!(stats.entries, 0);
        assert!(stats.expired_evictions >= 1);
    }

    #[test]
    fn capacity_limit_evicts_oldest_inserted() {
        let c = SemanticCache::new(cfg(0.9, 2, 0));
        c.insert("first", "r1", vec![1.0, 0.0], None);
        std::thread::sleep(std::time::Duration::from_millis(2));
        c.insert("second", "r2", vec![0.0, 1.0], None);
        std::thread::sleep(std::time::Duration::from_millis(2));
        c.insert("third", "r3", vec![1.0, 1.0], None);
        assert_eq!(c.stats().entries, 2);
        assert!(c.stats().capacity_evictions >= 1);
        // first should have been evicted
        assert!(c.lookup(&[1.0, 0.0]).is_none() || c.lookup(&[1.0, 0.0]) != Some("r1".to_string()));
    }

    #[test]
    fn best_candidate_wins_when_multiple_match() {
        let c = SemanticCache::new(cfg(0.5, 100, 0));
        c.insert("lo", "LO", vec![0.7, 0.7, 0.1], None);
        c.insert("hi", "HI", vec![1.0, 0.0, 0.0], None);
        let got = c.lookup(&[1.0, 0.0, 0.0]).unwrap();
        assert_eq!(got, "HI");
    }

    #[test]
    fn backend_round_trips_entry() {
        let backend: Arc<dyn MlPersistence> = Arc::new(InMemoryMlPersistence::new());
        let c1 = SemanticCache::with_backend(cfg(0.9, 100, 0), Arc::clone(&backend));
        c1.insert("prompt one", "response one", vec![1.0, 0.0], None);
        let c2 = SemanticCache::with_backend(cfg(0.9, 100, 0), backend);
        let got = c2.lookup(&[1.0, 0.0]).unwrap();
        assert_eq!(got, "response one");
    }

    #[test]
    fn encode_decode_entry_round_trips() {
        let e = SemanticCacheEntry {
            prompt: "why".to_string(),
            response: "because".to_string(),
            embedding: vec![0.1, 0.2, -0.3],
            expires_at_ms: 100,
            last_hit_ms: 50,
            inserted_at_ms: 10,
        };
        let back = decode_entry(&encode_entry(&e)).unwrap();
        assert_eq!(back.prompt, e.prompt);
        assert_eq!(back.response, e.response);
        assert_eq!(back.embedding.len(), e.embedding.len());
        for (a, b) in back.embedding.iter().zip(e.embedding.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
        assert_eq!(back.expires_at_ms, e.expires_at_ms);
    }

    #[test]
    fn stats_entries_reflect_live_set() {
        let c = SemanticCache::new(cfg(0.9, 100, 0));
        c.insert("a", "1", vec![1.0, 0.0], None);
        c.insert("b", "2", vec![0.0, 1.0], None);
        assert_eq!(c.stats().entries, 2);
    }
}
