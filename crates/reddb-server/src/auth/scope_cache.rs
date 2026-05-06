//! `(tenant, role) → HashSet<CollectionId>` visibility cache.
//!
//! Computed once per `(tenant, role)` tuple and reused for the 60-second
//! TTL window. Invalidated explicitly on:
//!
//!   * GRANT / REVOKE
//!   * CREATE POLICY / DROP POLICY (and policy attach/detach)
//!   * DROP COLLECTION
//!
//! Why a separate cache from `PermissionCache`: `PermissionCache` answers
//! "does (resource, action) match for this user?" and is keyed by
//! `UserId`. The AI pipeline needs the inverse — "what collections is
//! this caller allowed to see?" — keyed by `(tenant, role)` so two
//! users that share a tenant + role share the cache slot. A 60s TTL is
//! tight enough that policy churn becomes visible within one minute even
//! if an explicit invalidation was missed; the explicit invalidations
//! still fire on every relevant mutation so the common case is zero
//! staleness.
//!
//! The cache exposes hit/miss counters so the `AuthCache::stats()`
//! probe required by issue #119 can be wired into the runtime metrics
//! plane.

use std::collections::{HashMap, HashSet};
use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use super::Role;

/// Default TTL for a `visible_collections` cache entry.
pub const DEFAULT_TTL: Duration = Duration::from_secs(60);

/// Cache key — `(tenant, role)`. `None` tenant = platform tenant.
#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct ScopeKey {
    pub tenant: Option<String>,
    pub role: Role,
}

impl ScopeKey {
    pub fn new(tenant: Option<&str>, role: Role) -> Self {
        Self {
            tenant: tenant.map(|s| s.to_string()),
            role,
        }
    }
}

/// One entry in the cache: the resolved visible-collections set plus
/// its insertion timestamp so reads can enforce the TTL.
#[derive(Debug, Clone)]
struct ScopeEntry {
    collections: HashSet<String>,
    inserted_at: Instant,
}

/// Hit/miss/invalidate counters surfaced by `AuthCache::stats()`.
#[derive(Debug, Default, Clone, Copy)]
pub struct AuthCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub invalidations: u64,
}

impl AuthCacheStats {
    /// Hit-rate as a fraction of total lookups; `0.0` when there have
    /// been no lookups yet so callers can safely format the value.
    pub fn hit_rate(&self) -> f64 {
        let total = self.hits + self.misses;
        if total == 0 {
            0.0
        } else {
            self.hits as f64 / total as f64
        }
    }
}

/// Visible-collections cache. Thread-safe; cheaply cloneable through
/// an enclosing `Arc`. Construction uses `Default::default()` so the
/// cache can sit on `AuthStore` without extra plumbing.
#[derive(Debug, Default)]
pub struct AuthCache {
    entries: RwLock<HashMap<ScopeKey, ScopeEntry>>,
    ttl: Duration,
    hits: AtomicU64,
    misses: AtomicU64,
    invalidations: AtomicU64,
}

