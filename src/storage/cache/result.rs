//! Query Result Cache
//!
//! Caches query results for expensive cross-modal queries.
//! Supports TTL-based expiration and dependency-based invalidation.
//!
//! # Features
//!
//! - **Result Caching**: Cache expensive query results
//! - **TTL Expiration**: Automatic expiry based on time
//! - **Dependency Tracking**: Invalidate when underlying data changes
//! - **Size Management**: LRU eviction based on memory usage
//! - **Statistics**: Cache hit/miss tracking
//!
//! # Example
//!
//! ```ignore
//! use storage::cache::result::{ResultCache, CacheKey, CachePolicy};
//!
//! let mut cache = ResultCache::new(100_000_000); // 100MB max
//!
//! let key = CacheKey::new("attack_paths")
//!     .param("from", "external")
//!     .param("to", "database");
//!
//! if let Some(result) = cache.get(&key) {
//!     return result;
//! }
//!
//! let result = expensive_query();
//! cache.insert(key, result.clone(), CachePolicy::default()
//!     .ttl(Duration::from_secs(300))
//!     .depends_on(&["hosts", "vulnerabilities"]));
//! ```

use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

// ============================================================================
// Cache Key
// ============================================================================

/// Cache key for query results
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheKey {
    /// Query type identifier
    pub query_type: String,
    /// Query parameters (sorted for consistent hashing)
    pub params: Vec<(String, String)>,
    /// Hash for fast comparison
    hash: u64,
}

impl CacheKey {
    /// Create a new cache key
    pub fn new(query_type: impl Into<String>) -> Self {
        let query_type = query_type.into();
        let mut key = Self {
            query_type,
            params: Vec::new(),
            hash: 0,
        };
        key.rehash();
        key
    }

    /// Add a parameter to the key
    pub fn param(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.params.push((name.into(), value.into()));
        self.params.sort_by(|a, b| a.0.cmp(&b.0));
        self.rehash();
        self
    }

    /// Add multiple parameters
    pub fn params(mut self, params: impl IntoIterator<Item = (String, String)>) -> Self {
        self.params.extend(params);
        self.params.sort_by(|a, b| a.0.cmp(&b.0));
        self.rehash();
        self
    }

    fn rehash(&mut self) {
        use std::collections::hash_map::DefaultHasher;
        let mut hasher = DefaultHasher::new();
        self.query_type.hash(&mut hasher);
        for (k, v) in &self.params {
            k.hash(&mut hasher);
            v.hash(&mut hasher);
        }
        self.hash = hasher.finish();
    }
}

impl Hash for CacheKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write_u64(self.hash);
    }
}

// ============================================================================
// Cache Policy
// ============================================================================

/// Policy for cache entry behavior
#[derive(Debug, Clone)]
pub struct CachePolicy {
    /// Time to live
    pub ttl: Duration,
    /// Tables/collections this result depends on
    pub dependencies: HashSet<String>,
    /// Priority for eviction (higher = keep longer)
    pub priority: u8,
    /// Whether to refresh on access (sliding expiration)
    pub sliding: bool,
}

impl Default for CachePolicy {
    fn default() -> Self {
        Self {
            ttl: Duration::from_secs(300), // 5 minutes
            dependencies: HashSet::new(),
            priority: 50,
            sliding: false,
        }
    }
}

impl CachePolicy {
    /// Set TTL
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Add dependencies
    pub fn depends_on(mut self, deps: &[&str]) -> Self {
        for dep in deps {
            self.dependencies.insert(dep.to_string());
        }
        self
    }

    /// Set priority
    pub fn priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    /// Enable sliding expiration
    pub fn sliding(mut self) -> Self {
        self.sliding = true;
        self
    }
}

// ============================================================================
// Cache Entry
// ============================================================================

/// Cached query result
struct CacheEntry {
    /// Serialized result data
    data: Vec<u8>,
    /// Estimated size in bytes
    size: usize,
    /// When this entry was created
    created_at: Instant,
    /// Last access time
    last_accessed: Instant,
    /// Access count
    access_count: AtomicU64,
    /// Cache policy
    policy: CachePolicy,
}

impl CacheEntry {
    fn new(data: Vec<u8>, policy: CachePolicy) -> Self {
        let size = data.len();
        let now = Instant::now();
        Self {
            data,
            size,
            created_at: now,
            last_accessed: now,
            access_count: AtomicU64::new(0),
            policy,
        }
    }

    fn is_expired(&self) -> bool {
        let elapsed = if self.policy.sliding {
            self.last_accessed.elapsed()
        } else {
            self.created_at.elapsed()
        };
        elapsed > self.policy.ttl
    }

