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

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::{Condvar, Mutex};

use crate::storage::engine::distance::DistanceMetric;
use crate::storage::engine::turboquant::extent::TurboExtent;
use crate::storage::engine::turboquant::index::TurboQuantIndex;
use crate::storage::engine::Pager;
use crate::storage::schema::Value;
use crate::storage::unified::{EntityData, UnifiedStore};
use crate::storage::EntityId;
use reddb_file::{
    read_turboquant_snapshot as read_snapshot, write_turboquant_snapshot as write_snapshot,
    TurboQuantSnapshotError as SnapshotError,
};

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
    /// Issue #673 — per-collection readiness flag. Flips to `true`
    /// only after the background rebuild completes. SEARCH against a
    /// not-yet-ready collection waits (with bounded timeout) or
    /// returns a structured NOT_READY response.
    ready: std::sync::atomic::AtomicBool,
    /// Condvar-paired wait surface for SEARCH callers that want to
    /// block (bounded) until ready instead of immediately failing.
    ready_signal: Arc<(Mutex<bool>, Condvar)>,
    /// `.tv` snapshot file path for this collection (#674). `None` when
    /// the active `StorageLayout` opts out of snapshot files
    /// (`Minimal` / embedded). When `Some`, the checkpoint cycle dumps
    /// here and boot prefers loading from here over rebuilding from
    /// the extent.
    snapshot_path: Mutex<Option<PathBuf>>,
    /// Async-barrier handle for the most-recently-spawned snapshot
    /// dump worker (#674). The next checkpoint cycle joins the
    /// previous worker before starting a new one — bounds backpressure
    /// to at most one in-flight dump per collection.
    prev_snapshot_join: Mutex<Option<std::thread::JoinHandle<()>>>,
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
            ready: std::sync::atomic::AtomicBool::new(false),
            ready_signal: Arc::new((Mutex::new(false), Condvar::new())),
            snapshot_path: Mutex::new(None),
            prev_snapshot_join: Mutex::new(None),
        }
    }

    /// Set (or clear) the `.tv` snapshot path for this collection.
    /// Called by `RedDB::turbo_state` once the resolved
    /// `TieredLayoutPaths` is known.
    pub fn set_snapshot_path(&self, path: Option<PathBuf>) {
        *self.snapshot_path.lock() = path;
    }

    /// Current `.tv` snapshot path, if any.
    pub fn snapshot_path(&self) -> Option<PathBuf> {
        self.snapshot_path.lock().clone()
    }

    /// Returns the current readiness flag. `true` means the
    /// background rebuild (or lazy populate) has completed and the
    /// in-memory index reflects every WAL-acked INSERT.
    pub fn is_ready(&self) -> bool {
        self.ready.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Mark the collection as ready. Called at the end of
    /// `ensure_populated` and from the background rebuild worker.
    /// Wakes any SEARCH callers parked on `wait_until_ready`.
    fn mark_ready(&self) {
        self.ready.store(true, std::sync::atomic::Ordering::Release);
        let (lock, cv) = &*self.ready_signal;
        let mut flag = lock.lock();
        *flag = true;
        cv.notify_all();
    }

    /// Block the caller (with a bounded timeout) until the
    /// collection becomes ready. Returns `true` if ready within the
    /// timeout, `false` if the timeout fired. A zero-or-negative
    /// timeout still does one fast-path check.
    pub fn wait_until_ready(&self, timeout: Duration) -> bool {
        if self.is_ready() {
            return true;
        }
        if timeout.is_zero() {
            return self.is_ready();
        }
        let (lock, cv) = &*self.ready_signal;
        let mut flag = lock.lock();
        if *flag {
            return true;
        }
        let _ = cv.wait_for(&mut flag, timeout);
        *flag
    }

    /// Lazily populate the in-memory index from any vector entities
    /// already persisted in the collection, then drain any WAL-replayed
    /// `VectorInsert` records captured at store-open time (issue #694).
    ///
    /// Boot-time recovery: the WAL `VectorInsert` records are the
    /// authoritative source for vectors that may not have made it into
    /// the entity manager's persisted state (e.g. a crash between WAL
    /// fsync and the next paged flush). Replaying them in WAL order
    /// under a fixed codec seed reconstructs the in-memory
    /// `TurboQuantIndex` deterministically — including the
    /// partial-block tail introduced by ADR 0024.
    ///
    /// Non-vector traffic does not block on this rebuild: the runtime
    /// only takes this path on the first turbo INSERT/SEARCH after
    /// boot. `#673` wires the per-collection `ready: bool` flag on
    /// top of this hook to keep vector traffic from observing a
    /// half-built index while the rebuild is in flight.
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
        // Snapshot-first boot (#674). When a valid `.tv` exists at
        // the layout-derived path, replay its `(id, vector)` pairs in
        // stored order. The deterministic codec seed reproduces
        // byte-identical block/lane placement, so the index ends up
        // identical to what a from-scratch entity scan would build —
        // without walking the entity manager. Snapshots are purely a
        // cache: any failure (missing, truncated, crc-bad, dim/seed
        // drift) falls through to the legacy entity-scan path so
        // boot still succeeds.
        let mut snapshot_loaded = false;
        if let Some(path) = self.snapshot_path.lock().clone() {
            if path.exists() {
                match read_snapshot(&path, self.dim as u32, TURBO_CODEC_SEED) {
                    Ok(payload) => {
                        for (raw_id, vector) in payload.vectors {
                            index.insert(EntityId::new(raw_id), vector);
                        }
                        snapshot_loaded = true;
                    }
                    Err(err) => {
                        tracing::warn!(
                            target: "reddb::turbo::snapshot",
                            collection,
                            path = %path.display(),
                            error = %err,
                            "vector.turbo snapshot unusable; falling back to extent/WAL rebuild",
                        );
                    }
                }
            }
        }

        if !snapshot_loaded {
            if let Some(manager) = store.get_collection(collection) {
                for entity in manager.query_all(|_| true) {
                    if let EntityData::Vector(data) = &entity.data {
                        if data.dense.len() == self.dim {
                            index.insert(entity.id, data.dense.clone());
                        }
                    }
                }
            }
        }
        // Drain WAL-replayed VectorInsert records (#694). Apply in WAL
        // order so the resulting block/lane placement matches the
        // pre-restart state byte-for-byte. Duplicate ids (same vector
        // also present in the entity manager) overwrite via
        // `TurboQuantIndex::insert`'s id-replace branch, which is safe
        // and idempotent because the codec seed is constant.
        if let Some(records) = store.take_replayed_turbo_inserts(collection) {
            for (raw_id, vector) in records {
                if vector.len() == self.dim {
                    index.insert(EntityId::new(raw_id), vector);
                }
            }
        }
        self.populated.store(true, Ordering::Release);
        self.mark_ready();
    }

    /// Capture the current in-memory index state and dump it to the
    /// configured `.tv` path on a worker thread (#674). The next
    /// caller (typically the next WAL checkpoint cycle) blocks on the
    /// previous worker before starting a new one — bounded
    /// backpressure of at most one dump in flight per collection. The
    /// WAL checkpoint itself never waits for the snapshot fsync.
    ///
    /// No-op when `snapshot_path` is `None` (`StorageLayout::Minimal`
    /// or embedded mode) — preserves the single-file portability
    /// story.
    pub fn dump_snapshot_async(self: &Arc<Self>, lsn: u64) {
        let Some(path) = self.snapshot_path.lock().clone() else {
            return;
        };

        // Async barrier: wait for any previous dump to finish before
        // taking a fresh snapshot. Joining here (and not inside the
        // new worker) keeps the work serialized per collection and
        // ensures the on-disk file moves monotonically forward.
        if let Some(prev) = self.prev_snapshot_join.lock().take() {
            let _ = prev.join();
        }

        // Capture state under the index lock — the encode/decode
        // path uses the same lock — then drop the lock and serialize
        // off-thread so the checkpoint completion latency is not
        // blocked on snapshot fsync (acceptance criterion #674.5).
        let dim = self.dim as u32;
        let captured: Vec<(u64, Vec<f32>)> = {
            let guard = self.index.lock();
            guard
                .iter_persisted()
                .map(|(id, v)| (id.raw(), v.to_vec()))
                .collect()
        };

        let path_for_worker = path.clone();
        let handle = std::thread::Builder::new()
            .name("turbo-snapshot-dump".to_string())
            .spawn(move || {
                if let Some(parent) = path_for_worker.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                if let Err(err) =
                    write_snapshot(&path_for_worker, dim, TURBO_CODEC_SEED, lsn, &captured)
                {
                    tracing::warn!(
                        target: "reddb::turbo::snapshot",
                        path = %path_for_worker.display(),
                        error = %err,
                        "vector.turbo snapshot dump failed; cache will be rebuilt on next checkpoint",
                    );
                }
            })
            .ok();

        *self.prev_snapshot_join.lock() = handle;
    }

    /// Join the in-flight snapshot worker, if any. Used by `RedDB::Drop`
    /// to make sure no `.tv` write outlives the runtime, and by tests
    /// that want to assert the on-disk state synchronously.
    pub fn wait_snapshot(&self) {
        if let Some(prev) = self.prev_snapshot_join.lock().take() {
            let _ = prev.join();
        }
    }

    /// Surface `SnapshotError::Io` to callers that need it. Currently
    /// unused outside of tests but kept on the public path so the
    /// snapshot module stays an implementation detail of this state.
    #[allow(dead_code)]
    pub(crate) fn snapshot_error_is_fatal(err: &SnapshotError) -> bool {
        matches!(err, SnapshotError::Io(_))
    }

    /// Background-rebuild hook (#694 / #673). Drives the same code
    /// path as `ensure_populated`; safe to call from a worker thread.
    /// On completion the per-collection readiness flag flips and any
    /// SEARCH callers parked on `wait_until_ready` are woken.
    pub fn background_rebuild(&self, store: &UnifiedStore, collection: &str) {
        self.ensure_populated(store, collection);
    }
}

