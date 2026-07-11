//! Query Plan Cache
//!
//! LRU cache for compiled query plans with TTL validation.
//!
//! # Features
//!
//! - LRU eviction policy
//! - TTL-based invalidation
//! - Thread-safe access
//! - Statistics tracking

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::{CacheStats, QueryPlan};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheEntryState {
    Inactive,
    Active,
}

/// A cached query plan with metadata
#[derive(Debug, Clone)]
pub struct CachedPlan {
    /// The compiled query plan
    pub plan: QueryPlan,
    /// When this plan was cached
    pub cached_at: Instant,
    /// Number of times this plan was accessed
    pub access_count: u64,
    /// Last access time
    pub last_accessed: Instant,
    /// Query shape key used for parameter-insensitive cache grouping.
    pub shape_key: Option<String>,
    /// Last exact query string stored in this slot.
    pub exact_query: Option<std::sync::Arc<str>>,
    /// Runtime activation state inspired by Mongo's active/inactive plan cache.
    pub state: CacheEntryState,
    /// Moving expectation for storage reads (`rows_scanned`) on this shape.
    pub expected_rows_scanned: Option<u64>,
    /// Last observed runtime reads for the shape.
    pub last_observed_rows_scanned: Option<u64>,
    /// Number of literal binds expected by the cached shape skeleton.
    pub parameter_count: usize,
    /// When true, the next cache lookup forces a fresh replan.
    pub replan_pending: bool,
    /// Monotonic recency stamp assigned by the owning [`PlanCache`] on every
    /// insert and hit. The least-recently-used entry is the one with the
    /// smallest stamp; this replaces the old `Vec<String>` scan so promotion is
    /// O(1) and allocation-free on the cache-hit hot path.
    lru_seq: u64,
}

impl CachedPlan {
    /// Create a new cached plan
    pub fn new(plan: QueryPlan) -> Self {
        let now = Instant::now();
        Self {
            plan,
            cached_at: now,
            access_count: 0,
            last_accessed: now,
            shape_key: None,
            exact_query: None,
            state: CacheEntryState::Inactive,
            expected_rows_scanned: None,
            last_observed_rows_scanned: None,
            parameter_count: 0,
            replan_pending: false,
            lru_seq: 0,
        }
    }

    pub fn with_shape_key(mut self, shape_key: impl Into<String>) -> Self {
        self.shape_key = Some(shape_key.into());
        self
    }

    pub fn with_exact_query(mut self, query: impl Into<String>) -> Self {
        let s: String = query.into();
        self.exact_query = Some(std::sync::Arc::<str>::from(s));
        self
    }

    pub fn with_parameter_count(mut self, parameter_count: usize) -> Self {
        self.parameter_count = parameter_count;
        self
    }

    /// Check if the plan has expired
    pub fn is_expired(&self, ttl: Duration) -> bool {
        self.cached_at.elapsed() > ttl
    }

    /// Record an access
    pub fn touch(&mut self) {
        self.access_count += 1;
        self.last_accessed = Instant::now();
    }

    pub fn matches_exact_query(&self, query: &str) -> bool {
        self.exact_query.as_deref() == Some(query)
    }

    pub fn needs_replan(&self) -> bool {
        self.replan_pending
    }

    pub fn record_observation(&mut self, rows_scanned: u64) {
        self.last_observed_rows_scanned = Some(rows_scanned);
        match (self.state, self.expected_rows_scanned) {
            (_, None) => {
                self.expected_rows_scanned = Some(rows_scanned.max(1));
                self.replan_pending = false;
            }
            (CacheEntryState::Inactive, Some(expected)) => {
                if rows_scanned <= expected {
                    self.state = CacheEntryState::Active;
                    self.expected_rows_scanned = Some(rows_scanned.max(1));
                    self.replan_pending = false;
                } else {
                    self.expected_rows_scanned = Some(rows_scanned.min(expected.saturating_mul(2)));
                }
            }
            (CacheEntryState::Active, Some(expected)) => {
                if rows_scanned > expected.saturating_mul(10).max(10) {
                    self.state = CacheEntryState::Inactive;
                    self.expected_rows_scanned = Some(rows_scanned.max(1));
                    self.replan_pending = true;
                } else if rows_scanned < expected {
                    self.expected_rows_scanned = Some(rows_scanned.max(1));
                    self.replan_pending = false;
                }
            }
        }
    }
}

