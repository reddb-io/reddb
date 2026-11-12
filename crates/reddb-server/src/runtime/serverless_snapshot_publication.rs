//! Lease-generation-fenced publication of serverless snapshots.
//!
//! Payloads are uploaded under immutable keys. Only the small `HEAD.json`
//! pointer is mutable, and every owner claims and advances it with backend CAS.
//! A stale process may finish an expensive payload upload after its lease has
//! expired, but it cannot make that payload current after a successor claims
//! the head under a newer lease generation.

use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::replication::lease::WriterLease;
use crate::storage::backend::{
    AtomicRemoteBackend, BackendError, BackendObjectVersion, ConditionalPut,
};

#[derive(Debug)]
pub enum SnapshotPublicationError {
    Backend(BackendError),
    InvalidState(String),
    Fenced(String),
}

impl std::fmt::Display for SnapshotPublicationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backend(err) => write!(f, "snapshot publication backend error: {err}"),
            Self::InvalidState(message) => write!(f, "snapshot publication state error: {message}"),
            Self::Fenced(message) => write!(f, "snapshot publication fenced: {message}"),
        }
    }
}

impl std::error::Error for SnapshotPublicationError {}

impl From<BackendError> for SnapshotPublicationError {
    fn from(value: BackendError) -> Self {
        Self::Backend(value)
    }
}

#[derive(Clone)]
struct VersionedHead {
    head: reddb_file::ServerlessSnapshotHead,
    version: BackendObjectVersion,
}

enum PublicationState {
    Unobserved,
    Observed(Option<VersionedHead>),
    Claimed(VersionedHead),
    Fenced,
}

pub struct SnapshotPublicationStore {
    backend: Arc<dyn AtomicRemoteBackend>,
    database_key: String,
    head_key: String,
    state: Mutex<PublicationState>,
}

impl SnapshotPublicationStore {
    pub fn new(backend: Arc<dyn AtomicRemoteBackend>, database_key: impl Into<String>) -> Self {
        let database_key = database_key.into();
        let head_key = reddb_file::serverless_snapshot_head_key(&database_key);
        Self {
            backend,
            database_key,
            head_key,
            state: Mutex::new(PublicationState::Unobserved),
        }
    }

    /// Observe the exact head version used to hydrate this boot. The later
    /// claim must CAS this version; if publication advances while the runtime
    /// is opening, the claim fails and boot retries from the newer snapshot.
    pub fn observe_for_boot(&self) -> Result<Option<String>, SnapshotPublicationError> {
        let mut state = self
            .state
            .lock()
            .expect("poisoned snapshot publication state");
        match &*state {
            PublicationState::Unobserved => {}
            PublicationState::Observed(head) => {
                return Ok(head.as_ref().map(|head| head.head.snapshot_key.clone()));
            }
            PublicationState::Claimed(head) => {
                return Ok(Some(head.head.snapshot_key.clone()));
            }
            PublicationState::Fenced => {
                return Err(SnapshotPublicationError::Fenced(
                    "publication head was already superseded".to_string(),
                ));
            }
        }

        let observed = self.read_versioned_head()?;
        let snapshot_key = observed.as_ref().map(|head| head.head.snapshot_key.clone());
        *state = PublicationState::Observed(observed);
        Ok(snapshot_key)
    }

    /// Claim the observed head for `lease`. On the first serverless boot there
    /// is no head yet, so the already-open local database is uploaded once as
    /// an initial immutable snapshot and installed with `IfAbsent`.
    pub fn claim(
        &self,
        lease: &WriterLease,
        local_snapshot: &Path,
    ) -> Result<(), SnapshotPublicationError> {
        let mut state = self
            .state
            .lock()
            .expect("poisoned snapshot publication state");
        let observed = match &*state {
            PublicationState::Observed(observed) => observed.clone(),
            PublicationState::Unobserved => {
                return Err(SnapshotPublicationError::InvalidState(
                    "head must be observed before lease acquisition".to_string(),
                ));
            }
            PublicationState::Claimed(_) => {
                return Err(SnapshotPublicationError::InvalidState(
                    "head is already claimed".to_string(),
                ));
            }
            PublicationState::Fenced => {
                return Err(SnapshotPublicationError::Fenced(
                    "publication head was already superseded".to_string(),
                ));
            }
        };

        let (next, condition) = match observed {
            Some(current) => {
                if current.head.database_key != self.database_key {
                    return Err(SnapshotPublicationError::InvalidState(format!(
                        "head database '{}' does not match '{}'",
                        current.head.database_key, self.database_key
                    )));
                }
                let mut next = current.head;
                // Release deletes the lease object, so generations may repeat.
                // Reserve a sequence on every claim to avoid both payload-key
                // reuse and identical HEAD bytes (ABA) across clean restarts.
                next.publication_sequence =
                    next.publication_sequence.checked_add(1).ok_or_else(|| {
                        SnapshotPublicationError::InvalidState("snapshot sequence exhausted".into())
                    })?;
                next.holder_id = lease.holder_id.clone();
                next.term = lease.term;
                next.lease_generation = lease.generation;
                (next, ConditionalPut::IfVersion(current.version))
            }
            None => {
                let publication_sequence = 1;
                let snapshot_key = reddb_file::serverless_snapshot_key(
                    &self.database_key,
                    lease.generation,
                    publication_sequence,
                );
                self.backend.upload_conditional(
                    local_snapshot,
                    &snapshot_key,
                    ConditionalPut::IfAbsent,
                )?;
                (
                    reddb_file::ServerlessSnapshotHead {
                        database_key: self.database_key.clone(),
                        holder_id: lease.holder_id.clone(),
                        term: lease.term,
                        lease_generation: lease.generation,
                        publication_sequence,
                        snapshot_key,
                    },
                    ConditionalPut::IfAbsent,
                )
            }
        };

        match self.publish_head(&next, condition) {
            Ok(version) => {
                *state = PublicationState::Claimed(VersionedHead {
                    head: next,
                    version,
                });
                Ok(())
            }
            Err(SnapshotPublicationError::Backend(BackendError::PreconditionFailed(message))) => {
                *state = PublicationState::Fenced;
                Err(SnapshotPublicationError::Fenced(message))
            }
            Err(err) => Err(err),
        }
    }

