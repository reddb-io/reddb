//! Serverless writer lease (PLAN.md Phase 5 / W6).
//!
//! Multiple RedDB instances can attach to the same remote-backed
//! database key. To prevent two of them from concurrently mutating the
//! same remote artifacts (snapshots, WAL segments, head manifest),
//! exactly one of them must hold a *writer lease*. Other instances may
//! still attach as read-only replicas without acquiring a lease.
//!
//! ## Artifact format
//!
//! The lease is serialized as a serverless writer-lease artifact owned by
//! `reddb-file`. This module only computes policy and performs backend
//! compare-and-swap around that artifact.
//!
//! - `generation` increments every time a different holder acquires
//!   the key. The holder stamps its uploads with the generation so a
//!   stale writer (whose lease was poached because it expired) can be
//!   detected during reclaim by reading the current lease and
//!   comparing.
//! - `term` is the replication term the holder acquired under (issue
//!   #835, ADR 0030). A contender on a term *behind* the published
//!   lease's term is a deposed primary and is fenced
//!   (`LeaseError::Fenced`) — even if the lease has expired and would
//!   otherwise be poachable. Legacy lease objects without a `term`
//!   field decode as `DEFAULT_REPLICATION_TERM`.
//! - `expires_at_ms` is wall-clock millis since UNIX epoch. A holder
//!   refreshes it periodically; a contender treats anything past it as
//!   poachable.
//!
//! ## Atomicity contract
//!
//! Lease mutation requires backend-side compare-and-swap. Backends
//! advertise this through `RemoteBackend::supports_conditional_writes`
//! and implement object version tokens + conditional writes/deletes.
//! A backend that cannot enforce `IfAbsent` / `IfVersion` fails
//! closed before the instance is allowed to write. This keeps
//! serverless fencing out of "last writer wins" territory.

use std::sync::Arc;

use crate::storage::backend::{
    AtomicRemoteBackend, BackendError, BackendObjectVersion, ConditionalDelete, ConditionalPut,
};

pub use reddb_file::ServerlessWriterLease as WriterLease;

#[derive(Debug)]
pub enum LeaseError {
    Backend(BackendError),
    /// A different holder owns a non-expired lease.
    Held {
        current: WriterLease,
        now_ms: u64,
    },
    /// We uploaded a fresh lease but a re-read shows a different holder
    /// or generation, so we lost a concurrent acquire race.
    LostRace {
        attempted_holder: String,
        observed: WriterLease,
    },
    InvalidFormat(String),
    /// The release/refresh target no longer matches what's on the
    /// backend (lease was already poached or removed).
    Stale {
        attempted_holder: String,
        attempted_generation: u64,
        observed: Option<WriterLease>,
    },
    /// A holder on a term *behind* the current term tried to take or keep
    /// the lease (issue #835). The deposed primary is fenced: a newer term
    /// already owns the timeline, so the stale holder fails closed rather
    /// than mutate it.
    Fenced {
        attempted_holder: String,
        attempted_term: u64,
        current_term: u64,
    },
}

impl std::fmt::Display for LeaseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backend(err) => write!(f, "lease backend error: {err}"),
            Self::Held { current, now_ms } => {
                write!(
                    f,
                    "lease for '{}' held by '{}' (gen {}, expires in {} ms)",
                    current.database_key,
                    current.holder_id,
                    current.generation,
                    current.expires_at_ms.saturating_sub(*now_ms)
                )
            }
            Self::LostRace {
                attempted_holder,
                observed,
            } => write!(
                f,
                "lost lease acquire race: '{}' tried to take '{}' but '{}' (gen {}) won",
                attempted_holder, observed.database_key, observed.holder_id, observed.generation
            ),
            Self::InvalidFormat(msg) => write!(f, "invalid lease format: {msg}"),
            Self::Stale {
                attempted_holder,
                attempted_generation,
                observed,
            } => match observed {
                Some(o) => write!(
                    f,
                    "stale lease op: '{}' (gen {}) tried to act, but current is '{}' (gen {})",
                    attempted_holder, attempted_generation, o.holder_id, o.generation
                ),
                None => write!(
                    f,
                    "stale lease op: '{}' (gen {}) tried to act, but no lease present",
                    attempted_holder, attempted_generation
                ),
            },
            Self::Fenced {
                attempted_holder,
                attempted_term,
                current_term,
            } => write!(
                f,
                "fenced lease op: '{attempted_holder}' on stale term {attempted_term} \
                 is behind current term {current_term}"
            ),
        }
    }
}

