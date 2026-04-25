//! Process lifecycle state machine (PLAN.md Phase 1 — Lifecycle Contract).
//!
//! Tracks where the runtime is in its boot/serve/shutdown sequence so
//! that:
//! * health probes can answer `/health/live`, `/health/ready`,
//!   `/health/startup` deterministically (live: process responsive,
//!   ready: accepting queries, startup: K8s-style "still warming up");
//! * `WriteGate` can reject mutations once shutdown is initiated;
//! * `POST /admin/shutdown` is idempotent — subsequent calls return
//!   the same successful state without re-running the flush pipeline;
//! * orchestrators see a consistent transition pattern regardless of
//!   how shutdown was triggered (HTTP, SIGTERM, SIGINT).
//!
//! The state is a single `AtomicU8` in `RuntimeInner` plus a
//! `parking_lot::RwLock<ShutdownReport>` for the optional final
//! report. Phase transitions are monotonic — the runtime cannot move
//! backwards through them.

use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use parking_lot::RwLock;

/// Discrete lifecycle phases. Numeric values are monotonic so an
/// `AtomicU8` `compare_exchange` gives us atomic transitions without
/// a mutex. Decoded via `Phase::from_u8`.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    /// Engine is opening: WAL replay, restore-from-remote, initial
    /// checkpoint. Reads and writes are not yet served.
    Starting = 0,
    /// Engine is fully ready: every public surface accepts traffic.
    Ready = 1,
    /// `/admin/drain` was called or `/admin/shutdown` is in flight.
    /// New writes return 503; existing in-flight may finish.
    Draining = 2,
    /// `Engine::graceful_shutdown` is running its final flush +
    /// checkpoint + optional backup. The runtime is no longer usable
    /// for writes.
    ShuttingDown = 3,
    /// Shutdown completed; the process is safe to exit.
    Stopped = 4,
}

impl Phase {
    pub fn from_u8(v: u8) -> Self {
        match v {
            0 => Phase::Starting,
            1 => Phase::Ready,
            2 => Phase::Draining,
            3 => Phase::ShuttingDown,
            4 => Phase::Stopped,
            _ => Phase::Stopped,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Starting => "starting",
            Phase::Ready => "ready",
            Phase::Draining => "draining",
            Phase::ShuttingDown => "shutting_down",
            Phase::Stopped => "stopped",
        }
    }

    /// Whether public mutations should be allowed in this phase.
    /// Replica/read_only checks live in `WriteGate`; this is the
    /// orthogonal lifecycle check.
    pub fn accepts_writes(self) -> bool {
        matches!(self, Phase::Ready)
    }

    /// Whether the runtime is far enough along in boot to answer
    /// SQL queries. /health/ready follows this answer.
    pub fn accepts_queries(self) -> bool {
        matches!(self, Phase::Ready | Phase::Draining)
    }
}

/// Final report produced by a successful graceful shutdown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ShutdownReport {
    pub flushed_wal: bool,
    pub final_checkpoint: bool,
    pub backup_uploaded: bool,
    pub duration_ms: u64,
    pub started_at_ms: u64,
    pub completed_at_ms: u64,
}

/// Lifecycle state wrapper held in `RuntimeInner`.
pub struct Lifecycle {
    phase: AtomicU8,
    /// Wall-clock millis at runtime construction. Used by /health/*
    /// endpoints to report uptime + cold-start cost.
    started_at_ms: u64,
    /// Wall-clock millis when the runtime entered `Ready`. 0 while
    /// still starting. Read by `/health/ready` to expose the cold-
    /// start window.
    ready_at_ms: AtomicU64,
    /// PLAN.md Phase 9.1 — cold-start phase markers. Each AtomicU64
    /// is the wall-clock unix-ms when the runtime transitioned into
    /// the named phase. `0` until the phase fires. Operators tune
    /// the cold-start budget by reading the deltas through the
    /// `cold_start_phases()` accessor.
    restore_started_at_ms: AtomicU64,
    restore_ready_at_ms: AtomicU64,
    wal_replay_started_at_ms: AtomicU64,
    wal_replay_ready_at_ms: AtomicU64,
    index_warmup_started_at_ms: AtomicU64,
    index_warmup_ready_at_ms: AtomicU64,
    /// Reason string when /health/ready returns 503. Empty when
    /// ready or stopped. Updated under `report` write lock so
    /// readers of the JSON status see a consistent pair of phase +
    /// reason.
    report: RwLock<LifecycleReport>,
}