    pub fn publish(
        &self,
        lease: &WriterLease,
        local_snapshot: &Path,
    ) -> Result<(), SnapshotPublicationError> {
        let mut state = self
            .state
            .lock()
            .expect("poisoned snapshot publication state");
        let current = match &*state {
            PublicationState::Claimed(current) => current.clone(),
            PublicationState::Fenced => {
                return Err(SnapshotPublicationError::Fenced(
                    "publication head was superseded".to_string(),
                ));
            }
            _ => {
                return Err(SnapshotPublicationError::InvalidState(
                    "publication requires a claimed head".to_string(),
                ));
            }
        };
        if current.head.fencing_token() != lease.fencing_token()
            || current.head.holder_id != lease.holder_id
        {
            *state = PublicationState::Fenced;
            return Err(SnapshotPublicationError::Fenced(format!(
                "lease {} generation {} does not own head generation {}",
                lease.holder_id, lease.generation, current.head.lease_generation
            )));
        }

        let publication_sequence = current
            .head
            .publication_sequence
            .checked_add(1)
            .ok_or_else(|| {
                SnapshotPublicationError::InvalidState("snapshot sequence exhausted".into())
            })?;
        let snapshot_key = reddb_file::serverless_snapshot_key(
            &self.database_key,
            lease.generation,
            publication_sequence,
        );
        self.backend
            .upload_conditional(local_snapshot, &snapshot_key, ConditionalPut::IfAbsent)?;

        let mut next = current.head;
        next.publication_sequence = publication_sequence;
        next.snapshot_key = snapshot_key;
        match self.publish_head(&next, ConditionalPut::IfVersion(current.version)) {
            Ok(version) => {
                *state = PublicationState::Claimed(VersionedHead {
                    head: next,
                    version,
                });
                Ok(())
            }
            Err(SnapshotPublicationError::Backend(BackendError::PreconditionFailed(message))) => {
                *state = PublicationState::Fenced;
                Err(SnapshotPublicationError::Fenced(message))
            }
            Err(err) => Err(err),
        }
    }

    fn read_versioned_head(&self) -> Result<Option<VersionedHead>, SnapshotPublicationError> {
        let before = match self.backend.object_version(&self.head_key)? {
            Some(version) => version,
            None => return Ok(None),
        };
        let temp = reddb_file::ServerlessWriterLeaseTempFile::new("snapshot-head-read");
        if !self.backend.download(&self.head_key, temp.path())? {
            return Ok(None);
        }
        if self.backend.object_version(&self.head_key)?.as_ref() != Some(&before) {
            return Err(SnapshotPublicationError::Backend(
                BackendError::PreconditionFailed(
                    "serverless snapshot head changed while being read".to_string(),
                ),
            ));
        }
        let bytes = temp
            .read_bytes()
            .map_err(|err| BackendError::Transport(err.to_string()))?;
        let head = reddb_file::decode_serverless_snapshot_head_json(&bytes)
            .map_err(|err| BackendError::Internal(err.to_string()))?;
        Ok(Some(VersionedHead {
            head,
            version: before,
        }))
    }