/// LRU cache for query plans
///
/// Recency is tracked by a monotonic `clock` counter stamped onto each entry's
/// `lru_seq` on insert and hit, rather than a `Vec<String>` that had to be
/// linearly scanned (`position()` + `remove()`) on every promotion and removal.
/// Promotion (the cache-hit hot path) is now O(1) and allocation-free; eviction
/// picks the entry with the smallest stamp, which is the exact same victim the
/// old front-of-`Vec` policy would have chosen for any given access sequence.
pub struct PlanCache {
    /// Cached plans by key
    entries: HashMap<String, CachedPlan>,
    /// Monotonic recency clock; larger means more recently used.
    clock: u64,
    /// Maximum cache size
    capacity: usize,
    /// Time-to-live for entries
    ttl: Duration,
    /// Cache statistics
    hits: u64,
    misses: u64,
}

impl PlanCache {
    /// Create a new plan cache with the given capacity
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity),
            clock: 0,
            capacity,
            ttl: Duration::from_secs(3600), // 1 hour default TTL
            hits: 0,
            misses: 0,
        }
    }

    /// Advance the recency clock and return the fresh stamp.
    fn next_seq(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    /// Set the TTL for cache entries
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Read-only cache probe — no LRU promotion, no mutation.
    ///
    /// Use this with a `read()` lock in the hot query path so concurrent
    /// readers don't serialize on a write lock. Falls back to `None` when
    /// the entry is expired or has a pending replan; the caller must then
    /// re-acquire a `write()` lock and call `get()` to handle eviction and
    /// miss insertion.
    pub fn peek(&self, key: &str) -> Option<&CachedPlan> {
        let entry = self.entries.get(key)?;
        if entry.needs_replan() || entry.is_expired(self.ttl) {
            return None;
        }
        Some(entry)
    }

    /// Get a cached plan by key
    pub fn get(&mut self, key: &str) -> Option<&CachedPlan> {
        if self
            .entries
            .get(key)
            .is_some_and(|entry| entry.needs_replan())
        {
            self.remove(key);
            self.misses += 1;
            return None;
        }

        // Check if entry exists and is not expired
        let expired = match self.entries.get(key) {
            Some(entry) => entry.is_expired(self.ttl),
            None => {
                self.misses += 1;
                return None;
            }
        };
        if expired {
            // Remove expired entry
            self.remove(key);
            self.misses += 1;
            return None;
        }

        // Hit: stamp the entry as most-recently-used. O(1), no allocation.
        let seq = self.next_seq();
        if let Some(entry) = self.entries.get_mut(key) {
            entry.touch();
            entry.lru_seq = seq;
        }
        self.hits += 1;
        self.entries.get(key)
    }

    /// Insert a plan into the cache
    pub fn insert(&mut self, key: String, mut plan: CachedPlan) {
        // Remove existing entry if present
        if self.entries.contains_key(&key) {
            self.remove(&key);
        }

        // Evict if at capacity
        while self.entries.len() >= self.capacity && !self.entries.is_empty() {
            self.evict_lru();
        }

        // Insert new entry, stamped as most-recently-used.
        plan.lru_seq = self.next_seq();
        self.entries.insert(key, plan);
    }

    /// Remove an entry from the cache
    pub fn remove(&mut self, key: &str) -> Option<CachedPlan> {
        self.entries.remove(key)
    }

    /// Invalidate entries matching a predicate
    pub fn invalidate<F>(&mut self, predicate: F)
    where
        F: Fn(&str) -> bool,
    {
        let keys_to_remove: Vec<String> = self
            .entries
            .keys()
            .filter(|k| predicate(k))
            .cloned()
            .collect();

        for key in keys_to_remove {
            self.remove(&key);
        }
    }

    /// Clear all entries
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Get cache statistics
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            hits: self.hits,
            misses: self.misses,
            size: self.entries.len(),
            capacity: self.capacity,
        }
    }

    /// Evict the least recently used entry (smallest recency stamp).
    fn evict_lru(&mut self) {
        let victim = self
            .entries
            .iter()
            .min_by_key(|(_, entry)| entry.lru_seq)
            .map(|(key, _)| key.clone());
        if let Some(key) = victim {
            self.remove(&key);
        }
    }

    /// Prune expired entries
    pub fn prune_expired(&mut self) {
        let expired: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, v)| v.is_expired(self.ttl))
            .map(|(k, _)| k.clone())
            .collect();

        for key in expired {
            self.remove(&key);
        }
    }

    pub fn record_observation(&mut self, key: &str, rows_scanned: u64) {
        if let Some(entry) = self.entries.get_mut(key) {
            entry.record_observation(rows_scanned);
        }
    }
}

