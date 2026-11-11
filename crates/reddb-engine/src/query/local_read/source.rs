//! Logical row source shared by local storage strategies.

use super::LocalReadError;
use crate::storage::{
    entity::{EntityId, UnifiedEntity},
    metadata::Metadata,
    resident_store::ResidentStore,
};
use std::sync::atomic::Ordering;

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
    fn get_entity(&self, collection: &str, id: EntityId) -> Result<Option<UnifiedEntity>>;
    fn get_metadata(&self, collection: &str, id: EntityId) -> Result<Option<Metadata>>;
    fn next_entity_id(&self) -> Result<u64>;
}

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