    fn publish_head(
        &self,
        head: &reddb_file::ServerlessSnapshotHead,
        condition: ConditionalPut,
    ) -> Result<BackendObjectVersion, SnapshotPublicationError> {
        let bytes = reddb_file::encode_serverless_snapshot_head_json(head)
            .map_err(|err| BackendError::Internal(err.to_string()))?;
        let temp = reddb_file::ServerlessWriterLeaseTempFile::new("snapshot-head-write");
        temp.write_bytes(&bytes)
            .map_err(|err| BackendError::Transport(err.to_string()))?;
        Ok(self
            .backend
            .upload_conditional(temp.path(), &self.head_key, condition)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::backend::LocalBackend;

    fn lease(holder: &str, generation: u64) -> WriterLease {
        WriterLease {
            database_key: "main".to_string(),
            holder_id: holder.to_string(),
            term: 1,
            generation,
            acquired_at_ms: 1,
            expires_at_ms: u64::MAX,
        }
    }

    fn read_current_snapshot(database_key: &str) -> Vec<u8> {
        let head_path = reddb_file::serverless_snapshot_head_key(database_key);
        let head_bytes = std::fs::read(head_path).expect("read publication head");
        let head = reddb_file::decode_serverless_snapshot_head_json(&head_bytes)
            .expect("decode publication head");
        std::fs::read(head.snapshot_key).expect("read current immutable snapshot")
    }

    #[test]
    fn successor_claim_fences_delayed_old_writer_publication() {
        let root = tempfile::tempdir().expect("publication directory");
        let database_key = root.path().join("remote").join("main.rdb");
        let database_key = database_key.to_string_lossy().to_string();
        let old_local = root.path().join("old.rdb");
        let new_local = root.path().join("new.rdb");
        std::fs::write(&old_local, b"old-snapshot").expect("write old local snapshot");
        std::fs::write(&new_local, b"new-snapshot").expect("write new local snapshot");

        let old = SnapshotPublicationStore::new(Arc::new(LocalBackend), database_key.clone());
        assert_eq!(old.observe_for_boot().expect("observe empty head"), None);
        old.claim(&lease("old", 1), &old_local)
            .expect("old writer claims initial head");

        let successor = SnapshotPublicationStore::new(Arc::new(LocalBackend), database_key.clone());
        assert!(successor
            .observe_for_boot()
            .expect("observe old head")
            .is_some());
        successor
            .claim(&lease("new", 2), &old_local)
            .expect("successor claims head under new generation");

        assert!(matches!(
            old.publish(&lease("old", 1), &old_local),
            Err(SnapshotPublicationError::Fenced(_))
        ));
        successor
            .publish(&lease("new", 2), &new_local)
            .expect("successor publishes current snapshot");
        assert_eq!(read_current_snapshot(&database_key), b"new-snapshot");
    }

    #[test]
    fn reused_lease_identity_cannot_restore_an_old_heads_cas_version() {
        let root = tempfile::tempdir().expect("publication directory");
        let database_key = root.path().join("main.rdb").to_string_lossy().to_string();
        let local = root.path().join("local.rdb");
        std::fs::write(&local, b"committed").expect("local snapshot");
        let old = SnapshotPublicationStore::new(Arc::new(LocalBackend), database_key.clone());
        old.observe_for_boot().expect("observe first boot");
        old.claim(&lease("same-holder", 1), &local)
            .expect("first claim");
        // A clean release removes the lease key. Even with the same holder,
        // term and generation, the next boot must change the HEAD CAS version.
        let successor = SnapshotPublicationStore::new(Arc::new(LocalBackend), database_key.clone());
        successor.observe_for_boot().expect("observe next boot");
        successor
            .claim(&lease("same-holder", 1), &local)
            .expect("reused identity claims");
        std::fs::write(&local, b"stale").expect("old writer resumes");
        assert!(matches!(
            old.publish(&lease("same-holder", 1), &local),
            Err(SnapshotPublicationError::Fenced(_))
        ));
        assert_eq!(read_current_snapshot(&database_key), b"committed");
    }

    #[test]
    fn boot_claim_fails_when_head_advanced_during_hydration() {
        let root = tempfile::tempdir().expect("publication directory");
        let database_key = root.path().join("remote").join("main.rdb");
        let database_key = database_key.to_string_lossy().to_string();
        let first = root.path().join("first.rdb");
        let second = root.path().join("second.rdb");
        std::fs::write(&first, b"first").expect("write first snapshot");
        std::fs::write(&second, b"second").expect("write second snapshot");

        let writer = SnapshotPublicationStore::new(Arc::new(LocalBackend), database_key.clone());
        writer.observe_for_boot().expect("observe empty head");
        writer
            .claim(&lease("writer", 1), &first)
            .expect("claim first head");

        let booting = SnapshotPublicationStore::new(Arc::new(LocalBackend), database_key);
        booting.observe_for_boot().expect("observe first head");
        writer
            .publish(&lease("writer", 1), &second)
            .expect("advance head while successor hydrates");

        assert!(matches!(
            booting.claim(&lease("successor", 2), &first),
            Err(SnapshotPublicationError::Fenced(_))
        ));
    }
}