impl Default for PlanCache {
    fn default() -> Self {
        Self::new(1000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::query::ast::{Projection, QueryExpr, TableQuery};
    use crate::storage::query::planner::cost::PlanCost;

    fn make_test_plan() -> QueryPlan {
        QueryPlan::new(
            QueryExpr::Table(TableQuery {
                table: "test".to_string(),
                source: None,
                alias: None,
                select_items: Vec::new(),
                columns: vec![Projection::All],
                where_expr: None,
                filter: None,
                group_by_exprs: Vec::new(),
                group_by: Vec::new(),
                having_expr: None,
                having: None,
                order_by: vec![],
                limit: None,
                limit_param: None,
                offset: None,
                offset_param: None,
                expand: None,
                as_of: None,
                sessionize: None,
                distinct: false,
            }),
            QueryExpr::Table(TableQuery {
                table: "test".to_string(),
                source: None,
                alias: None,
                select_items: Vec::new(),
                columns: vec![Projection::All],
                where_expr: None,
                filter: None,
                group_by_exprs: Vec::new(),
                group_by: Vec::new(),
                having_expr: None,
                having: None,
                order_by: vec![],
                limit: None,
                limit_param: None,
                offset: None,
                offset_param: None,
                expand: None,
                as_of: None,
                sessionize: None,
                distinct: false,
            }),
            PlanCost::default(),
        )
    }

    #[test]
    fn test_cache_insert_and_get() {
        let mut cache = PlanCache::new(10);
        let plan = CachedPlan::new(make_test_plan());

        cache.insert("query1".to_string(), plan);
        assert!(cache.get("query1").is_some());
        assert!(cache.get("query2").is_none());
    }

    #[test]
    fn test_cache_lru_eviction() {
        let mut cache = PlanCache::new(2);

        cache.insert("q1".to_string(), CachedPlan::new(make_test_plan()));
        cache.insert("q2".to_string(), CachedPlan::new(make_test_plan()));

        // Access q1 to make it most recently used
        let _ = cache.get("q1");

        // Insert q3 - should evict q2 (LRU)
        cache.insert("q3".to_string(), CachedPlan::new(make_test_plan()));

        assert!(cache.get("q1").is_some());
        assert!(cache.get("q2").is_none()); // Evicted
        assert!(cache.get("q3").is_some());
    }

    #[test]
    fn test_cache_stats() {
        let mut cache = PlanCache::new(10);
        cache.insert("q1".to_string(), CachedPlan::new(make_test_plan()));

        let _ = cache.get("q1"); // Hit
        let _ = cache.get("q2"); // Miss
        let _ = cache.get("q1"); // Hit

        let stats = cache.stats();
        assert_eq!(stats.hits, 2);
        assert_eq!(stats.misses, 1);
    }

    #[test]
    fn test_cache_invalidation() {
        let mut cache = PlanCache::new(10);
        cache.insert(
            "hosts_query1".to_string(),
            CachedPlan::new(make_test_plan()),
        );
        cache.insert(
            "hosts_query2".to_string(),
            CachedPlan::new(make_test_plan()),
        );
        cache.insert("users_query".to_string(), CachedPlan::new(make_test_plan()));

        // Invalidate all hosts queries
        cache.invalidate(|k| k.starts_with("hosts_"));

        assert!(cache.get("hosts_query1").is_none());
        assert!(cache.get("hosts_query2").is_none());
        assert!(cache.get("users_query").is_some());
    }

    #[test]
    fn lru_eviction_picks_least_recently_used_victim() {
        // Regression guard for the #2011 recency-counter LRU: eviction must pick
        // the same victim the old front-of-`Vec` policy would have, for a given
        // access sequence. `peek` is used for verification so it does not itself
        // perturb recency.
        let mut cache = PlanCache::new(3);
        for k in ["a", "b", "c"] {
            cache.insert(k.to_string(), CachedPlan::new(make_test_plan()));
        }
        // Touch a and c, leaving b as the least-recently-used entry.
        assert!(cache.get("a").is_some());
        assert!(cache.get("c").is_some());

        // Inserting a fourth entry at capacity evicts b.
        cache.insert("d".to_string(), CachedPlan::new(make_test_plan()));

        assert!(
            cache.peek("b").is_none(),
            "b was least-recently-used and must be the eviction victim"
        );
        assert!(cache.peek("a").is_some());
        assert!(cache.peek("c").is_some());
        assert!(cache.peek("d").is_some());
    }

    #[test]
    fn active_entry_forces_replan_after_large_regression() {
        let mut cache = PlanCache::new(10);
        cache.insert("q1".to_string(), CachedPlan::new(make_test_plan()));

        cache.record_observation("q1", 10);
        cache.record_observation("q1", 10);
        cache.record_observation("q1", 500);

        assert!(cache.get("q1").is_none());
    }
}
