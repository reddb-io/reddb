//! Physical reads from either a complete resident collection or a published
//! paged base with an explicit, immutable mutation overlay. An absent overlay
//! entry is cold; an entry containing None is a completed physical deletion.
//!
//! Paged callers must supply an immutable/fenced base, admit overlay ownership
//! and retained generations, and bound read workspace. Raw reads preserve MVCC
//! stamps; query scans apply an explicit snapshot. This module does not publish
//! mutations. Captured Arcs retain their source, but do not turn in-place writes
//! into snapshots.
#![deny(
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::cast_possible_truncation
)]

use super::{
    entity::{EntityData, EntityId, EntityKind, UnifiedEntity},
    error::StoreError,
    manager::SegmentManager,
    metadata::Metadata,
    page_store::PageStore,
    paged_collection::{PagedCollectionReader, PagedEntityRecord},
};
use crate::transaction::read::SnapshotContext;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

/// Shared record owners let an admitted successor overlay retain unchanged
/// payloads without cloning them. Retained maps/owners still require accounting.
pub type CollectionReadChanges = BTreeMap<EntityId, Option<Arc<PagedEntityRecord>>>;

enum ReadAuthority<P: PageStore> {
    Resident(Arc<SegmentManager>),
    Paged {
        base: Arc<PagedCollectionReader<P>>,
        changes: Arc<CollectionReadChanges>,
        schema: Option<Arc<Vec<String>>>,
    },
}

pub struct CollectionReadView<P: PageStore> {
    authority: ReadAuthority<P>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MvccVacuumStats {
    pub scanned_versions: u64,
    pub retained_versions: u64,
    pub reclaimed_versions: u64,
    pub retained_history_versions: u64,
    pub reclaimed_history_versions: u64,
    pub retained_tombstones: u64,
    pub reclaimed_tombstones: u64,
}

impl MvccVacuumStats {
    pub fn add(&mut self, other: &Self) {
        self.scanned_versions += other.scanned_versions;
        self.retained_versions += other.retained_versions;
        self.reclaimed_versions += other.reclaimed_versions;
        self.retained_history_versions += other.retained_history_versions;
        self.reclaimed_history_versions += other.reclaimed_history_versions;
        self.retained_tombstones += other.retained_tombstones;
        self.reclaimed_tombstones += other.reclaimed_tombstones;
    }
}

impl<P: PageStore> CollectionReadView<P> {
    /// Prepare a complete physical deletion plan before the host mutates anything.
    /// The caller must retain its existing snapshot-horizon and mutation fences.
    pub fn plan_mvcc_vacuum(
        &self,
        cutoff_xid: u64,
    ) -> Result<(MvccVacuumStats, Vec<EntityId>), StoreError> {
        let mut logical = HashMap::<EntityId, (bool, u64)>::new();
        self.visit_vacuum_entities(|entity| {
            if matches!(entity.kind, EntityKind::TableRow { .. }) {
                let entry = logical.entry(entity.logical_id()).or_insert((false, 0));
                entry.0 |= entity.xmax == 0;
                entry.1 = entry.1.max(entity.xmin);
            }
        })?;
        let mut stats = MvccVacuumStats::default();
        let mut reclaim_ids = Vec::new();
        self.visit_vacuum_entities(|entity| {
            if !matches!(entity.kind, EntityKind::TableRow { .. }) || entity.xmax == 0 {
                return;
            }
            stats.scanned_versions += 1;
            let (has_live_version, max_xmin) = logical
                .get(&entity.logical_id())
                .copied()
                .unwrap_or((false, entity.xmin));
            let tombstone = !has_live_version && entity.xmin == max_xmin;
            if entity.xmax < cutoff_xid {
                stats.reclaimed_versions += 1;
                if tombstone {
                    stats.reclaimed_tombstones += 1;
                } else {
                    stats.reclaimed_history_versions += 1;
                }
                reclaim_ids.push(entity.id);
            } else {
                stats.retained_versions += 1;
                if tombstone {
                    stats.retained_tombstones += 1;
                } else {
                    stats.retained_history_versions += 1;
                }
            }
        })?;
        Ok((stats, reclaim_ids))
    }

    fn visit_vacuum_entities(
        &self,
        mut visit: impl FnMut(&UnifiedEntity),
    ) -> Result<(), StoreError> {
        if let ReadAuthority::Resident(manager) = &self.authority {
            manager.for_each_entity(|entity| {
                visit(entity);
                true
            });
        } else {
            self.visit_entities(|entity| {
                visit(entity);
                true
            })?;
        }
        Ok(())
    }
    pub fn resident(manager: Arc<SegmentManager>) -> Self {
        Self {
            authority: ReadAuthority::Resident(manager),
        }
    }