/// PLAN.md Phase 9.1 — cold-start phase snapshot. All fields are
/// wall-clock unix-ms; `0` means the phase hasn't fired yet.
#[derive(Debug, Default, Clone, Copy)]
pub struct ColdStartPhases {
    pub started_at_ms: u64,
    pub restore_started_at_ms: u64,
    pub restore_ready_at_ms: u64,
    pub wal_replay_started_at_ms: u64,
    pub wal_replay_ready_at_ms: u64,
    pub index_warmup_started_at_ms: u64,
    pub index_warmup_ready_at_ms: u64,
    pub ready_at_ms: u64,
}

impl ColdStartPhases {
    /// `(phase_name, duration_ms)` pairs for /metrics. Skips phases
    /// that haven't fired or haven't completed.
    pub fn durations_ms(&self) -> Vec<(&'static str, u64)> {
        let mut out = Vec::with_capacity(4);
        if self.restore_ready_at_ms >= self.restore_started_at_ms
            && self.restore_started_at_ms > 0
            && self.restore_ready_at_ms > 0
        {
            out.push((
                "restore",
                self.restore_ready_at_ms - self.restore_started_at_ms,
            ));
        }
        if self.wal_replay_ready_at_ms >= self.wal_replay_started_at_ms
            && self.wal_replay_started_at_ms > 0
            && self.wal_replay_ready_at_ms > 0
        {
            out.push((
                "wal_replay",
                self.wal_replay_ready_at_ms - self.wal_replay_started_at_ms,
            ));
        }
        if self.index_warmup_ready_at_ms >= self.index_warmup_started_at_ms
            && self.index_warmup_started_at_ms > 0
            && self.index_warmup_ready_at_ms > 0
        {
            out.push((
                "index_warmup",
                self.index_warmup_ready_at_ms - self.index_warmup_started_at_ms,
            ));
        }
        if self.ready_at_ms >= self.started_at_ms && self.ready_at_ms > 0 {
            out.push(("total", self.ready_at_ms - self.started_at_ms));
        }
        out
    }
}

#[derive(Debug, Default, Clone)]
struct LifecycleReport {
    not_ready_reason: Option<String>,
    shutdown: Option<ShutdownReport>,
}

impl Lifecycle {
    pub fn new() -> Self {
        Self {
            phase: AtomicU8::new(Phase::Starting as u8),
            started_at_ms: now_ms(),
            ready_at_ms: AtomicU64::new(0),
            restore_started_at_ms: AtomicU64::new(0),
            restore_ready_at_ms: AtomicU64::new(0),
            wal_replay_started_at_ms: AtomicU64::new(0),
            wal_replay_ready_at_ms: AtomicU64::new(0),
            index_warmup_started_at_ms: AtomicU64::new(0),
            index_warmup_ready_at_ms: AtomicU64::new(0),
            report: RwLock::new(LifecycleReport::default()),
        }
    }

    /// PLAN.md Phase 9.1 — mark a cold-start phase boundary. Boot
    /// code calls these as it transitions through restore-from-
    /// remote, WAL replay, and index warmup. Idempotent: only the
    /// first call sets the timestamp; subsequent calls are no-ops
    /// so a retried boot doesn't reset the gauge.
    pub fn mark_restore_started(&self) {
        let _ = self
            .restore_started_at_ms
            .compare_exchange(0, now_ms(), Ordering::AcqRel, Ordering::Acquire);
    }
    pub fn mark_restore_ready(&self) {
        let _ = self
            .restore_ready_at_ms
            .compare_exchange(0, now_ms(), Ordering::AcqRel, Ordering::Acquire);
    }
    pub fn mark_wal_replay_started(&self) {
        let _ = self
            .wal_replay_started_at_ms
            .compare_exchange(0, now_ms(), Ordering::AcqRel, Ordering::Acquire);
    }
    pub fn mark_wal_replay_ready(&self) {
        let _ = self
            .wal_replay_ready_at_ms
            .compare_exchange(0, now_ms(), Ordering::AcqRel, Ordering::Acquire);
    }
    pub fn mark_index_warmup_started(&self) {
        let _ = self
            .index_warmup_started_at_ms
            .compare_exchange(0, now_ms(), Ordering::AcqRel, Ordering::Acquire);
    }
    pub fn mark_index_warmup_ready(&self) {
        let _ = self
            .index_warmup_ready_at_ms
            .compare_exchange(0, now_ms(), Ordering::AcqRel, Ordering::Acquire);
    }

