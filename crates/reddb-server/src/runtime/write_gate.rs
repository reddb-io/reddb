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

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

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
    /// Operator-set read-only flag. Mutated by `POST /admin/readonly`
    /// and by boot-time resolution (CLI/env/persisted state). Sticky:
    /// the archive-lag auto-pause path (#519) never touches this — only
    /// the operator can clear an operator-set pin.
    read_only: AtomicBool,
    role: ReplicationRole,
    lease: AtomicU8,
    /// Issue #519 — engine-managed graceful read-only triggered by
    /// `REDDB_BACKUP_PAUSE_ON_LAG_SECS` when WAL archive lag exceeds
    /// the threshold. Independent of `read_only` so the two precedences
    /// (manual sticky, auto auto-resumes) cannot stomp each other.
    auto_paused: AtomicBool,
    /// Unix-ms timestamp of the last *successful* remote archive. `0`
    /// means "never observed since boot"; the lag evaluator treats
    /// that as "lag since boot" once it has been initialised by the
    /// caller (typical pattern: stamp `now` at construction so the
    /// instance gets a `threshold_secs` grace window).
    last_archive_at_ms: AtomicU64,
    /// Threshold from `REDDB_BACKUP_PAUSE_ON_LAG_SECS`. `0` = feature
    /// disabled — `evaluate_archive_lag` short-circuits without
    /// touching `auto_paused`.
    pause_threshold_secs: AtomicU64,
}

