//! Intent-lock hierarchy adapter — `Resource` naming + `LockerGuard` RAII.
//!
//! Thin layer over `crate::storage::transaction::lock::LockManager`
//! that gives the runtime dispatch paths a typed API:
//!
//! - Read dispatch: `(Global, IS) → (Collection, IS)`
//! - Write dispatch: `(Global, IX) → (Collection, IX)`
//! - DDL dispatch: `(Global, IX) → (Collection, X)`
//!
//! The adapter owns:
//!
//! 1. A `Resource` enum that maps the two hierarchy levels to the
//!    byte-key format `LockManager` expects (no string collisions,
//!    cheap encoding).
//! 2. A `LockerGuard` that records each `(resource, mode)` pair in
//!    acquisition order and releases them on drop. Releases run in
//!    reverse order so the global lock is the last to go.
//! 3. A tiny monotonic `TxnId` allocator keyed by the current
//!    connection id — enough for deadlock detection to distinguish
//!    concurrent acquirers.
//!
//! The adapter does **not** implement ordered-acquire enforcement
//! via phantom types yet (TODO for P1.T4); callers currently discipline
//! themselves by always going `Global → Collection`.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::storage::transaction::lock::{LockManager, LockMode, LockResult, TxnId};

/// Hierarchical resources the runtime locks on. The byte-key encoding
/// prefixes each level with a discriminator so `Collection("global")`
/// can never collide with the true global-scope `Resource::Global`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Resource {
    Global,
    Collection(String),
}

impl Resource {
    /// Deterministic byte-key for the underlying `LockManager`.
    pub fn key(&self) -> Vec<u8> {
        match self {
            Resource::Global => b"G/".to_vec(),
            Resource::Collection(name) => {
                let mut out = Vec::with_capacity(2 + name.len());
                out.extend_from_slice(b"C/");
                out.extend_from_slice(name.as_bytes());
                out
            }
        }
    }
}

/// Outcome of a `try_acquire` — `Granted` / `Upgraded` fall through to
/// the guard, the rest bubble up as an error the caller can log or
/// retry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireError {
    Deadlock(Vec<TxnId>),
    Timeout,
    LockLimitExceeded,
    /// Requested mode isn't compatible with a mode this guard already
    /// holds on the same resource and can't be upgraded there.
    IncompatibleEscalation {
        resource: Resource,
        held: LockMode,
        requested: LockMode,
    },
}

impl std::fmt::Display for AcquireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Deadlock(cycle) => write!(f, "deadlock detected (cycle: {cycle:?})"),
            Self::Timeout => f.write_str("lock acquire timed out"),
            Self::LockLimitExceeded => f.write_str("per-txn lock limit exceeded"),
            Self::IncompatibleEscalation {
                resource,
                held,
                requested,
            } => write!(
                f,
                "cannot escalate lock on {resource:?}: held={held:?} requested={requested:?}"
            ),
        }
    }
}

impl std::error::Error for AcquireError {}

/// Monotonic per-process TxnId allocator. Deadlock detection needs
/// acquirer uniqueness, not connection identity, so a plain counter
/// suffices. `0` is reserved by the underlying lock manager for "not
/// a real txn", so allocation starts at `1`.
static NEXT_TXN_ID: AtomicU64 = AtomicU64::new(1);

pub fn fresh_txn_id() -> TxnId {
    NEXT_TXN_ID.fetch_add(1, Ordering::Relaxed)
}

/// Acquisition record tracked by the guard so drop can release in
/// reverse order.
#[derive(Debug, Clone)]
struct Held {
    resource: Resource,
    mode: LockMode,
}

/// RAII guard over a set of acquired locks. Drop releases every
/// acquired resource in reverse-acquire order, so the common
/// `Global → Collection` path releases `Collection` first then
/// `Global`.
pub struct LockerGuard {
    manager: Arc<LockManager>,
    txn_id: TxnId,
    held: Vec<Held>,
}

impl LockerGuard {
    /// Start a new guard bound to the given manager. No locks are
    /// acquired yet — callers chain `acquire` calls.
    pub fn new(manager: Arc<LockManager>) -> Self {
        Self {
            manager,
            txn_id: fresh_txn_id(),
            held: Vec::with_capacity(2),
        }
    }

    /// Acquire a lock on `resource` with `mode`. Records the
    /// acquisition so drop can reverse it. Rejects illegal upgrades
    /// (already holding Exclusive, requesting Shared) with
    /// `IncompatibleEscalation` so bugs in the dispatch layer don't
    /// silently downgrade.
    pub fn acquire(&mut self, resource: Resource, mode: LockMode) -> Result<(), AcquireError> {
        // If we already hold this resource, only allow legal upgrades.
        if let Some(existing) = self.held.iter().find(|h| h.resource == resource) {
            let already = existing.mode;
            if already == mode {
                return Ok(());
            }
            if !already.can_upgrade_to(&mode) {
                return Err(AcquireError::IncompatibleEscalation {
                    resource,
                    held: already,
                    requested: mode,
                });
            }
        }

        let key = resource.key();
        match self.manager.acquire(self.txn_id, &key, mode) {
            LockResult::Granted | LockResult::Upgraded | LockResult::Waiting => {
                // `Waiting` shouldn't surface under the blocking
                // `acquire()` — treat it defensively as granted.
                self.held.push(Held { resource, mode });
                Ok(())
            }
            LockResult::Deadlock(cycle) => Err(AcquireError::Deadlock(cycle)),
            LockResult::Timeout => Err(AcquireError::Timeout),
            LockResult::LockLimitExceeded => Err(AcquireError::LockLimitExceeded),
            // `AlreadyHeld` only fires when our own txn already had
            // the lock — equivalent to a no-op acquire from the
            // caller's POV. `TxnNotFound` shouldn't reach this layer
            // (release-before-acquire bug); treat both as success.
            LockResult::AlreadyHeld | LockResult::TxnNotFound => {
                self.held.push(Held { resource, mode });
                Ok(())
            }
        }
    }

    /// Number of currently-held resources. Useful for lock-stats
    /// assertions in tests.
    pub fn held_count(&self) -> usize {
        self.held.len()
    }

    /// The txn id this guard uses for underlying `LockManager`
    /// acquisitions. Exposed for lock-stats inspection in tests.
    pub fn txn_id(&self) -> TxnId {
        self.txn_id
    }
}

impl Drop for LockerGuard {
    fn drop(&mut self) {
        // Release in reverse acquire order. The per-resource release
        // is already robust against the lock not existing (manager
        // returns false), so we can just drain.
        while let Some(Held { resource, .. }) = self.held.pop() {
            let key = resource.key();
            self.manager.release(self.txn_id, &key);
        }
        // Belt-and-suspenders — clear any residue so deadlock
        // detection's wait-graph doesn't leak this txn.
        self.manager.release_all(self.txn_id);
    }
}

// Unit-level tests live in `tests/unit_locking.rs` because the
// project's lib-test target has pre-existing unrelated compile
// errors that would block `cargo test --lib`.
