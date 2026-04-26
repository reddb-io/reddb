//! Public-mutation gate.
//!
//! Centralises the check that decides whether a write coming through any
//! public surface (SQL, HTTP, gRPC, PostgreSQL wire, native wire, admin
//! mutating endpoints) is allowed for this instance.
//!
//! Two inputs:
//! * `RedDBOptions::read_only` — set explicitly by operators.
//! * `ReplicationConfig::role`  — `Replica { .. }` is always read-only on
//!   public surfaces; internal logical-WAL apply (`LogicalChangeApplier`)
//!   reaches into the store directly and never crosses this gate.
//!
//! All public mutation paths consult `WriteGate::check` before dispatching
//! to storage. The replica internal apply path is the privileged surface
//! and bypasses the gate by construction.
//!
//! Serverless writer-lease state (`PLAN.md` Phase 5 / W6) is wired
//! through `LeaseGateState` — runtime flips it to `Held` after a
//! successful acquire/refresh and back to `NotHeld` when the lease is
//! lost, released, or has expired. Standalone / replica / lease-not-
//! configured deployments stay on `NotRequired` so the check is a
//! single atomic load of zero.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};

use crate::api::{RedDBError, RedDBOptions, RedDBResult};
use crate::replication::ReplicationRole;

/// Categorises the write so the rejection error can name a sensible
/// surface in operator-facing logs without leaking internal call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteKind {
    /// INSERT / UPDATE / DELETE on a user-visible collection.
    Dml,
    /// CREATE / DROP / ALTER TABLE, CREATE / DROP INDEX, etc.
    Ddl,
    /// Index build / rebuild outside a DDL statement (e.g. background reindex).
    IndexBuild,
    /// Reclaim / repair / retention sweeps that mutate state.
    Maintenance,
    /// Operator-triggered backup that mutates remote state.
    Backup,
    /// Serverless lifecycle endpoints that mutate state (attach / warmup
    /// / reclaim).
    Serverless,
}

impl WriteKind {
    fn label(self) -> &'static str {
        match self {
            WriteKind::Dml => "DML",
            WriteKind::Ddl => "DDL",
            WriteKind::IndexBuild => "index build",
            WriteKind::Maintenance => "maintenance",
            WriteKind::Backup => "backup trigger",
            WriteKind::Serverless => "serverless lifecycle",
        }
    }
}

/// Serverless writer-lease state wired through the gate.
///
/// `NotRequired` is the default — standalone, replica, and
/// lease-disabled serverless deployments all share it. `Held` /
/// `NotHeld` only matter for instances that opted into lease-fenced
/// writes; the lease loop flips the value as it acquires / refreshes /
/// loses the slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum LeaseGateState {
    NotRequired = 0,
    Held = 1,
    NotHeld = 2,
}

impl LeaseGateState {
    fn from_u8(raw: u8) -> Self {
        match raw {
            1 => Self::Held,
            2 => Self::NotHeld,
            _ => Self::NotRequired,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::NotRequired => "not_required",
            Self::Held => "held",
            Self::NotHeld => "not_held",
        }
    }
}

/// Live policy for public-mutation surfaces.
///
/// `read_only` was originally a `bool` snapshot taken at runtime
/// construction. PLAN.md Phase 4.3 promotes it to an `AtomicBool` so
/// `POST /admin/readonly` can flip the policy without a restart. The
/// `ReplicationRole` stays immutable — flipping a replica into a
/// primary mid-process would need a full handshake (Phase 3 work in
/// the data-safety plan), and shouldn't be a single-flag decision.
#[derive(Debug)]
pub struct WriteGate {
    read_only: AtomicBool,
    role: ReplicationRole,
    lease: AtomicU8,
}

impl WriteGate {
    pub fn from_options(options: &RedDBOptions) -> Self {
        Self {
            read_only: AtomicBool::new(options.read_only),
            role: options.replication.role.clone(),
            lease: AtomicU8::new(LeaseGateState::NotRequired as u8),
        }
    }

    /// Returns `Ok(())` if the public surface is allowed to perform `kind`.
    /// Returns `RedDBError::ReadOnly` otherwise.
    ///
    /// Reasoning order is intentional:
    /// 1. Replica role — a replica booted with `read_only = false`
    ///    must still reject; this is a structural property.
    /// 2. Lease lost — the strongest serverless correctness signal.
    ///    A writer that lost its lease must stop *immediately*; running
    ///    while another holder has been promoted causes split-brain.
    /// 3. Operator read-only flag — explicit /admin/readonly toggle
    ///    or boot-time pin; lower priority than lease loss because the
    ///    operator can revoke it without external coordination.
    pub fn check(&self, kind: WriteKind) -> RedDBResult<()> {
        if matches!(self.role, ReplicationRole::Replica { .. }) {
            return Err(RedDBError::ReadOnly(format!(
                "instance is a replica — {} rejected on public surface",
                kind.label()
            )));
        }
        if matches!(self.lease_state(), LeaseGateState::NotHeld) {
            return Err(RedDBError::ReadOnly(format!(
                "writer lease not held — {} rejected (serverless fence)",
                kind.label()
            )));
        }
        if self.read_only.load(Ordering::Acquire) {
            return Err(RedDBError::ReadOnly(format!(
                "instance is configured read_only — {} rejected",
                kind.label()
            )));
        }
        Ok(())
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only.load(Ordering::Acquire)
            || matches!(self.role, ReplicationRole::Replica { .. })
            || matches!(self.lease_state(), LeaseGateState::NotHeld)
    }

