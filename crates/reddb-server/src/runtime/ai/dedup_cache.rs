//! Embedding dedup cache — issue #277.
//!
//! Optional LRU cache keyed by SHA-256(text) → Vec<f32>.
//! Off by default; opt-in via `runtime.ai.embedding_dedup_enabled = true`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Global process-wide dedup hit counter (for /metrics).
pub static DEDUP_HITS_TOTAL: AtomicU64 = AtomicU64::new(0);
/// Global process-wide dedup miss counter (for /metrics).
pub static DEDUP_MISSES_TOTAL: AtomicU64 = AtomicU64::new(0);

use lru::LruCache;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};

pub const CONFIG_DEDUP_ENABLED: &str = "runtime.ai.embedding_dedup_enabled";
pub const CONFIG_DEDUP_TTL_MS: &str = "runtime.ai.embedding_dedup_ttl_ms";
pub const CONFIG_DEDUP_LRU_SIZE: &str = "runtime.ai.embedding_dedup_lru_size";

pub const DEFAULT_DEDUP_TTL_MS: u64 = 60_000;
pub const DEFAULT_DEDUP_LRU_SIZE: usize = 4096;

type HashKey = [u8; 32];

struct Entry {
    embedding: Vec<f32>,
    inserted_at: Instant,
}

pub struct EmbeddingDedupCache {
    inner: Mutex<LruCache<HashKey, Entry>>,
    ttl: Duration,
    hits: AtomicU64,
    misses: AtomicU64,
}

impl EmbeddingDedupCache {
    pub fn new(max_size: usize, ttl: Duration) -> Self {
        let capacity = std::num::NonZeroUsize::new(max_size.max(1))
            .expect("max_size >= 1");
        Self {
            inner: Mutex::new(LruCache::new(capacity)),
            ttl,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    /// Look up `text` in the cache. Returns `Some(embedding)` on hit.
    pub fn get(&self, text: &str) -> Option<Vec<f32>> {
        let key = hash(text);
        let mut guard = self.inner.lock();
        match guard.get(&key) {
            Some(entry) if entry.inserted_at.elapsed() < self.ttl => {
                self.hits.fetch_add(1, Ordering::Relaxed);
                DEDUP_HITS_TOTAL.fetch_add(1, Ordering::Relaxed);
                Some(entry.embedding.clone())
            }
            Some(_expired) => {
                // TTL expired — remove and treat as miss
                guard.pop(&key);
                self.misses.fetch_add(1, Ordering::Relaxed);
                DEDUP_MISSES_TOTAL.fetch_add(1, Ordering::Relaxed);
                None
            }
            None => {
                self.misses.fetch_add(1, Ordering::Relaxed);
                DEDUP_MISSES_TOTAL.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// Insert `embedding` for `text`.
    pub fn insert(&self, text: &str, embedding: Vec<f32>) {
        let key = hash(text);
        self.inner.lock().put(key, Entry { embedding, inserted_at: Instant::now() });
    }

    pub fn hits(&self) -> u64 {
        self.hits.load(Ordering::Relaxed)
    }

    pub fn misses(&self) -> u64 {
        self.misses.load(Ordering::Relaxed)
    }
}

fn hash(text: &str) -> HashKey {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cache(size: usize, ttl_ms: u64) -> EmbeddingDedupCache {
        EmbeddingDedupCache::new(size, Duration::from_millis(ttl_ms))
    }

    #[test]
    fn miss_then_hit() {
        let c = cache(16, 60_000);
        assert!(c.get("hello").is_none());
        c.insert("hello", vec![1.0, 2.0]);
        let v = c.get("hello").unwrap();
        assert_eq!(v, vec![1.0, 2.0]);
        assert_eq!(c.hits(), 1);
        assert_eq!(c.misses(), 1);
    }

    #[test]
    fn lru_eviction() {
        let c = cache(2, 60_000);
        c.insert("a", vec![1.0]);
        c.insert("b", vec![2.0]);
        // access "a" to make "b" the LRU
        c.get("a");
        c.insert("c", vec![3.0]); // evicts "b"
        assert!(c.get("b").is_none());
        assert!(c.get("a").is_some());
        assert!(c.get("c").is_some());
    }

    #[test]
    fn ttl_expired_treated_as_miss() {
        let c = cache(16, 1); // 1ms TTL
        c.insert("x", vec![9.9]);
        std::thread::sleep(Duration::from_millis(5));
        assert!(c.get("x").is_none());
    }

    #[test]
    fn dedup_1000_inputs_10_unique() {
        // simulate: 1000 inputs with 10 unique texts → only 10 misses
        let c = cache(1024, 60_000);
        let unique: Vec<String> = (0..10).map(|i| format!("text {i}")).collect();
        let inputs: Vec<String> = (0..1000).map(|i| unique[i % 10].clone()).collect();

        let mut miss_count = 0usize;
        let mut embeddings: Vec<Vec<f32>> = Vec::with_capacity(inputs.len());
        for text in &inputs {
            if let Some(cached) = c.get(text) {
                embeddings.push(cached);
            } else {
                miss_count += 1;
                let emb = vec![miss_count as f32];
                c.insert(text, emb.clone());
                embeddings.push(emb);
            }
        }

        assert_eq!(miss_count, 10, "only 10 unique texts → 10 provider calls");
        assert_eq!(embeddings.len(), 1000);
        assert_eq!(c.misses(), 10);
        assert_eq!(c.hits(), 990);
    }
}