    pub fn paged(
        base: Arc<PagedCollectionReader<P>>,
        changes: Arc<CollectionReadChanges>,
        schema: Option<Arc<Vec<String>>>,
    ) -> Self {
        assert!(
            changes
                .iter()
                .all(|(id, record)| record.as_ref().is_none_or(|record| record.entity.id == *id)),
            "invariant: mutation overlay keys are their physical entity IDs"
        );
        Self {
            authority: ReadAuthority::Paged {
                base,
                changes,
                schema,
            },
        }
    }

    pub fn get(&self, id: EntityId) -> Result<Option<UnifiedEntity>, StoreError> {
        let mut entity = match &self.authority {
            ReadAuthority::Resident(manager) => manager.get(id),
            ReadAuthority::Paged { base, changes, .. } => match changes.get(&id) {
                Some(record) => record.as_ref().map(|record| record.entity.clone()),
                None => base.get(id)?.map(|record| record.entity),
            },
        };
        if let Some(entity) = &mut entity {
            self.restore_owned_schema(entity);
        }
        Ok(entity)
    }

    /// Borrow payloads and preserve complete physical-ID order. Returning false
    /// stops before another source read. Resident callbacks run under a segment
    /// read guard and must not re-enter storage or memory admission.
    pub fn visit_entities(
        &self,
        mut visit: impl FnMut(&UnifiedEntity) -> bool,
    ) -> Result<bool, StoreError> {
        if let ReadAuthority::Resident(manager) = &self.authority {
            let mut completed = true;
            manager.for_each_entity_in_id_order(|entity| {
                completed = visit(entity);
                completed
            });
            return Ok(completed);
        }
        self.visit_records(|entity, _| visit(entity))
    }

    /// Retain metadata for checkpoint/index consumers without cloning the base
    /// or the overlay's complete payloads. This is a physical scan, not an MVCC
    /// snapshot; callers retain the existing visibility/publication boundaries.
    pub fn visit_records(
        &self,
        visit: impl FnMut(&UnifiedEntity, Option<&Metadata>) -> bool,
    ) -> Result<bool, StoreError> {
        self.visit_records_checked(|| true, visit)
    }

