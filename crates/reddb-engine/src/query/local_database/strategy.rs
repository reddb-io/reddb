//! Storage boundary for the local SQL session.
//!
//! Logical admission and execution stay in `local_database`; a strategy owns
//! committed visibility, transaction staging and durable publication.

use super::{super::local_read::source::StoreView, LocalReadError, Result};
use crate::catalog::codec::{deserialize_collection_contracts, serialize_collection_contracts};
use crate::storage::{
    collection_read::{CollectionCursor, CollectionRead},
    entity::{EntityId, UnifiedEntity},
    metadata::Metadata,
    paged_file::{PagedFileStore, RECORD_BYTES_MAX},
    store_wal::StoreWalAction,
};
#[cfg(feature = "full")]
use crate::storage::{
    manager::ManagerConfig, resident_file::EmbeddedResidentStore, resident_store::ResidentStore,
};
use reddb_file::VfsFile;
use reddb_rql::ast::QueryExpr;
use std::io;
#[cfg(feature = "full")]
use std::{collections::HashSet, path::PathBuf};

pub(super) trait StorageStrategy {
    type File: VfsFile;
    type Transaction: StoreView;

    fn admit(&self, statement: &QueryExpr) -> Result<()>;
    fn committed_view(&self) -> Result<&dyn StoreView>;
    fn begin_transaction(&self) -> Result<Self::Transaction>;
    fn apply_transaction(
        &self,
        transaction: &Self::Transaction,
        actions: &[StoreWalAction],
    ) -> Result<()>;
    fn publish_actions(&mut self, actions: &[StoreWalAction]) -> Result<()>;
    fn publish_catalog_actions(
        &mut self,
        actions: &[StoreWalAction],
        catalog: Vec<u8>,
    ) -> Result<()>;
    fn commit_transaction(
        &mut self,
        transaction: &Self::Transaction,
        actions: &[StoreWalAction],
        catalog: Option<Vec<u8>>,
    ) -> Result<()>;
    fn into_inner(self) -> Self::File;
}

#[cfg(feature = "full")]
pub(super) struct ResidentStrategy<F: VfsFile> {
    store: EmbeddedResidentStore<F>,
}

pub(super) struct PagedStrategy<F: VfsFile> {
    store: PagedFileStore<F>,
}

pub(super) struct UnavailableTransaction;

impl<F: VfsFile + 'static> PagedStrategy<F> {
    pub(super) fn create(file: F) -> Result<Self> {
        Ok(Self {
            store: PagedFileStore::create(file).map_err(storage_error)?,
        })
    }

    pub(super) fn open(file: F) -> Result<Self> {
        Ok(Self {
            store: PagedFileStore::open(file).map_err(storage_error)?,
        })
    }

    pub(super) fn cache_pages(&self) -> usize {
        self.store.cache_pages()
    }

    fn record(
        &self,
        collection: &str,
        id: EntityId,
    ) -> Result<Option<(UnifiedEntity, Option<Metadata>)>> {
        self.store
            .collection(collection)
            .map_err(storage_error)?
            .get_record(id, RECORD_BYTES_MAX)?
            .map(|record| {
                record.decode_with_budget(reddb_file::STORE_VERSION_CURRENT, RECORD_BYTES_MAX)
            })
            .transpose()
            .map_err(LocalReadError::from)
    }
}

#[cfg(feature = "full")]
impl<F: VfsFile> ResidentStrategy<F> {
    pub(super) fn create(file: F, name: impl Into<PathBuf>, now_ms: u64) -> Result<Self> {
        Ok(Self {
            store: EmbeddedResidentStore::create(
                file,
                name,
                u128::from(now_ms),
                ManagerConfig::default(),
                false,
                0,
            )?,
        })
    }

    pub(super) fn open(file: F, name: impl Into<PathBuf>) -> Result<Self> {
        Ok(Self {
            store: EmbeddedResidentStore::open(file, name, ManagerConfig::default(), false, 0)?,
        })
    }
}

#[cfg(feature = "full")]
impl<F: VfsFile> StorageStrategy for ResidentStrategy<F> {
    type File = F;
    type Transaction = ResidentStore;

    fn admit(&self, _statement: &QueryExpr) -> Result<()> {
        Ok(())
    }

    fn committed_view(&self) -> Result<&dyn StoreView> {
        Ok(self.store.committed_resident()?)
    }

    fn begin_transaction(&self) -> Result<Self::Transaction> {
        copy_resident(self.store.committed_resident()?)
    }

