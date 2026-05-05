//! Serverless writer-lease state machine.
//!
//! Single owner of the `{acquire, refresh, refresh_failed, lost_race,
//! release}` transitions. Centralises the side-effects that must
//! happen together so the `WriteGate` lease state and the
//! `AuditLogger` lease/* records can never drift out of sync.
//!
//! Before this module those transitions lived inline in
//! `lease_loop::spawn_refresh_thread` and in
//! `handlers_admin::handle_admin_failover_promote`, with each
//! caller manually pairing `write_gate.set_lease_state(...)` and
//! `audit_log.record("lease/...")`. Four call sites, four chances
//! for drift.
//!
//! Test surface:
//!   * Construct with stand-alone `WriteGate` + `AuditLogger`.
//!   * Inject a `MarkDraining` callback (production wires it to
//!     `Lifecycle::mark_draining`).
//!   * Drive transitions; assert gate state + audit lines together.

use std::sync::{Arc, Mutex};

use crate::api::{RedDBError, RedDBResult};
use crate::json::Value as JsonValue;
use crate::replication::lease::{LeaseError, LeaseStore, WriterLease};
use crate::runtime::audit_log::{AuditAuthSource, AuditEvent, AuditLogger, Outcome};
use crate::runtime::write_gate::{LeaseGateState, WriteGate};

/// Callback the lifecycle uses to ask the surrounding runtime to
/// drain when the lease is lost. Production wires it to
/// `Lifecycle::mark_draining`. Tests pass a recorder.
pub type MarkDraining = Arc<dyn Fn() + Send + Sync>;

/// Drives the serverless writer lease for one database key.
///
/// Owns the `WriterLease` snapshot, the `LeaseStore` it talks to,
/// and the side-effect channels (`WriteGate`, `AuditLogger`,
/// `MarkDraining`). All transitions go through methods on this
/// struct so the gate/audit pair stays consistent.
pub struct LeaseLifecycle {
    store: Arc<LeaseStore>,
    write_gate: Arc<WriteGate>,
    audit_log: Arc<AuditLogger>,
    mark_draining: MarkDraining,
    holder_id: String,
    database_key: String,
    ttl_ms: u64,
    current: Mutex<Option<WriterLease>>,
}

impl LeaseLifecycle {
    pub fn new(
        store: Arc<LeaseStore>,
        write_gate: Arc<WriteGate>,
        audit_log: Arc<AuditLogger>,
        mark_draining: MarkDraining,
        holder_id: String,
        database_key: String,
        ttl_ms: u64,
    ) -> Self {
        Self {
            store,
            write_gate,
            audit_log,
            mark_draining,
            holder_id,
            database_key,
            ttl_ms,
            current: Mutex::new(None),
        }
    }

    pub fn holder_id(&self) -> &str {
        &self.holder_id
    }

    pub fn database_key(&self) -> &str {
        &self.database_key
    }

    pub fn ttl_ms(&self) -> u64 {
        self.ttl_ms
    }

    pub fn current_lease(&self) -> Option<WriterLease> {
        self.current.lock().expect("poisoned lease mutex").clone()
    }

    /// Acquire the writer lease and flip the gate to `Held`. Audit
    /// line records the outcome (ok / err) either way.
    pub fn try_acquire(&self) -> RedDBResult<()> {
        match self
            .store
            .try_acquire(&self.database_key, &self.holder_id, self.ttl_ms)
        {
            Ok(lease) => {
                *self.current.lock().expect("poisoned lease mutex") = Some(lease.clone());
                self.write_gate.set_lease_state(LeaseGateState::Held);
                let mut details = crate::json::Map::new();
                details.insert(
                    "generation".to_string(),
                    JsonValue::Number(lease.generation as f64),
                );
                details.insert("ttl_ms".to_string(), JsonValue::Number(self.ttl_ms as f64));
                self.audit_log.record_event(
                    AuditEvent::builder("lease/acquire")
                        .principal(self.holder_id.clone())
                        .source(AuditAuthSource::System)
                        .resource(self.database_key.clone())
                        .outcome(Outcome::Success)
                        .detail(JsonValue::Object(details))
                        .build(),
                );
                Ok(())
            }
            Err(err) => {
                let mut detail = crate::json::Map::new();
                detail.insert("error".to_string(), JsonValue::String(format!("{err}")));
                self.audit_log.record_event(
                    AuditEvent::builder("lease/acquire")
                        .principal(self.holder_id.clone())
                        .source(AuditAuthSource::System)
                        .resource(self.database_key.clone())
                        .outcome(Outcome::Error)
                        .detail(JsonValue::Object(detail))
                        .build(),
                );
                Err(RedDBError::Internal(format!("acquire writer lease: {err}")))
            }
        }
    }

