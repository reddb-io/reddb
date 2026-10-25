//! Single-owner publication of committed logical store batches in a native artifact.
//! SQL/catalog admission and application transactions are supplied by the host.
#![deny(
    clippy::unwrap_used,
    clippy::too_many_lines,
    clippy::cast_possible_truncation
)]

use super::entity::{EntityId, UnifiedEntity};
use super::error::StoreError;
use super::manager::ManagerConfig;
use super::metadata::Metadata;
use super::resident_store::ResidentStore;
use super::store_wal::StoreWalAction;
use reddb_file::{EmbeddedRdbArtifact, EmbeddedRdbFile, EmbeddedRdbOpen, VfsFile};
use std::collections::HashSet;
use std::path::PathBuf;

/// Extra disk reservation for normal embedded overflow checkpoints, not a
/// per-write buffer allocation. Fresh files stay at 64 KiB. A single larger
/// indivisible action retains the existing capacity/oversize contract.
pub const EMBEDDED_WAL_HEADROOM_LIMIT: u64 = 1024 * 1024;

/// The host retains its exclusive file/worker fence for this object's lifetime.
/// Reads return owned values so a caller cannot observe provisional mutations.
/// An ambiguous write or interrupted publication fences reads and writes until
/// reopen. This controller publishes committed logical batches; it does not
/// supply SQL transaction isolation or catalog policy.
pub struct EmbeddedResidentStore<F: VfsFile> {
    file: EmbeddedRdbFile<F>,
    resident: ResidentStore,
    manager_config: ManagerConfig,
    auto_index_refs: bool,
    max_cross_refs: usize,
    failed: bool,
}

impl<F: VfsFile> EmbeddedResidentStore<F> {
    pub fn create(
        file: F,
        name: impl Into<PathBuf>,
        created_at_unix_ms: u128,
        manager_config: ManagerConfig,
        auto_index_refs: bool,
        max_cross_refs: usize,
    ) -> Result<Self, StoreError> {
        let resident = ResidentStore::new();
        let snapshot = resident.snapshot_bytes(&HashSet::new(), &HashSet::new(), || {});
        let file = EmbeddedRdbFile::create(file, name, &snapshot, created_at_unix_ms)
            .map_err(file_error)?;
        Ok(Self {
            file,
            resident,
            manager_config,
            auto_index_refs,
            max_cross_refs,
            failed: false,
        })
    }

    pub fn open(
        file: F,
        name: impl Into<PathBuf>,
        manager_config: ManagerConfig,
        auto_index_refs: bool,
        max_cross_refs: usize,
    ) -> Result<Self, StoreError> {
        let mut file = EmbeddedRdbFile::open(file, name).map_err(file_error)?;
        let resident =
            Self::recover_resident(&mut file, &manager_config, auto_index_refs, max_cross_refs)?;
        Ok(Self {
            file,
            resident,
            manager_config,
            auto_index_refs,
            max_cross_refs,
            failed: false,
        })
    }

    fn recover_resident(
        file: &mut EmbeddedRdbFile<F>,
        manager_config: &ManagerConfig,
        auto_index_refs: bool,
        max_cross_refs: usize,
    ) -> Result<ResidentStore, StoreError> {
        let snapshot = file.read_snapshot().map_err(file_error)?;
        let resident = match snapshot.filter(|bytes| !bytes.is_empty()) {
            Some(bytes) => {
                ResidentStore::from_snapshot(
                    &bytes,
                    manager_config,
                    auto_index_refs,
                    max_cross_refs,
                )
                .map_err(StoreError::Serialization)?
                .0
            }
            None => ResidentStore::new(),
        };
        for payload in file.read_wal_payloads().map_err(file_error)? {
            let action = StoreWalAction::decode(&payload).map_err(StoreError::Io)?;
            resident.apply_replayed_action(
                &action,
                reddb_file::STORE_VERSION_CURRENT,
                manager_config,
            )?;
        }
        Ok(resident)
    }