impl std::error::Error for LeaseError {}

impl From<BackendError> for LeaseError {
    fn from(value: BackendError) -> Self {
        Self::Backend(value)
    }
}

struct VersionedLease {
    lease: WriterLease,
    version: BackendObjectVersion,
}

/// Wraps an `AtomicRemoteBackend` with lease primitives. The lease
/// object is stored under a deterministic key derived from
/// `database_key`; the store reads/writes that one key.
///
/// The trait bound `AtomicRemoteBackend` is the type-system version
/// of "this backend can enforce CAS" — backends that cannot
/// (Turso, D1, plain HTTP without ETag) deliberately do not
/// implement the trait, so wiring them into a `LeaseStore` becomes
/// a compile error rather than a runtime fail-closed.
pub struct LeaseStore {
    backend: Arc<dyn AtomicRemoteBackend>,
    prefix: String,
}

impl LeaseStore {
    pub fn new(backend: Arc<dyn AtomicRemoteBackend>) -> Self {
        Self {
            backend,
            prefix: "leases/".to_string(),
        }
    }

    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        let p = prefix.into();
        self.prefix = if p.ends_with('/') { p } else { format!("{p}/") };
        self
    }

    fn key_for(&self, database_key: &str) -> String {
        reddb_file::serverless_writer_lease_key(&self.prefix, database_key)
    }

    /// Read whatever lease object is currently published. `None` means
    /// no lease has ever been written for this key.
    pub fn current(&self, database_key: &str) -> Result<Option<WriterLease>, LeaseError> {
        self.read_lease(database_key)
    }

    fn read_lease(&self, database_key: &str) -> Result<Option<WriterLease>, LeaseError> {
        let key = self.key_for(database_key);
        let temp = reddb_file::ServerlessWriterLeaseTempFile::new("read");
        let downloaded = self.backend.download(&key, temp.path())?;
        if !downloaded {
            return Ok(None);
        }
        let bytes = temp
            .read_bytes()
            .map_err(|err| LeaseError::Backend(BackendError::Transport(err.to_string())))?;
        decode_writer_lease(&bytes).map(Some)
    }

    fn current_versioned(&self, database_key: &str) -> Result<Option<VersionedLease>, LeaseError> {
        let key = self.key_for(database_key);
        let before = match self.backend.object_version(&key)? {
            Some(version) => version,
            None => return Ok(None),
        };
        let temp = reddb_file::ServerlessWriterLeaseTempFile::new("read");
        let downloaded = self.backend.download(&key, temp.path())?;
        if !downloaded {
            return Ok(None);
        }
        let after = self.backend.object_version(&key)?;
        if after.as_ref() != Some(&before) {
            return Err(LeaseError::Backend(BackendError::PreconditionFailed(
                "lease object changed while being read".to_string(),
            )));
        }
        let bytes = temp
            .read_bytes()
            .map_err(|err| LeaseError::Backend(BackendError::Transport(err.to_string())))?;
        Ok(Some(VersionedLease {
            lease: decode_writer_lease(&bytes)?,
            version: before,
        }))
    }

    /// Try to acquire the lease for `database_key` on behalf of
    /// `holder_id`, valid for `ttl_ms`. Returns the `WriterLease` we
    /// own on success. Errors:
    /// - `LeaseError::Held` if a different holder owns a non-expired
    ///   lease.
    /// - `LeaseError::LostRace` if a concurrent contender beat us.
    ///
    /// Acquires under the base replication term; use
    /// [`LeaseStore::try_acquire_for_term`] to stamp a specific term and
    /// fence stale-term contenders (issue #835).
    pub fn try_acquire(
        &self,
        database_key: &str,
        holder_id: &str,
        ttl_ms: u64,
    ) -> Result<WriterLease, LeaseError> {
        self.try_acquire_for_term(
            database_key,
            holder_id,
            ttl_ms,
            crate::replication::DEFAULT_REPLICATION_TERM,
        )
    }

    /// Like [`LeaseStore::try_acquire`] but stamps `term` onto the lease
    /// and **fences** any contender whose `term` is behind the published
    /// lease's term (issue #835, ADR 0030).
    ///
    /// The term tie is what makes a deposed primary fail closed: a new
    /// primary that won a higher term takes the lease under that term, so
    /// a returning ex-primary on the old term sees a published lease whose
    /// term is *ahead* of its own and is refused with `LeaseError::Fenced`
    /// — even when the lease has since expired and would otherwise be
    /// poachable. A stale holder can never re-take the writer slot until
    /// it adopts the new term.
    pub fn try_acquire_for_term(
        &self,
        database_key: &str,
        holder_id: &str,
        ttl_ms: u64,
        term: u64,
    ) -> Result<WriterLease, LeaseError> {
        let now_ms = crate::utils::now_unix_millis();

        let current = self.current_versioned(database_key)?;
        // Term fence first — a contender behind the published term is a
        // deposed writer and fails closed regardless of expiry or holder.
        if let Some(c) = &current {
            if term < c.lease.term {
                return Err(LeaseError::Fenced {
                    attempted_holder: holder_id.to_string(),
                    attempted_term: term,
                    current_term: c.lease.term,
                });
            }
        }
        // If a healthy lease exists held by someone else, refuse
        // immediately. Two cases collapse: either the current holder
        // is us (refresh) or it's somebody else with time left.
        let next_generation = match &current {
            Some(c) if !c.lease.is_expired(now_ms) && c.lease.holder_id != holder_id => {
                return Err(LeaseError::Held {
                    current: c.lease.clone(),
                    now_ms,
                });
            }
            Some(c) => c.lease.generation.saturating_add(1),
            None => 1,
        };

        let new_lease = WriterLease {
            database_key: database_key.to_string(),
            holder_id: holder_id.to_string(),
            term,
            generation: next_generation,
            acquired_at_ms: now_ms,
            expires_at_ms: now_ms.saturating_add(ttl_ms),
        };
        let condition = match current {
            Some(c) => ConditionalPut::IfVersion(c.version),
            None => ConditionalPut::IfAbsent,
        };
        if let Err(err) = self.publish_conditional(&new_lease, condition) {
            if matches!(
                err,
                LeaseError::Backend(BackendError::PreconditionFailed(_))
            ) {
                return self.acquire_race_error(database_key, holder_id, now_ms);
            }
            return Err(err);
        }

        // Re-read and verify nobody else won the same gap.
        match self.current(database_key)? {
            Some(observed)
                if observed.holder_id == holder_id
                    && observed.generation == new_lease.generation =>
            {
                Ok(new_lease)
            }
            Some(observed) => Err(LeaseError::LostRace {
                attempted_holder: holder_id.to_string(),
                observed,
            }),
            None => Err(LeaseError::LostRace {
                attempted_holder: holder_id.to_string(),
                observed: WriterLease {
                    database_key: database_key.to_string(),
                    holder_id: "<missing>".to_string(),
                    term: 0,
                    generation: 0,
                    acquired_at_ms: 0,
                    expires_at_ms: 0,
                },
            }),
        }
    }

    fn acquire_race_error(
        &self,
        database_key: &str,
        holder_id: &str,
        now_ms: u64,
    ) -> Result<WriterLease, LeaseError> {
        match self.current(database_key)? {
            Some(observed) if !observed.is_expired(now_ms) && observed.holder_id != holder_id => {
                Err(LeaseError::Held {
                    current: observed,
                    now_ms,
                })
            }
            Some(observed) => Err(LeaseError::LostRace {
                attempted_holder: holder_id.to_string(),
                observed,
            }),
            None => Err(LeaseError::LostRace {
                attempted_holder: holder_id.to_string(),
                observed: WriterLease {
                    database_key: database_key.to_string(),
                    holder_id: "<missing>".to_string(),
                    term: 0,
                    generation: 0,
                    acquired_at_ms: 0,
                    expires_at_ms: 0,
                },
            }),
        }
    }

    /// Refresh `lease.expires_at_ms` to `now + ttl_ms`. Fails with
    /// `Stale` if the holder/generation no longer matches what's
    /// currently published. The returned lease is the new
    /// in-effect record.
    pub fn refresh(&self, lease: &WriterLease, ttl_ms: u64) -> Result<WriterLease, LeaseError> {
        let now_ms = crate::utils::now_unix_millis();
        let observed = self.current_versioned(&lease.database_key)?;
        match observed {
            Some(o)
                if o.lease.holder_id == lease.holder_id
                    && o.lease.generation == lease.generation =>
            {
                let mut next = lease.clone();
                next.expires_at_ms = now_ms.saturating_add(ttl_ms);
                if let Err(err) =
                    self.publish_conditional(&next, ConditionalPut::IfVersion(o.version))
                {
                    if matches!(
                        err,
                        LeaseError::Backend(BackendError::PreconditionFailed(_))
                    ) {
                        return Err(LeaseError::Stale {
                            attempted_holder: lease.holder_id.clone(),
                            attempted_generation: lease.generation,
                            observed: self.current(&lease.database_key)?,
                        });
                    }
                    return Err(err);
                }
                Ok(next)
            }
            other => Err(LeaseError::Stale {
                attempted_holder: lease.holder_id.clone(),
                attempted_generation: lease.generation,
                observed: other.map(|v| v.lease),
            }),
        }
    }

    /// Refresh the lease, but **fail closed** if the holder's term has
    /// fallen behind `current_term` (issue #835). This is the keep-alive
    /// counterpart to the acquire fence: a primary that was deposed while
    /// holding a live lease cannot keep extending it once the cluster has
    /// moved to a newer term — its next refresh is fenced before it ever
    /// touches the backend, so it stops mutating and drains.
    ///
    /// When the holder's own term still matches or leads `current_term`,
    /// this is exactly [`LeaseStore::refresh`].
    pub fn refresh_for_term(
        &self,
        lease: &WriterLease,
        ttl_ms: u64,
        current_term: u64,
    ) -> Result<WriterLease, LeaseError> {
        if lease.fenced_by_term(current_term) {
            return Err(LeaseError::Fenced {
                attempted_holder: lease.holder_id.clone(),
                attempted_term: lease.term,
                current_term,
            });
        }
        self.refresh(lease, ttl_ms)
    }

    /// Release the lease. Only succeeds when the published lease
    /// matches `lease.holder_id + lease.generation`. A stolen or
    /// already-replaced lease returns `Stale`.
    pub fn release(&self, lease: &WriterLease) -> Result<(), LeaseError> {
        let observed = self.current_versioned(&lease.database_key)?;
        match observed {
            Some(o)
                if o.lease.holder_id == lease.holder_id
                    && o.lease.generation == lease.generation =>
            {
                let key = self.key_for(&lease.database_key);
                if let Err(err) = self
                    .backend
                    .delete_conditional(&key, ConditionalDelete::IfVersion(o.version))
                {
                    if matches!(err, BackendError::PreconditionFailed(_)) {
                        return Err(LeaseError::Stale {
                            attempted_holder: lease.holder_id.clone(),
                            attempted_generation: lease.generation,
                            observed: self.current(&lease.database_key)?,
                        });
                    }
                    return Err(err.into());
                }
                Ok(())
            }
            other => Err(LeaseError::Stale {
                attempted_holder: lease.holder_id.clone(),
                attempted_generation: lease.generation,
                observed: other.map(|v| v.lease),
            }),
        }
    }

    fn publish_conditional(
        &self,
        lease: &WriterLease,
        condition: ConditionalPut,
    ) -> Result<BackendObjectVersion, LeaseError> {
        let key = self.key_for(&lease.database_key);
        let bytes = reddb_file::encode_serverless_writer_lease_json(lease)
            .map_err(|err| LeaseError::Backend(BackendError::Internal(err.to_string())))?;
        let temp = reddb_file::ServerlessWriterLeaseTempFile::new("write");
        temp.write_bytes(&bytes)
            .map_err(|err| LeaseError::Backend(BackendError::Transport(err.to_string())))?;
        Ok(self
            .backend
            .upload_conditional(temp.path(), &key, condition)?)
    }
}

