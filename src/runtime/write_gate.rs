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
//! Future inputs (planned but not yet wired):
//! * Serverless writer-lease state (`PLAN.md` Phase 5 / W6).

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
    read_only: std::sync::atomic::AtomicBool,
    role: ReplicationRole,
}

impl WriteGate {
    pub fn from_options(options: &RedDBOptions) -> Self {
        Self {
            read_only: std::sync::atomic::AtomicBool::new(options.read_only),
            role: options.replication.role.clone(),
        }
    }

    /// Returns `Ok(())` if the public surface is allowed to perform `kind`.
    /// Returns `RedDBError::ReadOnly` otherwise.
    ///
    /// Reasoning order is intentional: replica role first so a replica
    /// that was somehow booted with `read_only = false` still rejects.
    pub fn check(&self, kind: WriteKind) -> RedDBResult<()> {
        if matches!(self.role, ReplicationRole::Replica { .. }) {
            return Err(RedDBError::ReadOnly(format!(
                "instance is a replica — {} rejected on public surface",
                kind.label()
            )));
        }
        if self.read_only.load(std::sync::atomic::Ordering::Acquire) {
            return Err(RedDBError::ReadOnly(format!(
                "instance is configured read_only — {} rejected",
                kind.label()
            )));
        }
        Ok(())
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only.load(std::sync::atomic::Ordering::Acquire)
            || matches!(self.role, ReplicationRole::Replica { .. })
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
        self.read_only
            .swap(enabled, std::sync::atomic::Ordering::AcqRel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gate(read_only: bool, role: ReplicationRole) -> WriteGate {
        WriteGate {
            read_only: std::sync::atomic::AtomicBool::new(read_only),
            role,
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