impl WriteGate {
    pub fn from_options(options: &RedDBOptions) -> Self {
        Self {
            read_only: AtomicBool::new(options.read_only),
            role: options.replication.role.clone(),
            lease: AtomicU8::new(LeaseGateState::NotRequired as u8),
            auto_paused: AtomicBool::new(false),
            last_archive_at_ms: AtomicU64::new(0),
            pause_threshold_secs: AtomicU64::new(0),
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
        self.check_consent(kind).map(|_| ())
    }

    /// Same as `check` but on success returns a sealed
    /// `WriteConsent` token. Mutating port methods that take
    /// `&OperationContext` demand `ctx.write_consent.is_some()`;
    /// the only way to mint such a token is to call this method,
    /// so forgetting to consult the gate becomes a structural
    /// property — not a discipline question.
    pub fn check_consent(&self, kind: WriteKind) -> RedDBResult<crate::application::WriteConsent> {
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
        if self.auto_paused.load(Ordering::Acquire) {
            return Err(RedDBError::ReadOnly(format!(
                "instance is paused — WAL archive lag exceeded threshold — {} rejected",
                kind.label()
            )));
        }
        Ok(crate::application::WriteConsent {
            kind,
            _seal: crate::application::WriteConsentSeal::new(),
        })
    }

    pub fn is_read_only(&self) -> bool {
        self.read_only.load(Ordering::Acquire)
            || self.auto_paused.load(Ordering::Acquire)
            || matches!(self.role, ReplicationRole::Replica { .. })
            || matches!(self.lease_state(), LeaseGateState::NotHeld)
    }

    /// Whether the operator explicitly pinned this instance read-only
    /// (via boot config or `POST /admin/readonly`). Distinct from
    /// [`is_read_only`] which also returns `true` for structural
    /// reasons (replica role, lease lost, archive-lag pause).
    pub fn is_manual_read_only(&self) -> bool {
        self.read_only.load(Ordering::Acquire)
    }

    /// Whether the engine-managed archive-lag pause (#519) is
    /// currently active. Mutually independent of [`is_manual_read_only`]
    /// so callers like `/backup/status` can report both.
    pub fn is_auto_paused(&self) -> bool {
        self.auto_paused.load(Ordering::Acquire)
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

    /// Issue #519 — install the archive-lag pause threshold and the
    /// baseline "last archive observed at" stamp. Threshold `0`
    /// disables auto-pause; subsequent `record_archive_success` /
    /// `evaluate_archive_lag` calls then become no-ops.
    ///
    /// Idempotent: callers should invoke once during startup after
    /// parsing `REDDB_BACKUP_PAUSE_ON_LAG_SECS`. Stamping `last_archive_at_ms`
    /// to "now" at construction grants a `threshold_secs` grace
    /// window before the first auto-pause can fire — without it, a
    /// freshly-booted instance with a never-archived WAL would flip
    /// to read-only on the first poll.
    pub fn configure_archive_lag_pause(&self, threshold_secs: u64, baseline_ms: u64) {
        self.pause_threshold_secs
            .store(threshold_secs, Ordering::Release);
        self.last_archive_at_ms
            .store(baseline_ms, Ordering::Release);
    }

    /// Stamp `last_archive_at_ms` after a successful remote archive.
    /// Called by the WAL-archive task wrapper in `service_cli` after
    /// `runtime.trigger_backup()` returns `Ok`.
    pub fn record_archive_success(&self, now_ms: u64) {
        self.last_archive_at_ms.store(now_ms, Ordering::Release);
    }

    /// Current archive-lag threshold in seconds. `0` means the
    /// feature is disabled.
    pub fn archive_pause_threshold_secs(&self) -> u64 {
        self.pause_threshold_secs.load(Ordering::Acquire)
    }

    /// Last archive observation timestamp (unix ms).
    pub fn last_archive_at_ms(&self) -> u64 {
        self.last_archive_at_ms.load(Ordering::Acquire)
    }

    /// Re-evaluate the archive-lag state. Returns the resulting
    /// `auto_paused` value.
    ///
    /// Semantics (issue #519):
    /// * Threshold `0` → feature disabled, returns current state
    ///   without writing.
    /// * Manual read-only is **sticky** — when [`is_manual_read_only`]
    ///   is true, this method never modifies `auto_paused`. The
    ///   operator must clear the manual pin first; only then does the
    ///   auto-path take over again on the next tick.
    /// * Lag > threshold and manual=false → set `auto_paused = true`.
    /// * Lag <= threshold and `auto_paused = true` → clear it
    ///   (auto-resume). If `auto_paused` was already false, no-op.
    pub fn evaluate_archive_lag(&self, now_ms: u64) -> bool {
        let threshold = self.pause_threshold_secs.load(Ordering::Acquire);
        if threshold == 0 {
            return self.auto_paused.load(Ordering::Acquire);
        }
        if self.read_only.load(Ordering::Acquire) {
            return self.auto_paused.load(Ordering::Acquire);
        }
        let last_ms = self.last_archive_at_ms.load(Ordering::Acquire);
        let lag_secs = now_ms.saturating_sub(last_ms) / 1000;
        let should_pause = lag_secs > threshold;
        self.auto_paused.store(should_pause, Ordering::Release);
        should_pause
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
            auto_paused: AtomicBool::new(false),
            last_archive_at_ms: AtomicU64::new(0),
            pause_threshold_secs: AtomicU64::new(0),
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

    // ---------------------------------------------------------------
    // Issue #519 — graceful read-only mode when WAL archive lag
    // exceeds REDDB_BACKUP_PAUSE_ON_LAG_SECS.
    // ---------------------------------------------------------------

    #[test]
    fn archive_lag_disabled_threshold_is_noop() {
        let g = gate(false, ReplicationRole::Standalone);
        g.configure_archive_lag_pause(0, 1_000);
        // Even with an ancient timestamp, threshold=0 must not pause.
        assert!(!g.evaluate_archive_lag(10_000_000_000));
        assert!(!g.is_auto_paused());
        assert!(g.check(WriteKind::Dml).is_ok());
    }

    #[test]
    fn archive_lag_triggers_auto_pause_past_threshold() {
        let g = gate(false, ReplicationRole::Standalone);
        // Last archive at t=1_000_000ms; threshold = 60s.
        g.configure_archive_lag_pause(60, 1_000_000);
        // 30s later — still under threshold.
        assert!(!g.evaluate_archive_lag(1_000_000 + 30_000));
        assert!(g.check(WriteKind::Dml).is_ok());

        // 120s later — over threshold; must auto-pause.
        assert!(g.evaluate_archive_lag(1_000_000 + 120_000));
        assert!(g.is_auto_paused());
        let err = g.check(WriteKind::Dml).unwrap_err();
        match err {
            RedDBError::ReadOnly(msg) => assert!(msg.contains("WAL archive lag"), "{msg}"),
            other => panic!("expected ReadOnly, got {other:?}"),
        }
        assert!(g.is_read_only());
    }

    #[test]
    fn archive_lag_auto_resume_after_recovery() {
        let g = gate(false, ReplicationRole::Standalone);
        g.configure_archive_lag_pause(60, 1_000_000);
        // Trip the auto-pause.
        assert!(g.evaluate_archive_lag(1_000_000 + 120_000));
        assert!(g.is_auto_paused());
        // Archiver catches up — stamp success and re-evaluate.
        g.record_archive_success(1_000_000 + 130_000);
        assert!(!g.evaluate_archive_lag(1_000_000 + 130_000));
        assert!(!g.is_auto_paused());
        assert!(g.check(WriteKind::Dml).is_ok());
    }

    #[test]
    fn manual_read_only_blocks_auto_pause_writes_and_is_sticky() {
        // Operator pinned read-only *before* lag condition. The
        // auto-pause path must be a no-op while manual is set, and
        // archive recovery must NOT auto-clear the manual pin.
        let g = gate(true, ReplicationRole::Standalone);
        g.configure_archive_lag_pause(60, 1_000_000);

        // Lag past threshold; but manual is set so auto stays false.
        assert!(!g.evaluate_archive_lag(1_000_000 + 120_000));
        assert!(!g.is_auto_paused());
        assert!(g.is_manual_read_only());
        // Writes still rejected — for the manual reason.
        let err = g.check(WriteKind::Dml).unwrap_err();
        match err {
            RedDBError::ReadOnly(msg) => {
                assert!(msg.contains("read_only"), "{msg}");
                assert!(!msg.contains("WAL archive lag"), "{msg}");
            }
            other => panic!("expected ReadOnly, got {other:?}"),
        }

        // Archiver recovers; re-evaluate. Manual still set ⇒ auto stays false,
        // manual stays true ⇒ instance stays read-only by operator intent.
        g.record_archive_success(1_000_000 + 130_000);
        assert!(!g.evaluate_archive_lag(1_000_000 + 130_000));
        assert!(g.is_manual_read_only(), "manual must stay set");
        assert!(!g.is_auto_paused());
        assert!(g.check(WriteKind::Dml).is_err());
    }

    #[test]
    fn manual_clearing_resumes_auto_evaluation() {
        // Manual was set; operator clears it; lag is still bad.
        // Next evaluation must auto-pause.
        let g = gate(true, ReplicationRole::Standalone);
        g.configure_archive_lag_pause(60, 1_000_000);
        // No-op while manual.
        assert!(!g.evaluate_archive_lag(1_000_000 + 120_000));
        // Operator unsets manual.
        g.set_read_only(false);
        // Now the lag condition must fire.
        assert!(g.evaluate_archive_lag(1_000_000 + 120_000));
        assert!(g.is_auto_paused());
    }

    #[test]
    fn archive_lag_pause_state_independent_from_manual_flag() {
        let g = gate(false, ReplicationRole::Standalone);
        g.configure_archive_lag_pause(60, 1_000_000);
        assert!(g.evaluate_archive_lag(1_000_000 + 120_000));
        // Operator separately pins manual on top; still both true.
        let prev = g.set_read_only(true);
        assert!(!prev);
        assert!(g.is_manual_read_only());
        assert!(g.is_auto_paused());
        // Operator clears manual; auto pause survives.
        g.set_read_only(false);
        assert!(g.is_auto_paused());
        assert!(g.check(WriteKind::Dml).is_err());
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