    fn touch(&mut self) {
        self.access_count.fetch_add(1, Ordering::Relaxed);
        self.last_accessed = Instant::now();
    }

    /// Calculate eviction score (lower = more likely to evict)
    fn eviction_score(&self) -> u64 {
        let frequency = self.access_count.load(Ordering::Relaxed);
        let recency = self.last_accessed.elapsed().as_secs();
        let priority = self.policy.priority as u64;

        // Score: frequency * priority / recency
        // Higher = keep longer
        if recency == 0 {
            frequency * priority * 1000
        } else {
            frequency * priority / recency
        }
    }
}

// ============================================================================
// Cache Statistics
// ============================================================================

/// Cache statistics
#[derive(Debug, Clone, Default)]
pub struct ResultCacheStats {
    /// Cache hits
    pub hits: u64,
    /// Cache misses
    pub misses: u64,
    /// Entries evicted
    pub evictions: u64,
    /// Current entry count
    pub entry_count: usize,
    /// Current memory usage in bytes
    pub memory_bytes: usize,
    /// Maximum memory limit
    pub max_memory_bytes: usize,
    /// Entries expired
    pub expirations: u64,
    /// Invalidations by dependency
    pub invalidations: u64,
}

impl ResultCacheStats {
    /// Calculate hit rate
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }

    /// Calculate memory utilization
    pub fn memory_utilization(&self) -> f64 {
        if self.max_memory_bytes == 0 {
            0.0
        } else {
            self.memory_bytes as f64 / self.max_memory_bytes as f64
        }
    }
}

// ============================================================================
// Result Cache
// ============================================================================

/// LRU cache for query results with memory management
pub struct ResultCache {
    /// Cached entries
    entries: HashMap<CacheKey, CacheEntry>,
    /// Dependency index: table -> keys depending on it
    dependency_index: HashMap<String, HashSet<CacheKey>>,
    /// Maximum memory in bytes
    max_memory: usize,
    /// Current memory usage
    current_memory: usize,
    /// Statistics
    stats: ResultCacheStats,
}

impl ResultCache {
    /// Create a new result cache with max memory limit
    pub fn new(max_memory_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            dependency_index: HashMap::new(),
            max_memory: max_memory_bytes,
            current_memory: 0,
            stats: ResultCacheStats {
                max_memory_bytes: max_memory_bytes,
                ..Default::default()
            },
        }
    }

    /// Get a cached result
    pub fn get(&mut self, key: &CacheKey) -> Option<Vec<u8>> {
        // Remove expired entry if present
        if let Some(entry) = self.entries.get(key) {
            if entry.is_expired() {
                self.remove(key);
                self.stats.expirations += 1;
                self.stats.misses += 1;
                return None;
            }
        }

        if let Some(entry) = self.entries.get_mut(key) {
            entry.touch();
            self.stats.hits += 1;
            Some(entry.data.clone())
        } else {
            self.stats.misses += 1;
            None
        }
    }

    /// Check if a key exists (without touching)
    pub fn contains(&self, key: &CacheKey) -> bool {
        if let Some(entry) = self.entries.get(key) {
            !entry.is_expired()
        } else {
            false
        }
    }

    /// Insert a result into the cache
    pub fn insert(&mut self, key: CacheKey, data: Vec<u8>, policy: CachePolicy) {
        let entry_size = data.len() + std::mem::size_of::<CacheEntry>();

        // Remove existing entry if present
        if self.entries.contains_key(&key) {
            self.remove(&key);
        }

        // Evict until we have space
        while self.current_memory + entry_size > self.max_memory && !self.entries.is_empty() {
            self.evict_one();
        }

        // Index dependencies
        for dep in &policy.dependencies {
            self.dependency_index
                .entry(dep.clone())
                .or_insert_with(HashSet::new)
                .insert(key.clone());
        }

        let entry = CacheEntry::new(data, policy);
        self.current_memory += entry.size;
        self.entries.insert(key, entry);
        self.stats.entry_count = self.entries.len();
        self.stats.memory_bytes = self.current_memory;
    }

    /// Remove an entry
    pub fn remove(&mut self, key: &CacheKey) -> Option<Vec<u8>> {
        if let Some(entry) = self.entries.remove(key) {
            self.current_memory = self.current_memory.saturating_sub(entry.size);

            // Remove from dependency index
            for dep in &entry.policy.dependencies {
                if let Some(keys) = self.dependency_index.get_mut(dep) {
                    keys.remove(key);
                }
            }

            self.stats.entry_count = self.entries.len();
            self.stats.memory_bytes = self.current_memory;
            Some(entry.data)
        } else {
            None
        }
    }

    /// Invalidate all entries depending on a table/collection
    pub fn invalidate_by_dependency(&mut self, dependency: &str) {
        if let Some(keys) = self.dependency_index.remove(dependency) {
            for key in keys {
                if self.entries.remove(&key).is_some() {
                    self.stats.invalidations += 1;
                }
            }
            self.stats.entry_count = self.entries.len();
            // Recalculate memory
            self.current_memory = self.entries.values().map(|e| e.size).sum();
            self.stats.memory_bytes = self.current_memory;
        }
    }

    /// Invalidate entries matching a predicate
    pub fn invalidate_where<F>(&mut self, predicate: F)
    where
        F: Fn(&CacheKey) -> bool,
    {
        let keys_to_remove: Vec<CacheKey> = self
            .entries
            .keys()
            .filter(|k| predicate(k))
            .cloned()
            .collect();

        for key in keys_to_remove {
            self.remove(&key);
            self.stats.invalidations += 1;
        }
    }

    /// Prune all expired entries
    pub fn prune_expired(&mut self) {
        let expired: Vec<CacheKey> = self
            .entries
            .iter()
            .filter(|(_, v)| v.is_expired())
            .map(|(k, _)| k.clone())
            .collect();

        for key in expired {
            self.remove(&key);
            self.stats.expirations += 1;
        }
    }

    /// Clear all entries
    pub fn clear(&mut self) {
        self.entries.clear();
        self.dependency_index.clear();
        self.current_memory = 0;
        self.stats.entry_count = 0;
        self.stats.memory_bytes = 0;
    }

    /// Get cache statistics
    pub fn stats(&self) -> &ResultCacheStats {
        &self.stats
    }

    /// Evict one entry (lowest eviction score)
    fn evict_one(&mut self) {
        let victim = self
            .entries
            .iter()
            .min_by_key(|(_, v)| v.eviction_score())
            .map(|(k, _)| k.clone());

        if let Some(key) = victim {
            self.remove(&key);
            self.stats.evictions += 1;
        }
    }
}

