use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use super::super::extended_ttl::{EffectiveExpiry, ExpiryDecision, ExtendedTtlPolicy};
use super::{BlobCacheHit, BlobCachePolicy};

#[derive(Debug)]
pub(super) struct Entry {
    pub(super) bytes: Arc<[u8]>,
    pub(super) content_metadata: BTreeMap<String, String>,
    pub(super) tags: BTreeSet<String>,
    pub(super) dependencies: BTreeSet<String>,
    pub(super) size: usize,
    pub(super) visited: bool,
    pub(super) expires_at_unix_ms: Option<u64>,
    pub(super) priority: u8,
    pub(super) version: Option<u64>,
    pub(super) namespace_generation: u64,
    pub(super) slot_index: usize,
    /// Wall-clock time of the most recent access (`put` or successful
    /// `get`). Updated on hits to drive [`ExtendedTtlPolicy::idle_ttl_ms`].
    /// L1-only — never propagated to the L2 record (cache is the source of
    /// truth for access patterns).
    pub(super) last_access_unix_ms: u64,
    /// Extended TTL knobs captured from the [`BlobCachePolicy`] at insert
    /// time, including any jitter expansion that was already applied to
    /// `expires_at_unix_ms`.
    pub(super) extended: ExtendedTtlPolicy,
}

impl Entry {
    pub(super) fn new(
        bytes: Vec<u8>,
        content_metadata: BTreeMap<String, String>,
        tags: BTreeSet<String>,
        dependencies: BTreeSet<String>,
        policy: BlobCachePolicy,
        namespace_generation: u64,
        now_ms: u64,
        namespace: &str,
        key: &str,
    ) -> Self {
        let size = bytes.len();
        Self {
            bytes: Arc::<[u8]>::from(bytes),
            content_metadata,
            tags,
            dependencies,
            size,
            visited: true,
            expires_at_unix_ms: effective_expires_at_unix_ms(policy, now_ms, namespace, key),
            priority: policy.priority_value(),
            version: policy.version_value(),
            namespace_generation,
            slot_index: 0,
            last_access_unix_ms: now_ms,
            extended: policy.extended_value(),
        }
    }

    pub(super) fn hit(&self) -> BlobCacheHit {
        BlobCacheHit::new(
            Arc::clone(&self.bytes),
            self.content_metadata.clone(),
            self.version,
        )
    }

    pub(super) fn hit_stale(&self, window_remaining_ms: u64) -> BlobCacheHit {
        BlobCacheHit::new_stale(
            Arc::clone(&self.bytes),
            self.content_metadata.clone(),
            self.version,
            window_remaining_ms,
        )
    }

    pub(super) fn is_expired_at(&self, now_ms: u64) -> bool {
        self.expires_at_unix_ms
            .is_some_and(|expires_at| now_ms >= expires_at)
    }
}

/// Stable seed for [`EffectiveExpiry::jittered_ttl_ms`] derived from the
/// (namespace, key, now_ms) triple. The same triple always yields the
/// same seed so jitter is deterministic per insert.
pub(super) fn jitter_seed(namespace: &str, key: &str, now_ms: u64) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    namespace.hash(&mut hasher);
    key.hash(&mut hasher);
    now_ms.hash(&mut hasher);
    hasher.finish()
}

pub(super) fn effective_expires_at_unix_ms(
    policy: BlobCachePolicy,
    now_ms: u64,
    namespace: &str,
    key: &str,
) -> Option<u64> {
    let extended = policy.extended_value();
    // Jitter only applies to the relative `ttl_ms` knob; an absolute
    // `expires_at_unix_ms` is treated as a hard ceiling and is never
    // pushed out by jitter.
    let jittered_ttl = policy.ttl_ms_value().map(|base| {
        if extended.jitter_pct > 0 {
            EffectiveExpiry::jittered_ttl_ms(
                base,
                extended.jitter_pct,
                jitter_seed(namespace, key, now_ms),
            )
        } else {
            base
        }
    });
    match (jittered_ttl, policy.expires_at_unix_ms_value()) {
        (Some(ttl), Some(abs)) => Some(now_ms.saturating_add(ttl).min(abs)),
        (Some(ttl), None) => Some(now_ms.saturating_add(ttl)),
        (None, Some(abs)) => Some(abs),
        (None, None) => None,
    }
}