    fn apply_transaction(
        &self,
        transaction: &Self::Transaction,
        actions: &[StoreWalAction],
    ) -> Result<()> {
        apply(transaction, actions)
    }

    fn publish_actions(&mut self, actions: &[StoreWalAction]) -> Result<()> {
        Ok(self.store.publish_actions(actions)?)
    }

    fn publish_catalog_actions(
        &mut self,
        actions: &[StoreWalAction],
        catalog: Vec<u8>,
    ) -> Result<()> {
        let staged = copy_resident(self.store.committed_resident()?)?;
        apply(&staged, actions)?;
        staged.set_aux_metadata(catalog);
        Ok(self.store.publish_snapshot(&snapshot(&staged))?)
    }

    fn commit_transaction(
        &mut self,
        transaction: &Self::Transaction,
        actions: &[StoreWalAction],
        catalog: Option<Vec<u8>>,
    ) -> Result<()> {
        match catalog {
            Some(catalog) => {
                transaction.set_aux_metadata(catalog);
                Ok(self.store.publish_snapshot(&snapshot(transaction))?)
            }
            None => Ok(self.store.publish_actions(actions)?),
        }
    }

    fn into_inner(self) -> Self::File {
        self.store.into_inner()
    }
}

impl<F: VfsFile + 'static> StoreView for PagedStrategy<F> {
    fn aux_metadata(&self) -> Result<Vec<u8>> {
        Ok(serialize_collection_contracts(
            &self.store.contracts().cloned().collect::<Vec<_>>(),
        ))
    }

    fn contains_collection(&self, name: &str) -> Result<bool> {
        self.store.contains_collection(name).map_err(storage_error)
    }

    fn scan_collection(
        &self,
        name: &str,
        visit: &mut dyn FnMut(&UnifiedEntity, Option<&Metadata>) -> bool,
    ) -> Result<()> {
        let collection = self.store.collection(name).map_err(storage_error)?;
        let mut cursor = collection.scan()?;
        while let Some(record) = cursor.next_record(RECORD_BYTES_MAX)? {
            let (entity, metadata) =
                record.decode_with_budget(reddb_file::STORE_VERSION_CURRENT, RECORD_BYTES_MAX)?;
            if !visit(&entity, metadata.as_ref()) {
                break;
            }
        }
        Ok(())
    }

    #[cfg(feature = "full")]
    fn get_entity(&self, collection: &str, id: EntityId) -> Result<Option<UnifiedEntity>> {
        Ok(self.record(collection, id)?.map(|(entity, _)| entity))
    }

    fn get_metadata(&self, collection: &str, id: EntityId) -> Result<Option<Metadata>> {
        Ok(self
            .record(collection, id)?
            .and_then(|(_, metadata)| metadata))
    }

    fn next_entity_id(&self) -> Result<u64> {
        self.store.next_entity_id().map_err(storage_error)
    }
}

impl StoreView for UnavailableTransaction {
    fn aux_metadata(&self) -> Result<Vec<u8>> {
        Err(transactions_unavailable())
    }

    fn contains_collection(&self, _name: &str) -> Result<bool> {
        Err(transactions_unavailable())
    }

    fn scan_collection(
        &self,
        _name: &str,
        _visit: &mut dyn FnMut(&UnifiedEntity, Option<&Metadata>) -> bool,
    ) -> Result<()> {
        Err(transactions_unavailable())
    }

    #[cfg(feature = "full")]
    fn get_entity(&self, _collection: &str, _id: EntityId) -> Result<Option<UnifiedEntity>> {
        Err(transactions_unavailable())
    }

    fn get_metadata(&self, _collection: &str, _id: EntityId) -> Result<Option<Metadata>> {
        Err(transactions_unavailable())
    }

    fn next_entity_id(&self) -> Result<u64> {
        Err(transactions_unavailable())
    }
}