// ============================================================================
// Materialized View Cache
// ============================================================================

/// Definition of a materialized view
#[derive(Debug, Clone)]
pub struct MaterializedViewDef {
    /// View name
    pub name: String,
    /// Query that populates the view
    pub query: String,
    /// Tables this view depends on
    pub dependencies: Vec<String>,
    /// Refresh policy
    pub refresh: RefreshPolicy,
}

/// How to refresh a materialized view
#[derive(Debug, Clone)]
pub enum RefreshPolicy {
    /// Refresh on demand only
    Manual,
    /// Refresh when dependencies change
    OnChange,
    /// Refresh periodically
    Periodic(Duration),
    /// Refresh after N invalidations
    AfterWrites(usize),
}

/// Materialized view cache entry
struct MaterializedView {
    /// The cached result
    data: Vec<u8>,
    /// View definition
    def: MaterializedViewDef,
    /// When last refreshed
    last_refresh: Instant,
    /// Write count since last refresh
    writes_since_refresh: usize,
    /// Whether the view is stale
    stale: bool,
}

/// Cache for materialized views
pub struct MaterializedViewCache {
    /// Views by name
    views: HashMap<String, MaterializedView>,
    /// Dependency index: table -> view names
    dependency_index: HashMap<String, HashSet<String>>,
}

impl MaterializedViewCache {
    /// Create a new materialized view cache
    pub fn new() -> Self {
        Self {
            views: HashMap::new(),
            dependency_index: HashMap::new(),
        }
    }

    /// Register a view definition
    pub fn register(&mut self, def: MaterializedViewDef) {
        // Index dependencies
        for dep in &def.dependencies {
            self.dependency_index
                .entry(dep.clone())
                .or_insert_with(HashSet::new)
                .insert(def.name.clone());
        }

        let view = MaterializedView {
            data: Vec::new(),
            def,
            last_refresh: Instant::now(),
            writes_since_refresh: 0,
            stale: true,
        };

        self.views.insert(view.def.name.clone(), view);
    }