/// Public hook (#673) — issued by `RedDB::turbo_state` the first
/// time a turbo collection's state is materialised. Spawns a worker
/// thread that drains the WAL replay buffer + persisted vector
/// entities and flips the readiness flag. Non-vector traffic never
/// blocks on this; vector SEARCH/INSERT against a not-yet-ready
/// collection observes the flag via `is_ready` / `wait_until_ready`.
pub fn spawn_background_rebuild(
    store: Arc<UnifiedStore>,
    collection: String,
    state: Arc<TurboCollectionState>,
) -> std::thread::JoinHandle<()> {
    // Worker holds a *weak* handle to the store so it can detect
    // shutdown when the upgrade fails. The runtime registers the
    // returned `JoinHandle` and joins it in `RedDB::Drop` before
    // releasing its own `Arc<UnifiedStore>`, which is what
    // guarantees a clean restart on the same database path.
    let store_weak = Arc::downgrade(&store);
    drop(store);
    std::thread::Builder::new()
        .name(format!("turbo-rebuild-{collection}"))
        .spawn(move || {
            let Some(store) = store_weak.upgrade() else {
                return;
            };
            state.background_rebuild(&store, &collection);
            store.set_config_tree(
                &format!("red.collection.{collection}.vector.turbo.ready"),
                &crate::serde_json::Value::Bool(true),
            );
        })
        .expect("spawn turbo background rebuild thread")
}

/// Read the persisted readiness flag from the catalog. Used by
/// admin/introspection paths that don't hold the runtime
/// `TurboCollectionState` (e.g. SHOW COLLECTION metadata, cross-
/// process tooling). The in-memory `TurboCollectionState::is_ready`
/// is the authoritative live signal; this is the persisted shadow.
pub fn ready_flag_from_catalog(store: &UnifiedStore, collection: &str) -> bool {
    let key = format!("red.collection.{collection}.vector.turbo.ready");
    match store.get_config(&key) {
        Some(Value::Boolean(b)) => b,
        _ => false,
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