    /// Refresh the held lease. On success, updates the in-memory
    /// snapshot and returns `Ok(())`. On any backend error, flips
    /// the gate to `NotHeld`, audits `lease/lost`, asks the runtime
    /// to drain, and returns `Err`.
    ///
    /// The refresh thread should treat `Err` as terminal and exit
    /// — re-acquiring after a loss could race a freshly promoted
    /// writer.
    pub fn refresh(&self) -> RedDBResult<()> {
        let snapshot = match self.current.lock().expect("poisoned lease mutex").clone() {
            Some(lease) => lease,
            None => {
                return Err(RedDBError::Internal(
                    "refresh called without an acquired lease".to_string(),
                ));
            }
        };
        match self.store.refresh(&snapshot, self.ttl_ms) {
            Ok(updated) => {
                *self.current.lock().expect("poisoned lease mutex") = Some(updated);
                Ok(())
            }
            Err(err) => {
                self.on_refresh_lost(err);
                Err(RedDBError::Internal("writer lease lost".to_string()))
            }
        }
    }

    /// Release the held lease and flip the gate to `NotHeld`. Best-
    /// effort: a backend failure is logged but does not propagate
    /// — shutdown should not block on a remote release.
    pub fn release(&self) -> RedDBResult<()> {
        let snapshot = match self.current.lock().expect("poisoned lease mutex").take() {
            Some(lease) => lease,
            None => {
                // Idempotent: already released. Make sure the gate is
                // closed in case a previous transition didn't run.
                self.write_gate.set_lease_state(LeaseGateState::NotHeld);
                return Ok(());
            }
        };
        let result = self.store.release(&snapshot);
        self.write_gate.set_lease_state(LeaseGateState::NotHeld);
        match result {
            Ok(()) => {
                self.audit_log.record_event(
                    AuditEvent::builder("lease/release")
                        .principal(self.holder_id.clone())
                        .source(AuditAuthSource::System)
                        .resource(self.database_key.clone())
                        .outcome(Outcome::Success)
                        .build(),
                );
                Ok(())
            }
            Err(err) => {
                let mut detail = crate::json::Map::new();
                detail.insert("error".to_string(), JsonValue::String(format!("{err}")));
                self.audit_log.record_event(
                    AuditEvent::builder("lease/release")
                        .principal(self.holder_id.clone())
                        .source(AuditAuthSource::System)
                        .resource(self.database_key.clone())
                        .outcome(Outcome::Error)
                        .detail(JsonValue::Object(detail))
                        .build(),
                );
                tracing::warn!(
                    target: "reddb::serverless::lease",
                    error = %err,
                    "lease release on shutdown failed"
                );
                Ok(())
            }
        }
    }

    fn on_refresh_lost(&self, err: LeaseError) {
        tracing::error!(
            target: "reddb::serverless::lease",
            error = %err,
            holder = %self.holder_id,
            database_key = %self.database_key,
            "lease refresh failed; flipping to NotHeld + drain"
        );
        *self.current.lock().expect("poisoned lease mutex") = None;
        self.write_gate.set_lease_state(LeaseGateState::NotHeld);
        let mut detail = crate::json::Map::new();
        detail.insert("error".to_string(), JsonValue::String(format!("{err}")));
        self.audit_log.record_event(
            AuditEvent::builder("lease/lost")
                .principal(self.holder_id.clone())
                .source(AuditAuthSource::System)
                .resource(self.database_key.clone())
                .outcome(Outcome::Error)
                .detail(JsonValue::Object(detail))
                .build(),
        );
        (self.mark_draining)();
    }
}

