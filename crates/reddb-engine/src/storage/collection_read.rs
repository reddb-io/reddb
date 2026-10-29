//! Physical reads from either a complete resident collection or a published
//! paged base with an explicit, immutable mutation overlay. An absent overlay
//! entry is cold; an entry containing None is a completed physical deletion.
//!
//! Paged callers must supply an immutable/fenced base, admit overlay ownership
//! and retained generations, and bound read workspace. This module neither
//! publishes mutations nor applies MVCC visibility. Captured Arcs retain their
//! source, but do not make in-place writes to that source into snapshots.
#![deny(
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::cast_possible_truncation
)]

use super::{
    entity::{EntityData, EntityId, UnifiedEntity},
    error::StoreError,
    manager::SegmentManager,
    metadata::Metadata,
    page_store::PageStore,
    paged_collection::{PagedCollectionReader, PagedEntityRecord},
};
use std::{collections::BTreeMap, sync::Arc};

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

impl<P: PageStore> CollectionReadView<P> {
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
        mut visit: impl FnMut(&UnifiedEntity, Option<&Metadata>) -> bool,
    ) -> Result<bool, StoreError> {
        let (base, changes) = match &self.authority {
            ReadAuthority::Resident(manager) => {
                let mut completed = true;
                manager.for_each_entity_with_metadata_in_id_order(|entity, metadata| {
                    completed = visit(entity, metadata.as_ref());
                    completed
                });
                return Ok(completed);
            }
            ReadAuthority::Paged { base, changes, .. } => (base, changes),
        };
        let mut changes = changes.iter().peekable();
        let mut cursor = base.cursor()?;
        while let Some(record) = cursor.next()? {
            while changes
                .peek()
                .is_some_and(|(id, _)| **id < record.entity.id)
            {
                let (_, change) = changes.next().expect("peeked overlay entry exists");
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
            if let Some(change) = change {
                if !visit(&change.entity, change.metadata.as_ref()) {
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