    fn visit_records_checked(
        &self,
        mut before_work: impl FnMut() -> bool,
        mut visit: impl FnMut(&UnifiedEntity, Option<&Metadata>) -> bool,
    ) -> Result<bool, StoreError> {
        if !before_work() {
            return Ok(false);
        }
        let (base, changes) = match &self.authority {
            ReadAuthority::Resident(manager) => {
                let mut completed = true;
                manager.for_each_entity_with_metadata_in_id_order(|entity, metadata| {
                    completed = before_work() && visit(entity, metadata.as_ref());
                    completed
                });
                return Ok(completed);
            }
            ReadAuthority::Paged { base, changes, .. } => (base, changes),
        };
        let mut changes = changes.iter().peekable();
        let mut cursor = base.cursor()?;
        loop {
            if !before_work() {
                return Ok(false);
            }
            let Some(record) = cursor.next()? else {
                break;
            };
            while changes
                .peek()
                .is_some_and(|(id, _)| **id < record.entity.id)
            {
                let (_, change) = changes.next().expect("peeked overlay entry exists");
                if !before_work() {
                    return Ok(false);
                }
                if let Some(change) = change {
                    if !visit(&change.entity, change.metadata.as_ref()) {
                        return Ok(false);
                    }
                }
            }
            if changes
                .peek()
                .is_some_and(|(id, _)| **id == record.entity.id)
            {
                let (_, change) = changes.next().expect("matching overlay entry exists");
                if !before_work() {
                    return Ok(false);
                }
                if let Some(change) = change {
                    if !visit(&change.entity, change.metadata.as_ref()) {
                        return Ok(false);
                    }
                }
            } else if !visit(&record.entity, record.metadata.as_ref()) {
                return Ok(false);
            }
        }
        for (_, change) in changes {
            if !before_work() {
                return Ok(false);
            }
            if let Some(change) = change {
                if !visit(&change.entity, change.metadata.as_ref()) {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// Statement visibility matches resident scans, including moderation and
    /// serializable read tracking. Check work before cold I/O, even when all
    /// inspected physical versions are hidden. Consumers run outside storage
    /// guards. Resident cursors retain their captured append boundaries.
    pub fn scan_for_each(
        &self,
        snapshot: Option<&SnapshotContext>,
        mut before_work: impl FnMut() -> bool,
        mut visit: impl FnMut(&UnifiedEntity) -> bool,
    ) -> Result<bool, StoreError> {
        if !before_work() {
            return Ok(false);
        }
        if let ReadAuthority::Resident(manager) = &self.authority {
            return Ok(manager.scan_owned(snapshot, before_work, visit));
        }
        self.visit_records_checked(before_work, |entity, _| {
            !SegmentManager::scan_entity_visible(snapshot, entity) || visit(entity)
        })
    }

    pub fn column_schema(&self) -> Option<Arc<Vec<String>>> {
        match &self.authority {
            ReadAuthority::Resident(manager) => manager.column_schema(),
            ReadAuthority::Paged { schema, .. } => schema.clone(),
        }
    }

    /// Preserve resident parallel aggregation without treating resident rows as
    /// complete cold state. Paged authority folds the complete fallible stream.
    /// Resident workers run under read guards: folds must be pure and may not
    /// re-enter storage. Budgeted callers use scan_for_each instead.
    pub fn try_fold_parallel<T, FInit, FFold, FReduce>(
        &self,
        snapshot: Option<&SnapshotContext>,
        init: FInit,
        fold: FFold,
        reduce: FReduce,
    ) -> Result<T, StoreError>
    where
        T: Send,
        FInit: Fn() -> T + Send + Sync,
        FFold: Fn(T, &UnifiedEntity) -> T + Send + Sync,
        FReduce: Fn(T, T) -> T + Send + Sync,
    {
        if let ReadAuthority::Resident(manager) = &self.authority {
            return Ok(manager.scan_fold_parallel(snapshot, init, fold, reduce));
        }
        let mut accumulator = Some(init());
        self.scan_for_each(
            snapshot,
            || true,
            |entity| {
                let current = accumulator.take().expect("fold owns its accumulator");
                accumulator = Some(fold(current, entity));
                true
            },
        )?;
        Ok(accumulator.expect("fold restores its accumulator"))
    }

    /// Materialize one candidate per next() in either authority, preserving
    /// input order, misses and duplicates. Creating or dropping the iterator
    /// never hydrates the unconsumed suffix, including on LIMIT/cancellation.
    /// Callers check work BEFORE next() and admit result/workspace ownership.
    pub fn read_ids<'a>(&'a self, ids: &'a [EntityId]) -> CollectionIdReads<'a, P> {
        CollectionIdReads {
            view: self,
            ids: ids.iter(),
            failed: false,
        }
    }

    /// Borrow the resident fast path. Callback/budget hooks must not re-enter
    /// resident storage. Cold reads release page guards before callbacks.
    pub fn visit_ids(
        &self,
        ids: &[EntityId],
        mut before_work: impl FnMut() -> bool,
        mut visit: impl FnMut(usize, &UnifiedEntity) -> bool,
    ) -> Result<bool, StoreError> {
        if !before_work() {
            return Ok(false);
        }
        if let ReadAuthority::Resident(manager) = &self.authority {
            let mut completed = true;
            manager.for_each_id(ids, |index, entity| {
                if completed {
                    completed = before_work() && visit(index, entity);
                }
            });
            return Ok(completed);
        }
        for (index, id) in ids.iter().enumerate() {
            if !before_work() {
                return Ok(false);
            }
            if let Some(entity) = self.get(*id)? {
                if !visit(index, &entity) {
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    /// Clone only the requested window. Count and offset refer to the same
    /// complete physical scan, including historical versions and overlay masks.
    pub fn page_in_id_order(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<(usize, Vec<UnifiedEntity>), StoreError> {
        let mut total = 0usize;
        let mut items = Vec::new();
        let end = offset.saturating_add(limit);
        self.visit_entities(|entity| {
            if total >= offset && total < end {
                items.push(entity.clone());
            }
            total = total.saturating_add(1);
            true
        })?;
        for entity in &mut items {
            self.restore_owned_schema(entity);
        }
        Ok((total, items))
    }

    fn restore_owned_schema(&self, entity: &mut UnifiedEntity) {
        let EntityData::Row(row) = &mut entity.data else {
            return;
        };
        if row.schema.is_some() || row.named.is_some() {
            return;
        }
        row.schema = match &self.authority {
            ReadAuthority::Resident(manager) => manager.column_schema(),
            ReadAuthority::Paged { schema, .. } => schema.clone(),
        };
    }
}

pub struct CollectionIdReads<'a, P: PageStore> {
    view: &'a CollectionReadView<P>,
    ids: std::slice::Iter<'a, EntityId>,
    failed: bool,
}

impl<P: PageStore> Iterator for CollectionIdReads<'_, P> {
    type Item = Result<Option<UnifiedEntity>, StoreError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.failed {
            return None;
        }
        let result = self.view.get(*self.ids.next()?);
        self.failed = result.is_err();
        Some(result)
    }
}
