//! Serverless writer-lease boot path (PLAN.md Phase 5 / W6).
//!
//! Boot-time entrypoint that opts the runtime into lease-fenced
//! writes when:
//!   * `RED_LEASE_REQUIRED=true` (or `1`) is set, and
//!   * a remote backend is configured (S3, FS, HTTP, …).
//!
//! All transitions (acquire / refresh / lost / release) are
//! delegated to `LeaseLifecycle`; this module only owns env-var
//! parsing, lifecycle construction, and the refresh thread.
//!
//! The refresh thread uses `LeaseTimerWheel` instead of a fixed
//! `thread::sleep(ttl_ms / 3)` polling loop. The wheel wakes the
//! thread exactly when the next refresh is due and reschedules after
//! each successful refresh, so idle leases cost zero CPU.
//!
//! Env knobs:
//!   * `RED_LEASE_REQUIRED` — `true` / `1` to enable. Default off.
//!   * `RED_LEASE_TTL_SECS` — lease TTL in seconds. Default 60.
//!   * `RED_LEASE_HOLDER_ID` — explicit holder id. Default
//!     `<hostname>-<pid>`.
//!   * `RED_LEASE_PREFIX` — backend prefix for the lease object key.
//!     Default `leases/`.

use std::sync::Arc;
use std::thread;
use std::time::Instant;

use crate::api::{RedDBError, RedDBResult};
use crate::replication::lease::LeaseStore;
use crate::runtime::lease_lifecycle::{LeaseLifecycle, MarkDraining};
use crate::runtime::lease_timer_wheel::LeaseTimerWheel;
use crate::runtime::serverless_snapshot_publication::SnapshotPublicationStore;
use crate::runtime::RedDBRuntime;

pub struct LeaseBootPlan {
    store: Arc<LeaseStore>,
    snapshot_publication: Arc<SnapshotPublicationStore>,
    holder_id: String,
    database_key: String,
    ttl_ms: u64,
}

/// Observe the exact snapshot head before runtime hydration. The later lease
/// claim CASes that observed version; publication racing with boot therefore
/// aborts this boot instead of letting it serve a stale local snapshot.
pub fn prepare_lease_loop_if_required(
    options: &mut crate::api::RedDBOptions,
) -> RedDBResult<Option<LeaseBootPlan>> {
    if !lease_required() {
        return Ok(None);
    }
    if options.read_only {
        return Err(RedDBError::InvalidOperation(
            "serverless writer leases require a writable local cache".into(),
        ));
    }
    let backend = options.remote_backend_atomic.clone().ok_or_else(|| {
        RedDBError::Internal(
            "RED_LEASE_REQUIRED=true but the configured backend does not support atomic CAS — \
             use s3, fs, or http with RED_HTTP_CONDITIONAL_WRITES=true"
                .to_string(),
        )
    })?;
    let database_key = options
        .remote_key
        .clone()
        .filter(|key| !key.is_empty())
        .filter(|_| options.remote_backend.is_some())
        .ok_or_else(|| {
            RedDBError::InvalidOperation(
                "serverless writer leases require a remote backend and database key".into(),
            )
        })?;
    let prefix = std::env::var("RED_LEASE_PREFIX")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "leases/".to_string());
    let snapshot_publication = Arc::new(SnapshotPublicationStore::new(
        backend.clone(),
        database_key.clone(),
    ));
    options.remote_bootstrap_key = snapshot_publication
        .observe_for_boot()
        .map_err(|err| RedDBError::Internal(err.to_string()))?;
    if options.remote_bootstrap_key.is_some()
        && options.storage_profile.deploy_profile != crate::storage::DeployProfile::Embedded
    {
        // Operational caches have WAL and metadata siblings. Hydrate into an
        // isolated boot directory so an old local WAL cannot replay over the
        // authoritative snapshot. Reuse the existing ephemeral cleanup owner.
        let cache = crate::api::RedDBOptions::in_memory();
        options.data_path = cache.data_path;
        options.metadata.insert(
            crate::api::EPHEMERAL_RUNTIME_METADATA_KEY.into(),
            "true".into(),
        );
    }
    Ok(Some(LeaseBootPlan {
        store: Arc::new(LeaseStore::new(backend).with_prefix(prefix)),
        snapshot_publication,
        holder_id: lease_holder_id(),
        database_key,
        ttl_ms: lease_ttl_secs() * 1000,
    }))
}

/// Try to start the writer-lease lifecycle if the operator opted in.
/// Returns `Ok(())` when:
///   * `RED_LEASE_REQUIRED` is unset / false, or
///   * the lease was acquired and the refresh thread is running.
///
/// Returns `Err` when the operator asked for a lease and we couldn't
/// get one — the caller should refuse to serve in that case.
pub fn start_lease_loop_if_required(runtime: &RedDBRuntime) -> RedDBResult<()> {
    let mut options = runtime.db().options().clone();
    let plan = prepare_lease_loop_if_required(&mut options)?;
    if options.remote_bootstrap_key.is_some() {
        return Err(RedDBError::InvalidOperation(
            "serverless snapshot head must be observed before runtime hydration; use the prepared lease boot path"
                .to_string(),
        ));
    }
    start_prepared_lease_loop(runtime, plan)
}

