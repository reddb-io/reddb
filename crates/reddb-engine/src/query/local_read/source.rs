//! Logical row source shared by local storage strategies.

use super::LocalReadError;
use crate::catalog::enforcement::{
    INTERNAL_EXPIRES_AT_KEY, INTERNAL_TTL_MILLIS_KEY, INTERNAL_TTL_SECONDS_KEY,
};
use crate::query::unified::UnifiedRecord;
#[cfg(feature = "full")]
use crate::storage::resident_store::ResidentStore;
use crate::storage::{
    entity::{EntityData, EntityId, UnifiedEntity},
    metadata::Metadata,
};
#[cfg(feature = "full")]
use std::sync::atomic::Ordering;
use std::{cell::RefCell, sync::Arc};

type Result<T> = std::result::Result<T, LocalReadError>;

/// Logical rows needed by the shared local SQL executor.
///
/// The callback intentionally borrows one row at a time so a paged strategy
/// can decode through a bounded cursor without constructing a `ResidentStore`.
pub(crate) trait StoreView {
    fn aux_metadata(&self) -> Result<Vec<u8>>;
    fn contains_collection(&self, name: &str) -> Result<bool>;
    fn scan_collection(
        &self,
        name: &str,
        visit: &mut dyn FnMut(&UnifiedEntity, Option<&Metadata>) -> bool,
    ) -> Result<()>;
    #[cfg(feature = "full")]
    fn get_entity(&self, collection: &str, id: EntityId) -> Result<Option<UnifiedEntity>>;
    fn get_metadata(&self, collection: &str, id: EntityId) -> Result<Option<Metadata>>;
    fn next_entity_id(&self) -> Result<u64>;
}

pub(crate) type RowVisitor<'a> = dyn FnMut(UnifiedRecord) -> Result<bool> + 'a;

pub(crate) trait RowSource {
    fn scan(&self, visit: &mut RowVisitor<'_>) -> Result<()>;
    fn estimated_rows(&self) -> usize;
    fn materialize(&self) -> bool {
        true
    }
}

pub(crate) struct StoreRows<'a> {
    pub store: &'a dyn StoreView,
    pub table: &'a str,
    pub schema: Arc<Vec<String>>,
    pub allow_materialization: bool,
}

impl RowSource for StoreRows<'_> {
    fn scan(&self, visit: &mut RowVisitor<'_>) -> Result<()> {
        let error = RefCell::new(None);
        self.store
            .scan_collection(self.table, &mut |entity, metadata| {
                if !matches!(entity.data, EntityData::Row(_)) {
                    *error.borrow_mut() = Some(LocalReadError::Unsupported(
                        "local table contains non-row entities",
                    ));
                    return false;
                }
                if metadata.is_some_and(|metadata| {
                    metadata.has(INTERNAL_TTL_SECONDS_KEY)
                        || metadata.has(INTERNAL_TTL_MILLIS_KEY)
                        || metadata.has(INTERNAL_EXPIRES_AT_KEY)
                }) {
                    *error.borrow_mut() = Some(LocalReadError::Unsupported(
                        "local SELECT does not yet admit row TTL metadata",
                    ));
                    return false;
                }
                let Some(record) =
                    crate::query::records::runtime_table_record_from_entity_ref_with_schema(
                        entity,
                        Some(&self.schema),
                    )
                else {
                    return true;
                };
                match visit(record) {
                    Ok(keep_going) => keep_going,
                    Err(cause) => {
                        *error.borrow_mut() = Some(cause);
                        false
                    }
                }
            })?;
        match error.into_inner() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    fn estimated_rows(&self) -> usize {
        usize::MAX
    }

    fn materialize(&self) -> bool {
        self.allow_materialization
    }
}

#[cfg(feature = "full")]
impl StoreView for ResidentStore {
    fn aux_metadata(&self) -> Result<Vec<u8>> {
        Ok(self.aux_metadata())
    }

    fn contains_collection(&self, name: &str) -> Result<bool> {
        Ok(self.get_collection(name).is_some())
    }

    fn scan_collection(
        &self,
        name: &str,
        visit: &mut dyn FnMut(&UnifiedEntity, Option<&Metadata>) -> bool,
    ) -> Result<()> {
        let manager = self
            .get_collection(name)
            .ok_or_else(|| LocalReadError::Catalog(format!("collection {name} is missing")))?;
        manager.scan_for_each(None, |entity| {
            let metadata = manager.get_metadata(entity.id);
            visit(entity, metadata.as_ref())
        });
        Ok(())
    }

    #[cfg(feature = "full")]
    fn get_entity(&self, collection: &str, id: EntityId) -> Result<Option<UnifiedEntity>> {
        Ok(self
            .get_collection(collection)
            .and_then(|manager| manager.get(id)))
    }

    fn get_metadata(&self, collection: &str, id: EntityId) -> Result<Option<Metadata>> {
        Ok(self
            .get_collection(collection)
            .and_then(|manager| manager.get_metadata(id)))
    }

    fn next_entity_id(&self) -> Result<u64> {
        Ok(self.next_entity_id.load(Ordering::SeqCst))
    }
}
