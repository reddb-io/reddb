//! Foundation for `KIND vector.turbo` collections (PRD #668, issue #693).
//!
//! Mirrors `blockchain_kind` for the marker side of the contract:
//! `red.collection.{name}.kind = "turbo"` is the durable signal that
//! distinguishes a TurboQuant-backed vector collection from the legacy
//! `vector` kind. Routing in `impl_ddl`, `create_vector`, and the
//! vector-search executor reads `is_turbo` to branch.
//!
//! The per-collection runtime state (`TurboCollectionState`) owns the
//! in-memory `TurboQuantIndex` and a `TurboExtent` when a pager is
//! available, plus the codec dimension / metric inherited from the
//! collection contract.

use std::sync::Arc;

use parking_lot::Mutex;

use crate::storage::engine::distance::DistanceMetric;
use crate::storage::engine::turboquant::extent::TurboExtent;
use crate::storage::engine::turboquant::index::TurboQuantIndex;
use crate::storage::engine::Pager;
use crate::storage::schema::Value;
use crate::storage::unified::{EntityData, UnifiedStore};

/// Value stored under `red.collection.{name}.kind` for vector.turbo
/// collections. Must be distinct from `blockchain_kind::CHAIN_KIND_TAG`
/// and from any future kind marker.
pub const TURBO_KIND_TAG: &str = "turbo";

/// Deterministic codec seed shared by every turbo collection in this
/// process. The seed feeds the rotation matrix + codebook; a fixed seed
/// guarantees that re-encoding the same vector across restarts produces
/// the same code, which is what makes lazy re-population from the
/// persisted vector entities safe.
pub const TURBO_CODEC_SEED: u64 = 0x7155_8807_FED4_2913;

fn kind_key(collection: &str) -> String {
    format!("red.collection.{collection}.kind")
}

/// Persist the `turbo` kind marker. Called once at collection creation.
/// Idempotent against `IF NOT EXISTS`: re-stamping the same value is a
/// no-op at the config-tree layer.
pub fn mark_as_turbo(store: &UnifiedStore, collection: &str) {
    store.set_config_tree(
        &kind_key(collection),
        &crate::serde_json::Value::String(TURBO_KIND_TAG.to_string()),
    );
}

/// True if `mark_as_turbo` was ever called for this collection. Cheap
/// (one config-tree read) and safe to call on every INSERT/SEARCH —
/// the legacy `vector` path takes the `false` branch with no extra
/// cost beyond the lookup.
pub fn is_turbo(store: &UnifiedStore, collection: &str) -> bool {
    match store.get_config(&kind_key(collection)) {
        Some(Value::Text(s)) => s.as_ref() == TURBO_KIND_TAG,
        _ => false,
    }
}

/// Per-collection runtime state for a `vector.turbo` collection.
///
/// `index` is the in-memory TurboQuant index that owns the encoded
/// codes + raw vectors; `extent` is the per-collection page-backed
/// payload buffer when the store is in paged mode (None for in-memory
/// runtimes). Both are wrapped in `Mutex` because INSERTs serialize on
/// the per-collection state — the contention point lives here rather
/// than on the global store lock.
pub struct TurboCollectionState {
    pub dim: usize,
    pub metric: DistanceMetric,
    pub index: Mutex<TurboQuantIndex>,
    pub extent: Mutex<Option<TurboExtent>>,
    /// Set once the lazy rebuild from persisted vector entities has
    /// happened. Subsequent INSERT/SEARCH calls skip the scan.
    populated: std::sync::atomic::AtomicBool,
}

impl TurboCollectionState {
    pub fn new(dim: usize, metric: DistanceMetric, pager: Option<&Arc<Pager>>) -> Self {
        let index = TurboQuantIndex::new(dim, TURBO_CODEC_SEED);
        let extent = pager
            .and_then(|p| TurboExtent::new(Arc::clone(p)).ok())
            .map(Some)
            .unwrap_or(None);
        Self {
            dim,
            metric,
            index: Mutex::new(index),
            extent: Mutex::new(extent),
            populated: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Lazily populate the in-memory index from any vector entities
    /// already persisted in the collection. Called on first access
    /// after restart so a fresh `vector.turbo` runtime sees the
    /// pre-restart data without requiring WAL replay (which lands in
    /// the crash-safety slice, #673).
    pub fn ensure_populated(&self, store: &UnifiedStore, collection: &str) {
        use std::sync::atomic::Ordering;
        if self.populated.load(Ordering::Acquire) {
            return;
        }
        let mut index = self.index.lock();
        // Double-checked: another writer may have populated while we
        // were waiting for the lock.
        if self.populated.load(Ordering::Acquire) {
            return;
        }
        if let Some(manager) = store.get_collection(collection) {
            for entity in manager.query_all(|_| true) {
                if let EntityData::Vector(data) = &entity.data {
                    if data.dense.len() == self.dim {
                        index.insert(entity.id, data.dense.clone());
                    }
                }
            }
        }
        self.populated.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::unified::UnifiedStore;

    #[test]
    fn mark_and_detect_turbo_kind() {
        let store = UnifiedStore::new();
        assert!(!is_turbo(&store, "v"));
        mark_as_turbo(&store, "v");
        assert!(is_turbo(&store, "v"));
    }
}