    /// PLAN.md Phase 9.1 — backfill phase markers with an explicit
    /// timestamp. Used when the runtime captures the wall-clock
    /// before Lifecycle is constructible (e.g. before storage
    /// open) and wants to replay it into the markers afterwards.
    /// Idempotent (only sets when current value is 0).
    pub fn set_restore_started_at_ms(&self, ms: u64) {
        let _ = self
            .restore_started_at_ms
            .compare_exchange(0, ms, Ordering::AcqRel, Ordering::Acquire);
    }
    pub fn set_restore_ready_at_ms(&self, ms: u64) {
        let _ = self
            .restore_ready_at_ms
            .compare_exchange(0, ms, Ordering::AcqRel, Ordering::Acquire);
    }
    pub fn set_wal_replay_started_at_ms(&self, ms: u64) {
        let _ = self
            .wal_replay_started_at_ms
            .compare_exchange(0, ms, Ordering::AcqRel, Ordering::Acquire);
    }
    pub fn set_wal_replay_ready_at_ms(&self, ms: u64) {
        let _ = self
            .wal_replay_ready_at_ms
            .compare_exchange(0, ms, Ordering::AcqRel, Ordering::Acquire);
    }

    /// Snapshot every cold-start marker in one read. Callers compute
    /// per-phase deltas via `ColdStartPhases::durations_ms()`.
    pub fn cold_start_phases(&self) -> ColdStartPhases {
        ColdStartPhases {
            started_at_ms: self.started_at_ms,
            restore_started_at_ms: self.restore_started_at_ms.load(Ordering::Acquire),
            restore_ready_at_ms: self.restore_ready_at_ms.load(Ordering::Acquire),
            wal_replay_started_at_ms: self.wal_replay_started_at_ms.load(Ordering::Acquire),
            wal_replay_ready_at_ms: self.wal_replay_ready_at_ms.load(Ordering::Acquire),
            index_warmup_started_at_ms: self.index_warmup_started_at_ms.load(Ordering::Acquire),
            index_warmup_ready_at_ms: self.index_warmup_ready_at_ms.load(Ordering::Acquire),
            ready_at_ms: self.ready_at_ms.load(Ordering::Acquire),
        }
    }

    pub fn phase(&self) -> Phase {
        Phase::from_u8(self.phase.load(Ordering::Acquire))
    }

    pub fn started_at_ms(&self) -> u64 {
        self.started_at_ms
    }

    pub fn ready_at_ms(&self) -> Option<u64> {
        match self.ready_at_ms.load(Ordering::Acquire) {
            0 => None,
            v => Some(v),
        }
    }

    pub fn not_ready_reason(&self) -> Option<String> {
        self.report.read().not_ready_reason.clone()
    }

    pub fn shutdown_report(&self) -> Option<ShutdownReport> {
        self.report.read().shutdown.clone()
    }

    /// Mark a transient "still starting, here's why" reason. Called
    /// from boot stages so /health/ready 503 carries operator-useful
    /// context (e.g. "wal_replay", "restore_from_remote",
    /// "initial_checkpoint").
    pub fn set_starting_reason(&self, reason: impl Into<String>) {
        self.report.write().not_ready_reason = Some(reason.into());
    }

    /// Transition Starting → Ready. Idempotent: a second call leaves
    /// the existing `ready_at_ms` untouched. Returns true if this
    /// call effected the transition.
    pub fn mark_ready(&self) -> bool {
        let prev = self
            .phase
            .compare_exchange(
                Phase::Starting as u8,
                Phase::Ready as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok();
        if prev {
            self.ready_at_ms.store(now_ms(), Ordering::Release);
            self.report.write().not_ready_reason = None;
        }
        prev
    }

    /// Transition Ready → Draining (best effort — already-Stopped
    /// runtimes stay Stopped). Idempotent.
    pub fn mark_draining(&self) {
        let _ = self.phase.compare_exchange(
            Phase::Ready as u8,
            Phase::Draining as u8,
            Ordering::AcqRel,
            Ordering::Acquire,
        );
    }

    /// Transition into ShuttingDown if not already past it. Returns
    /// true if this call started the shutdown (caller should run the
    /// actual flush pipeline); false if shutdown was already started
    /// or finished by someone else (caller should poll for the
    /// existing report instead).
    pub fn begin_shutdown(&self) -> bool {
        loop {
            let current = self.phase.load(Ordering::Acquire);
            let p = Phase::from_u8(current);
            match p {
                Phase::Starting | Phase::Ready | Phase::Draining => {
                    if self
                        .phase
                        .compare_exchange(
                            current,
                            Phase::ShuttingDown as u8,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return true;
                    }
                    // raced with a concurrent transition; loop and re-check.
                }
                Phase::ShuttingDown | Phase::Stopped => return false,
            }
        }
    }

    /// Stamp the final report and move to Stopped. Called by
    /// `Engine::graceful_shutdown` after the flush pipeline finishes.
    pub fn finish_shutdown(&self, report: ShutdownReport) {
        self.report.write().shutdown = Some(report);
        self.phase.store(Phase::Stopped as u8, Ordering::Release);
    }
}

fn now_ms() -> u64 {
    crate::utils::now_unix_millis()
}
