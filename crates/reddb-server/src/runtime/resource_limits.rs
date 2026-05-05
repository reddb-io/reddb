//! Operator-imposed resource limits (PLAN.md Phase 4.1).
//!
//! Cloud-agnostic deployments need hard upper bounds enforced
//! regardless of cgroup or namespace presence — a process that
//! grows past its allocated capacity slot can starve every other
//! tenant on the same host. The limits here are read once at boot
//! from `RED_MAX_*` env vars and held in a single immutable struct
//! consulted by the various enforcement points (write path, accept
//! loop, query timer, batch validator).
//!
//! `Option<u64>` semantics: `None` means "operator did not pin a
//! cap at boot — fall through to whatever upstream layer (cgroup,
//! kernel `RLIMIT_*`, runtime defaults) decides". `Some(0)` is
//! reserved as "explicitly unbounded" so an operator who sets an
//! env var to the literal `0` can disable a default without
//! scripting.

use std::time::Duration;

/// Snapshot of the `RED_MAX_*` env vars read at runtime
/// construction. Held by `RuntimeInner` and accessible via
/// `RedDBRuntime::resource_limits()` so observability and
/// enforcement consult the same values.
#[derive(Debug, Clone, Default)]
pub struct ResourceLimits {
    /// Maximum primary-database file size in bytes. Writes that
    /// would push the file past this value return
    /// `RedDBError::QuotaExceeded` with a structured payload
    /// (`{limit:"max_db_size", current, max}`). Operator-level
    /// behaviour PLAN.md prescribes: returns HTTP 507 Insufficient
    /// Storage at the public surface.
    pub max_db_size_bytes: Option<u64>,

    /// Maximum concurrent client connections. Saturated accept
    /// loops return HTTP 503 / wire-protocol error so callers back
    /// off cleanly.
    pub max_connections: Option<u64>,

    /// Soft memory budget in bytes. Cache eviction fires at this
    /// threshold; the runtime never panics on OOM. `0` disables
    /// the soft cap entirely.
    pub max_memory_bytes: Option<u64>,

    /// Maximum queries-per-second sustained per-instance. Token
    /// bucket fires HTTP 429 / wire backoff on excess.
    pub max_qps: Option<u64>,

    /// Maximum wall time for any single query. Queries past this
    /// threshold are killed and return HTTP 504. `None` defers to
    /// the OS / cgroup CPU policy.
    pub max_query_duration: Option<Duration>,

    /// Maximum response payload size in bytes. Larger responses
    /// are truncated or errored (decided by the surface).
    pub max_result_bytes: Option<u64>,

    /// Maximum rows per bulk insert / update / delete. Caps the
    /// memory the server allocates for any one batch.
    pub max_batch_size: Option<u64>,
}

impl ResourceLimits {
    /// Read limits from env vars. Accepts both the cloud-agnostic
    /// `RED_MAX_*` family (PLAN.md spec) and the legacy `REDDB_MAX_*`
    /// form for existing dev installs. Missing or unparseable
    /// values stay `None`. `0` is treated as "explicitly
    /// unbounded" so operators can disable a deployment-default
    /// cap without unsetting the env.
    pub fn from_env() -> Self {
        let mut out = Self {
            max_db_size_bytes: Self::read_u64("MAX_DB_SIZE_BYTES"),
            max_connections: Self::read_u64("MAX_CONNECTIONS"),
            max_memory_bytes: Self::read_u64("MAX_MEMORY_MB")
                .map(|mb| mb.saturating_mul(1_048_576)),
            max_qps: Self::read_u64("MAX_QPS"),
            max_query_duration: Self::read_u64("MAX_QUERY_DURATION_MS").map(Duration::from_millis),
            max_result_bytes: Self::read_u64("MAX_RESULT_BYTES"),
            max_batch_size: Self::read_u64("MAX_BATCH_SIZE"),
        };

        // PLAN.md Phase 4.2 — auto-detect container memory cap when
        // the operator didn't pin one. Cgroup v2 first
        // (`memory.max`), v1 fallback
        // (`memory/memory.limit_in_bytes`). Cross-platform: missing
        // files / non-Linux just leave the field `None`. The
        // explicit env var still wins so an operator can override
        // a too-tight cgroup detect without restructuring the
        // container.
        if out.max_memory_bytes.is_none() {
            out.max_memory_bytes = read_cgroup_memory_max();
        }

        out
    }

    fn read_u64(suffix: &str) -> Option<u64> {
        std::env::var(format!("RED_{suffix}"))
            .or_else(|_| std::env::var(format!("REDDB_{suffix}")))
            .ok()
            .and_then(|raw| raw.trim().parse::<u64>().ok())
    }