impl<F: VfsFile + 'static> StorageStrategy for PagedStrategy<F> {
    type File = F;
    type Transaction = UnavailableTransaction;

    fn admit(&self, statement: &QueryExpr) -> Result<()> {
        if matches!(statement, QueryExpr::Update(_) | QueryExpr::Delete(_)) {
            return Err(LocalReadError::Unsupported(
                "paged local storage currently supports INSERT mutations only",
            ));
        }
        Ok(())
    }

    fn committed_view(&self) -> Result<&dyn StoreView> {
        self.store.check_healthy().map_err(storage_error)?;
        Ok(self)
    }

    fn begin_transaction(&self) -> Result<Self::Transaction> {
        Err(transactions_unavailable())
    }

    fn apply_transaction(
        &self,
        _transaction: &Self::Transaction,
        _actions: &[StoreWalAction],
    ) -> Result<()> {
        Err(transactions_unavailable())
    }

    fn publish_actions(&mut self, actions: &[StoreWalAction]) -> Result<()> {
        if actions.is_empty() {
            return Ok(());
        }
        let mut name = None;
        let mut records = Vec::with_capacity(actions.len());
        let mut required_capabilities = 0;
        for action in actions {
            let StoreWalAction::UpsertEntityRecord { collection, record } = action else {
                return Err(LocalReadError::Unsupported(
                    "paged local storage currently supports INSERT mutations only",
                ));
            };
            if name.as_ref().is_some_and(|name| name != collection) {
                return Err(LocalReadError::Unsupported(
                    "one paged mutation may target only one table",
                ));
            }
            name = Some(collection.clone());
            let mut budget = reddb_file::native_value::DecodeBudget::new(RECORD_BYTES_MAX);
            let (entity, metadata) =
                crate::storage::metadata_codec::deserialize_entity_record_with_budget(
                    record,
                    reddb_file::STORE_VERSION_CURRENT,
                    &mut budget,
                )?;
            if metadata.is_some()
                || entity.logical_id() != entity.id
                || entity.xmin != 0
                || entity.xmax != 0
            {
                return Err(LocalReadError::Unsupported(
                    "paged local INSERT requires committed rows without metadata",
                ));
            }
            required_capabilities |=
                crate::storage::entity_codec::required_file_capabilities(&entity);
            records.push((entity.id.raw(), record.clone()));
        }
        if records.first().map(|(id, _)| *id)
            != Some(self.store.next_entity_id().map_err(storage_error)?)
        {
            return Err(LocalReadError::Unsupported(
                "paged local storage does not update existing rows yet",
            ));
        }
        self.store
            .insert_records(
                name.as_deref().expect("nonempty actions"),
                &records,
                required_capabilities,
            )
            .map_err(storage_error)
    }

    fn publish_catalog_actions(
        &mut self,
        actions: &[StoreWalAction],
        catalog: Vec<u8>,
    ) -> Result<()> {
        let [StoreWalAction::CreateCollection { name }] = actions else {
            return Err(LocalReadError::Unsupported(
                "paged catalog publication accepts one CREATE TABLE",
            ));
        };
        let contract = deserialize_collection_contracts(&catalog)
            .map_err(|error| LocalReadError::Catalog(error.to_string()))?
            .into_iter()
            .find(|contract| contract.name == *name)
            .ok_or_else(|| LocalReadError::Catalog("created table contract is missing".into()))?;
        let now_ms = u64::try_from(contract.created_at_unix_ms)
            .map_err(|_| LocalReadError::Catalog("contract timestamp exceeds u64".into()))?;
        self.store.register(contract, now_ms).map_err(storage_error)
    }

    fn commit_transaction(
        &mut self,
        _transaction: &Self::Transaction,
        _actions: &[StoreWalAction],
        _catalog: Option<Vec<u8>>,
    ) -> Result<()> {
        Err(transactions_unavailable())
    }

    fn into_inner(self) -> Self::File {
        self.store.into_inner()
    }
}

#[cfg(feature = "full")]
fn snapshot(resident: &ResidentStore) -> Vec<u8> {
    resident.snapshot_bytes(&HashSet::new(), &HashSet::new(), || {})
}

#[cfg(feature = "full")]
fn copy_resident(resident: &ResidentStore) -> Result<ResidentStore> {
    ResidentStore::from_snapshot(&snapshot(resident), &ManagerConfig::default(), false, 0)
        .map(|(resident, _)| resident)
        .map_err(|error| LocalReadError::Query(error.to_string()))
}

#[cfg(feature = "full")]
fn apply(resident: &ResidentStore, actions: &[StoreWalAction]) -> Result<()> {
    for action in actions {
        resident.apply_replayed_action(
            action,
            reddb_file::STORE_VERSION_CURRENT,
            &ManagerConfig::default(),
        )?;
    }
    Ok(())
}

fn storage_error(error: io::Error) -> LocalReadError {
    LocalReadError::Storage(error.into())
}

fn transactions_unavailable() -> LocalReadError {
    LocalReadError::Unsupported("paged local transactions are not implemented")
}