/// Admin-driven failover promotion: acquire the writer lease as a
/// stand-alone action without flipping the local gate. The instance
/// stays a `Replica` until the operator restarts it as primary; the
/// gate flip is deliberately left out so an unintended promotion
/// can't accept writes mid-process.
///
/// Audited under `admin/failover/promote` regardless of outcome.
pub fn admin_promote_lease(
    store: &LeaseStore,
    audit_log: &AuditLogger,
    database_key: &str,
    holder_id: &str,
    ttl_ms: u64,
) -> Result<WriterLease, LeaseError> {
    match store.try_acquire(database_key, holder_id, ttl_ms) {
        Ok(lease) => {
            let mut details = crate::json::Map::new();
            details.insert(
                "holder_id".to_string(),
                JsonValue::String(lease.holder_id.clone()),
            );
            details.insert(
                "generation".to_string(),
                JsonValue::Number(lease.generation as f64),
            );
            details.insert("ttl_ms".to_string(), JsonValue::Number(ttl_ms as f64));
            audit_log.record_event(
                AuditEvent::builder("admin/failover/promote")
                    .principal(lease.holder_id.clone())
                    .source(AuditAuthSource::System)
                    .resource(database_key.to_string())
                    .outcome(Outcome::Success)
                    .detail(JsonValue::Object(details))
                    .build(),
            );
            Ok(lease)
        }
        Err(err) => {
            let mut detail = crate::json::Map::new();
            detail.insert("error".to_string(), JsonValue::String(format!("{err}")));
            audit_log.record_event(
                AuditEvent::builder("admin/failover/promote")
                    .principal(holder_id.to_string())
                    .source(AuditAuthSource::System)
                    .resource(database_key.to_string())
                    .outcome(Outcome::Error)
                    .detail(JsonValue::Object(detail))
                    .build(),
            );
            Err(err)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::RedDBOptions;
    use crate::storage::backend::LocalBackend;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_prefix(tag: &str) -> PathBuf {
        let mut p = PathBuf::from(std::env::temp_dir());
        p.push(format!(
            "reddb-lease-lifecycle-{tag}-{}-{}",
            std::process::id(),
            crate::utils::now_unix_nanos(),
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn build_lifecycle(
        tag: &str,
    ) -> (
        Arc<LeaseLifecycle>,
        Arc<WriteGate>,
        Arc<AuditLogger>,
        Arc<AtomicUsize>,
        PathBuf,
    ) {
        let prefix = temp_prefix(tag);
        let store = Arc::new(
            LeaseStore::new(Arc::new(LocalBackend))
                .with_prefix(prefix.to_string_lossy().to_string()),
        );
        let mut opts = RedDBOptions::default();
        opts.read_only = false;
        let write_gate = Arc::new(WriteGate::from_options(&opts));
        let audit_log = Arc::new(AuditLogger::for_data_path(&prefix.join("data.rdb")));
        let drain_counter = Arc::new(AtomicUsize::new(0));
        let drain_counter_clone = Arc::clone(&drain_counter);
        let mark_draining: MarkDraining = Arc::new(move || {
            drain_counter_clone.fetch_add(1, Ordering::SeqCst);
        });
        let lifecycle = Arc::new(LeaseLifecycle::new(
            store,
            Arc::clone(&write_gate),
            Arc::clone(&audit_log),
            mark_draining,
            "writer-1".to_string(),
            "main".to_string(),
            60_000,
        ));
        (lifecycle, write_gate, audit_log, drain_counter, prefix)
    }

    #[test]
    fn acquire_flips_gate_to_held_and_records_audit() {
        let (lifecycle, gate, audit, drain, prefix) = build_lifecycle("acquire");
        assert!(lifecycle.try_acquire().is_ok());
        assert_eq!(gate.lease_state(), LeaseGateState::Held);
        assert!(lifecycle.current_lease().is_some());
        assert_eq!(drain.load(Ordering::SeqCst), 0);
        // Audit log file should contain one acquire success line.
        assert!(audit.wait_idle(std::time::Duration::from_secs(2)));
        let body = std::fs::read_to_string(audit.path()).unwrap();
        assert!(body.contains("lease/acquire"));
        assert!(body.contains("\"outcome\":\"success\""));
        let _ = std::fs::remove_dir_all(&prefix);
    }

    #[test]
    fn release_flips_gate_to_not_held_and_clears_inner_state() {
        let (lifecycle, gate, audit, _drain, prefix) = build_lifecycle("release");
        lifecycle.try_acquire().unwrap();
        assert!(lifecycle.release().is_ok());
        assert_eq!(gate.lease_state(), LeaseGateState::NotHeld);
        assert!(lifecycle.current_lease().is_none());
        assert!(audit.wait_idle(std::time::Duration::from_secs(2)));
        let body = std::fs::read_to_string(audit.path()).unwrap();
        assert!(body.contains("lease/release"));
        let _ = std::fs::remove_dir_all(&prefix);
    }

    #[test]
    fn release_is_idempotent_when_no_lease_held() {
        let (lifecycle, gate, _audit, _drain, prefix) = build_lifecycle("release-idem");
        // No prior acquire; release should still close the gate.
        assert!(lifecycle.release().is_ok());
        assert_eq!(gate.lease_state(), LeaseGateState::NotHeld);
        let _ = std::fs::remove_dir_all(&prefix);
    }

    #[test]
    fn refresh_without_acquire_returns_error_without_touching_gate() {
        let (lifecycle, gate, _audit, drain, prefix) = build_lifecycle("refresh-noop");
        let err = lifecycle.refresh().unwrap_err();
        match err {
            RedDBError::Internal(msg) => assert!(msg.contains("without an acquired lease")),
            other => panic!("expected Internal, got {other:?}"),
        }
        assert_eq!(gate.lease_state(), LeaseGateState::NotRequired);
        assert_eq!(drain.load(Ordering::SeqCst), 0);
        let _ = std::fs::remove_dir_all(&prefix);
    }

    #[test]
    fn admin_promote_lease_audits_success() {
        let prefix = temp_prefix("admin-ok");
        let store = LeaseStore::new(Arc::new(LocalBackend))
            .with_prefix(prefix.to_string_lossy().to_string());
        let audit = AuditLogger::for_data_path(&prefix.join("data.rdb"));
        let lease = admin_promote_lease(&store, &audit, "main", "promoter-1", 30_000).unwrap();
        assert_eq!(lease.holder_id, "promoter-1");
        assert!(audit.wait_idle(std::time::Duration::from_secs(2)));
        let body = std::fs::read_to_string(audit.path()).unwrap();
        assert!(body.contains("admin/failover/promote"));
        assert!(body.contains("\"outcome\":\"success\""));
        let _ = std::fs::remove_dir_all(&prefix);
    }

    #[test]
    fn admin_promote_lease_does_not_flip_a_separate_gate() {
        // The promote helper takes no gate; verify the documentation
        // contract by constructing a gate and confirming nothing
        // touches it. (Trivial, but locks the contract in the test
        // suite so a future refactor that adds a gate-flip side
        // effect breaks this case.)
        let prefix = temp_prefix("admin-no-gate");
        let store = LeaseStore::new(Arc::new(LocalBackend))
            .with_prefix(prefix.to_string_lossy().to_string());
        let audit = AuditLogger::for_data_path(&prefix.join("data.rdb"));
        let mut opts = RedDBOptions::default();
        opts.read_only = false;
        let gate = WriteGate::from_options(&opts);
        let _ = admin_promote_lease(&store, &audit, "main", "promoter-2", 30_000).unwrap();
        assert_eq!(gate.lease_state(), LeaseGateState::NotRequired);
        let _ = std::fs::remove_dir_all(&prefix);
    }
}