    /// Whether `max_db_size_bytes` is set and `current_bytes`
    /// exceeds it. Cheap branch — caller decides what to do
    /// (surface-specific error code, refuse new writes, suspend).
    pub fn db_size_exceeded(&self, current_bytes: u64) -> bool {
        match self.max_db_size_bytes {
            Some(limit) if limit > 0 => current_bytes > limit,
            _ => false,
        }
    }

    pub fn batch_size_exceeded(&self, requested: usize) -> bool {
        match self.max_batch_size {
            Some(limit) if limit > 0 => (requested as u64) > limit,
            _ => false,
        }
    }
}

/// Read the active cgroup memory cap, returning bytes when known.
/// Cgroup v2 first (`/sys/fs/cgroup/memory.max`), v1 fallback
/// (`/sys/fs/cgroup/memory/memory.limit_in_bytes`). The literal
/// string `max` (cgroup v2 "no cap") returns `None` so the
/// resource-limits struct stays at "no cap pinned".
///
/// Non-Linux / missing files / unparseable contents → `None`. Never
/// panics; the caller treats absence as "fall through to whatever
/// upstream layer decides".
fn read_cgroup_memory_max() -> Option<u64> {
    // cgroup v2
    if let Ok(raw) = std::fs::read_to_string("/sys/fs/cgroup/memory.max") {
        let trimmed = raw.trim();
        if trimmed != "max" && !trimmed.is_empty() {
            if let Ok(bytes) = trimmed.parse::<u64>() {
                if bytes > 0 && bytes < u64::MAX / 2 {
                    return Some(bytes);
                }
            }
        }
    }
    // cgroup v1
    if let Ok(raw) = std::fs::read_to_string("/sys/fs/cgroup/memory/memory.limit_in_bytes") {
        if let Ok(bytes) = raw.trim().parse::<u64>() {
            // Kernels report `9223372036854771712` as "unlimited" in
            // cgroup v1; treat any value that's effectively
            // unbounded as `None`.
            if bytes > 0 && bytes < (u64::MAX / 2) {
                return Some(bytes);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_set(key: &str, value: &str) {
        unsafe {
            std::env::set_var(key, value);
        }
    }
    fn env_unset(key: &str) {
        unsafe {
            std::env::remove_var(key);
        }
    }

    #[test]
    fn from_env_reads_max_db_size() {
        env_set("RED_MAX_DB_SIZE_BYTES", "1073741824");
        let limits = ResourceLimits::from_env();
        env_unset("RED_MAX_DB_SIZE_BYTES");
        assert_eq!(limits.max_db_size_bytes, Some(1_073_741_824));
    }

    #[test]
    fn legacy_reddb_prefix_is_accepted() {
        env_set("REDDB_MAX_BATCH_SIZE", "10000");
        let limits = ResourceLimits::from_env();
        env_unset("REDDB_MAX_BATCH_SIZE");
        assert_eq!(limits.max_batch_size, Some(10_000));
    }

    #[test]
    fn unset_env_yields_no_limit() {
        env_unset("RED_MAX_QPS");
        env_unset("REDDB_MAX_QPS");
        let limits = ResourceLimits::from_env();
        assert!(limits.max_qps.is_none());
    }

    #[test]
    fn db_size_exceeded_respects_zero_as_unbounded() {
        let limits = ResourceLimits {
            max_db_size_bytes: Some(0),
            ..Default::default()
        };
        assert!(!limits.db_size_exceeded(u64::MAX));
    }

    #[test]
    fn db_size_exceeded_triggers_above_limit() {
        let limits = ResourceLimits {
            max_db_size_bytes: Some(1024),
            ..Default::default()
        };
        assert!(!limits.db_size_exceeded(1024));
        assert!(limits.db_size_exceeded(1025));
    }

    #[test]
    fn memory_mb_converts_to_bytes() {
        env_set("RED_MAX_MEMORY_MB", "256");
        let limits = ResourceLimits::from_env();
        env_unset("RED_MAX_MEMORY_MB");
        assert_eq!(limits.max_memory_bytes, Some(256 * 1024 * 1024));
    }

    #[test]
    fn query_duration_parses_to_duration() {
        env_set("RED_MAX_QUERY_DURATION_MS", "30000");
        let limits = ResourceLimits::from_env();
        env_unset("RED_MAX_QUERY_DURATION_MS");
        assert_eq!(limits.max_query_duration, Some(Duration::from_secs(30)));
    }
}