impl AuthCache {
    pub fn new(ttl: Duration) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            ttl,
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
            invalidations: AtomicU64::new(0),
        }
    }

    /// Look up the cached visible-collections set for `(tenant, role)`.
    /// Returns `None` on miss or when the entry has expired (the
    /// expired entry stays in place — the next `insert` overwrites it).
    pub fn get(&self, key: &ScopeKey) -> Option<HashSet<String>> {
        let guard = self.entries.read().ok()?;
        let entry = guard.get(key)?;
        if entry.inserted_at.elapsed() >= self.ttl {
            // TTL'd out — count as miss so the runtime rebuilds.
            self.misses.fetch_add(1, Ordering::Relaxed);
            tracing::trace!(
                target: "auth_cache",
                tenant = ?key.tenant,
                role = ?key.role,
                "scope_cache miss (TTL expired)"
            );
            return None;
        }
        self.hits.fetch_add(1, Ordering::Relaxed);
        tracing::trace!(
            target: "auth_cache",
            tenant = ?key.tenant,
            role = ?key.role,
            "scope_cache hit"
        );
        Some(entry.collections.clone())
    }

    /// Bookkeeping helper called by the runtime when it has to rebuild
    /// because `get` returned `None`. Counts the miss and inserts the
    /// freshly-computed set so the next caller hits the cache.
    pub fn insert(&self, key: ScopeKey, collections: HashSet<String>) {
        self.misses.fetch_add(1, Ordering::Relaxed);
        tracing::trace!(
            target: "auth_cache",
            tenant = ?key.tenant,
            role = ?key.role,
            n = collections.len(),
            "scope_cache miss → insert"
        );
        if let Ok(mut guard) = self.entries.write() {
            guard.insert(
                key,
                ScopeEntry {
                    collections,
                    inserted_at: Instant::now(),
                },
            );
        }
    }

    /// Invalidate every entry — used after global IAM events
    /// (CREATE/DROP POLICY, DROP COLLECTION). Increments the
    /// `invalidations` counter once regardless of map size.
    pub fn invalidate_all(&self) {
        if let Ok(mut guard) = self.entries.write() {
            guard.clear();
        }
        self.invalidations.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(target: "auth_cache", "scope_cache invalidate_all");
    }

    /// Invalidate every entry whose tenant matches `tenant`. Used by
    /// GRANT/REVOKE which know the affected tenant from the principal.
    pub fn invalidate_tenant(&self, tenant: Option<&str>) {
        if let Ok(mut guard) = self.entries.write() {
            guard.retain(|k, _| k.tenant.as_deref() != tenant);
        }
        self.invalidations.fetch_add(1, Ordering::Relaxed);
        tracing::debug!(target: "auth_cache", tenant = ?tenant, "scope_cache invalidate_tenant");
    }

    pub fn stats(&self) -> AuthCacheStats {
        AuthCacheStats {
            hits: self.hits.load(Ordering::Relaxed),
            misses: self.misses.load(Ordering::Relaxed),
            invalidations: self.invalidations.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    fn key(tenant: &str, role: Role) -> ScopeKey {
        ScopeKey::new(Some(tenant), role)
    }

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn miss_then_hit() {
        let cache = AuthCache::new(DEFAULT_TTL);
        let k = key("acme", Role::Read);
        assert!(cache.get(&k).is_none(), "first lookup is a miss");
        cache.insert(k.clone(), set(&["orders", "customers"]));
        let hit = cache.get(&k).expect("post-insert hit");
        assert_eq!(hit, set(&["orders", "customers"]));
        let stats = cache.stats();
        // `insert` counts a miss (the rebuild that motivated it), then
        // `get` returns a hit on the freshly-cached entry.
        assert_eq!(stats.hits, 1);
        assert!(stats.misses >= 1);
    }

    #[test]
    fn ttl_evicts() {
        let cache = AuthCache::new(Duration::from_millis(20));
        let k = key("acme", Role::Read);
        cache.insert(k.clone(), set(&["x"]));
        sleep(Duration::from_millis(40));
        assert!(
            cache.get(&k).is_none(),
            "TTL'd entry must be treated as a miss"
        );
    }

    #[test]
    fn invalidate_tenant_drops_only_matching() {
        let cache = AuthCache::new(DEFAULT_TTL);
        cache.insert(key("acme", Role::Read), set(&["a"]));
        cache.insert(key("globex", Role::Read), set(&["b"]));
        cache.invalidate_tenant(Some("acme"));
        assert!(cache.get(&key("acme", Role::Read)).is_none());
        assert!(cache.get(&key("globex", Role::Read)).is_some());
        assert_eq!(cache.stats().invalidations, 1);
    }

    #[test]
    fn invalidate_all_drops_every_entry() {
        let cache = AuthCache::new(DEFAULT_TTL);
        cache.insert(key("acme", Role::Read), set(&["a"]));
        cache.insert(key("globex", Role::Write), set(&["b"]));
        cache.invalidate_all();
        assert!(cache.get(&key("acme", Role::Read)).is_none());
        assert!(cache.get(&key("globex", Role::Write)).is_none());
    }

    #[test]
    fn hit_rate_handles_zero_lookups() {
        let cache = AuthCache::new(DEFAULT_TTL);
        assert_eq!(cache.stats().hit_rate(), 0.0);
    }
}