    /// No caller can read staged rows while this exclusive mutable borrow is
    /// publishing. The batch becomes available only after the native WAL and
    /// root barriers succeed. A rejected batch reloads the committed image;
    /// ambiguous I/O keeps the instance fenced instead.
    pub fn publish_actions(&mut self, actions: &[StoreWalAction]) -> Result<(), StoreError> {
        self.check_healthy()?;
        if actions.is_empty() {
            return Ok(());
        }
        let payloads: Vec<_> = actions.iter().map(StoreWalAction::encode).collect();
        let encoded_len =
            EmbeddedRdbArtifact::wal_payloads_encoded_len(&payloads).map_err(file_error)?;

        // Set before mutation/I/O so unwinding cannot expose provisional state.
        self.failed = true;
        self.reserve_wal_capacity(encoded_len)?;
        for action in actions {
            if let Err(error) = self.resident.apply_replayed_action(
                action,
                reddb_file::STORE_VERSION_CURRENT,
                &self.manager_config,
            ) {
                self.resident = Self::recover_resident(
                    &mut self.file,
                    &self.manager_config,
                    self.auto_index_refs,
                    self.max_cross_refs,
                )?;
                self.failed = false;
                return Err(error);
            }
        }
        self.file
            .append_wal_payloads(&payloads)
            .map_err(file_error)?;
        self.failed = false;
        Ok(())
    }

    fn reserve_wal_capacity(&mut self, encoded_len: u64) -> Result<(), StoreError> {
        let state = self.file.state().map_err(file_error)?;
        let available = state
            .manifest
            .wal_region_bytes
            .saturating_sub(state.manifest.wal_live_bytes);
        if encoded_len > available {
            // Fold the old committed state before staging this batch. Never
            // checkpoint provisional rows to make room for their own WAL.
            let snapshot = self
                .resident
                .snapshot_bytes(&HashSet::new(), &HashSet::new(), || {});
            let snapshot_len = u64::try_from(snapshot.len()).expect("snapshot length fits u64");
            let capacity = encoded_len.max(snapshot_len.min(EMBEDDED_WAL_HEADROOM_LIMIT));
            self.file
                .write_snapshot(&snapshot, capacity)
                .map_err(file_error)?;
        }
        Ok(())
    }

    /// The host may call this only for the controller's committed state; SQL
    /// provisional versions and catalog changes must not be staged here.
    pub fn checkpoint(&mut self) -> Result<(), StoreError> {
        self.check_healthy()?;
        self.failed = true;
        let snapshot = self
            .resident
            .snapshot_bytes(&HashSet::new(), &HashSet::new(), || {});
        self.file.write_snapshot(&snapshot, 0).map_err(file_error)?;
        self.failed = false;
        Ok(())
    }

    /// Read a physical record; query visibility is supplied by the host.
    pub fn get(
        &self,
        collection: &str,
        id: EntityId,
    ) -> Result<Option<(UnifiedEntity, Option<Metadata>)>, StoreError> {
        self.check_healthy()?;
        Ok(self
            .resident
            .get_collection(collection)
            .and_then(|manager| {
                manager
                    .get(id)
                    .map(|entity| (entity, manager.get_metadata(id)))
            }))
    }

    pub fn collection_names(&self) -> Result<Vec<String>, StoreError> {
        self.check_healthy()?;
        Ok(self.resident.list_collections())
    }

    pub fn state(&mut self) -> Result<EmbeddedRdbOpen, StoreError> {
        self.check_healthy()?;
        self.failed = true;
        let state = self.file.state().map_err(file_error)?;
        self.failed = false;
        Ok(state)
    }

    /// Consume the controller to close or reopen its exclusively owned handle.
    pub fn into_inner(self) -> F {
        self.file.into_inner()
    }

    fn check_healthy(&self) -> Result<(), StoreError> {
        if self.failed {
            return Err(StoreError::Io(std::io::Error::other(
                "resident store must be reopened after interrupted publication",
            )));
        }
        Ok(())
    }
}

fn file_error(error: reddb_file::RdbFileError) -> StoreError {
    StoreError::Io(std::io::Error::other(error.to_string()))
}
