//! Serverless writer lease (PLAN.md Phase 5 / W6).
//!
//! Multiple RedDB instances can attach to the same remote-backed
//! database key. To prevent two of them from concurrently mutating the
//! same remote artifacts (snapshots, WAL segments, head manifest),
//! exactly one of them must hold a *writer lease*. Other instances may
//! still attach as read-only replicas without acquiring a lease.
//!
//! ## Wire format
//!
//! The lease is serialized as JSON under
//! `leases/{database_key}.lease.json` on the remote backend:
//!
//! ```json
//! {
//!   "database_key": "main",
//!   "holder_id": "instance-uuid",
//!   "generation": 7,
//!   "acquired_at_ms": 1730000000000,
//!   "expires_at_ms":  1730000060000
//! }
//! ```
//!
//! - `generation` increments every time a different holder acquires
//!   the key. The holder stamps its uploads with the generation so a
//!   stale writer (whose lease was poached because it expired) can be
//!   detected during reclaim by reading the current lease and
//!   comparing.
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

use crate::serde_json::{self, Value as JsonValue};
use crate::storage::backend::{
    BackendError, BackendObjectVersion, ConditionalDelete, ConditionalPut, RemoteBackend,
};
use serde_json::Map;

/// One snapshot of who owns the writer lease for a database key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriterLease {
    pub database_key: String,
    pub holder_id: String,
    pub generation: u64,
    pub acquired_at_ms: u64,
    pub expires_at_ms: u64,
}

impl WriterLease {
    pub fn is_expired(&self, now_ms: u64) -> bool {
        self.expires_at_ms <= now_ms
    }

    fn to_json(&self) -> JsonValue {
        let mut object = Map::new();
        object.insert(
            "database_key".to_string(),
            JsonValue::String(self.database_key.clone()),
        );
        object.insert(
            "holder_id".to_string(),
            JsonValue::String(self.holder_id.clone()),
        );
        object.insert(
            "generation".to_string(),
            JsonValue::Number(self.generation as f64),
        );
        object.insert(
            "acquired_at_ms".to_string(),
            JsonValue::Number(self.acquired_at_ms as f64),
        );
        object.insert(
            "expires_at_ms".to_string(),
            JsonValue::Number(self.expires_at_ms as f64),
        );
        JsonValue::Object(object)
    }

    fn from_json(value: &JsonValue) -> Result<Self, LeaseError> {
        let obj = value
            .as_object()
            .ok_or_else(|| LeaseError::InvalidFormat("lease json is not an object".into()))?;
        Ok(Self {
            database_key: obj
                .get("database_key")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| LeaseError::InvalidFormat("missing database_key".into()))?
                .to_string(),
            holder_id: obj
                .get("holder_id")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| LeaseError::InvalidFormat("missing holder_id".into()))?
                .to_string(),
            generation: obj
                .get("generation")
                .and_then(JsonValue::as_u64)
                .ok_or_else(|| LeaseError::InvalidFormat("missing generation".into()))?,
            acquired_at_ms: obj
                .get("acquired_at_ms")
                .and_then(JsonValue::as_u64)
                .ok_or_else(|| LeaseError::InvalidFormat("missing acquired_at_ms".into()))?,
            expires_at_ms: obj
                .get("expires_at_ms")
                .and_then(JsonValue::as_u64)
                .ok_or_else(|| LeaseError::InvalidFormat("missing expires_at_ms".into()))?,
        })
    }
}

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

/// Wraps a `RemoteBackend` with lease primitives. The lease object is
/// stored under a deterministic key derived from `database_key`; the
/// store reads/writes that one key.
///
/// Mutating operations use backend-native conditional writes. A
/// backend that cannot expose object versions is rejected by
/// `try_acquire`, `refresh`, and `release`.
pub struct LeaseStore {
    backend: Arc<dyn RemoteBackend>,
    prefix: String,
}

impl LeaseStore {
    pub fn new(backend: Arc<dyn RemoteBackend>) -> Self {
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
        format!("{}{}.lease.json", self.prefix, database_key)
    }

    fn ensure_conditional_backend(&self) -> Result<(), LeaseError> {
        if self.backend.supports_conditional_writes() {
            return Ok(());
        }
        Err(LeaseError::Backend(BackendError::Config(format!(
            "writer lease requires a backend with conditional writes; '{}' is not eligible",
            self.backend.name()
        ))))
    }

    /// Read whatever lease object is currently published. `None` means
    /// no lease has ever been written for this key.
    pub fn current(&self, database_key: &str) -> Result<Option<WriterLease>, LeaseError> {
        self.read_lease(database_key)
    }

