//! Serverless writer-lease lifecycle (PLAN.md Phase 5 / W6).
//!
//! Boot-time entrypoint that opts the runtime into lease-fenced
//! writes when:
//!   * `RED_LEASE_REQUIRED=true` (or `1`) is set, and
//!   * a remote backend is configured (S3, FS, HTTP, …).
//!
//! Behaviour:
//!   1. Build a `LeaseStore` against the runtime's remote backend.
//!   2. `try_acquire` for the configured holder id + TTL. Failure to
//!      acquire returns `Err` so the bootstrap aborts fast — the
//!      operator opted into fencing, so a missing lease is fatal.
//!   3. Flip `WriteGate::set_lease_state(Held)` so public mutations
//!      and remote uploads start passing the gate.
//!   4. Spawn a daemon thread that calls `refresh` every `ttl/3`. On
//!      `LostRace` / `Stale` / backend error: flip to `NotHeld`,
//!      audit, mark the lifecycle as draining, and exit. The refresh
//!      thread does *not* attempt to re-acquire — losing the lease
//!      means a new writer was promoted and we must not race it.
//!
//! Env knobs:
//!   * `RED_LEASE_REQUIRED` — `true` / `1` to enable. Default off.
//!   * `RED_LEASE_TTL_SECS` — lease TTL in seconds. Default 60.
//!   * `RED_LEASE_HOLDER_ID` — explicit holder id. Default
//!     `<hostname>-<pid>`. The `holder_id` is what shows up in audit
//!     and in the published lease object on the backend.
//!   * `RED_LEASE_PREFIX` — backend prefix for the lease object key.
//!     Default `leases/`.
//!
//! Caveat: without backend-native CAS, two contenders racing on an
//! expired lease can both win the in-process check; `LeaseStore`
//! re-reads after publish to detect that case and reports
//! `LostRace`. Backends with native CAS (S3 conditional PUT, R2
//! `If-Match`) get the same protocol with stronger guarantees once
//! the trait grows a `try_acquire_cas` method.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::api::{RedDBError, RedDBResult};
use crate::json::Value as JsonValue;
use crate::replication::lease::{LeaseStore, WriterLease};
use crate::runtime::write_gate::LeaseGateState;
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
        .remote_backend
        .clone()
        .ok_or_else(|| {
            RedDBError::Internal(
                "RED_LEASE_REQUIRED=true but no remote backend configured (RED_BACKEND=none)"
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

    let store = LeaseStore::new(backend).with_prefix(prefix);
    let lease = store
        .try_acquire(&database_key, &holder_id, ttl_ms)
        .map_err(|err| RedDBError::Internal(format!("acquire writer lease: {err}")))?;

    runtime
        .write_gate()
        .set_lease_state(LeaseGateState::Held);

    let mut details = crate::json::Map::new();
    details.insert(
        "generation".to_string(),
        JsonValue::Number(lease.generation as f64),
    );
    details.insert(
        "ttl_ms".to_string(),
        JsonValue::Number(ttl_ms as f64),
    );
    runtime.audit_log().record(
        "lease/acquire",
        &holder_id,
        &database_key,
        "ok",
        JsonValue::Object(details),
    );

    spawn_refresh_thread(runtime.clone(), Arc::new(store), lease, holder_id, database_key, ttl_ms);
    Ok(())
}

fn spawn_refresh_thread(
    runtime: RedDBRuntime,
    store: Arc<LeaseStore>,
    lease: WriterLease,
    holder_id: String,
    database_key: String,
    ttl_ms: u64,
) {
    let lease = Arc::new(Mutex::new(lease));
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
                    let current = lease.lock().expect("poisoned lease mutex").clone();
                    if let Err(err) = store.release(&current) {
                        tracing::warn!(
                            target: "reddb::serverless::lease",
                            error = %err,
                            "lease release on shutdown failed"
                        );
                    }
                    runtime
                        .write_gate()
                        .set_lease_state(LeaseGateState::NotHeld);
                    runtime.audit_log().record(
                        "lease/release",
                        &holder_id,
                        &database_key,
                        "ok",
                        JsonValue::Null,
                    );
                    return;
                }

                let current = lease.lock().expect("poisoned lease mutex").clone();
                match store.refresh(&current, ttl_ms) {
                    Ok(updated) => {
                        *lease.lock().expect("poisoned lease mutex") = updated;
                    }
                    Err(err) => {
                        tracing::error!(
                            target: "reddb::serverless::lease",
                            error = %err,
                            holder = %holder_id,
                            database_key = %database_key,
                            "lease refresh failed; flipping to NotHeld + drain"
                        );
                        runtime
                            .write_gate()
                            .set_lease_state(LeaseGateState::NotHeld);
                        runtime.audit_log().record(
                            "lease/lost",
                            &holder_id,
                            &database_key,
                            &format!("err: {err}"),
                            JsonValue::Null,
                        );
                        runtime.lifecycle().mark_draining();
                        return;
                    }
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
    if let Ok(explicit) = std::env::var("RED_LEASE_HOLDER_ID") {
        if !explicit.trim().is_empty() {
            return explicit;
        }
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