fn decode_writer_lease(bytes: &[u8]) -> Result<WriterLease, LeaseError> {
    reddb_file::decode_serverless_writer_lease_json(bytes)
        .map_err(|err| LeaseError::InvalidFormat(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::backend::LocalBackend;
    use std::path::Path;

    fn store() -> LeaseStore {
        LeaseStore::new(Arc::new(LocalBackend)).with_prefix(format!(
            "{}/leases-test-{}",
            std::env::temp_dir().to_string_lossy(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    #[test]
    fn first_acquire_assigns_generation_one() {
        let s = store();
        let lease = s.try_acquire("db", "writer-a", 60_000).unwrap();
        assert_eq!(lease.generation, 1);
        assert_eq!(lease.holder_id, "writer-a");
    }

    #[test]
    fn second_holder_rejected_while_first_alive() {
        let s = store();
        let _ = s.try_acquire("db", "writer-a", 60_000).unwrap();
        let err = s.try_acquire("db", "writer-b", 60_000).unwrap_err();
        match err {
            LeaseError::Held { current, .. } => {
                assert_eq!(current.holder_id, "writer-a");
                assert_eq!(current.generation, 1);
            }
            other => panic!("expected Held, got {other:?}"),
        }
    }

    #[test]
    fn expired_lease_is_poachable() {
        let s = store();
        let _ = s.try_acquire("db", "writer-a", 1).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let lease = s.try_acquire("db", "writer-b", 60_000).unwrap();
        assert_eq!(lease.holder_id, "writer-b");
        assert_eq!(
            lease.generation, 2,
            "generation must increment when poaching"
        );
    }

    #[test]
    fn release_clears_so_anyone_can_take_again() {
        let s = store();
        let lease = s.try_acquire("db", "writer-a", 60_000).unwrap();
        s.release(&lease).unwrap();
        // After release the slot is empty — generation resets to 1
        // because the previous record is gone.
        let next = s.try_acquire("db", "writer-b", 60_000).unwrap();
        assert_eq!(next.holder_id, "writer-b");
        assert_eq!(next.generation, 1);
    }

    #[test]
    fn refresh_extends_expiration_for_same_holder() {
        let s = store();
        let lease = s.try_acquire("db", "writer-a", 1_000).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let refreshed = s.refresh(&lease, 60_000).unwrap();
        assert_eq!(refreshed.generation, lease.generation);
        assert!(refreshed.expires_at_ms > lease.expires_at_ms);
    }

    #[test]
    fn refresh_fails_when_someone_else_owns() {
        let s = store();
        let lease = s.try_acquire("db", "writer-a", 1).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let _ = s.try_acquire("db", "writer-b", 60_000).unwrap();
        let err = s.refresh(&lease, 60_000).unwrap_err();
        assert!(matches!(err, LeaseError::Stale { .. }));
    }

    #[test]
    fn acquire_stamps_term_onto_lease() {
        let s = store();
        let lease = s.try_acquire_for_term("db", "writer-a", 60_000, 7).unwrap();
        assert_eq!(lease.term, 7);
        assert_eq!(lease.fencing_token(), (7, 1));
    }

    #[test]
    fn legacy_lease_defaults_to_base_term() {
        // A lease acquired through the term-agnostic API carries the base
        // replication term, so it is never fenced until a termed primary
        // re-stamps it.
        let s = store();
        let lease = s.try_acquire("db", "writer-a", 60_000).unwrap();
        assert_eq!(lease.term, crate::replication::DEFAULT_REPLICATION_TERM);
        assert!(!lease.fenced_by_term(crate::replication::DEFAULT_REPLICATION_TERM));
    }

    #[test]
    fn stale_term_contender_is_fenced_even_when_lease_expired() {
        // New primary holds the lease at term 5. The lease then expires,
        // but a returning ex-primary on the old term 4 still cannot poach
        // it — the term fence refuses before expiry is even consulted.
        let s = store();
        let _new_primary = s.try_acquire_for_term("db", "new-primary", 1, 5).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let err = s
            .try_acquire_for_term("db", "ex-primary", 60_000, 4)
            .unwrap_err();
        match err {
            LeaseError::Fenced {
                attempted_term,
                current_term,
                ..
            } => {
                assert_eq!(attempted_term, 4);
                assert_eq!(current_term, 5);
            }
            other => panic!("expected Fenced, got {other:?}"),
        }
    }

    #[test]
    fn same_or_higher_term_contender_may_poach_expired_lease() {
        // The fence only bites a *behind* term. A contender at the same or
        // a higher term takes an expired lease normally, and the generation
        // advances with the handover.
        let s = store();
        let _ = s.try_acquire_for_term("db", "old", 1, 5).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let lease = s.try_acquire_for_term("db", "new", 60_000, 6).unwrap();
        assert_eq!(lease.holder_id, "new");
        assert_eq!(lease.term, 6);
        assert_eq!(lease.generation, 2, "poaching advances the generation");
    }

    #[test]
    fn refresh_for_term_fails_closed_once_term_advances() {
        // A primary holds a live lease at term 4, then the cluster moves to
        // term 5 underneath it. Its keep-alive refresh is fenced before it
        // touches the backend — the deposed holder stops mutating.
        let s = store();
        let lease = s.try_acquire_for_term("db", "deposed", 60_000, 4).unwrap();
        let err = s.refresh_for_term(&lease, 60_000, 5).unwrap_err();
        match err {
            LeaseError::Fenced {
                attempted_holder,
                attempted_term,
                current_term,
            } => {
                assert_eq!(attempted_holder, "deposed");
                assert_eq!(attempted_term, 4);
                assert_eq!(current_term, 5);
            }
            other => panic!("expected Fenced, got {other:?}"),
        }
    }

    #[test]
    fn refresh_for_term_succeeds_while_term_holds() {
        let s = store();
        let lease = s.try_acquire_for_term("db", "primary", 1_000, 5).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let refreshed = s.refresh_for_term(&lease, 60_000, 5).unwrap();
        assert_eq!(refreshed.term, 5);
        assert!(refreshed.expires_at_ms > lease.expires_at_ms);
    }

    // The legacy `acquire_fails_closed_without_backend_conditional_writes`
    // test was deleted with the trait split: `LeaseStore::new` now requires
    // `Arc<dyn AtomicRemoteBackend>`, so a non-CAS backend cannot even be
    // wired into the constructor — the test is enforced at compile time
    // (see tests/lease_atomic_http_opt_in.rs for the runtime-config branch).
}