pub fn start_prepared_lease_loop(
    runtime: &RedDBRuntime,
    plan: Option<LeaseBootPlan>,
) -> RedDBResult<()> {
    let Some(plan) = plan else {
        return Ok(());
    };
    let local_snapshot = runtime.capture_serverless_snapshot()?;
    let runtime_for_drain = Arc::downgrade(&runtime.inner);
    let mark_draining: MarkDraining = Arc::new(move || {
        if let Some(inner) = runtime_for_drain.upgrade() {
            inner.lifecycle.mark_draining();
        }
    });
    let term = runtime.current_replication_term();
    let current_term = Arc::new(move || term);
    let lifecycle = Arc::new(
        LeaseLifecycle::new(
            plan.store,
            runtime.write_gate_arc(),
            runtime.audit_log_arc(),
            mark_draining,
            current_term,
            plan.holder_id,
            plan.database_key,
            plan.ttl_ms,
        )
        .with_snapshot_publication(plan.snapshot_publication, local_snapshot.path()),
    );
    lifecycle.try_acquire()?;

    // Stash the lifecycle on the runtime so admin handlers and the
    // refresh thread share one instance. The OnceLock guarantees
    // idempotency — re-entering the boot path (tests, double-init)
    // returns Err and we drop the duplicate.
    let lifecycle_for_runtime = Arc::clone(&lifecycle);
    let _ = runtime.set_lease_lifecycle(lifecycle_for_runtime);

    spawn_refresh_thread(runtime.clone(), lifecycle);
    Ok(())
}

fn spawn_refresh_thread(runtime: RedDBRuntime, lifecycle: Arc<LeaseLifecycle>) {
    // Timer wheel replaces thread::sleep(interval) — wakes exactly when the
    // next refresh is due. Zero CPU during the idle window.
    let wheel = Arc::new(LeaseTimerWheel::new(1));
    wheel.schedule(
        "lease-refresh".to_string(),
        Instant::now() + lifecycle.refresh_delay(),
    );

    let wheel_for_handler = Arc::clone(&wheel);
    let _ = thread::Builder::new()
        .name("reddb-lease-refresh".into())
        .spawn(move || {
            wheel.run_until_shutdown(move |_id| {
                // Bail out cleanly on shutdown so the holder thread does not
                // refresh past the runtime's lifetime and confuse the next
                // writer's acquire attempt.
                let phase = runtime.lifecycle().phase();
                if matches!(
                    phase,
                    crate::runtime::lifecycle::Phase::Draining
                        | crate::runtime::lifecycle::Phase::Stopped
                ) {
                    let _ = lifecycle.release();
                    return false;
                }

                if lifecycle.refresh().is_err() {
                    return false;
                }

                // Reschedule the next refresh.
                wheel_for_handler.schedule(
                    "lease-refresh".to_string(),
                    Instant::now() + lifecycle.refresh_delay(),
                );
                true
            });
        });
}

fn lease_required() -> bool {
    std::env::var("RED_LEASE_REQUIRED")
        .ok()
        .map(|v| {
            let t = v.trim();
            t.eq_ignore_ascii_case("true") || t == "1" || t.eq_ignore_ascii_case("yes")
        })
        .unwrap_or(false)
}

fn lease_ttl_secs() -> u64 {
    std::env::var("RED_LEASE_TTL_SECS")
        .ok()
        .and_then(|s| s.trim().parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(60)
}

fn lease_holder_id() -> String {
    if let Some(explicit) = crate::utils::env_with_file_fallback("RED_LEASE_HOLDER_ID") {
        return explicit;
    }
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("HOST"))
        .unwrap_or_else(|_| "unknown-host".to_string());
    format!("{}-{}", host, std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_required_parses_truthy_values() {
        unsafe {
            std::env::set_var("RED_LEASE_REQUIRED", "true");
        }
        assert!(lease_required());
        unsafe {
            std::env::set_var("RED_LEASE_REQUIRED", "1");
        }
        assert!(lease_required());
        unsafe {
            std::env::set_var("RED_LEASE_REQUIRED", "yes");
        }
        assert!(lease_required());
        unsafe {
            std::env::set_var("RED_LEASE_REQUIRED", "false");
        }
        assert!(!lease_required());
        unsafe {
            std::env::remove_var("RED_LEASE_REQUIRED");
        }
        assert!(!lease_required());
    }

    #[test]
    fn ttl_defaults_to_60_when_unset() {
        unsafe {
            std::env::remove_var("RED_LEASE_TTL_SECS");
        }
        assert_eq!(lease_ttl_secs(), 60);
    }

    #[test]
    fn ttl_rejects_zero_and_negative() {
        unsafe {
            std::env::set_var("RED_LEASE_TTL_SECS", "0");
        }
        assert_eq!(lease_ttl_secs(), 60);
        unsafe {
            std::env::set_var("RED_LEASE_TTL_SECS", "abc");
        }
        assert_eq!(lease_ttl_secs(), 60);
        unsafe {
            std::env::set_var("RED_LEASE_TTL_SECS", "30");
        }
        assert_eq!(lease_ttl_secs(), 30);
        unsafe {
            std::env::remove_var("RED_LEASE_TTL_SECS");
        }
    }

    #[test]
    fn holder_id_falls_back_when_no_env() {
        unsafe {
            std::env::remove_var("RED_LEASE_HOLDER_ID");
        }
        let id = lease_holder_id();
        assert!(id.contains('-'));
        assert!(!id.is_empty());
    }

    #[test]
    fn holder_id_uses_explicit_when_set() {
        unsafe {
            std::env::set_var("RED_LEASE_HOLDER_ID", "explicit-writer-1");
        }
        assert_eq!(lease_holder_id(), "explicit-writer-1");
        unsafe {
            std::env::remove_var("RED_LEASE_HOLDER_ID");
        }
    }
}