    fn read_lease(&self, database_key: &str) -> Result<Option<WriterLease>, LeaseError> {
        let key = self.key_for(database_key);
        let temp = std::env::temp_dir().join(format!(
            "reddb-lease-read-{}-{}.json",
            std::process::id(),
            crate::utils::now_unix_nanos()
        ));
        let downloaded = self.backend.download(&key, &temp)?;
        if !downloaded {
            return Ok(None);
        }
        let bytes = std::fs::read(&temp)
            .map_err(|err| LeaseError::Backend(BackendError::Transport(err.to_string())))?;
        let _ = std::fs::remove_file(&temp);
        let json: JsonValue = serde_json::from_slice(&bytes)
            .map_err(|err| LeaseError::InvalidFormat(format!("lease json parse: {err}")))?;
        WriterLease::from_json(&json).map(Some)
    }

    fn current_versioned(&self, database_key: &str) -> Result<Option<VersionedLease>, LeaseError> {
        let key = self.key_for(database_key);
        let before = match self.backend.object_version(&key)? {
            Some(version) => version,
            None => return Ok(None),
        };
        let temp = std::env::temp_dir().join(format!(
            "reddb-lease-read-{}-{}.json",
            std::process::id(),
            crate::utils::now_unix_nanos()
        ));
        let downloaded = self.backend.download(&key, &temp)?;
        if !downloaded {
            return Ok(None);
        }
        let after = self.backend.object_version(&key)?;
        if after.as_ref() != Some(&before) {
            let _ = std::fs::remove_file(&temp);
            return Err(LeaseError::Backend(BackendError::PreconditionFailed(
                "lease object changed while being read".to_string(),
            )));
        }
        let bytes = std::fs::read(&temp)
            .map_err(|err| LeaseError::Backend(BackendError::Transport(err.to_string())))?;
        let _ = std::fs::remove_file(&temp);
        let json: JsonValue = serde_json::from_slice(&bytes)
            .map_err(|err| LeaseError::InvalidFormat(format!("lease json parse: {err}")))?;
        Ok(Some(VersionedLease {
            lease: WriterLease::from_json(&json)?,
            version: before,
        }))
    }

    /// Try to acquire the lease for `database_key` on behalf of
    /// `holder_id`, valid for `ttl_ms`. Returns the `WriterLease` we
    /// own on success. Errors:
    /// - `LeaseError::Held` if a different holder owns a non-expired
    ///   lease.
    /// - `LeaseError::LostRace` if a concurrent contender beat us.
    pub fn try_acquire(
        &self,
        database_key: &str,
        holder_id: &str,
        ttl_ms: u64,
    ) -> Result<WriterLease, LeaseError> {
        self.ensure_conditional_backend()?;
        let now_ms = crate::utils::now_unix_millis();

        let current = self.current_versioned(database_key)?;
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
        self.ensure_conditional_backend()?;
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

    /// Release the lease. Only succeeds when the published lease
    /// matches `lease.holder_id + lease.generation`. A stolen or
    /// already-replaced lease returns `Stale`.
    pub fn release(&self, lease: &WriterLease) -> Result<(), LeaseError> {
        self.ensure_conditional_backend()?;
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
        let json = lease.to_json();
        let bytes = serde_json::to_vec(&json).map_err(|err| {
            LeaseError::Backend(BackendError::Internal(format!("serialize lease: {err}")))
        })?;
        let temp = std::env::temp_dir().join(format!(
            "reddb-lease-write-{}-{}.json",
            std::process::id(),
            crate::utils::now_unix_nanos()
        ));
        std::fs::write(&temp, &bytes)
            .map_err(|err| LeaseError::Backend(BackendError::Transport(err.to_string())))?;
        let res = self.backend.upload_conditional(&temp, &key, condition);
        let _ = std::fs::remove_file(&temp);
        Ok(res?)
    }
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

    struct UnsupportedBackend;

    impl RemoteBackend for UnsupportedBackend {
        fn name(&self) -> &str {
            "unsupported"
        }

        fn download(&self, _remote_key: &str, _local_path: &Path) -> Result<bool, BackendError> {
            Ok(false)
        }

        fn upload(&self, _local_path: &Path, _remote_key: &str) -> Result<(), BackendError> {
            Ok(())
        }

        fn exists(&self, _remote_key: &str) -> Result<bool, BackendError> {
            Ok(false)
        }

        fn delete(&self, _remote_key: &str) -> Result<(), BackendError> {
            Ok(())
        }

        fn list(&self, _prefix: &str) -> Result<Vec<String>, BackendError> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn acquire_fails_closed_without_backend_conditional_writes() {
        let s = LeaseStore::new(Arc::new(UnsupportedBackend));
        let err = s.try_acquire("db", "writer-a", 60_000).unwrap_err();
        match err {
            LeaseError::Backend(BackendError::Config(msg)) => {
                assert!(msg.contains("conditional writes"));
            }
            other => panic!("expected backend config error, got {other:?}"),
        }
    }
}
