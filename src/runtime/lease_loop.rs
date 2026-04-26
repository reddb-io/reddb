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
//! Env knobs:
//!   * `RED_LEASE_REQUIRED` — `true` / `1` to enable. Default off.
//!   * `RED_LEASE_TTL_SECS` — lease TTL in seconds. Default 60.
//!   * `RED_LEASE_HOLDER_ID` — explicit holder id. Default
//!     `<hostname>-<pid>`.
//!   * `RED_LEASE_PREFIX` — backend prefix for the lease object key.
//!     Default `leases/`.

use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::api::{RedDBError, RedDBResult};
use crate::replication::lease::LeaseStore;
use crate::runtime::lease_lifecycle::{LeaseLifecycle, MarkDraining};
use crate::runtime::RedDBRuntime;

/// Try to start the writer-lease lifecycle if the operator opted in.
/// Returns `Ok(())` when:
///   * `RED_LEASE_REQUIRED` is unset / false, or
///   * the lease was acquired and the refresh thread is running.
/// Returns `Err` when the operator asked for a lease and we couldn't
/// get one — the caller should refuse to serve in that case.
pub fn start_lease_loop_if_required(runtime: &RedDBRuntime) -> RedDBResult<()> {
    if !lease_required() {
        return Ok(());
    }

    let backend = runtime
        .db()
        .options()
        .remote_backend_atomic
        .clone()
        .ok_or_else(|| {
            RedDBError::Internal(
                "RED_LEASE_REQUIRED=true but the configured backend does not support atomic \
                 CAS — use s3, fs, or http with RED_HTTP_CONDITIONAL_WRITES=true"
                    .to_string(),
            )
        })?;

    let database_key = runtime
        .db()
        .options()
        .remote_key
        .clone()
        .unwrap_or_else(|| "main".to_string());
    let ttl_ms = lease_ttl_secs() * 1000;
    let holder_id = lease_holder_id();
    let prefix = std::env::var("RED_LEASE_PREFIX")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "leases/".to_string());

    let store = Arc::new(LeaseStore::new(backend).with_prefix(prefix));
    let runtime_for_drain = runtime.clone();
    let mark_draining: MarkDraining = Arc::new(move || {
        runtime_for_drain.lifecycle().mark_draining();
    });
    let lifecycle = Arc::new(LeaseLifecycle::new(
        store,
        runtime.write_gate_arc(),
        runtime.audit_log_arc(),
        mark_draining,
        holder_id,
        database_key,
        ttl_ms,
    ));
    lifecycle.try_acquire()?;

    // Stash the lifecycle on the runtime so admin handlers and the
    // refresh thread share one instance. The OnceLock guarantees
    // idempotency — re-entering the boot path (tests, double-init)
    // returns Err and we drop the duplicate.
    let lifecycle_for_runtime = Arc::clone(&lifecycle);
    let _ = runtime.set_lease_lifecycle(lifecycle_for_runtime);

    spawn_refresh_thread(runtime.clone(), lifecycle, ttl_ms);
    Ok(())
}

fn spawn_refresh_thread(runtime: RedDBRuntime, lifecycle: Arc<LeaseLifecycle>, ttl_ms: u64) {
    let interval = Duration::from_millis(ttl_ms.saturating_div(3).max(1_000));
    let _ = thread::Builder::new()
        .name("reddb-lease-refresh".into())
        .spawn(move || {
            loop {
                thread::sleep(interval);

                // Bail out cleanly on shutdown — the holder thread
                // would otherwise refresh past the runtime's lifetime
                // and confuse the next writer's acquire attempt.
                let phase = runtime.lifecycle().phase();
                if matches!(
                    phase,
                    crate::runtime::lifecycle::Phase::Draining
                        | crate::runtime::lifecycle::Phase::ShuttingDown
                        | crate::runtime::lifecycle::Phase::Stopped
                ) {
                    let _ = lifecycle.release();
                    return;
                }

                if lifecycle.refresh().is_err() {
                    return;
                }
            }
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
