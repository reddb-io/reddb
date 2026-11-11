//! Storage boundary for the local SQL session.
//!
//! Logical admission and execution stay in `local_database`; a strategy owns
//! committed visibility, transaction staging and durable publication.

use super::{super::local_read::source::StoreView, LocalReadError, Result};
use crate::storage::{
    manager::ManagerConfig, resident_file::EmbeddedResidentStore, resident_store::ResidentStore,
    store_wal::StoreWalAction,
};
use reddb_file::VfsFile;
use std::{collections::HashSet, path::PathBuf};

pub(super) trait StorageStrategy {
    type File: VfsFile;
    type Transaction: StoreView;

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

pub(super) struct ResidentStrategy<F: VfsFile> {
    store: EmbeddedResidentStore<F>,
}

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

impl<F: VfsFile> StorageStrategy for ResidentStrategy<F> {
    type File = F;
    type Transaction = ResidentStore;

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

fn snapshot(resident: &ResidentStore) -> Vec<u8> {
    resident.snapshot_bytes(&HashSet::new(), &HashSet::new(), || {})
}

fn copy_resident(resident: &ResidentStore) -> Result<ResidentStore> {
    ResidentStore::from_snapshot(&snapshot(resident), &ManagerConfig::default(), false, 0)
        .map(|(resident, _)| resident)
        .map_err(|error| LocalReadError::Query(error.to_string()))
}

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
