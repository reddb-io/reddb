//! DML chain-integrity & integrity-tombstone helpers extracted from `impl_dml`.
//!
//! Behaviour-preserving move (issue #1634). Names and behaviour are unchanged
//! from `impl_dml`; the only adjustment is `pub(super)` visibility on the one
//! private method still called from the sibling `impl_dml` module
//! (`is_chain_integrity_broken`) so its INSERT path keeps calling it by bare
//! name.

use super::*;
use crate::storage::query::unified::UnifiedResult;

impl RedDBRuntime {
    /// Issue #524 — public read of the in-memory chain tip. Returns `None`
    /// when the collection is not a chain or has no rows (pre-genesis). On a
    /// cold cache the first call falls back to a one-time scan so the HTTP
    /// `GET /collections/:name/chain-tip` handler stays consistent with the
    /// INSERT path after a restart.
    pub fn chain_tip_for_collection(
        &self,
        collection: &str,
    ) -> Option<crate::runtime::blockchain_kind::ChainTipFull> {
        let store = self.inner.db.store();
        if !crate::runtime::blockchain_kind::is_chain(&store, collection) {
            return None;
        }
        let mut cache = self.inner.chain_tip_cache.lock();
        if let Some(existing) = cache.get(collection) {
            return Some(existing.clone());
        }
        let scanned = crate::runtime::blockchain_kind::chain_tip_full(&store, collection)?;
        cache.insert(collection.to_string(), scanned.clone());
        Some(scanned)
    }

    /// Issue #525 — walks the chain end-to-end, recomputes each block's hash
    /// against the stored fields, and returns the verification outcome.  On
    /// `ok == false` the integrity flag is persisted and the in-memory cache
    /// is updated so subsequent INSERTs surface `ChainIntegrityBroken`.
    ///
    /// Returns `None` when the collection is absent or not a `KIND blockchain`.
    pub fn verify_chain_for_collection(
        &self,
        collection: &str,
    ) -> Option<crate::runtime::blockchain_kind::VerifyChainOutcome> {
        let store = self.inner.db.store();
        let outcome = crate::runtime::blockchain_kind::verify_chain_outcome(&store, collection)?;
        if !outcome.ok {
            crate::runtime::blockchain_kind::persist_integrity_flag(&store, collection, true);
            self.inner
                .chain_integrity_broken
                .lock()
                .insert(collection.to_string(), true);
        }
        Some(outcome)
    }

    /// Issue #525 — admin clears the `ChainIntegrityBroken` flag so the chain
    /// accepts INSERTs again.  Returns `false` when the collection is not a
    /// chain.
    pub fn clear_chain_integrity_flag(&self, collection: &str) -> bool {
        let store = self.inner.db.store();
        if !crate::runtime::blockchain_kind::is_chain(&store, collection) {
            return false;
        }
        crate::runtime::blockchain_kind::persist_integrity_flag(&store, collection, false);
        self.inner
            .chain_integrity_broken
            .lock()
            .insert(collection.to_string(), false);
        true
    }

    /// Issue #525 — INSERT-time check.  Combines in-memory cache (fast path)
    /// with a one-time scan of `red_config` on cold start so the flag survives
    /// restart.
    pub(super) fn is_chain_integrity_broken(&self, collection: &str) -> bool {
        {
            let cache = self.inner.chain_integrity_broken.lock();
            if let Some(v) = cache.get(collection) {
                return *v;
            }
        }
        let store = self.inner.db.store();
        let persisted =
            crate::runtime::blockchain_kind::is_integrity_broken_persisted(&store, collection)
                .unwrap_or(false);
        self.inner
            .chain_integrity_broken
            .lock()
            .insert(collection.to_string(), persisted);
        persisted
    }

    /// Issue #765 / S6 — lazily hydrate the integrity-tombstone cache from
    /// `red_config` on first access. Returns `true` when at least one
    /// tombstone range is present. Subsequent calls observe the cached state
    /// flag (`1` empty / `2` present) and skip the store scan.
    fn ensure_integrity_tombstones_loaded(&self) -> bool {
        use std::sync::atomic::Ordering;
        match self
            .inner
            .integrity_tombstones_state
            .load(Ordering::Relaxed)
        {
            1 => return false,
            2 => return true,
            _ => {}
        }
        // Cold: load under the cache lock so a concurrent reader cannot
        // observe a half-populated vector.
        let mut guard = self.inner.integrity_tombstones.lock();
        if self
            .inner
            .integrity_tombstones_state
            .load(Ordering::Relaxed)
            == 0
        {
            let ranges = crate::runtime::integrity_tombstone::load_ranges(&self.inner.db.store());
            let present = !ranges.is_empty();
            *guard = ranges;
            self.inner
                .integrity_tombstones_state
                .store(if present { 2 } else { 1 }, Ordering::Relaxed);
        }
        self.inner
            .integrity_tombstones_state
            .load(Ordering::Relaxed)
            == 2
    }

    /// Issue #765 / S6 — durably record an integrity tombstone over the
    /// inclusive RID range `[lo, hi]` of `table` (the committed rows of an
    /// input stream whose end-to-end SHA-256 digest did not match). The range
    /// is persisted to `red_config` (survives restart) and folded into the
    /// in-memory cache so the same process filters it immediately.
    pub fn record_integrity_tombstone(&self, table: &str, lo: u64, hi: u64) {
        use std::sync::atomic::Ordering;
        self.ensure_integrity_tombstones_loaded();
        let mut guard = self.inner.integrity_tombstones.lock();
        guard.push(crate::runtime::integrity_tombstone::TombstoneRange::new(
            table.to_string(),
            lo,
            hi,
        ));
        crate::runtime::integrity_tombstone::persist_ranges(&self.inner.db.store(), &guard);
        self.inner
            .integrity_tombstones_state
            .store(2, Ordering::Relaxed);
    }

    /// Issue #765 / S6 — snapshot of the currently-cached tombstone ranges.
    /// Intended for tests and forensic surfaces; the read path uses
    /// [`Self::filter_integrity_tombstoned`] which avoids the clone.
    pub fn integrity_tombstone_ranges(
        &self,
    ) -> Vec<crate::runtime::integrity_tombstone::TombstoneRange> {
        self.ensure_integrity_tombstones_loaded();
        self.inner.integrity_tombstones.lock().clone()
    }

    /// Issue #765 / S6 — drop tombstoned rows from a SELECT result in place.
    /// Fast no-op (one relaxed atomic load) when no tombstone has ever been
    /// recorded. Clears `pre_serialized_json` when any row is removed so the
    /// fast-path JSON cannot leak a filtered row back onto the wire.
    pub fn filter_integrity_tombstoned(&self, result: &mut UnifiedResult) {
        if !self.ensure_integrity_tombstones_loaded() {
            return;
        }
        let guard = self.inner.integrity_tombstones.lock();
        if guard.is_empty() {
            return;
        }
        let before = result.records.len();
        result.records.retain(|record| {
            !crate::runtime::integrity_tombstone::record_tombstoned(&guard, record)
        });
        if result.records.len() != before {
            result.pre_serialized_json = None;
        }
    }
}