    pub fn role(&self) -> &ReplicationRole {
        &self.role
    }

    /// PLAN.md Phase 4.3 — dynamic read-only toggle. Flipping a
    /// replica back to writable here is a no-op for `check()` because
    /// the role check fires first; the operator must change the
    /// replication role through a separate, audited path.
    ///
    /// Returns the previous read_only value so callers can detect
    /// idempotent calls (toggle to the same value = no work to do).
    pub fn set_read_only(&self, enabled: bool) -> bool {
        self.read_only.swap(enabled, Ordering::AcqRel)
    }

    /// Current writer-lease gate state. `NotRequired` for standalone,
    /// replica, and lease-disabled serverless instances.
    pub fn lease_state(&self) -> LeaseGateState {
        LeaseGateState::from_u8(self.lease.load(Ordering::Acquire))
    }

    /// Flip the lease gate state. Only `LeaseLifecycle` should call
    /// this — other callers must go through the lifecycle so the
    /// gate flip and the corresponding `lease/*` audit record
    /// stay paired.
    ///
    /// Returns the previous state so the caller can detect idempotent
    /// transitions and avoid spamming audit / metrics.
    pub(crate) fn set_lease_state(&self, state: LeaseGateState) -> LeaseGateState {
        LeaseGateState::from_u8(self.lease.swap(state as u8, Ordering::AcqRel))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(read_only: bool, role: ReplicationRole) -> WriteGate {
        WriteGate {
            read_only: AtomicBool::new(read_only),
            role,
            lease: AtomicU8::new(LeaseGateState::NotRequired as u8),
        }
    }

    #[test]
    fn standalone_allows_writes() {
        let g = gate(false, ReplicationRole::Standalone);
        assert!(g.check(WriteKind::Dml).is_ok());
        assert!(g.check(WriteKind::Ddl).is_ok());
        assert!(!g.is_read_only());
    }

    #[test]
    fn primary_allows_writes() {
        let g = gate(false, ReplicationRole::Primary);
        assert!(g.check(WriteKind::Dml).is_ok());
        assert!(!g.is_read_only());
    }

    #[test]
    fn replica_rejects_every_kind() {
        let g = gate(
            true,
            ReplicationRole::Replica {
                primary_addr: "http://primary:50051".into(),
            },
        );
        for kind in [
            WriteKind::Dml,
            WriteKind::Ddl,
            WriteKind::IndexBuild,
            WriteKind::Maintenance,
            WriteKind::Backup,
            WriteKind::Serverless,
        ] {
            let err = g.check(kind).unwrap_err();
            match err {
                RedDBError::ReadOnly(msg) => assert!(msg.contains("replica")),
                other => panic!("expected ReadOnly, got {other:?}"),
            }
        }
        assert!(g.is_read_only());
    }

    #[test]
    fn read_only_flag_rejects_writes_on_standalone() {
        let g = gate(true, ReplicationRole::Standalone);
        let err = g.check(WriteKind::Dml).unwrap_err();
        match err {
            RedDBError::ReadOnly(msg) => assert!(msg.contains("read_only")),
            other => panic!("expected ReadOnly, got {other:?}"),
        }
    }

    #[test]
    fn lease_not_held_rejects_writes_on_primary() {
        let g = gate(false, ReplicationRole::Primary);
        g.set_lease_state(LeaseGateState::NotHeld);
        let err = g.check(WriteKind::Dml).unwrap_err();
        match err {
            RedDBError::ReadOnly(msg) => assert!(msg.contains("lease")),
            other => panic!("expected ReadOnly, got {other:?}"),
        }
        assert!(g.is_read_only());
    }

    #[test]
    fn lease_held_allows_writes_on_primary() {
        let g = gate(false, ReplicationRole::Primary);
        g.set_lease_state(LeaseGateState::Held);
        assert!(g.check(WriteKind::Dml).is_ok());
        assert!(!g.is_read_only());
    }

    #[test]
    fn lease_state_transitions_return_previous() {
        let g = gate(false, ReplicationRole::Primary);
        assert_eq!(
            g.set_lease_state(LeaseGateState::Held),
            LeaseGateState::NotRequired
        );
        assert_eq!(
            g.set_lease_state(LeaseGateState::NotHeld),
            LeaseGateState::Held
        );
        assert_eq!(g.lease_state(), LeaseGateState::NotHeld);
    }

    #[test]
    fn lease_loss_overrides_writable_read_only_flag() {
        // Even with read_only=false, losing the lease must reject.
        let g = gate(false, ReplicationRole::Primary);
        g.set_lease_state(LeaseGateState::NotHeld);
        let err = g.check(WriteKind::Ddl).unwrap_err();
        match err {
            RedDBError::ReadOnly(msg) => assert!(msg.contains("lease")),
            other => panic!("expected ReadOnly, got {other:?}"),
        }
    }

    #[test]
    fn replica_role_overrides_missing_read_only_flag() {
        let g = gate(
            false,
            ReplicationRole::Replica {
                primary_addr: "http://primary:50051".into(),
            },
        );
        let err = g.check(WriteKind::Dml).unwrap_err();
        match err {
            RedDBError::ReadOnly(msg) => assert!(msg.contains("replica")),
            other => panic!("expected ReadOnly, got {other:?}"),
        }
    }
}
