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
        self.try_chain_tip_for_collection(collection).ok().flatten()
    }

    pub fn try_chain_tip_for_collection(
        &self,
        collection: &str,
    ) -> RedDBResult<Option<crate::runtime::blockchain_kind::ChainTipFull>> {
        let store = self.inner.db.store();
        if !super::blockchain_kind::try_is_chain(&store, collection)? {
            return Ok(None);
        }
        let mut cache = self.inner.chain_tip_cache.lock();
        if let Some(existing) = cache.get(collection) {
            return Ok(Some(existing.clone()));
        }
        let Some(scanned) = super::blockchain_kind::try_chain_tip_full(&store, collection)? else {
            return Ok(None);
        };
        cache.insert(collection.to_string(), scanned.clone());
        Ok(Some(scanned))
    }

    /// Verify every physical block, preserving storage failures as errors.
    pub fn verify_chain_for_collection(
        &self,
        collection: &str,
    ) -> Option<crate::runtime::blockchain_kind::VerifyChainOutcome> {
        self.try_verify_chain_for_collection(collection)
            .ok()
            .flatten()
    }

    pub fn try_verify_chain_for_collection(
        &self,
        collection: &str,
    ) -> RedDBResult<Option<crate::runtime::blockchain_kind::VerifyChainOutcome>> {
        let store = self.inner.db.store();
        let Some(outcome) = super::blockchain_kind::try_verify_chain_outcome(&store, collection)?
        else {
            return Ok(None);
        };
        if !outcome.ok {
            // Refuse further local writes even if publishing the durable flag fails.
            self.inner
                .chain_integrity_broken
                .lock()
                .insert(collection.to_string(), true);
            super::blockchain_kind::try_persist_integrity_flag(&store, collection, true)?;
        }
        Ok(Some(outcome))
    }

    pub fn clear_chain_integrity_flag(&self, collection: &str) -> bool {
        self.try_clear_chain_integrity_flag(collection)
            .unwrap_or(false)
    }

    pub fn try_clear_chain_integrity_flag(&self, collection: &str) -> RedDBResult<bool> {
        let store = self.inner.db.store();
        if !super::blockchain_kind::try_is_chain(&store, collection)? {
            return Ok(false);
        }
        super::blockchain_kind::try_persist_integrity_flag(&store, collection, false)?;
        self.inner
            .chain_integrity_broken
            .lock()
            .insert(collection.to_string(), false);
        Ok(true)
    }

    pub(super) fn is_chain_integrity_broken(&self, collection: &str) -> RedDBResult<bool> {
        if let Some(broken) = self.inner.chain_integrity_broken.lock().get(collection) {
            return Ok(*broken);
        }
        let persisted = super::blockchain_kind::try_is_integrity_broken_persisted(
            &self.inner.db.store(),
            collection,
        )?
        .unwrap_or(false);
        self.inner
            .chain_integrity_broken
            .lock()
            .insert(collection.to_string(), persisted);
        Ok(persisted)
    }

    /// Issue #765 / S6 — lazily hydrate the integrity-tombstone cache from
    /// `red_config` on first access. Returns `true` when at least one
    /// tombstone range is present. Subsequent calls observe the cached state
    /// flag (`1` empty / `2` present) and skip the store scan.
    pub(crate) fn ensure_integrity_tombstones_loaded(&self) -> RedDBResult<bool> {
        use std::sync::atomic::Ordering;
        match self
            .inner
            .integrity_tombstones_state
            .load(Ordering::Relaxed)
        {
            1 => return Ok(false),
            2 => return Ok(true),
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
            let ranges = crate::runtime::integrity_tombstone::load_ranges(&self.inner.db.store())?;
            let present = !ranges.is_empty();
            *guard = ranges;
            self.inner
                .integrity_tombstones_state
                .store(if present { 2 } else { 1 }, Ordering::Relaxed);
        }
        Ok(self
            .inner
            .integrity_tombstones_state
            .load(Ordering::Relaxed)
            == 2)
    }

    /// Issue #765 / S6 — durably record an integrity tombstone over the
    /// inclusive RID range `[lo, hi]` of `table` (the committed rows of an
    /// input stream whose end-to-end SHA-256 digest did not match). The range
    /// is persisted to `red_config` (survives restart) and folded into the
    /// in-memory cache so the same process filters it immediately.
    pub fn record_integrity_tombstone(&self, table: &str, lo: u64, hi: u64) -> RedDBResult<()> {
        self.record_integrity_tombstones(&[
            crate::runtime::integrity_tombstone::TombstoneRange::new(table, lo, hi),
        ])
    }

    pub(crate) fn record_integrity_tombstones(
        &self,
        ranges: &[crate::runtime::integrity_tombstone::TombstoneRange],
    ) -> RedDBResult<()> {
        use std::sync::atomic::Ordering;
        if ranges.is_empty() {
            return Ok(());
        }
        if ranges.iter().any(|range| range.lo > range.hi) {
            return Err(RedDBError::Query(
                "integrity tombstone range is reversed".into(),
            ));
        }
        self.ensure_integrity_tombstones_loaded()?;
        let mut guard = self.inner.integrity_tombstones.lock();
        guard.extend_from_slice(ranges);
        // Quarantine in this process even when durable publication fails.
        // The caller must report the persistence failure to the stream client.
        self.inner
            .integrity_tombstones_state
            .store(2, Ordering::Relaxed);
        crate::runtime::integrity_tombstone::persist_ranges(&self.inner.db.store(), &guard)?;
        Ok(())
    }

    /// Issue #765 / S6 — snapshot of the currently-cached tombstone ranges.
    /// Intended for tests and forensic surfaces; the read path uses
    /// [`Self::filter_integrity_tombstoned`] which avoids the clone.
    pub fn integrity_tombstone_ranges(
        &self,
    ) -> RedDBResult<Vec<crate::runtime::integrity_tombstone::TombstoneRange>> {
        self.ensure_integrity_tombstones_loaded()?;
        Ok(self.inner.integrity_tombstones.lock().clone())
    }

    /// Issue #765 / S6 — drop tombstoned rows from a SELECT result in place.
    /// Fast no-op (one relaxed atomic load) when no tombstone has ever been
    /// recorded. Clears `pre_serialized_json` when any row is removed so the
    /// fast-path JSON cannot leak a filtered row back onto the wire.
    pub fn filter_integrity_tombstoned(&self, result: &mut UnifiedResult) -> RedDBResult<()> {
        if !self.ensure_integrity_tombstones_loaded()? {
            return Ok(());
        }
        let guard = self.inner.integrity_tombstones.lock();
        if guard.is_empty() {
            return Ok(());
        }
        let before = result.records.len();
        result.records.retain(|record| {
            !crate::runtime::integrity_tombstone::record_tombstoned(&guard, record)
        });
        if result.records.len() != before {
            result.pre_serialized_json = None;
        }
        Ok(())
    }
}