    /// Get view data (if not stale)
    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.views
            .get(name)
            .filter(|v| !v.stale && !v.data.is_empty())
            .map(|v| v.data.as_slice())
    }

    /// Check if view needs refresh
    pub fn needs_refresh(&self, name: &str) -> bool {
        self.views.get(name).map(|v| v.stale).unwrap_or(false)
    }

    /// Refresh a view with new data
    pub fn refresh(&mut self, name: &str, data: Vec<u8>) {
        if let Some(view) = self.views.get_mut(name) {
            view.data = data;
            view.last_refresh = Instant::now();
            view.writes_since_refresh = 0;
            view.stale = false;
        }
    }

    /// Mark views depending on a table as stale
    pub fn mark_stale(&mut self, table: &str) {
        if let Some(view_names) = self.dependency_index.get(table) {
            for name in view_names.clone() {
                if let Some(view) = self.views.get_mut(&name) {
                    view.writes_since_refresh += 1;

                    match &view.def.refresh {
                        RefreshPolicy::OnChange => {
                            view.stale = true;
                        }
                        RefreshPolicy::AfterWrites(threshold) => {
                            if view.writes_since_refresh >= *threshold {
                                view.stale = true;
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    /// Get views needing periodic refresh
    pub fn due_for_refresh(&self) -> Vec<String> {
        self.views
            .values()
            .filter(|v| {
                if let RefreshPolicy::Periodic(interval) = &v.def.refresh {
                    v.last_refresh.elapsed() >= *interval
                } else {
                    false
                }
            })
            .map(|v| v.def.name.clone())
            .collect()
    }

    /// Remove a view
    pub fn remove(&mut self, name: &str) {
        if let Some(view) = self.views.remove(name) {
            for dep in &view.def.dependencies {
                if let Some(names) = self.dependency_index.get_mut(dep) {
                    names.remove(name);
                }
            }
        }
    }

    /// List all view names
    pub fn list(&self) -> Vec<&str> {
        self.views.keys().map(|s| s.as_str()).collect()
    }
}

impl Default for MaterializedViewCache {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_key_hashing() {
        let key1 = CacheKey::new("attack_paths")
            .param("from", "host1")
            .param("to", "host2");

        let key2 = CacheKey::new("attack_paths")
            .param("to", "host2")
            .param("from", "host1"); // Different order

        assert_eq!(key1, key2);
        assert_eq!(key1.hash, key2.hash);
    }

    #[test]
    fn test_result_cache_basic() {
        let mut cache = ResultCache::new(1024 * 1024); // 1MB

        let key = CacheKey::new("test_query").param("id", "123");
        let data = vec![1, 2, 3, 4, 5];

        cache.insert(key.clone(), data.clone(), CachePolicy::default());

        let result = cache.get(&key);
        assert_eq!(result, Some(data));
        assert_eq!(cache.stats().hits, 1);
    }

    #[test]
    fn test_cache_expiration() {
        let mut cache = ResultCache::new(1024 * 1024);

        let key = CacheKey::new("test");
        let data = vec![1, 2, 3];

        // Very short TTL
        cache.insert(
            key.clone(),
            data,
            CachePolicy::default().ttl(Duration::from_millis(1)),
        );

        // Wait for expiration
        std::thread::sleep(Duration::from_millis(10));

        assert!(cache.get(&key).is_none());
        assert_eq!(cache.stats().expirations, 1);
    }

    #[test]
    fn test_dependency_invalidation() {
        let mut cache = ResultCache::new(1024 * 1024);

        let key = CacheKey::new("host_query");
        cache.insert(
            key.clone(),
            vec![1, 2, 3],
            CachePolicy::default().depends_on(&["hosts"]),
        );

        assert!(cache.contains(&key));

        // Invalidate hosts table
        cache.invalidate_by_dependency("hosts");

        assert!(!cache.contains(&key));
        assert_eq!(cache.stats().invalidations, 1);
    }

    #[test]
    fn test_memory_eviction() {
        let mut cache = ResultCache::new(100); // Very small

        // Insert enough to trigger eviction
        for i in 0..10 {
            let key = CacheKey::new("query").param("i", i.to_string());
            cache.insert(key, vec![0u8; 20], CachePolicy::default());
        }

        // Should have evicted some entries
        assert!(cache.stats().evictions > 0);
        assert!(cache.stats().memory_bytes <= 100);
    }

    #[test]
    fn test_materialized_view() {
        let mut cache = MaterializedViewCache::new();

        cache.register(MaterializedViewDef {
            name: "active_hosts".to_string(),
            query: "SELECT * FROM hosts WHERE status = 'active'".to_string(),
            dependencies: vec!["hosts".to_string()],
            refresh: RefreshPolicy::OnChange,
        });

        // Initially stale
        assert!(cache.needs_refresh("active_hosts"));

        // Refresh it
        cache.refresh("active_hosts", vec![1, 2, 3]);
        assert!(!cache.needs_refresh("active_hosts"));
        assert_eq!(cache.get("active_hosts"), Some(&[1, 2, 3][..]));

        // Mark stale due to dependency
        cache.mark_stale("hosts");
        assert!(cache.needs_refresh("active_hosts"));
    }
}
