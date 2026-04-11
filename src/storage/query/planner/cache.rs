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
        }
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
}

/// LRU cache for query plans
pub struct PlanCache {
    /// Cached plans by key
    entries: HashMap<String, CachedPlan>,
    /// LRU tracking - key ordering
    lru_order: Vec<String>,
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
            lru_order: Vec::with_capacity(capacity),
            capacity,
            ttl: Duration::from_secs(3600), // 1 hour default TTL
            hits: 0,
            misses: 0,
        }
    }

    /// Set the TTL for cache entries
    pub fn with_ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Get a cached plan by key
    pub fn get(&mut self, key: &str) -> Option<&CachedPlan> {
        // Check if entry exists and is not expired
        if let Some(entry) = self.entries.get_mut(key) {
            if entry.is_expired(self.ttl) {
                // Remove expired entry
                self.remove(key);
                self.misses += 1;
                return None;
            }

            entry.touch();
            self.promote(key);
            self.hits += 1;
            return self.entries.get(key);
        }

        self.misses += 1;
        None
    }

    /// Insert a plan into the cache
    pub fn insert(&mut self, key: String, plan: CachedPlan) {
        // Remove existing entry if present
        if self.entries.contains_key(&key) {
            self.remove(&key);
        }

        // Evict if at capacity
        while self.entries.len() >= self.capacity {
            self.evict_lru();
        }

        // Insert new entry
        self.entries.insert(key.clone(), plan);
        self.lru_order.push(key);
    }

    /// Remove an entry from the cache
    pub fn remove(&mut self, key: &str) -> Option<CachedPlan> {
        if let Some(pos) = self.lru_order.iter().position(|k| k == key) {
            self.lru_order.remove(pos);
        }
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
        self.lru_order.clear();
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

    /// Promote a key to most recently used
    fn promote(&mut self, key: &str) {
        if let Some(pos) = self.lru_order.iter().position(|k| k == key) {
            let key = self.lru_order.remove(pos);
            self.lru_order.push(key);
        }
    }

    /// Evict the least recently used entry
    fn evict_lru(&mut self) {
        if let Some(key) = self.lru_order.first().cloned() {
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
                alias: None,
                columns: vec![Projection::All],
                filter: None,
                group_by: Vec::new(),
                having: None,
                order_by: vec![],
                limit: None,
                offset: None,
                expand: None,
            }),
            QueryExpr::Table(TableQuery {
                table: "test".to_string(),
                alias: None,
                columns: vec![Projection::All],
                filter: None,
                group_by: Vec::new(),
                having: None,
                order_by: vec![],
                limit: None,
                offset: None,
                expand: None,
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
}
