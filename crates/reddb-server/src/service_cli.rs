use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::auth::store::AuthStore;
use crate::replication::ReplicationConfig;
use crate::runtime::RedDBRuntime;
use crate::service_router::{serve_tcp_router, InProcessRouterConfig};
use crate::storage::StorageProfileSelection;
use crate::{
    GrpcServerOptions, RedDBGrpcServer, RedDBOptions, RedDBServer, ServerOptions, StorageMode,
};

pub const DEFAULT_ROUTER_BIND_ADDR: &str = "127.0.0.1:5050";

/// Detect available CPU cores and suggest worker thread count.
pub fn detect_runtime_config() -> RuntimeConfig {
    let cpus = thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);

    // Reserve 1 core for OS, use the rest for workers (minimum 1)
    let suggested_workers = cpus.saturating_sub(1).max(1);

    RuntimeConfig {
        available_cpus: cpus,
        suggested_workers,
        stack_size: 8 * 1024 * 1024, // 16MB default
    }
}

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub available_cpus: usize,
    pub suggested_workers: usize,
    pub stack_size: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerTransport {
    Grpc,
    Http,
    Wire,
}

impl ServerTransport {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Grpc => "gRPC",
            Self::Http => "HTTP",
            Self::Wire => "wire",
        }
    }

    pub const fn default_bind_addr(self) -> &'static str {
        match self {
            Self::Grpc => "127.0.0.1:5555",
            Self::Http => "127.0.0.1:5055",
            Self::Wire => "127.0.0.1:5050",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServerCommandConfig {
    pub path: Option<PathBuf>,
    pub router_bind_addr: Option<String>,
    pub router_bind_explicit: bool,
    pub grpc_bind_addr: Option<String>,
    pub grpc_bind_explicit: bool,
    /// TLS-encrypted gRPC bind address. Can run side-by-side with
    /// `grpc_bind_addr` (e.g. `:50051` plain + `:50052` TLS) or
    /// stand alone for TLS-only deploys. Defaults to `None`.
    pub grpc_tls_bind_addr: Option<String>,
    /// Path to PEM-encoded gRPC server certificate. Resolved through
    /// `REDDB_GRPC_TLS_CERT` (with `_FILE` companion for k8s secret
    /// mounts). When `None` and dev-mode is enabled
    /// (`RED_GRPC_TLS_DEV=1`) the server auto-generates a self-signed
    /// cert and prints its SHA-256 fingerprint to stderr.
    pub grpc_tls_cert: Option<PathBuf>,
    /// Path to PEM-encoded gRPC server private key. Same env-var
    /// pattern as `grpc_tls_cert`.
    pub grpc_tls_key: Option<PathBuf>,
    /// Optional path to a PEM bundle of trust anchors used to verify
    /// client certificates. When set, the gRPC listener requires
    /// every client to present a cert that chains to this CA — i.e.
    /// mutual TLS. When unset, one-way TLS only.
    pub grpc_tls_client_ca: Option<PathBuf>,
    pub http_bind_addr: Option<String>,
    pub http_bind_explicit: bool,
    /// HTTPS bind address. When set, the HTTP server also serves a
    /// TLS-terminated listener on this addr. Plain HTTP and HTTPS can
    /// run side by side (e.g. 8080 plain + 8443 TLS).
    pub http_tls_bind_addr: Option<String>,
    /// PEM cert for HTTPS. Reads `REDDB_HTTP_TLS_CERT` / its `_FILE`
    /// companion when not set explicitly.
    pub http_tls_cert: Option<PathBuf>,
    /// PEM key for HTTPS. Reads `REDDB_HTTP_TLS_KEY` / its `_FILE`
    /// companion when not set explicitly.
    pub http_tls_key: Option<PathBuf>,
    /// Optional PEM CA bundle. When set, the HTTPS listener requires
    /// every client to present a cert that chains to a CA in this
    /// bundle (mTLS). When unset, plain server-side TLS only.
    pub http_tls_client_ca: Option<PathBuf>,
    pub wire_bind_addr: Option<String>,
    pub wire_bind_explicit: bool,
    /// TLS-encrypted wire protocol bind address
    pub wire_tls_bind_addr: Option<String>,
    /// Path to TLS cert PEM (if None + wire_tls_bind, auto-generate)
    pub wire_tls_cert: Option<PathBuf>,
    /// Path to TLS key PEM
    pub wire_tls_key: Option<PathBuf>,
    /// PostgreSQL wire protocol bind address (Phase 3.1 PG parity).
    /// When set the server accepts psql / JDBC / DBeaver clients on
    /// this port via the v3 protocol. Defaults to None (disabled).
    pub pg_bind_addr: Option<String>,
    pub create_if_missing: bool,
    pub read_only: bool,
    pub role: String,
    pub primary_addr: Option<String>,
    pub storage_profile: StorageProfileSelection,
    pub vault: bool,
    /// Issue #663 — explicit `--no-auth` / `--dev` flag for local
    /// no-password mode. When `true`, the boot pipeline force-disables
    /// auth (`auth.enabled = false`, `auth.require_auth = false`,
    /// `auth.vault_enabled = false`) and skips
    /// `AuthStore::bootstrap_from_env`, so an explicit
    /// `REDDB_USERNAME` + `REDDB_PASSWORD` pair cannot accidentally
    /// turn anonymous access into a logged-in admin. An unmissable
    /// warning is emitted at startup. Default `false` — the existing
    /// implicit no-auth behaviour (just don't set the envs) is
    /// unchanged.
    pub no_auth: bool,
    /// Override worker thread count (None = auto-detect from CPUs)
    pub workers: Option<usize>,
    /// Telemetry config (Phase 6 logging). `None` falls back to the
    /// built-in default derived from `path` + stderr-only.
    pub telemetry: Option<crate::telemetry::TelemetryConfig>,
    /// HTTP handler-pool knobs from the CLI layer (issue #574 slice 5).
    /// Carries flag and env values; red_config and built-in defaults
    /// are applied later by [`crate::server::http_limits::resolve_http_limits`]
    /// once the runtime is open.
    pub http_limits_cli: crate::server::HttpLimitsCliInput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportListenerState {
    pub transport: String,
    pub bind_addr: String,
    pub explicit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransportListenerFailure {
    pub transport: String,
    pub bind_addr: String,
    pub explicit: bool,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransportReadiness {
    pub active: Vec<TransportListenerState>,
    pub failed: Vec<TransportListenerFailure>,
}

impl TransportReadiness {
    fn active(&mut self, transport: &str, bind_addr: &str, explicit: bool) {
        self.active.push(TransportListenerState {
            transport: transport.to_string(),
            bind_addr: bind_addr.to_string(),
            explicit,
        });
    }

    fn failed(&mut self, transport: &str, bind_addr: &str, explicit: bool, reason: String) {
        self.failed.push(TransportListenerFailure {
            transport: transport.to_string(),
            bind_addr: bind_addr.to_string(),
            explicit,
            reason,
        });
    }
}

#[derive(Debug, Clone)]
pub struct SystemdServiceConfig {
    pub service_name: String,
    pub binary_path: PathBuf,
    pub run_user: String,
    pub run_group: String,
    pub data_path: PathBuf,
    pub router_bind_addr: Option<String>,
    pub grpc_bind_addr: Option<String>,
    pub http_bind_addr: Option<String>,
}

impl SystemdServiceConfig {
    pub fn data_dir(&self) -> PathBuf {
        self.data_path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    pub fn unit_path(&self) -> PathBuf {
        PathBuf::from(format!("/etc/systemd/system/{}.service", self.service_name))
    }
}

/// Build a sane default `TelemetryConfig` from a server path when the
/// caller didn't set one explicitly. Writes rotating logs into the
/// parent directory of the DB file (or `./logs` for in-memory /
/// pathless runs). Level defaults to `info`, pretty stderr format.
pub fn default_telemetry_for_path(
    path: Option<&std::path::Path>,
) -> crate::telemetry::TelemetryConfig {
    let log_dir = match path {
        Some(p) => p
            .parent()
            .map(|parent| parent.join("logs"))
            .or_else(|| Some(std::path::PathBuf::from("./logs"))),
        None => None, // in-memory — no file, stderr-only
    };
    crate::telemetry::TelemetryConfig {
        log_dir,
        file_prefix: "reddb.log".to_string(),
        level_filter: std::env::var("RUST_LOG").unwrap_or_else(|_| "info".to_string()),
        format: if std::io::IsTerminal::is_terminal(&std::io::stderr()) {
            crate::telemetry::LogFormat::Pretty
        } else {
            crate::telemetry::LogFormat::Json
        },
        rotation_keep_days: 14,
        service_name: "reddb",
        // Implicit defaults — no CLI flag has claimed these values yet.
        level_explicit: false,
        format_explicit: false,
        rotation_keep_days_explicit: false,
        file_prefix_explicit: false,
        log_dir_explicit: false,
        log_file_disabled: false,
    }
}

/// Metadata key used to thread the parsed backup config from
/// `to_db_options` down to runner threads. The runner reads it back
/// (via `runner_backup_intervals`) to spawn the periodic checkpointer
/// and WAL-flush tasks. Threading through `metadata` avoids extending
/// `RedDBOptions` with a public field that has no meaning for
/// library consumers.
const BACKUP_INTERVAL_META_CHECKPOINT: &str = "red.boot.backup.checkpoint_interval_secs";
const BACKUP_INTERVAL_META_WAL_FLUSH: &str = "red.boot.backup.wal_flush_interval_secs";
const BACKUP_KIND_META: &str = "red.boot.backup.backend_kind";
/// Issue #519 — threaded through `metadata` like the existing interval
/// values. `0` (default) means "feature disabled" and the runner skips
/// the lag-monitor wiring entirely.
const BACKUP_PAUSE_ON_LAG_META: &str = "red.boot.backup.pause_on_lag_secs";

/// Issue #663 — metadata key set by `to_db_options` when the operator
/// passes `--no-auth` / `--dev`. Read in `build_runtime_with_telemetry`
/// to (a) skip `AuthStore::bootstrap_from_env` (so a stray
/// `REDDB_USERNAME`/`REDDB_PASSWORD` cannot auto-create an admin) and
/// (b) emit the loud "auth disabled" warning. Threaded via `metadata`
/// — rather than a public `RedDBOptions` field — to keep the flag a
/// CLI/boot concern with no meaning for library consumers.
pub(crate) const NO_AUTH_META: &str = "red.boot.no_auth";

/// Returns `true` when `--no-auth` / `--dev` was active for this boot,
/// i.e. when `to_db_options` stamped [`NO_AUTH_META`] on `options.metadata`.
pub(crate) fn no_auth_active(options: &RedDBOptions) -> bool {
    options
        .metadata
        .get(NO_AUTH_META)
        .map(|v| v == "true")
        .unwrap_or(false)
}

/// Loud, unmissable warning printed when the operator opts into
/// anonymous access via `--no-auth` / `--dev`. Goes to stderr (always
/// visible at startup) **and** the tracing layer (captured by file
/// logs once telemetry is wired).
const NO_AUTH_WARNING: &str =
    "⚠ auth disabled (--no-auth) — anonymous access, do NOT use in production";

impl ServerCommandConfig {
    fn to_db_options(&self) -> Result<RedDBOptions, String> {
        let mut options = match &self.path {
            Some(path) => RedDBOptions::persistent(path),
            None => RedDBOptions::in_memory(),
        };

        options.mode = StorageMode::Persistent;
        options.create_if_missing = self.create_if_missing;
        // PLAN.md Phase 4.3 — read_only resolution priority:
        //   1. CLI flag (`--readonly`) — explicit operator intent.
        //   2. `RED_READONLY=true` env — orchestrator override.
        //   3. Persisted `<data>/.runtime-state.json` from a prior
        //      `POST /admin/readonly` — survives restart.
        //   4. Default `false`.
        options.read_only = self.read_only
            || env_nonempty("RED_READONLY")
                .or_else(|| env_nonempty("REDDB_READONLY"))
                .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
                .unwrap_or(false)
            || self.path.as_ref().is_some_and(|data_path| {
                crate::server::handlers_admin::load_runtime_readonly(std::path::Path::new(
                    data_path,
                ))
                .unwrap_or(false)
            });

        options.replication = match self.role.as_str() {
            "primary" => ReplicationConfig::primary(),
            "replica" => {
                let primary_addr = self
                    .primary_addr
                    .clone()
                    .unwrap_or_else(|| "http://127.0.0.1:5555".to_string());
                // Public-mutation rejection on replicas is enforced by
                // `WriteGate` at the runtime/RPC boundary (PLAN.md W1).
                // Leaving `options.read_only = false` keeps the pager
                // writable so the internal logical-WAL apply path can
                // ingest records from the primary; WriteGate ensures no
                // client request reaches storage.
                ReplicationConfig::replica(primary_addr)
            }
            _ => ReplicationConfig::standalone(),
        };
        options.storage_profile = self.storage_profile.validate()?;

        if self.vault {
            options.auth.vault_enabled = true;
        }

        // Issue #663 — `--no-auth` / `--dev` is the last word on auth
        // for this boot: force every auth knob off, regardless of any
        // env-derived config (`--vault`, `REDDB_USERNAME`/`PASSWORD`,
        // `REDDB_VAULT_KEY`, OAuth, cert) the operator may also have
        // set. We *also* stamp [`NO_AUTH_META`] so the auth-store
        // builder downstream knows to skip `bootstrap_from_env`
        // (which would otherwise auto-create an admin from
        // `REDDB_USERNAME`/`REDDB_PASSWORD` even with auth disabled,
        // a footgun for the local-dev workflow this flag exists to
        // support).
        if self.no_auth {
            options.auth.enabled = false;
            options.auth.require_auth = false;
            options.auth.vault_enabled = false;
            options
                .metadata
                .insert(NO_AUTH_META.to_string(), "true".to_string());
        }

        // Issue #652 — Control Event Ledger Compliance Mode.
        // `REDDB_COMPLIANCE_MODE=true|1|yes|on` makes the producer
        // slices (652b/c/d) fail-closed on ledger persistence
        // failures. Default `false` — log-and-continue on emit error.
        if let Some(raw) = env_nonempty("REDDB_COMPLIANCE_MODE") {
            options.control_events.compliance_mode = matches!(
                raw.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            );
        }
        if env_nonempty(PRESET_ENV).is_some_and(|s| s.trim() == PRESET_REGULATED) {
            options.control_events.compliance_mode = true;
            options.query_audit = crate::runtime::query_audit::QueryAuditConfig::regulated();
        }

        // Issue #517 — canonical `REDDB_BACKUP_*` contract takes
        // precedence. On Err, surface the partial-config message so
        // boot exits non-zero with a clear operator message. On
        // Ok(None), fall through to the legacy backend-from-env path.
        match crate::backup_bootstrap::from_env(|k| std::env::var(k).ok()) {
            Err(msg) => {
                return Err(format!("backup bootstrap: {msg}"));
            }
            Ok(Some(cfg)) => {
                apply_backup_config(&mut options, &cfg);
            }
            Ok(None) => {
                configure_remote_backend_from_env(&mut options);
            }
        }

        if options.remote_backend.is_some()
            || options
                .metadata
                .get(BACKUP_INTERVAL_META_CHECKPOINT)
                .is_some()
        {
            let mut selection = options.storage_profile;
            selection.managed_backup = true;
            options.storage_profile = selection.validate()?;
        }

        Ok(options)
    }

    pub fn enabled_transports(&self) -> Vec<ServerTransport> {
        let mut transports = Vec::with_capacity(3);
        if self.router_bind_addr.is_some() || self.grpc_bind_addr.is_some() {
            transports.push(ServerTransport::Grpc);
        }
        if self.router_bind_addr.is_some() || self.http_bind_addr.is_some() {
            transports.push(ServerTransport::Http);
        }
        if self.router_bind_addr.is_some() || self.wire_bind_addr.is_some() {
            transports.push(ServerTransport::Wire);
        }
        transports
    }
}

/// Read an env var, treating empty / whitespace-only as `None`.
/// Honors the `<NAME>_FILE` convention. Re-exports the shared
/// `crate::utils::env_with_file_fallback` helper so call sites in
/// this module can keep their short local name.
fn env_nonempty(name: &str) -> Option<String> {
    crate::utils::env_with_file_fallback(name)
}

fn env_truthy(name: &str) -> bool {
    env_nonempty(name)
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
}

/// Apply a parsed [`BackupConfig`] to `options`. Wires the S3
/// backend via `with_remote_backend` + `with_atomic_remote_backend`
/// when the `backend-s3` feature is on, stashes intervals + backend
/// kind in `metadata` so the runner can spawn the periodic tasks,
/// and emits the startup INFO log required by #517.
fn apply_backup_config(options: &mut RedDBOptions, cfg: &crate::backup_bootstrap::BackupConfig) {
    let endpoint_host = endpoint_host(&cfg.endpoint);

    options.metadata.insert(
        BACKUP_INTERVAL_META_CHECKPOINT.to_string(),
        cfg.checkpoint_interval_secs.to_string(),
    );
    options.metadata.insert(
        BACKUP_INTERVAL_META_WAL_FLUSH.to_string(),
        cfg.wal_flush_interval_secs.to_string(),
    );
    options
        .metadata
        .insert(BACKUP_KIND_META.to_string(), "s3".to_string());
    options.metadata.insert(
        BACKUP_PAUSE_ON_LAG_META.to_string(),
        cfg.pause_on_lag_secs.to_string(),
    );

    #[cfg(feature = "backend-s3")]
    {
        let s3_cfg = crate::storage::backend::S3Config {
            endpoint: cfg.endpoint.clone(),
            bucket: cfg.bucket.clone(),
            key_prefix: cfg.prefix.clone(),
            access_key: cfg.access_key_id.clone(),
            secret_key: cfg.secret_access_key.clone(),
            region: cfg.region.clone(),
            path_style: true,
        };
        let backend = Arc::new(crate::storage::backend::S3Backend::new(s3_cfg));
        options.remote_backend = Some(backend.clone());
        options.remote_backend_atomic = Some(backend);
        // Use the operator-supplied prefix as the snapshot key root.
        // The existing helpers (`default_snapshot_prefix`,
        // `default_wal_archive_prefix`) derive sub-prefixes from the
        // parent of `remote_key`.
        let trimmed = cfg.prefix.trim_end_matches('/');
        options.remote_key = Some(format!("{}/data.rdb", trimmed));

        tracing::info!(
            backend = "s3",
            endpoint = %endpoint_host,
            bucket = %cfg.bucket,
            prefix = %cfg.prefix,
            checkpoint_interval_secs = cfg.checkpoint_interval_secs,
            wal_flush_interval_secs = cfg.wal_flush_interval_secs,
            "backup backend configured from REDDB_BACKUP_* env"
        );
    }

    #[cfg(not(feature = "backend-s3"))]
    {
        tracing::warn!(
            backend = "s3",
            endpoint = %endpoint_host,
            bucket = %cfg.bucket,
            prefix = %cfg.prefix,
            "REDDB_BACKUP_S3_* configured but binary built without `backend-s3` feature; \
             backend wiring skipped (archiver/checkpointer also disabled)"
        );
    }
}

fn endpoint_host(endpoint: &str) -> &str {
    let after_scheme = endpoint
        .split_once("://")
        .map(|(_, r)| r)
        .unwrap_or(endpoint);
    after_scheme.split('/').next().unwrap_or(after_scheme)
}

/// If `options` carry backup-task intervals threaded in via
/// [`apply_backup_config`], spawn periodic checkpointer + WAL-flush
/// tasks against `runtime`. Returns a `BackupTasksHandle` that
/// stops the tasks when dropped; runners keep it alive for the
/// server lifetime.
fn spawn_backup_tasks_if_configured(
    options: &RedDBOptions,
    runtime: &RedDBRuntime,
) -> Option<BackupTasksHandle> {
    let checkpoint_secs: u64 = options
        .metadata
        .get(BACKUP_INTERVAL_META_CHECKPOINT)?
        .parse()
        .ok()?;
    let wal_secs: u64 = options
        .metadata
        .get(BACKUP_INTERVAL_META_WAL_FLUSH)?
        .parse()
        .ok()?;
    // Issue #519 — opt-in graceful read-only when remote archive lag
    // exceeds the threshold. `0` (default) keeps legacy behaviour.
    let pause_on_lag_secs: u64 = options
        .metadata
        .get(BACKUP_PAUSE_ON_LAG_META)
        .and_then(|raw| raw.parse().ok())
        .unwrap_or(0);
    options.remote_backend.as_ref()?;

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Stamp the gate with the threshold + a "now" baseline so the
    // first auto-pause can only fire after `pause_on_lag_secs` of
    // archive silence. The poller below re-evaluates on the same
    // clock the archive-task wrapper uses.
    if pause_on_lag_secs > 0 {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        runtime
            .write_gate()
            .configure_archive_lag_pause(pause_on_lag_secs, now_ms);
        tracing::info!(
            pause_on_lag_secs,
            "archive-lag pause enabled — engine will transition to read-only after threshold seconds of archiver silence"
        );
    }

    let checkpoint_handle = {
        let stop = Arc::clone(&stop);
        let runtime = runtime.clone();
        let interval = Duration::from_secs(checkpoint_secs);
        thread::Builder::new()
            .name("red-checkpointer".into())
            .spawn(move || {
                periodic_loop(stop, interval, move || {
                    if let Err(err) = runtime.checkpoint() {
                        tracing::warn!(error = %err, "periodic checkpoint failed");
                    }
                })
            })
            .ok()
    };

    let archiver_handle = {
        let stop = Arc::clone(&stop);
        let runtime = runtime.clone();
        let interval = Duration::from_secs(wal_secs);
        let lag_enabled = pause_on_lag_secs > 0;
        thread::Builder::new()
            .name("red-wal-archiver".into())
            .spawn(move || {
                periodic_loop(stop, interval, move || match runtime.trigger_backup() {
                    Ok(_) if lag_enabled => {
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_millis() as u64)
                            .unwrap_or(0);
                        runtime.write_gate().record_archive_success(now_ms);
                        // Same-tick re-evaluation: catching up while
                        // already auto-paused must auto-resume without
                        // waiting for the poller's cadence.
                        runtime.write_gate().evaluate_archive_lag(now_ms);
                    }
                    Ok(_) => {}
                    Err(err) => {
                        tracing::warn!(error = %err, "periodic WAL archive/backup failed");
                    }
                })
            })
            .ok()
    };

    // Issue #519 — lag poller. Wakes on a short cadence so a frozen
    // archiver (the worst case) still flips the gate within ~5s of
    // crossing the threshold, instead of waiting up to a full
    // `wal_secs` for the next archive attempt that may never come.
    let lag_monitor_handle = if pause_on_lag_secs > 0 {
        let stop = Arc::clone(&stop);
        let runtime = runtime.clone();
        // 5s is short enough that the threshold is honoured tightly
        // and long enough that the atomic loads stay invisible at the
        // process level.
        let interval = Duration::from_secs(5);
        thread::Builder::new()
            .name("red-archive-lag-monitor".into())
            .spawn(move || {
                periodic_loop(stop, interval, move || {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    let was_paused = runtime.write_gate().is_auto_paused();
                    let now_paused = runtime.write_gate().evaluate_archive_lag(now_ms);
                    if now_paused && !was_paused {
                        tracing::warn!(
                            pause_on_lag_secs,
                            last_archive_at_ms = runtime.write_gate().last_archive_at_ms(),
                            "WAL archive lag exceeded threshold — entering graceful read-only mode (issue #519)"
                        );
                    } else if !now_paused && was_paused {
                        tracing::info!(
                            "WAL archive caught up — exiting graceful read-only mode (issue #519)"
                        );
                    }
                })
            })
            .ok()
    } else {
        None
    };

    tracing::info!(
        checkpoint_interval_secs = checkpoint_secs,
        wal_flush_interval_secs = wal_secs,
        "backup tasks spawned (checkpointer + WAL archiver)"
    );

    Some(BackupTasksHandle {
        stop,
        _checkpoint_handle: checkpoint_handle,
        _archiver_handle: archiver_handle,
        _lag_monitor_handle: lag_monitor_handle,
    })
}

/// Shutdown handle for the periodic checkpointer + archiver tasks.
/// Drop signals both loops to exit on their next wake.
pub struct BackupTasksHandle {
    stop: Arc<std::sync::atomic::AtomicBool>,
    _checkpoint_handle: Option<thread::JoinHandle<()>>,
    _archiver_handle: Option<thread::JoinHandle<()>>,
    /// Issue #519 — periodic archive-lag poller, only spawned when
    /// `REDDB_BACKUP_PAUSE_ON_LAG_SECS > 0`.
    _lag_monitor_handle: Option<thread::JoinHandle<()>>,
}

impl Drop for BackupTasksHandle {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
    }
}

fn periodic_loop<F: FnMut()>(
    stop: Arc<std::sync::atomic::AtomicBool>,
    interval: Duration,
    mut tick: F,
) {
    // Wake on a short cadence so shutdown is responsive even when the
    // operator-configured interval is large (e.g. 1h checkpoint).
    let wake = Duration::from_secs(1);
    let mut elapsed = Duration::ZERO;
    while !stop.load(std::sync::atomic::Ordering::Acquire) {
        thread::sleep(wake);
        elapsed += wake;
        if elapsed >= interval {
            tick();
            elapsed = Duration::ZERO;
        }
    }
}

fn configure_remote_backend_from_env(options: &mut RedDBOptions) {
    // PLAN.md (cloud-agnostic) — prefer the new spelling
    // `RED_BACKEND`; accept the legacy `REDDB_REMOTE_BACKEND` for
    // existing dev installs. `none` (default) means standalone — no
    // remote backend, valid for development and on-prem without
    // remote.
    let backend = env_nonempty("RED_BACKEND")
        .or_else(|| env_nonempty("REDDB_REMOTE_BACKEND"))
        .unwrap_or_else(|| "none".to_string())
        .to_ascii_lowercase();

    match backend.as_str() {
        // Universal S3-compatible — covers AWS, R2, MinIO, Ceph,
        // GCS-interop, B2, DO Spaces, Wasabi, Garage, SeaweedFS,
        // IDrive, Storj. The `path_style` toggle in S3Config picks
        // the right addressing for self-hosted vs hosted.
        "s3" | "minio" | "r2" => {
            #[cfg(feature = "backend-s3")]
            {
                if let Some(config) = s3_config_from_env() {
                    let remote_key = env_nonempty("RED_REMOTE_KEY")
                        .or_else(|| env_nonempty("REDDB_REMOTE_KEY"))
                        .unwrap_or_else(|| "clusters/dev/data.rdb".to_string());
                    let backend = Arc::new(crate::storage::backend::S3Backend::new(config));
                    options.remote_backend = Some(backend.clone());
                    options.remote_backend_atomic = Some(backend);
                    options.remote_key = Some(remote_key);
                }
            }
            #[cfg(not(feature = "backend-s3"))]
            {
                tracing::warn!(
                    backend = %backend,
                    "RED_BACKEND={backend} requested but binary was built without `backend-s3` feature"
                );
            }
        }
        // Filesystem backend (NFS/EFS/SMB/local-disk). PLAN.md spec
        // calls it `fs`; legacy code shipped it as `local`. Both
        // names map to LocalBackend, with the remote_key derived
        // from `RED_FS_PATH` + a per-database suffix when provided.
        "fs" | "local" => {
            let base_path = env_nonempty("RED_FS_PATH").or_else(|| env_nonempty("REDDB_FS_PATH"));
            let remote_key = match (
                base_path,
                env_nonempty("RED_REMOTE_KEY").or_else(|| env_nonempty("REDDB_REMOTE_KEY")),
            ) {
                (Some(base), Some(rel)) => Some(format!(
                    "{}/{}",
                    base.trim_end_matches('/'),
                    rel.trim_start_matches('/')
                )),
                (Some(base), None) => Some(format!(
                    "{}/clusters/dev/data.rdb",
                    base.trim_end_matches('/')
                )),
                (None, Some(rel)) => Some(rel),
                (None, None) => None,
            };
            if let Some(key) = remote_key {
                let backend = Arc::new(crate::storage::backend::LocalBackend);
                options.remote_backend = Some(backend.clone());
                options.remote_backend_atomic = Some(backend);
                options.remote_key = Some(key);
            }
        }
        // Generic HTTP backend (PLAN.md Phase 2.3). Maximum
        // portability: any service exposing PUT/GET/DELETE serves
        // as a backup target. Optional auth via *_FILE secret
        // path keeps the token out of the env.
        "http" => {
            let base_url = match env_nonempty("RED_HTTP_BACKEND_URL")
                .or_else(|| env_nonempty("REDDB_HTTP_BACKEND_URL"))
            {
                Some(u) => u,
                None => {
                    tracing::warn!(
                        "RED_BACKEND=http requires RED_HTTP_BACKEND_URL — backend disabled"
                    );
                    return;
                }
            };
            let prefix = env_nonempty("RED_HTTP_BACKEND_PREFIX")
                .or_else(|| env_nonempty("REDDB_HTTP_BACKEND_PREFIX"))
                .unwrap_or_default();
            let auth_header = if let Some(path) = env_nonempty("RED_HTTP_BACKEND_AUTH_HEADER_FILE")
                .or_else(|| env_nonempty("REDDB_HTTP_BACKEND_AUTH_HEADER_FILE"))
            {
                std::fs::read_to_string(&path)
                    .ok()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
            } else {
                env_nonempty("RED_HTTP_BACKEND_AUTH_HEADER")
                    .or_else(|| env_nonempty("REDDB_HTTP_BACKEND_AUTH_HEADER"))
            };

            let mut config =
                crate::storage::backend::HttpBackendConfig::new(base_url).with_prefix(prefix);
            if let Some(auth) = auth_header {
                config = config.with_auth_header(auth);
            }
            let conditional_writes = env_truthy("RED_HTTP_CONDITIONAL_WRITES")
                || env_truthy("RED_HTTP_BACKEND_CONDITIONAL_WRITES")
                || env_truthy("REDDB_HTTP_BACKEND_CONDITIONAL_WRITES");
            config = config.with_conditional_writes(conditional_writes);
            // Always populate the snapshot-transport handle. When the
            // operator confirmed CAS support, also populate the atomic
            // handle via AtomicHttpBackend — without that flag,
            // LeaseStore must remain unreachable on this backend.
            if conditional_writes {
                match crate::storage::backend::AtomicHttpBackend::try_new(config.clone()) {
                    Ok(atomic) => {
                        let atomic_arc = Arc::new(atomic);
                        options.remote_backend = Some(atomic_arc.clone());
                        options.remote_backend_atomic = Some(atomic_arc);
                    }
                    Err(err) => {
                        tracing::warn!(error = %err, "AtomicHttpBackend init failed; falling back to plain HTTP (no CAS)");
                        options.remote_backend =
                            Some(Arc::new(crate::storage::backend::HttpBackend::new(config)));
                    }
                }
            } else {
                options.remote_backend =
                    Some(Arc::new(crate::storage::backend::HttpBackend::new(config)));
            }
            options.remote_key = env_nonempty("RED_REMOTE_KEY")
                .or_else(|| env_nonempty("REDDB_REMOTE_KEY"))
                .or_else(|| Some("clusters/dev/data.rdb".to_string()));
        }
        // `none` is the explicit standalone — no remote, no backup
        // pipeline. Boot never blocks on network reachability.
        "none" | "" => {}
        other => {
            tracing::warn!(
                backend = %other,
                "unknown RED_BACKEND value — supported: s3 | fs | http | none"
            );
        }
    }
}

/// Resolve a value from env, accepting both the cloud-agnostic
/// `RED_S3_*` family (PLAN.md spec) and the legacy `REDDB_S3_*` form
/// kept for existing dev installs. The new spelling wins; the
/// legacy spelling is read with a warning hint in callers' logs.
#[cfg(feature = "backend-s3")]
fn env_s3(suffix: &str) -> Option<String> {
    env_nonempty(&format!("RED_S3_{suffix}"))
        .or_else(|| env_nonempty(&format!("REDDB_S3_{suffix}")))
}

/// Read a secret value from either the literal env var or a file
/// path supplied via `*_FILE` (PLAN.md spec — compatible with
/// Kubernetes / Docker Secrets, HashiCorp Vault Agent, sealed-
/// secrets). The `_FILE` variant wins so an operator can set it to
/// override the inline value without touching the inline env.
#[cfg(feature = "backend-s3")]
fn env_s3_secret(suffix: &str) -> Option<String> {
    let file_key_red = format!("RED_S3_{suffix}_FILE");
    let file_key_legacy = format!("REDDB_S3_{suffix}_FILE");
    if let Some(path) = env_nonempty(&file_key_red).or_else(|| env_nonempty(&file_key_legacy)) {
        return std::fs::read_to_string(&path)
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
    }
    env_s3(suffix)
}

#[cfg(feature = "backend-s3")]
fn s3_config_from_env() -> Option<crate::storage::backend::S3Config> {
    let endpoint = env_s3("ENDPOINT")?;
    let bucket = env_s3("BUCKET")?;
    let access_key = env_s3_secret("ACCESS_KEY")?;
    let secret_key = env_s3_secret("SECRET_KEY")?;
    let region = env_s3("REGION").unwrap_or_else(|| "us-east-1".to_string());
    let key_prefix = env_s3("KEY_PREFIX")
        .or_else(|| env_s3("PREFIX"))
        .unwrap_or_default();
    let path_style = env_s3("PATH_STYLE")
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(true);
    Some(crate::storage::backend::S3Config {
        endpoint,
        bucket,
        key_prefix,
        access_key,
        secret_key,
        region,
        path_style,
    })
}

pub fn render_systemd_unit(config: &SystemdServiceConfig) -> String {
    let data_dir = config.data_dir();
    let exec_start = render_systemd_exec_start(config);
    format!(
        "[Unit]\n\
Description=RedDB unified database service\n\
After=network-online.target\n\
Wants=network-online.target\n\
\n\
[Service]\n\
Type=simple\n\
User={user}\n\
Group={group}\n\
WorkingDirectory={workdir}\n\
ExecStart={exec_start}\n\
Restart=always\n\
RestartSec=2\n\
LimitSTACK=16M\n\
NoNewPrivileges=true\n\
PrivateTmp=true\n\
ProtectSystem=strict\n\
ProtectHome=true\n\
ProtectControlGroups=true\n\
ProtectKernelTunables=true\n\
ProtectKernelModules=true\n\
RestrictNamespaces=true\n\
LockPersonality=true\n\
MemoryDenyWriteExecute=true\n\
ReadWritePaths={workdir}\n\
\n\
[Install]\n\
WantedBy=multi-user.target\n",
        user = config.run_user,
        group = config.run_group,
        workdir = data_dir.display(),
        exec_start = exec_start,
    )
}

/// Install a systemd unit + start the service.
///
/// Linux-only. The helper shells out to `systemctl`, `useradd`,
/// `groupadd`, `install`, `getent`, and `id` — none of which exist on
/// Windows or macOS. The Windows/macOS branch returns a hard error so
/// callers (the CLI) surface a clear message instead of a confusing
/// syscall failure. A proper Windows-service equivalent (sc.exe /
/// NSSM) is a Phase 3.6 follow-up.
#[cfg(target_os = "linux")]
pub fn install_systemd_service(config: &SystemdServiceConfig) -> Result<(), String> {
    ensure_root()?;
    ensure_command_available("systemctl")?;
    ensure_command_available("getent")?;
    ensure_command_available("groupadd")?;
    ensure_command_available("useradd")?;
    ensure_command_available("install")?;
    ensure_executable(&config.binary_path)?;

    if !command_success("getent", ["group", config.run_group.as_str()])? {
        run_command("groupadd", ["--system", config.run_group.as_str()])?;
    }

    if !command_success("id", ["-u", config.run_user.as_str()])? {
        let data_dir = config.data_dir();
        run_command(
            "useradd",
            [
                "--system",
                "--gid",
                config.run_group.as_str(),
                "--home-dir",
                data_dir.to_string_lossy().as_ref(),
                "--shell",
                "/usr/sbin/nologin",
                config.run_user.as_str(),
            ],
        )?;
    }

    let data_dir = config.data_dir();
    run_command(
        "install",
        [
            "-d",
            "-o",
            config.run_user.as_str(),
            "-g",
            config.run_group.as_str(),
            "-m",
            "0750",
            data_dir.to_string_lossy().as_ref(),
        ],
    )?;

    std::fs::write(config.unit_path(), render_systemd_unit(config))
        .map_err(|err| format!("failed to write systemd unit: {err}"))?;

    run_command("systemctl", ["daemon-reload"])?;
    run_command(
        "systemctl",
        [
            "enable",
            "--now",
            format!("{}.service", config.service_name).as_str(),
        ],
    )?;

    Ok(())
}

/// Non-Linux fallback — systemd is Linux-specific. Keep the signature
/// identical so callers compile on every platform; surface a clear
/// error at runtime. Windows/macOS service-manager integration is a
/// Phase 3.6 follow-up (sc.exe + NSSM on Windows, launchd on macOS).
#[cfg(not(target_os = "linux"))]
pub fn install_systemd_service(_config: &SystemdServiceConfig) -> Result<(), String> {
    Err("systemd install is Linux-only — use sc.exe (Windows) or \
         launchd (macOS) to install the service manually using the \
         unit printed by `red service print-unit`"
        .to_string())
}

#[cfg(target_os = "linux")]
fn ensure_root() -> Result<(), String> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .map_err(|err| format!("failed to determine current uid: {err}"))?;
    if !output.status.success() {
        return Err("failed to determine current uid".to_string());
    }
    let uid = String::from_utf8_lossy(&output.stdout);
    if uid.trim() != "0" {
        return Err("run this command as root (sudo)".to_string());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn ensure_command_available(command: &str) -> Result<(), String> {
    let status = Command::new("sh")
        .args(["-lc", &format!("command -v {command} >/dev/null 2>&1")])
        .status()
        .map_err(|err| format!("failed to check command '{command}': {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("required command not found: {command}"))
    }
}

#[cfg(target_os = "linux")]
fn ensure_executable(path: &std::path::Path) -> Result<(), String> {
    let metadata = std::fs::metadata(path)
        .map_err(|err| format!("binary not found '{}': {err}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(format!("binary is not executable: {}", path.display()));
        }
    }
    #[cfg(not(unix))]
    {
        if !metadata.is_file() {
            return Err(format!("binary is not a file: {}", path.display()));
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn command_success<const N: usize>(program: &str, args: [&str; N]) -> Result<bool, String> {
    Command::new(program)
        .args(args)
        .status()
        .map(|status| status.success())
        .map_err(|err| format!("failed to run {program}: {err}"))
}

#[cfg(target_os = "linux")]
fn run_command<const N: usize>(program: &str, args: [&str; N]) -> Result<(), String> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|err| format!("failed to run {program}: {err}"))?;
    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if !stderr.is_empty() {
        stderr
    } else if !stdout.is_empty() {
        stdout
    } else {
        format!("exit status {}", output.status)
    };
    Err(format!("{program} failed: {detail}"))
}

pub fn run_server_with_large_stack(config: ServerCommandConfig) -> Result<(), String> {
    let has_any = config.router_bind_addr.is_some()
        || config.grpc_bind_addr.is_some()
        || config.http_bind_addr.is_some()
        || config.wire_bind_addr.is_some()
        || config.pg_bind_addr.is_some();
    if !has_any {
        return Err("at least one server bind address must be configured".into());
    }
    let thread_name = if config.router_bind_addr.is_some() {
        "red-server-router"
    } else {
        match (
            config.grpc_bind_addr.is_some(),
            config.http_bind_addr.is_some(),
        ) {
            (true, true) => "red-server-dual",
            (true, false) => "red-server-grpc",
            (false, true) => "red-server-http",
            (false, false) if config.wire_bind_addr.is_some() => "red-server-wire",
            (false, false) => "red-server-pg-wire",
        }
    };

    let handle = thread::Builder::new()
        .name(thread_name.into())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || run_configured_servers(config))
        .map_err(|err| format!("failed to spawn server thread: {err}"))?;

    match handle.join() {
        Ok(result) => result,
        Err(_) => Err("server thread panicked".to_string()),
    }
}

fn render_systemd_exec_start(config: &SystemdServiceConfig) -> String {
    let mut parts = vec![
        config.binary_path.display().to_string(),
        "server".to_string(),
        "--path".to_string(),
        config.data_path.display().to_string(),
    ];

    if let Some(bind_addr) = &config.router_bind_addr {
        parts.push("--bind".to_string());
        parts.push(bind_addr.clone());
    } else if let Some(bind_addr) = &config.grpc_bind_addr {
        parts.push("--grpc-bind".to_string());
        parts.push(bind_addr.clone());
    }
    if let Some(bind_addr) = &config.http_bind_addr {
        parts.push("--http-bind".to_string());
        parts.push(bind_addr.clone());
    }

    parts.join(" ")
}

pub fn probe_listener(target: &str, timeout: Duration) -> bool {
    let addresses: Vec<SocketAddr> = match target.to_socket_addrs() {
        Ok(addresses) => addresses.collect(),
        Err(_) => return false,
    };

    addresses
        .into_iter()
        .any(|address| TcpStream::connect_timeout(&address, timeout).is_ok())
}

#[inline(never)]
fn run_configured_servers(config: ServerCommandConfig) -> Result<(), String> {
    // Phase 6 logging is initialised inside each runner once the
    // runtime is open — see `build_runtime_and_auth_store`. Going
    // after DB open lets us read `red.logging.*` config keys out of
    // the persistent red_config store and merge them with the CLI
    // flags (flag > red_config > built-in default).
    if let Some(router_bind_addr) = config.router_bind_addr.clone() {
        return run_routed_server(config, router_bind_addr);
    }

    match (config.grpc_bind_addr.clone(), config.http_bind_addr.clone()) {
        (Some(grpc_bind_addr), Some(http_bind_addr)) => {
            run_dual_server(config, grpc_bind_addr, http_bind_addr)
        }
        (Some(grpc_bind_addr), None) => run_grpc_server(config, grpc_bind_addr),
        (None, Some(http_bind_addr)) => run_http_server(config, http_bind_addr),
        (None, None) => {
            if let Some(wire_addr) = config.wire_bind_addr.clone() {
                run_wire_only_server(config, wire_addr)
            } else if let Some(pg_addr) = config.pg_bind_addr.clone() {
                run_pg_only_server(config, pg_addr)
            } else {
                Err("at least one server bind address must be configured".to_string())
            }
        }
    }
}

/// Bind a TCP listener for a transport at startup and record the
/// outcome in the shared [`TransportReadiness`] state.
///
/// The split between *explicit* and *implicit/default* binds is the
/// contract from issue #545:
///
/// * `explicit == true` — the operator named this transport on the
///   CLI / env / config. A failed bind is fatal: this returns `Err`
///   and the boot path must propagate the error so the process exits
///   non-zero with the recorded `reason`.
/// * `explicit == false` — this is a default listener the server
///   would have spun up regardless. A failed bind degrades: this
///   returns `Ok(None)` (no listener) but the failure is still
///   recorded in `readiness.failed`, so the server keeps running and
///   the next `/health` probe enumerates the degraded listener.
///
/// On success the bound listener lands in `readiness.active`.
pub fn bind_listener_for_startup(
    readiness: &mut TransportReadiness,
    transport: &str,
    bind_addr: &str,
    explicit: bool,
) -> Result<Option<TcpListener>, String> {
    match TcpListener::bind(bind_addr) {
        Ok(listener) => {
            readiness.active(transport, bind_addr, explicit);
            Ok(Some(listener))
        }
        Err(err) => {
            let reason = format!("{transport} listener bind {bind_addr}: {err}");
            readiness.failed(transport, bind_addr, explicit, reason.clone());
            if explicit {
                tracing::error!(
                    transport,
                    bind = %bind_addr,
                    error = %err,
                    "fatal explicit bind failure"
                );
                Err(format!("explicit {reason}"))
            } else {
                tracing::warn!(
                    transport,
                    bind = %bind_addr,
                    error = %err,
                    "non-fatal implicit bind failure; listener degraded"
                );
                Ok(None)
            }
        }
    }
}

/// Wire SIGTERM and SIGINT to `RedDBRuntime::graceful_shutdown`.
///
/// PLAN.md Phase 1.1 — orchestrators (K8s preStop, Fly autostop, ECS
/// drain, systemd, plain `docker stop`) all rely on SIGTERM with a
/// grace window. SIGKILL after that grace window is the OS's
/// fallback; we are responsible for finishing in time.
///
/// Spawns a tokio task on the caller's runtime that:
///   1. Awaits the first of SIGTERM / SIGINT.
///   2. Calls `runtime.graceful_shutdown(backup_on_shutdown)`. The
///      runtime moves to `Stopped` on its own; this just runs the
///      flush + checkpoint pipeline and (optionally) a final backup.
///   3. Calls `std::process::exit(0)` so the orchestrator sees a
///      clean exit code.
///
/// `RED_BACKUP_ON_SHUTDOWN` (default `true` if a remote backend is
/// configured) toggles step 3's backup branch. The flush + checkpoint
/// always run.
///
/// Idempotent across signal storms — `graceful_shutdown` returns the
/// cached report on second call, but we exit on the first one
/// regardless, so the second SIGTERM never reaches the handler.
async fn spawn_lifecycle_signal_handler(runtime: RedDBRuntime) {
    let backup_on_shutdown = std::env::var("RED_BACKUP_ON_SHUTDOWN")
        .ok()
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(true);

    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sigterm = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "could not install SIGTERM handler; orchestrator graceful shutdown will fall back to SIGKILL"
                );
                return;
            }
        };
        let mut sigint = match signal(SignalKind::interrupt()) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!(error = %err, "could not install SIGINT handler");
                return;
            }
        };
        // PLAN.md Phase 6.4 — SIGHUP triggers a reload of secrets from
        // their `_FILE` companions without restarting the process.
        // Useful for credential rotation pipelines (kubectl create
        // secret + kubectl rollout restart, but for systemd / Nomad /
        // bare-metal where rolling the process is heavier).
        let mut sighup = match signal(SignalKind::hangup()) {
            Ok(s) => Some(s),
            Err(err) => {
                tracing::warn!(error = %err, "could not install SIGHUP handler; secret reload via signal disabled");
                None
            }
        };

        let reload_runtime = runtime.clone();
        tokio::spawn(async move {
            loop {
                let signal_name = match &mut sighup {
                    Some(hup) => tokio::select! {
                        _ = sigterm.recv() => "SIGTERM",
                        _ = sigint.recv() => "SIGINT",
                        _ = hup.recv() => "SIGHUP",
                    },
                    None => tokio::select! {
                        _ = sigterm.recv() => "SIGTERM",
                        _ = sigint.recv() => "SIGINT",
                    },
                };

                if signal_name == "SIGHUP" {
                    handle_sighup_reload(&reload_runtime);
                    continue; // stay alive; SIGHUP isn't a shutdown
                }

                tracing::info!(
                    signal = signal_name,
                    "lifecycle signal received; shutting down"
                );
                match runtime.graceful_shutdown(backup_on_shutdown) {
                    Ok(report) => {
                        tracing::info!(
                            duration_ms = report.duration_ms,
                            flushed_wal = report.flushed_wal,
                            final_checkpoint = report.final_checkpoint,
                            backup_uploaded = report.backup_uploaded,
                            "graceful shutdown complete"
                        );
                    }
                    Err(err) => {
                        tracing::error!(error = %err, "graceful shutdown failed");
                        // Issue #205 — graceful shutdown returning Err
                        // means the runtime is exiting without a clean
                        // flush/checkpoint. Operator-grade event so the
                        // operator notices the dirty exit even when the
                        // process restarts before they read tracing logs.
                        crate::telemetry::operator_event::OperatorEvent::ShutdownForced {
                            reason: format!("graceful shutdown failed: {err}"),
                        }
                        .emit_global();
                    }
                }
                std::process::exit(0);
            }
        });
    }

    #[cfg(not(unix))]
    {
        tokio::spawn(async move {
            let interrupted = tokio::signal::ctrl_c().await;
            if let Err(err) = interrupted {
                tracing::warn!(error = %err, "could not install Ctrl+C handler");
                return;
            }

            tracing::info!(
                signal = "Ctrl+C",
                "lifecycle signal received; shutting down"
            );
            match runtime.graceful_shutdown(backup_on_shutdown) {
                Ok(report) => {
                    tracing::info!(
                        duration_ms = report.duration_ms,
                        flushed_wal = report.flushed_wal,
                        final_checkpoint = report.final_checkpoint,
                        backup_uploaded = report.backup_uploaded,
                        "graceful shutdown complete"
                    );
                }
                Err(err) => {
                    tracing::error!(error = %err, "graceful shutdown failed");
                }
            }
            std::process::exit(0);
        });
    }
}

/// PLAN.md Phase 6.4 — re-read secrets from `*_FILE` companion env
/// vars. Today this only refreshes the audit log + records the
/// reload event; the runtime modules that hold cached secret
/// material (S3 backend credentials, admin token, JWT keys) read
/// the env on each request so the next call after SIGHUP picks up
/// the new file contents automatically. A future extension can
/// punch through to the LeaseStore / AuthStore for in-memory
/// caches that don't re-read on each call.
fn handle_sighup_reload(runtime: &RedDBRuntime) {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    tracing::info!(
        target: "reddb::secrets",
        ts_unix_ms = now_ms,
        "SIGHUP received; secrets will be re-read from *_FILE on next access"
    );
    // Routed through AuditFieldEscaper (ADR 0010 / issue #177) so
    // every emission goes through the typed-field guard. The
    // arguments here are static, but using the typed entry point
    // keeps the discipline uniform across call sites.
    use crate::runtime::audit_log::{AuditAuthSource, AuditEvent, AuditFieldEscaper, Outcome};
    runtime.audit_log().record_event(
        AuditEvent::builder("config/sighup_reload")
            .source(AuditAuthSource::System)
            .resource("secrets")
            .outcome(Outcome::Success)
            .field(AuditFieldEscaper::field("ts_unix_ms", now_ms))
            .build(),
    );
}

#[inline(never)]
fn run_routed_server(config: ServerCommandConfig, router_bind_addr: String) -> Result<(), String> {
    let workers = config.workers;
    let cli_telemetry = config.telemetry.clone();
    let db_options = config.to_db_options()?;
    let rt_config = detect_runtime_config();
    let worker_threads = workers.unwrap_or(rt_config.suggested_workers);
    let (runtime, auth_store, _telemetry_guard) =
        build_runtime_and_auth_store(db_options.clone(), cli_telemetry)?;
    let _backup_tasks = spawn_backup_tasks_if_configured(&db_options, &runtime);

    spawn_admin_metrics_listeners(&runtime, &auth_store);

    // Issue #933: collapse the loopback proxy. All three transports are
    // served from one in-process acceptor that shares the single tokio
    // runtime (ADR 0035) — no internal HTTP/gRPC/wire listeners, no
    // `copy_bidirectional` hop. We build the handler objects here and hand
    // them to the demux, which classifies each connection and dispatches.
    let http_server = build_http_server(
        runtime.clone(),
        auth_store.clone(),
        router_bind_addr.clone(),
    );
    let http_server = apply_http_limits(http_server, &config, &runtime);

    let grpc_server = RedDBGrpcServer::with_options(
        runtime.clone(),
        GrpcServerOptions {
            bind_addr: router_bind_addr.clone(),
            tls: None,
        },
        auth_store,
    );

    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(worker_threads)
        .thread_stack_size(rt_config.stack_size)
        .build()
        .map_err(|err| format!("tokio runtime: {err}"))?;

    let signal_runtime = runtime.clone();
    let wire_runtime = Arc::new(runtime);
    tokio_runtime.block_on(async move {
        spawn_lifecycle_signal_handler(signal_runtime).await;
        tracing::info!(
            bind = %router_bind_addr,
            cpus = rt_config.available_cpus,
            workers = worker_threads,
            "router bootstrapping"
        );
        serve_tcp_router(InProcessRouterConfig {
            bind_addr: router_bind_addr,
            http_server,
            grpc_server,
            wire_runtime,
        })
        .await
        .map_err(|err| err.to_string())
    })
}

/// Spawn RedWire listeners (plaintext + TLS) as background tokio tasks.
async fn spawn_wire_listeners(
    config: &ServerCommandConfig,
    runtime: &RedDBRuntime,
    readiness: &mut TransportReadiness,
) -> Result<(), String> {
    // Plaintext RedWire — TCP or Unix socket
    if let Some(wire_addr) = config.wire_bind_addr.clone() {
        let wire_rt = Arc::new(runtime.clone());
        // Address starting with `unix://` or an absolute filesystem path
        // switches to Unix domain sockets.
        #[cfg(unix)]
        {
            if wire_addr.starts_with("unix://") || wire_addr.starts_with('/') {
                readiness.active("wire", &wire_addr, config.wire_bind_explicit);
                tokio::spawn(async move {
                    if let Err(e) = crate::wire::redwire::listener::start_redwire_unix_listener(
                        &wire_addr, wire_rt,
                    )
                    .await
                    {
                        tracing::error!(err = %e, "redwire unix listener error");
                    }
                });
                return Ok(());
            }
        }
        match tokio::net::TcpListener::bind(&wire_addr).await {
            Ok(listener) => {
                readiness.active("wire", &wire_addr, config.wire_bind_explicit);
                tokio::spawn(async move {
                    if let Err(e) =
                        crate::wire::redwire::listener::start_redwire_listener_on(listener, wire_rt)
                            .await
                    {
                        tracing::error!(err = %e, "redwire listener error");
                    }
                });
            }
            Err(err) => {
                let reason = format!("wire listener bind {wire_addr}: {err}");
                readiness.failed(
                    "wire",
                    &wire_addr,
                    config.wire_bind_explicit,
                    reason.clone(),
                );
                if config.wire_bind_explicit {
                    tracing::error!(
                        transport = "wire",
                        bind = %wire_addr,
                        error = %err,
                        "fatal explicit bind failure"
                    );
                    return Err(format!("explicit {reason}"));
                }
                tracing::warn!(
                    transport = "wire",
                    bind = %wire_addr,
                    error = %err,
                    "non-fatal implicit bind failure; listener degraded"
                );
            }
        }
    }

    // RedWire over TLS
    if let Some(wire_tls_addr) = config.wire_tls_bind_addr.clone() {
        let tls_config = resolve_wire_tls_config(config);
        match tls_config {
            Ok(tls_cfg) => {
                let wire_rt = Arc::new(runtime.clone());
                tokio::spawn(async move {
                    if let Err(e) =
                        crate::wire::start_redwire_tls_listener(&wire_tls_addr, wire_rt, &tls_cfg)
                            .await
                    {
                        tracing::error!(err = %e, "redwire+tls listener error");
                    }
                });
            }
            Err(e) => tracing::error!(err = %e, "redwire TLS config error"),
        }
    }
    Ok(())
}

/// Spawn the PostgreSQL wire-protocol listener (Phase 3.1 PG parity).
///
/// Only runs when `--pg-bind` is supplied. Uses the v3 protocol so
/// psql, JDBC drivers, DBeaver, etc. can connect directly. Runs
/// alongside the native wire listener; the two transports do not
/// share a port.
fn spawn_pg_listener(config: &ServerCommandConfig, runtime: &RedDBRuntime) {
    if let Some(pg_addr) = config.pg_bind_addr.clone() {
        let rt = Arc::new(runtime.clone());
        tokio::spawn(async move {
            let cfg = crate::wire::PgWireConfig {
                bind_addr: pg_addr,
                ..Default::default()
            };
            if let Err(e) = crate::wire::start_pg_wire_listener(cfg, rt).await {
                tracing::error!(err = %e, "pg wire listener error");
            }
        });
    }
}

/// Resolve gRPC TLS material into PEM bytes.
///
/// Lookup order, in priority:
///   1. Explicit `config.grpc_tls_cert` / `config.grpc_tls_key` (paths
///      passed via CLI/env). Both must be present together.
///   2. `RED_GRPC_TLS_DEV=1` — auto-generate a self-signed cert next
///      to the data dir. Refuses to run without the env flag so an
///      operator can't accidentally ship a dev cert in prod.
///
/// `client_ca` is loaded when `config.grpc_tls_client_ca` is set,
/// turning the listener into a mutual-TLS endpoint that requires
/// every client to present a chain that anchors at one of the CAs
/// in the bundle.
fn resolve_grpc_tls_options(config: &ServerCommandConfig) -> Result<crate::GrpcTlsOptions, String> {
    use crate::utils::secret_file::expand_file_env;

    // Best-effort *_FILE expansion for every TLS env knob. Errors here
    // surface as warnings; the fallback path (explicit cert paths) will
    // cover the common case.
    for var in [
        "REDDB_GRPC_TLS_CERT",
        "REDDB_GRPC_TLS_KEY",
        "REDDB_GRPC_TLS_CLIENT_CA",
    ] {
        if let Err(err) = expand_file_env(var) {
            tracing::warn!(
                target: "reddb::secrets",
                env = %var,
                err = %err,
                "could not expand *_FILE companion for gRPC TLS"
            );
        }
    }

    let (cert_pem, key_pem) = match (&config.grpc_tls_cert, &config.grpc_tls_key) {
        (Some(cert), Some(key)) => {
            let cert_pem = std::fs::read(cert)
                .map_err(|e| format!("read grpc cert {}: {e}", cert.display()))?;
            let key_pem =
                std::fs::read(key).map_err(|e| format!("read grpc key {}: {e}", key.display()))?;
            (cert_pem, key_pem)
        }
        _ => {
            // No explicit material → only proceed when dev-mode is on.
            let dev = std::env::var("RED_GRPC_TLS_DEV")
                .ok()
                .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
                .unwrap_or(false);
            if !dev {
                return Err("gRPC TLS configured but no cert/key supplied — set \
                     REDDB_GRPC_TLS_CERT / REDDB_GRPC_TLS_KEY (or \
                     RED_GRPC_TLS_DEV=1 to auto-generate a self-signed cert)"
                    .to_string());
            }
            let dir = config
                .path
                .as_ref()
                .and_then(|p| p.parent())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            let (cert_pem_str, key_pem_str) =
                crate::wire::tls::generate_self_signed_cert("localhost")
                    .map_err(|e| format!("auto-generate dev grpc cert: {e}"))?;

            // Constant-time-friendly fingerprint to stderr so the
            // operator can pin a client trust store. We log via
            // `tracing::warn!` so it stands out next to ordinary
            // listener-online events.
            let fp = sha256_pem_fingerprint(cert_pem_str.as_bytes());
            tracing::warn!(
                target: "reddb::security",
                transport = "grpc",
                cert_sha256 = %fp,
                "RED_GRPC_TLS_DEV=1: using auto-generated self-signed cert; \
                 DO NOT use in production"
            );
            // Persist so that restarts reuse the same identity.
            let cert_path = dir.join("grpc-tls-cert.pem");
            let key_path = dir.join("grpc-tls-key.pem");
            if !cert_path.exists() || !key_path.exists() {
                let _ = std::fs::create_dir_all(&dir);
                std::fs::write(&cert_path, cert_pem_str.as_bytes())
                    .map_err(|e| format!("write grpc dev cert: {e}"))?;
                std::fs::write(&key_path, key_pem_str.as_bytes())
                    .map_err(|e| format!("write grpc dev key: {e}"))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ =
                        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600));
                }
            }
            (cert_pem_str.into_bytes(), key_pem_str.into_bytes())
        }
    };

    let client_ca_pem = match &config.grpc_tls_client_ca {
        Some(path) => Some(
            std::fs::read(path)
                .map_err(|e| format!("read grpc client CA {}: {e}", path.display()))?,
        ),
        None => None,
    };

    Ok(crate::GrpcTlsOptions {
        cert_pem,
        key_pem,
        client_ca_pem,
    })
}

/// Spawn a TLS-terminated gRPC listener when `grpc_tls_bind_addr` is
/// configured. Logs and continues on TLS-config errors so the plain
/// listener stays up; this matches the wire-listener pattern.
fn spawn_grpc_tls_listener_if_configured(
    config: &ServerCommandConfig,
    runtime: RedDBRuntime,
    auth_store: Arc<AuthStore>,
) {
    let Some(tls_bind) = config.grpc_tls_bind_addr.clone() else {
        return;
    };
    let tls_opts = match resolve_grpc_tls_options(config) {
        Ok(opts) => opts,
        Err(err) => {
            tracing::error!(
                target: "reddb::security",
                transport = "grpc",
                err = %err,
                "gRPC TLS config error; TLS listener will not start"
            );
            return;
        }
    };
    tokio::spawn(async move {
        let server = RedDBGrpcServer::with_options(
            runtime,
            GrpcServerOptions {
                bind_addr: tls_bind.clone(),
                tls: Some(tls_opts),
            },
            auth_store,
        );
        tracing::info!(transport = "grpc+tls", bind = %tls_bind, "listener online");
        if let Err(err) = server.serve().await {
            tracing::error!(transport = "grpc+tls", err = %err, "gRPC TLS listener error");
        }
    });
}

/// Hex-encoded SHA-256 of a PEM blob, used for cert-pin operator log
/// lines. Constant-time hash; no token contents leave this fn.
fn sha256_pem_fingerprint(pem: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(pem);
    let d = h.finalize();
    let mut buf = String::with_capacity(64);
    for b in d.iter() {
        buf.push_str(&format!("{b:02x}"));
    }
    buf
}

/// Resolve TLS config: use provided cert/key or auto-generate.
fn resolve_wire_tls_config(
    config: &ServerCommandConfig,
) -> Result<crate::wire::WireTlsConfig, String> {
    match (&config.wire_tls_cert, &config.wire_tls_key) {
        (Some(cert), Some(key)) => Ok(crate::wire::WireTlsConfig {
            cert_path: cert.clone(),
            key_path: key.clone(),
        }),
        _ => {
            // Auto-generate self-signed cert
            let dir = config
                .path
                .as_ref()
                .and_then(|p| p.parent())
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            crate::wire::tls::auto_generate_cert(&dir).map_err(|e| e.to_string())
        }
    }
}

#[inline(never)]
fn run_wire_only_server(config: ServerCommandConfig, wire_addr: String) -> Result<(), String> {
    let rt_config = detect_runtime_config();
    let workers = config.workers.unwrap_or(rt_config.suggested_workers);
    let cli_telemetry = config.telemetry.clone();
    let db_options = config.to_db_options()?;
    let mut transport_readiness = TransportReadiness::default();

    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(workers)
        .thread_stack_size(rt_config.stack_size)
        .build()
        .map_err(|err| format!("tokio runtime: {err}"))?;

    // Guard lives on the outer thread's stack so it outlives the
    // tokio runtime — dropping it only after the listener returns
    // flushes the file log writer.
    let (runtime, _auth_store, _telemetry_guard) =
        build_runtime_and_auth_store(db_options.clone(), cli_telemetry)?;
    let _backup_tasks = spawn_backup_tasks_if_configured(&db_options, &runtime);
    let signal_runtime = runtime.clone();
    tokio_runtime.block_on(async move {
        spawn_lifecycle_signal_handler(signal_runtime).await;
        spawn_pg_listener(&config, &runtime);
        let wire_rt = Arc::new(runtime);
        let listener = tokio::net::TcpListener::bind(&wire_addr)
            .await
            .map_err(|err| {
                let reason = format!("wire listener bind {wire_addr}: {err}");
                transport_readiness.failed(
                    "wire",
                    &wire_addr,
                    config.wire_bind_explicit,
                    reason.clone(),
                );
                if config.wire_bind_explicit {
                    format!("explicit {reason}")
                } else {
                    reason
                }
            })?;
        transport_readiness.active("wire", &wire_addr, config.wire_bind_explicit);
        crate::wire::redwire::listener::start_redwire_listener_on(listener, wire_rt)
            .await
            .map_err(|e| e.to_string())
    })
}

#[inline(never)]
fn run_pg_only_server(config: ServerCommandConfig, pg_addr: String) -> Result<(), String> {
    let rt_config = detect_runtime_config();
    let workers = config.workers.unwrap_or(rt_config.suggested_workers);
    let cli_telemetry = config.telemetry.clone();
    let db_options = config.to_db_options()?;

    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(workers)
        .thread_stack_size(rt_config.stack_size)
        .build()
        .map_err(|err| format!("tokio runtime: {err}"))?;

    let (runtime, _auth_store, _telemetry_guard) =
        build_runtime_and_auth_store(db_options.clone(), cli_telemetry)?;
    let _backup_tasks = spawn_backup_tasks_if_configured(&db_options, &runtime);
    let signal_runtime = runtime.clone();
    tokio_runtime.block_on(async move {
        spawn_lifecycle_signal_handler(signal_runtime).await;
        let cfg = crate::wire::PgWireConfig {
            bind_addr: pg_addr,
            ..Default::default()
        };
        crate::wire::start_pg_wire_listener(cfg, Arc::new(runtime))
            .await
            .map_err(|e| e.to_string())
    })
}

#[inline(never)]
fn build_runtime_and_auth_store(
    db_options: RedDBOptions,
    cli_telemetry: Option<crate::telemetry::TelemetryConfig>,
) -> Result<
    (
        RedDBRuntime,
        Arc<AuthStore>,
        Option<crate::telemetry::TelemetryGuard>,
    ),
    String,
> {
    // Return the TelemetryGuard so server runners can bind it for
    // their full lifetime. Dropping the guard tears down the
    // non-blocking log writer thread and, because that writer is
    // built with `.lossy(true)`, any subsequent log event routed to
    // the file sink is silently dropped — so callers MUST keep the
    // returned `Option<TelemetryGuard>` alive until shutdown.
    build_runtime_with_telemetry(db_options, cli_telemetry)
}

/// Open the runtime, initialise structured logging from merged CLI +
/// `red_config` settings, and return a guard the caller must keep
/// alive for the server lifetime (drop flushes pending log writes).
///
/// Merge priority: CLI flag (explicit `Some`) beats `red.logging.*`
/// in red_config, beats the built-in default. A CLI-flag value of
/// `None` / empty means "inherit from config or default" — never
/// "disable". The one exception is `--no-log-file` which forces
/// `log_dir = None` regardless of config.
pub(crate) fn build_runtime_with_telemetry(
    db_options: RedDBOptions,
    cli_telemetry: Option<crate::telemetry::TelemetryConfig>,
) -> Result<
    (
        RedDBRuntime,
        Arc<AuthStore>,
        Option<crate::telemetry::TelemetryGuard>,
    ),
    String,
> {
    let runtime = RedDBRuntime::with_options(db_options.clone()).map_err(|err| {
        // Issue #205 — runtime construction failure is the canonical
        // StartupFailed phase. The audit sink isn't installed yet
        // (it would have been registered inside `with_options`), so
        // the emit falls through to tracing+eprintln only — operator
        // still sees it on stderr.
        let msg = err.to_string();
        crate::telemetry::operator_event::OperatorEvent::StartupFailed {
            phase: "runtime_construction".to_string(),
            error: msg.clone(),
        }
        .emit_global();
        msg
    })?;

    // PLAN.md Phase 5 / W6 — opt into serverless writer-lease fencing
    // when `RED_LEASE_REQUIRED=true`. Failure here aborts boot: the
    // operator asked for a fence; running unfenced would silently
    // expose split-brain risk.
    crate::runtime::lease_loop::start_lease_loop_if_required(&runtime).map_err(|err| {
        let msg = err.to_string();
        crate::telemetry::operator_event::OperatorEvent::StartupFailed {
            phase: "lease_loop".to_string(),
            error: msg.clone(),
        }
        .emit_global();
        msg
    })?;

    // #213 — edge-triggered disk-space watchdog. Watches the data
    // directory; falls back to polling when fanotify is unavailable
    // (non-Linux or unprivileged container).
    if let Some(data_path) = db_options.data_path.as_deref() {
        let watch_dir = data_path.parent().unwrap_or(data_path);
        crate::runtime::disk_space_monitor::DiskSpaceMonitor::new(watch_dir, 90).spawn();
    }

    // #214 — inotify config hot-reload watcher. Watches the config file
    // (REDDB_CONFIG_FILE or /etc/reddb/config.json) for changes and
    // applies hot-reloadable keys without restart.
    {
        let config_path = crate::runtime::config_overlay::config_file_path();
        let store = runtime.db().store();
        crate::runtime::config_watcher::ConfigWatcher::new(config_path, store).spawn();
    }

    // Phase 6 logging: merge red_config overrides onto the CLI-built
    // telemetry config, then install the global subscriber.
    let merged = merge_telemetry_with_config(
        cli_telemetry
            .unwrap_or_else(|| default_telemetry_for_path(db_options.data_path.as_deref())),
        &runtime,
    );
    let telemetry_guard = crate::telemetry::init(merged);

    let no_auth = no_auth_active(&db_options);
    let auth_store =
        if db_options.auth.vault_enabled {
            let pager =
                runtime.db().store().pager().cloned().ok_or_else(|| {
                    "vault requires a paged database (persistent mode)".to_string()
                })?;
            let store = AuthStore::with_vault(db_options.auth.clone(), pager, None)
                .map_err(|err| err.to_string())?;
            Arc::new(store)
        } else {
            Arc::new(AuthStore::new(db_options.auth.clone()))
        };
    auth_store.configure_control_events(
        runtime.control_event_ledger(),
        runtime.control_event_config(),
    );
    // Issue #663 — when `--no-auth` is active, deliberately skip the
    // preset machinery. Otherwise a stray `REDDB_USERNAME`+`REDDB_PASSWORD`
    // pair in the operator's environment would silently create an admin
    // user, defeating the whole point of opting into anonymous mode.
    if no_auth {
        eprintln!("{NO_AUTH_WARNING}");
        tracing::warn!("{NO_AUTH_WARNING}");
    } else {
        apply_preset(&runtime, &auth_store)?;
        maybe_apply_policy_break_glass(&auth_store);
    }

    // Background session purge (every 5 minutes)
    {
        let store = Arc::clone(&auth_store);
        std::thread::Builder::new()
            .name("reddb-session-purge".into())
            .spawn(move || loop {
                std::thread::sleep(std::time::Duration::from_secs(300));
                store.purge_expired_sessions();
            })
            .ok();
    }

    Ok((runtime, auth_store, telemetry_guard))
}

/// Honour `REDDB_POLICY_BREAK_GLASS=1` at boot — see issue #713 and
/// the [`crate::auth::self_lock_guard`] module. Anything other than
/// `1`/`true`/`yes` (case-insensitive) is treated as not set.
fn maybe_apply_policy_break_glass(auth_store: &Arc<AuthStore>) {
    use crate::auth::self_lock_guard::BREAK_GLASS_ENV;

    let enabled = std::env::var(BREAK_GLASS_ENV)
        .ok()
        .map(|v| {
            let trimmed = v.trim().to_ascii_lowercase();
            matches!(trimmed.as_str(), "1" | "true" | "yes")
        })
        .unwrap_or(false);
    if !enabled {
        return;
    }
    let now = crate::utils::now_unix_millis() as u128;
    match auth_store.apply_policy_break_glass(now) {
        Ok(()) => {
            tracing::warn!(env = BREAK_GLASS_ENV, "policy break-glass recovery applied");
        }
        Err(err) => {
            tracing::error!(env = BREAK_GLASS_ENV, %err, "policy break-glass recovery failed");
        }
    }
}

/// Reserved config keys describing first-boot bootstrap state (issue #650).
/// Presence of [`BOOTSTRAP_COMPLETED_KEY`] is the idempotency hinge: when
/// it is set, [`apply_preset`] silently no-ops on subsequent boots so a
/// container restart with the same env is a no-op.
pub(crate) const BOOTSTRAP_COMPLETED_KEY: &str = "system.bootstrap.completed";
pub(crate) const BOOTSTRAP_PRESET_KEY: &str = "system.bootstrap.preset";
pub(crate) const BOOTSTRAP_FIRST_ADMIN_KEY: &str = "system.bootstrap.first_admin_id";

/// Env var selecting the bootstrap preset. Default = `simple`.
pub(crate) const PRESET_ENV: &str = "REDDB_PRESET";
pub(crate) const PRESET_SIMPLE: &str = "simple";
pub(crate) const PRESET_PRODUCTION: &str = "production";
pub(crate) const PRESET_REGULATED: &str = "regulated";

/// Policy id installed by the `production` preset and attached to the
/// first admin. Grants `"*"` on `"*"` so the admin has policy-derived
/// broad authority (acceptance #3) — not an authorization bypass.
pub(crate) const FIRST_ADMIN_ALLOW_ALL_POLICY: &str = "system.bootstrap.first-admin-allow-all";
pub(crate) const REGULATED_PROTECT_MANAGED_POLICY: &str = "system.regulated.protect-managed";
pub(crate) const REGULATED_AUDIT_CONFIG_NAMESPACE: &str = "red.config.audit";
pub(crate) const REGULATED_EVIDENCE_CONFIG_NAMESPACE: &str = "red.config.evidence";
pub(crate) const REGULATED_QUERY_AUDIT_CONFIG_NAMESPACE: &str = "red.config.query_audit";

/// Apply the bootstrap preset selected by `REDDB_PRESET` (default
/// `simple`). Idempotent — if `system.bootstrap.completed` is already
/// set, this is a one-line `tracing::info!` no-op and the server proceeds
/// (issue #650 acceptance #5).
///
/// Caller must have already short-circuited the `--no-auth` / `--dev`
/// path (issue #663): when that flag is set, the preset must be skipped
/// entirely — no admin created, no bootstrap state written.
pub(crate) fn apply_preset(
    runtime: &RedDBRuntime,
    auth_store: &Arc<AuthStore>,
) -> Result<(), String> {
    let store = runtime.db().store();

    if store.get_config(BOOTSTRAP_COMPLETED_KEY).is_some() {
        crate::cli::bootstrap_manifest::rehydrate_manifest_registry(
            runtime,
            &runtime.config_registry(),
        )?;
        tracing::info!("bootstrap state present, skipping preset application");
        return Ok(());
    }

    // `_FILE` companion expansion for k8s secret mounts. Errors here
    // (e.g. both `REDDB_PASSWORD` and `REDDB_PASSWORD_FILE` set) are
    // operator misconfigs and should fail the boot loudly.
    for var in ["REDDB_USERNAME", "REDDB_PASSWORD"] {
        crate::utils::expand_file_env(var).map_err(|err| format!("expand {var}_FILE: {err}"))?;
    }

    let preset = std::env::var(PRESET_ENV)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| PRESET_SIMPLE.to_string());

    if let Ok(path) = std::env::var(crate::cli::bootstrap_manifest::MANIFEST_ENV) {
        let path = path.trim();
        if !path.is_empty() {
            let first_admin_id = crate::cli::bootstrap_manifest::apply_manifest_file(
                runtime,
                auth_store,
                &runtime.config_registry(),
                std::path::Path::new(path),
            )?;
            persist_bootstrap_state(runtime, "manifest", Some(&first_admin_id));
            tracing::info!("bootstrap manifest applied");
            return Ok(());
        }
    }

    let first_admin_id = match preset.as_str() {
        PRESET_SIMPLE => {
            // `simple` is the default low-friction preset. Auth knobs
            // remain whatever the CLI/env set; we only persist the
            // bootstrap state so subsequent boots are idempotent.
            None
        }
        PRESET_PRODUCTION => Some(apply_production_preset(auth_store)?),
        PRESET_REGULATED => {
            apply_regulated_preset(runtime, auth_store)?;
            None
        }
        other => {
            return Err(format!(
                "REDDB_PRESET={other:?} is not recognised (expected `simple`, `production`, or `regulated`)"
            ));
        }
    };

    persist_bootstrap_state(runtime, &preset, first_admin_id.as_deref());
    tracing::info!(preset = %preset, "bootstrap preset applied");
    Ok(())
}

fn apply_production_preset(auth_store: &Arc<AuthStore>) -> Result<String, String> {
    use crate::auth::store::PrincipalRef;
    use crate::auth::{policies::Policy, UserId};

    let username = std::env::var("REDDB_USERNAME")
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            "REDDB_PRESET=production requires REDDB_USERNAME (or REDDB_USERNAME_FILE)".to_string()
        })?;
    let password = std::env::var("REDDB_PASSWORD")
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            "REDDB_PRESET=production requires REDDB_PASSWORD (or REDDB_PASSWORD_FILE)".to_string()
        })?;

    // (1) Create the first admin as system-owned + platform-scoped so
    // #649's `ManagedConfigGate` accepts them on managed-config writes.
    let result = auth_store
        .bootstrap_system_admin(&username, &password)
        .map_err(|err| format!("bootstrap first admin: {err}"))?;
    let first_admin = UserId::platform(result.user.username.clone());

    // (2) Install the allow-all policy as an ordinary policy row.
    let policy = Policy::from_json_str(&format!(
        r#"{{
            "id": "{id}",
            "version": 1,
            "statements": [{{
                "effect": "allow",
                "actions": ["*"],
                "resources": ["*"]
            }}]
        }}"#,
        id = FIRST_ADMIN_ALLOW_ALL_POLICY
    ))
    .map_err(|err| format!("compile allow-all policy: {err}"))?;
    auth_store
        .put_policy(policy)
        .map_err(|err| format!("install allow-all policy: {err}"))?;

    // (3) Attach the policy to the first admin.
    auth_store
        .attach_policy(
            PrincipalRef::User(first_admin.clone()),
            FIRST_ADMIN_ALLOW_ALL_POLICY,
        )
        .map_err(|err| format!("attach allow-all policy: {err}"))?;

    Ok(first_admin.to_string())
}

fn apply_regulated_preset(
    runtime: &RedDBRuntime,
    auth_store: &Arc<AuthStore>,
) -> Result<(), String> {
    use crate::auth::policies::Policy;
    use crate::auth::registry::EvidenceRequirement;

    runtime.query_audit().enable_infrastructure();

    let policy = Policy::from_json_str(&format!(
        r#"{{
            "id": "{id}",
            "version": 1,
            "statements": [
                {{
                    "effect": "deny",
                    "actions": ["policy:put", "policy:drop", "policy:attach", "policy:detach"],
                    "resources": ["policy:{id}"]
                }},
                {{
                    "effect": "deny",
                    "actions": ["config:write"],
                    "resources": [
                        "config:{audit}.*",
                        "config:{evidence}.*",
                        "config:{query_audit}.*"
                    ]
                }}
            ]
        }}"#,
        id = REGULATED_PROTECT_MANAGED_POLICY,
        audit = REGULATED_AUDIT_CONFIG_NAMESPACE,
        evidence = REGULATED_EVIDENCE_CONFIG_NAMESPACE,
        query_audit = REGULATED_QUERY_AUDIT_CONFIG_NAMESPACE,
    ))
    .map_err(|err| format!("compile regulated guardrail policy: {err}"))?;
    auth_store
        .put_policy(policy)
        .map_err(|err| format!("install regulated guardrail policy: {err}"))?;

    let now_ms = crate::utils::now_unix_millis() as u128;
    let entries = vec![
        regulated_registry_entry(
            REGULATED_PROTECT_MANAGED_POLICY,
            crate::auth::managed_policy::RESOURCE_TYPE_POLICY,
            "iam_policy",
            "policy:*",
            &format!("policy:{REGULATED_PROTECT_MANAGED_POLICY}"),
            EvidenceRequirement::Metadata,
            now_ms,
        ),
        regulated_registry_entry(
            REGULATED_AUDIT_CONFIG_NAMESPACE,
            crate::auth::managed_config::RESOURCE_TYPE_CONFIG_NAMESPACE,
            "config_namespace",
            "config:write",
            &format!("config:{REGULATED_AUDIT_CONFIG_NAMESPACE}.*"),
            EvidenceRequirement::Metadata,
            now_ms,
        ),
        regulated_registry_entry(
            REGULATED_EVIDENCE_CONFIG_NAMESPACE,
            crate::auth::managed_config::RESOURCE_TYPE_CONFIG_NAMESPACE,
            "config_namespace",
            "config:write",
            &format!("config:{REGULATED_EVIDENCE_CONFIG_NAMESPACE}.*"),
            EvidenceRequirement::Metadata,
            now_ms,
        ),
        regulated_registry_entry(
            REGULATED_QUERY_AUDIT_CONFIG_NAMESPACE,
            crate::auth::managed_config::RESOURCE_TYPE_CONFIG_NAMESPACE,
            "config_namespace",
            "config:write",
            &format!("config:{REGULATED_QUERY_AUDIT_CONFIG_NAMESPACE}.*"),
            EvidenceRequirement::Metadata,
            now_ms,
        ),
    ];

    for entry in entries.iter().cloned() {
        runtime
            .config_registry()
            .restore_bootstrap_entry(entry)
            .map_err(|err| format!("install regulated registry entry: {err}"))?;
    }
    crate::cli::bootstrap_manifest::persist_registry_state(runtime, &entries)?;
    Ok(())
}

fn regulated_registry_entry(
    id: &str,
    resource_type: &str,
    schema: &str,
    required_action: &str,
    required_resource: &str,
    evidence_requirement: crate::auth::registry::EvidenceRequirement,
    updated_at_ms: u128,
) -> crate::auth::registry::ConfigRegistryEntry {
    crate::auth::registry::ConfigRegistryEntry {
        id: id.to_string(),
        version: 1,
        resource_type: resource_type.to_string(),
        schema: schema.to_string(),
        mutability: crate::auth::registry::Mutability::Immutable,
        sensitivity: crate::auth::registry::Sensitivity::Internal,
        managed: true,
        required_action: required_action.to_string(),
        required_resource: required_resource.to_string(),
        evidence_requirement,
        updated_by: "system:regulated-preset".to_string(),
        updated_at_ms,
    }
}

fn persist_bootstrap_state(runtime: &RedDBRuntime, preset: &str, first_admin_id: Option<&str>) {
    let store = runtime.db().store();
    let mut tree = crate::serde_json::Map::new();
    tree.insert(
        BOOTSTRAP_COMPLETED_KEY.to_string(),
        crate::serde_json::Value::Bool(true),
    );
    tree.insert(
        BOOTSTRAP_PRESET_KEY.to_string(),
        crate::serde_json::Value::String(preset.to_string()),
    );
    if let Some(id) = first_admin_id {
        tree.insert(
            BOOTSTRAP_FIRST_ADMIN_KEY.to_string(),
            crate::serde_json::Value::String(id.to_string()),
        );
    }
    let json = crate::serde_json::Value::Object(tree);
    store.set_config_tree("", &json);
}

/// Read `red.logging.*` keys from the persistent config store and
/// merge them into the CLI-built `TelemetryConfig`. Merge priority:
/// explicit CLI flag > red_config > built-in default.
///
/// The "was a flag passed" signal comes from the `*_explicit` bools
/// on `TelemetryConfig`, populated by the CLI parser. This replaces
/// an earlier equality-to-default heuristic that silently dropped
/// config whenever the CLI-derived default diverged from
/// `TelemetryConfig::default()` (e.g. path-derived `log_dir`,
/// non-TTY `format`) and that silently overrode `--no-log-file`.
fn merge_telemetry_with_config(
    mut cli: crate::telemetry::TelemetryConfig,
    runtime: &RedDBRuntime,
) -> crate::telemetry::TelemetryConfig {
    use crate::storage::schema::Value;

    let store = runtime.db().store();

    if !cli.level_explicit {
        if let Some(Value::Text(v)) = store.get_config("red.logging.level") {
            cli.level_filter = v.to_string();
        }
    }
    if !cli.format_explicit {
        if let Some(Value::Text(v)) = store.get_config("red.logging.format") {
            if let Some(parsed) = crate::telemetry::LogFormat::parse(&v) {
                cli.format = parsed;
            }
        }
    }
    if !cli.rotation_keep_days_explicit {
        match store.get_config("red.logging.keep_days") {
            Some(Value::Integer(n)) if n >= 0 && n <= u16::MAX as i64 => {
                cli.rotation_keep_days = n as u16
            }
            Some(Value::UnsignedInteger(n)) if n <= u16::MAX as u64 => {
                cli.rotation_keep_days = n as u16
            }
            Some(Value::Text(v)) => {
                if let Ok(n) = v.parse::<u16>() {
                    cli.rotation_keep_days = n;
                }
            }
            _ => {}
        }
    }
    if !cli.file_prefix_explicit {
        if let Some(Value::Text(v)) = store.get_config("red.logging.file_prefix") {
            if !v.is_empty() {
                cli.file_prefix = v.to_string();
            }
        }
    }
    // --no-log-file is a kill-switch: config cannot resurrect the
    // file sink. Explicit --log-dir also wins.
    if !cli.log_dir_explicit && !cli.log_file_disabled {
        if let Some(Value::Text(v)) = store.get_config("red.logging.dir") {
            if !v.is_empty() {
                cli.log_dir = Some(std::path::PathBuf::from(v.as_ref()));
            }
        }
    }

    cli
}

#[cfg(test)]
mod telemetry_merge_tests {
    use super::*;
    use crate::telemetry::{LogFormat, TelemetryConfig};

    fn fresh_runtime() -> RedDBRuntime {
        RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime")
    }

    fn set_str(runtime: &RedDBRuntime, key: &str, value: &str) {
        runtime
            .db()
            .store()
            .set_config_tree(key, &crate::serde_json::Value::String(value.to_string()));
    }

    fn cli_base() -> TelemetryConfig {
        // Emulate default_telemetry_for_path(Some(path)) on a non-TTY host:
        // log_dir = Some(...), format = Json. Nothing marked explicit.
        TelemetryConfig {
            log_dir: Some(std::path::PathBuf::from("/tmp/reddb-default/logs")),
            format: LogFormat::Json,
            ..Default::default()
        }
    }

    #[test]
    fn config_log_dir_promoted_when_flag_absent() {
        let runtime = fresh_runtime();
        set_str(&runtime, "red.logging.dir", "/var/log/reddb");
        let merged = merge_telemetry_with_config(cli_base(), &runtime);
        assert_eq!(
            merged.log_dir.as_deref(),
            Some(std::path::Path::new("/var/log/reddb"))
        );
    }

    #[test]
    fn explicit_log_dir_wins_over_config() {
        let runtime = fresh_runtime();
        set_str(&runtime, "red.logging.dir", "/var/log/reddb");
        let mut cli = cli_base();
        cli.log_dir = Some(std::path::PathBuf::from("/custom/dir"));
        cli.log_dir_explicit = true;
        let merged = merge_telemetry_with_config(cli, &runtime);
        assert_eq!(
            merged.log_dir.as_deref(),
            Some(std::path::Path::new("/custom/dir"))
        );
    }

    #[test]
    fn no_log_file_beats_config_log_dir() {
        let runtime = fresh_runtime();
        set_str(&runtime, "red.logging.dir", "/var/log/reddb");
        let mut cli = cli_base();
        cli.log_dir = None;
        cli.log_file_disabled = true;
        let merged = merge_telemetry_with_config(cli, &runtime);
        assert!(
            merged.log_dir.is_none(),
            "--no-log-file must veto config dir"
        );
    }

    #[test]
    fn config_format_promoted_on_non_tty_default() {
        // On non-TTY, default_telemetry_for_path yields format=Json even
        // though TelemetryConfig::default() is Pretty. The old equality
        // check silently dropped config here.
        let runtime = fresh_runtime();
        set_str(&runtime, "red.logging.format", "pretty");
        let merged = merge_telemetry_with_config(cli_base(), &runtime);
        assert_eq!(merged.format, LogFormat::Pretty);
    }

    #[test]
    fn explicit_format_wins_over_config() {
        let runtime = fresh_runtime();
        set_str(&runtime, "red.logging.format", "pretty");
        let mut cli = cli_base();
        cli.format = LogFormat::Json;
        cli.format_explicit = true;
        let merged = merge_telemetry_with_config(cli, &runtime);
        assert_eq!(merged.format, LogFormat::Json);
    }
}

#[inline(never)]
fn build_http_server(
    runtime: RedDBRuntime,
    auth_store: Arc<AuthStore>,
    bind_addr: String,
) -> RedDBServer {
    build_http_server_with_transport_readiness(
        runtime,
        auth_store,
        bind_addr,
        TransportReadiness::default(),
    )
}

/// Apply the resolved HTTP limits to a freshly-built `RedDBServer`.
///
/// Centralised here so every `run_*` path goes through the same
/// resolver and the structured startup log line carries the same
/// `http_limits.*` fields regardless of transport combination.
fn apply_http_limits(
    server: RedDBServer,
    config: &ServerCommandConfig,
    runtime: &RedDBRuntime,
) -> RedDBServer {
    let store = runtime.db().store();
    let resolved =
        crate::server::http_limits::resolve_http_limits(&config.http_limits_cli, |key| match store
            .get_config(key)
        {
            Some(crate::storage::schema::Value::Text(v)) => Some(v.to_string()),
            Some(crate::storage::schema::Value::Integer(n)) if n >= 0 => Some(n.to_string()),
            Some(crate::storage::schema::Value::UnsignedInteger(n)) => Some(n.to_string()),
            _ => None,
        });
    tracing::info!(
        target: "reddb::http_limits",
        max_handlers = resolved.max_handlers,
        handler_timeout_ms = resolved.handler_timeout_ms,
        retry_after_secs = resolved.retry_after_secs,
        max_inflight_per_principal = resolved.max_inflight_per_principal,
        "http_limits resolved"
    );
    server.with_http_limits(resolved)
}

#[inline(never)]
fn build_http_server_with_transport_readiness(
    runtime: RedDBRuntime,
    auth_store: Arc<AuthStore>,
    bind_addr: String,
    transport_readiness: TransportReadiness,
) -> RedDBServer {
    RedDBServer::with_options(
        runtime,
        ServerOptions {
            bind_addr,
            transport_readiness,
            ..ServerOptions::default()
        },
    )
    .with_auth(auth_store)
}

/// PLAN.md Phase 6.2 — build a listener that only serves
/// `/admin/*` + `/metrics` + `/health/*`. Defaults to `127.0.0.1`
/// when the env var has no host (loopback-only by default per spec).
#[inline(never)]
fn build_admin_only_server(
    runtime: RedDBRuntime,
    auth_store: Arc<AuthStore>,
    bind_addr: String,
) -> RedDBServer {
    RedDBServer::with_options(
        runtime,
        ServerOptions {
            bind_addr,
            surface: crate::server::ServerSurface::AdminOnly,
            ..ServerOptions::default()
        },
    )
    .with_auth(auth_store)
}

/// PLAN.md Phase 6.2 — build a listener that only serves `/metrics`
/// + `/health/*`. Suitable for Prometheus scrape ports that may be
///   exposed wider than the admin port.
#[inline(never)]
fn build_metrics_only_server(
    runtime: RedDBRuntime,
    auth_store: Arc<AuthStore>,
    bind_addr: String,
) -> RedDBServer {
    RedDBServer::with_options(
        runtime,
        ServerOptions {
            bind_addr,
            surface: crate::server::ServerSurface::MetricsOnly,
            ..ServerOptions::default()
        },
    )
    .with_auth(auth_store)
}

/// Spawn dedicated admin / metrics listeners when the operator set
/// `RED_ADMIN_BIND` / `RED_METRICS_BIND`. Both are optional; when
/// unset the existing listener keeps serving everything (back-compat).
fn spawn_admin_metrics_listeners(runtime: &RedDBRuntime, auth_store: &Arc<AuthStore>) {
    if let Some(addr) = env_nonempty("RED_ADMIN_BIND") {
        let server = build_admin_only_server(runtime.clone(), auth_store.clone(), addr.clone());
        let _ = server.serve_in_background();
        tracing::info!(transport = "http", surface = "admin", bind = %addr, "listener online");
    }
    if let Some(addr) = env_nonempty("RED_METRICS_BIND") {
        let server = build_metrics_only_server(runtime.clone(), auth_store.clone(), addr.clone());
        let _ = server.serve_in_background();
        tracing::info!(transport = "http", surface = "metrics", bind = %addr, "listener online");
    }
}

#[inline(never)]
fn run_http_server(config: ServerCommandConfig, bind_addr: String) -> Result<(), String> {
    let cli_telemetry = config.telemetry.clone();
    let mut transport_readiness = TransportReadiness::default();
    let Some(listener) = bind_listener_for_startup(
        &mut transport_readiness,
        "http",
        &bind_addr,
        config.http_bind_explicit,
    )?
    else {
        return Err(format!(
            "no HTTP listener started; implicit bind {} failed",
            bind_addr
        ));
    };
    let db_options = config.to_db_options()?;
    let (runtime, auth_store, _telemetry_guard) =
        build_runtime_and_auth_store(db_options.clone(), cli_telemetry)?;
    let _backup_tasks = spawn_backup_tasks_if_configured(&db_options, &runtime);
    spawn_admin_metrics_listeners(&runtime, &auth_store);
    spawn_http_tls_listener(&config, &runtime, &auth_store)?;
    let server = build_http_server_with_transport_readiness(
        runtime.clone(),
        auth_store,
        bind_addr.clone(),
        transport_readiness,
    );
    let server = apply_http_limits(server, &config, &runtime);
    tracing::info!(transport = "http", bind = %bind_addr, "listener online");
    server.serve_on(listener).map_err(|err| err.to_string())
}

/// PLAN.md HTTP TLS — when `http_tls_bind_addr` is set, spawn a
/// rustls-terminated listener alongside the plain HTTP server. Cert
/// + key paths come from CLI flags or `REDDB_HTTP_TLS_*` env vars; if
///   both are absent and `RED_HTTP_TLS_DEV=1` is set, a self-signed cert
///   is auto-generated next to the data directory (refused otherwise).
fn spawn_http_tls_listener(
    config: &ServerCommandConfig,
    runtime: &RedDBRuntime,
    auth_store: &Arc<AuthStore>,
) -> Result<(), String> {
    let Some(addr) = config.http_tls_bind_addr.clone() else {
        return Ok(());
    };

    let tls_config = resolve_http_tls_config(config)?;
    let server_config = crate::server::tls::build_server_config(&tls_config)
        .map_err(|err| format!("HTTP TLS: {err}"))?;

    let server = build_http_server(runtime.clone(), auth_store.clone(), addr.clone());
    let server = apply_http_limits(server, config, runtime);
    let _handle = server.serve_tls_in_background(server_config);
    tracing::info!(
        transport = "https",
        bind = %addr,
        mtls = %tls_config.client_ca_path.is_some(),
        "TLS listener online"
    );
    Ok(())
}

/// Resolve the HTTP TLS config from CLI / env / dev defaults.
fn resolve_http_tls_config(
    config: &ServerCommandConfig,
) -> Result<crate::server::tls::HttpTlsConfig, String> {
    match (&config.http_tls_cert, &config.http_tls_key) {
        (Some(cert), Some(key)) => Ok(crate::server::tls::HttpTlsConfig {
            cert_path: cert.clone(),
            key_path: key.clone(),
            client_ca_path: config.http_tls_client_ca.clone(),
        }),
        (None, None) => {
            // Dev-mode auto-generate next to the data directory.
            let dir = config
                .path
                .as_ref()
                .and_then(|p| p.parent().map(std::path::PathBuf::from))
                .unwrap_or_else(|| std::path::PathBuf::from("."));
            let auto = crate::server::tls::auto_generate_dev_cert(&dir)
                .map_err(|err| format!("HTTP TLS dev: {err}"))?;
            Ok(crate::server::tls::HttpTlsConfig {
                cert_path: auto.cert_path,
                key_path: auto.key_path,
                client_ca_path: config.http_tls_client_ca.clone(),
            })
        }
        _ => Err("HTTP TLS requires both --http-tls-cert and --http-tls-key (or neither, with RED_HTTP_TLS_DEV=1)".to_string()),
    }
}

#[inline(never)]
fn run_grpc_server(config: ServerCommandConfig, bind_addr: String) -> Result<(), String> {
    let workers = config.workers;
    let cli_telemetry = config.telemetry.clone();
    let db_options = config.to_db_options()?;
    let rt_config = detect_runtime_config();
    let mut transport_readiness = TransportReadiness::default();
    let Some(grpc_listener) = bind_listener_for_startup(
        &mut transport_readiness,
        "grpc",
        &bind_addr,
        config.grpc_bind_explicit,
    )?
    else {
        return Err(format!(
            "no gRPC listener started; implicit bind {} failed",
            bind_addr
        ));
    };

    let worker_threads = workers.unwrap_or(rt_config.suggested_workers);

    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(worker_threads)
        .thread_stack_size(rt_config.stack_size)
        .build()
        .map_err(|err| format!("tokio runtime: {err}"))?;

    // Guard lives on the outer stack so it outlives the tokio runtime.
    let (runtime, auth_store, _telemetry_guard) =
        build_runtime_and_auth_store(db_options.clone(), cli_telemetry)?;
    let _backup_tasks = spawn_backup_tasks_if_configured(&db_options, &runtime);
    let signal_runtime = runtime.clone();
    tokio_runtime.block_on(async move {
        spawn_lifecycle_signal_handler(signal_runtime).await;
        // Start wire protocol listeners (plaintext + TLS)
        spawn_wire_listeners(&config, &runtime, &mut transport_readiness).await?;

        // Start PostgreSQL wire listener when --pg-bind is configured.
        spawn_pg_listener(&config, &runtime);

        // Optional TLS gRPC listener. When `grpc_tls_bind_addr` is set
        // it spawns a separate listener so plaintext + TLS can run
        // side-by-side (50051 plain + 50052 TLS, etc.).
        spawn_grpc_tls_listener_if_configured(&config, runtime.clone(), auth_store.clone());

        let server = RedDBGrpcServer::with_options(
            runtime,
            GrpcServerOptions {
                bind_addr: bind_addr.clone(),
                tls: None,
            },
            auth_store,
        );

        tracing::info!(
            transport = "grpc",
            bind = %bind_addr,
            cpus = rt_config.available_cpus,
            workers = worker_threads,
            "listener online"
        );
        server
            .serve_on(grpc_listener)
            .await
            .map_err(|err| err.to_string())
    })
}

#[inline(never)]
fn run_dual_server(
    config: ServerCommandConfig,
    grpc_bind_addr: String,
    http_bind_addr: String,
) -> Result<(), String> {
    let workers = config.workers;
    let cli_telemetry = config.telemetry.clone();
    let db_options = config.to_db_options()?;
    let rt_config = detect_runtime_config();
    let worker_threads = workers.unwrap_or(rt_config.suggested_workers);
    let mut transport_readiness = TransportReadiness::default();
    let http_listener = bind_listener_for_startup(
        &mut transport_readiness,
        "http",
        &http_bind_addr,
        config.http_bind_explicit,
    )?;
    let grpc_listener = bind_listener_for_startup(
        &mut transport_readiness,
        "grpc",
        &grpc_bind_addr,
        config.grpc_bind_explicit,
    )?;
    if http_listener.is_none() && grpc_listener.is_none() {
        return Err("no listener started; implicit HTTP and gRPC binds failed".to_string());
    }
    let (runtime, auth_store, _telemetry_guard) =
        build_runtime_and_auth_store(db_options.clone(), cli_telemetry)?;
    let _backup_tasks = spawn_backup_tasks_if_configured(&db_options, &runtime);

    spawn_admin_metrics_listeners(&runtime, &auth_store);
    spawn_http_tls_listener(&config, &runtime, &auth_store)?;

    let http_handle = if let Some(listener) = http_listener {
        let http_server = build_http_server_with_transport_readiness(
            runtime.clone(),
            auth_store.clone(),
            http_bind_addr.clone(),
            transport_readiness.clone(),
        );
        let http_server = apply_http_limits(http_server, &config, &runtime);
        Some(http_server.serve_in_background_on(listener))
    } else {
        None
    };

    thread::sleep(Duration::from_millis(150));
    if let Some(handle) = http_handle.as_ref() {
        if handle.is_finished() {
            let handle = http_handle.unwrap();
            return match handle.join() {
                Ok(Ok(())) => Err("HTTP server exited unexpectedly".to_string()),
                Ok(Err(err)) => Err(err.to_string()),
                Err(_) => Err("HTTP server thread panicked".to_string()),
            };
        }
    }
    if grpc_listener.is_none() {
        let Some(handle) = http_handle else {
            return Err("no listener started".to_string());
        };
        return match handle.join() {
            Ok(Ok(())) => Err("HTTP server exited unexpectedly".to_string()),
            Ok(Err(err)) => Err(err.to_string()),
            Err(_) => Err("HTTP server thread panicked".to_string()),
        };
    }
    let grpc_listener = grpc_listener.expect("checked above");

    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(worker_threads)
        .thread_stack_size(rt_config.stack_size)
        .build()
        .map_err(|err| format!("tokio runtime: {err}"))?;

    let signal_runtime = runtime.clone();
    tokio_runtime.block_on(async move {
        spawn_lifecycle_signal_handler(signal_runtime).await;
        // Start wire protocol listeners (plaintext + TLS)
        spawn_wire_listeners(&config, &runtime, &mut transport_readiness).await?;

        // Start PostgreSQL wire listener when --pg-bind is configured.
        spawn_pg_listener(&config, &runtime);

        // Optional TLS gRPC listener — runs alongside the plaintext one.
        spawn_grpc_tls_listener_if_configured(&config, runtime.clone(), auth_store.clone());

        let server = RedDBGrpcServer::with_options(
            runtime,
            GrpcServerOptions {
                bind_addr: grpc_bind_addr.clone(),
                tls: None,
            },
            auth_store,
        );

        tracing::info!(transport = "http", bind = %http_bind_addr, "listener online");
        tracing::info!(
            transport = "grpc",
            bind = %grpc_bind_addr,
            cpus = rt_config.available_cpus,
            workers = worker_threads,
            "listener online"
        );
        server
            .serve_on(grpc_listener)
            .await
            .map_err(|err| err.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_systemd_unit_contains_expected_execstart() {
        let config = SystemdServiceConfig {
            service_name: "reddb".to_string(),
            binary_path: PathBuf::from("/usr/local/bin/red"),
            run_user: "reddb".to_string(),
            run_group: "reddb".to_string(),
            data_path: PathBuf::from("/var/lib/reddb/data.rdb"),
            router_bind_addr: None,
            grpc_bind_addr: Some("0.0.0.0:5555".to_string()),
            http_bind_addr: None,
        };

        let unit = render_systemd_unit(&config);
        assert!(unit.contains("ExecStart=/usr/local/bin/red server --path /var/lib/reddb/data.rdb --grpc-bind 0.0.0.0:5555"));
        assert!(unit.contains("ReadWritePaths=/var/lib/reddb"));
    }

    #[test]
    fn systemd_service_config_derives_paths() {
        let config = SystemdServiceConfig {
            service_name: "reddb-api".to_string(),
            binary_path: PathBuf::from("/usr/local/bin/red"),
            run_user: "reddb".to_string(),
            run_group: "reddb".to_string(),
            data_path: PathBuf::from("/srv/reddb/live/data.rdb"),
            router_bind_addr: None,
            grpc_bind_addr: None,
            http_bind_addr: Some("127.0.0.1:5055".to_string()),
        };

        assert_eq!(config.data_dir(), PathBuf::from("/srv/reddb/live"));
        assert_eq!(
            config.unit_path(),
            PathBuf::from("/etc/systemd/system/reddb-api.service")
        );
    }

    #[test]
    fn render_systemd_unit_supports_dual_transport() {
        let config = SystemdServiceConfig {
            service_name: "reddb".to_string(),
            binary_path: PathBuf::from("/usr/local/bin/red"),
            run_user: "reddb".to_string(),
            run_group: "reddb".to_string(),
            data_path: PathBuf::from("/var/lib/reddb/data.rdb"),
            router_bind_addr: None,
            grpc_bind_addr: Some("0.0.0.0:5555".to_string()),
            http_bind_addr: Some("0.0.0.0:5055".to_string()),
        };

        let unit = render_systemd_unit(&config);
        assert!(unit.contains("--grpc-bind 0.0.0.0:5555"));
        assert!(unit.contains("--http-bind 0.0.0.0:5055"));
    }

    #[test]
    fn render_systemd_unit_supports_router_mode() {
        let config = SystemdServiceConfig {
            service_name: "reddb".to_string(),
            binary_path: PathBuf::from("/usr/local/bin/red"),
            run_user: "reddb".to_string(),
            run_group: "reddb".to_string(),
            data_path: PathBuf::from("/var/lib/reddb/data.rdb"),
            router_bind_addr: Some(DEFAULT_ROUTER_BIND_ADDR.to_string()),
            grpc_bind_addr: None,
            http_bind_addr: None,
        };

        let unit = render_systemd_unit(&config);
        assert!(unit.contains("--bind 127.0.0.1:5050"));
        assert!(!unit.contains("--grpc-bind"));
        assert!(!unit.contains("--http-bind"));
    }

    #[test]
    fn explicit_bind_collision_is_fatal() {
        let held = TcpListener::bind("127.0.0.1:0").expect("hold test port");
        let addr = held.local_addr().expect("held addr").to_string();
        let mut readiness = TransportReadiness::default();

        let error = bind_listener_for_startup(&mut readiness, "http", &addr, true).unwrap_err();

        assert!(error.contains("explicit http listener bind"));
        assert_eq!(readiness.active.len(), 0);
        assert_eq!(readiness.failed.len(), 1);
        assert!(readiness.failed[0].explicit);
        assert_eq!(readiness.failed[0].bind_addr, addr);
    }

    // ---------- Issue #663 — `--no-auth` / `--dev` ----------

    // Env access in tests is process-global; serialise the two
    // `--no-auth` tests so the REDDB_USERNAME / REDDB_PASSWORD pair
    // one of them sets cannot leak into the other under cargo's
    // default parallel runner.
    fn no_auth_env_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    fn no_auth_test_config(no_auth: bool) -> ServerCommandConfig {
        ServerCommandConfig {
            path: None,
            router_bind_addr: Some(DEFAULT_ROUTER_BIND_ADDR.to_string()),
            router_bind_explicit: false,
            grpc_bind_addr: None,
            grpc_bind_explicit: false,
            grpc_tls_bind_addr: None,
            grpc_tls_cert: None,
            grpc_tls_key: None,
            grpc_tls_client_ca: None,
            http_bind_addr: None,
            http_bind_explicit: false,
            http_tls_bind_addr: None,
            http_tls_cert: None,
            http_tls_key: None,
            http_tls_client_ca: None,
            wire_bind_addr: None,
            wire_bind_explicit: false,
            wire_tls_bind_addr: None,
            wire_tls_cert: None,
            wire_tls_key: None,
            pg_bind_addr: None,
            create_if_missing: true,
            read_only: false,
            role: "standalone".to_string(),
            primary_addr: None,
            storage_profile: StorageProfileSelection::embedded_single_file(),
            // Operator-set `--vault`: `--no-auth` must override this
            // alongside REDDB_USERNAME/PASSWORD.
            vault: true,
            no_auth,
            workers: None,
            telemetry: None,
            http_limits_cli: crate::server::HttpLimitsCliInput::default(),
        }
    }

    #[test]
    fn no_auth_flag_disables_every_auth_knob_and_stamps_metadata() {
        let _g = no_auth_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        // Pre-existing env that *would* turn auth on if `--no-auth`
        // weren't the last word. The acceptance criterion is that
        // the flag wins over env.
        // SAFETY: serialised by `no_auth_env_lock` above.
        unsafe {
            std::env::set_var("REDDB_USERNAME", "admin");
            std::env::set_var("REDDB_PASSWORD", "hunter2");
        }
        let config = no_auth_test_config(true);
        let options = config.to_db_options().expect("to_db_options");

        assert!(no_auth_active(&options), "metadata should be stamped");
        assert!(!options.auth.enabled, "auth.enabled must be forced off");
        assert!(
            !options.auth.require_auth,
            "require_auth must be forced off"
        );
        assert!(
            !options.auth.vault_enabled,
            "vault_enabled must be forced off (overrides --vault)"
        );
        assert_eq!(
            options.metadata.get(NO_AUTH_META).map(String::as_str),
            Some("true"),
        );

        // SAFETY: serialised by `no_auth_env_lock` above.
        unsafe {
            std::env::remove_var("REDDB_USERNAME");
            std::env::remove_var("REDDB_PASSWORD");
        }
    }

    #[test]
    fn default_behaviour_without_no_auth_flag_is_unchanged() {
        let _g = no_auth_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        let config = no_auth_test_config(false);
        let options = config.to_db_options().expect("to_db_options");

        assert!(
            !no_auth_active(&options),
            "default boot must not be marked no-auth"
        );
        assert!(
            options.metadata.get(NO_AUTH_META).is_none(),
            "metadata key must be absent when flag is off"
        );
        // `--vault` should still take effect when `--no-auth` is not set.
        assert!(options.auth.vault_enabled);
    }

    #[test]
    fn no_auth_active_blocks_bootstrap_from_env() {
        let _g = no_auth_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        // SAFETY: serialised by `no_auth_env_lock` above. The pair
        // would normally cause `AuthStore::bootstrap_from_env` to
        // create an admin; the boot pipeline must suppress that call
        // whenever `no_auth_active` is true.
        unsafe {
            std::env::set_var("REDDB_USERNAME", "admin");
            std::env::set_var("REDDB_PASSWORD", "hunter2");
        }

        let options = no_auth_test_config(true)
            .to_db_options()
            .expect("to_db_options");

        // Mirror the exact branch in `build_runtime_with_telemetry`:
        // build a non-vault AuthStore from `options.auth`, then call
        // `bootstrap_from_env` *only* when the no-auth gate is off.
        let auth_store = AuthStore::new(options.auth.clone());
        if !no_auth_active(&options) {
            auth_store.bootstrap_from_env();
        }

        assert!(
            auth_store.needs_bootstrap(),
            "no admin user must be bootstrapped under --no-auth even with REDDB_USERNAME/PASSWORD set"
        );

        // SAFETY: serialised by `no_auth_env_lock` above.
        unsafe {
            std::env::remove_var("REDDB_USERNAME");
            std::env::remove_var("REDDB_PASSWORD");
        }
    }

    // ---------- Issue #650 — bootstrap presets ----------

    // Preset tests mutate process-global env (`REDDB_PRESET`,
    // `REDDB_USERNAME`, `REDDB_PASSWORD`) and the global tracing
    // subscriber. Share the no_auth lock so they don't race with each
    // other or with the --no-auth tests above.
    fn clear_preset_env() {
        // SAFETY: callers hold `no_auth_env_lock()`.
        unsafe {
            std::env::remove_var(PRESET_ENV);
            std::env::remove_var("REDDB_BOOTSTRAP_MANIFEST");
            std::env::remove_var("REDDB_USERNAME");
            std::env::remove_var("REDDB_PASSWORD");
            std::env::remove_var("REDDB_USERNAME_FILE");
            std::env::remove_var("REDDB_PASSWORD_FILE");
        }
    }

    fn clear_backup_env() {
        // SAFETY: callers hold `no_auth_env_lock()`.
        unsafe {
            std::env::remove_var("REDDB_BACKUP_S3_ENDPOINT");
            std::env::remove_var("REDDB_BACKUP_S3_BUCKET");
            std::env::remove_var("REDDB_BACKUP_S3_PREFIX");
            std::env::remove_var("REDDB_BACKUP_S3_ACCESS_KEY_ID");
            std::env::remove_var("REDDB_BACKUP_S3_SECRET_ACCESS_KEY");
            std::env::remove_var("REDDB_BACKUP_S3_REGION");
            std::env::remove_var("REDDB_BACKUP_CHECKPOINT_INTERVAL_SECS");
            std::env::remove_var("REDDB_BACKUP_WAL_FLUSH_INTERVAL_SECS");
            std::env::remove_var("REDDB_BACKUP_PAUSE_ON_LAG_SECS");
        }
    }

    fn fresh_runtime_and_store() -> (RedDBRuntime, Arc<AuthStore>) {
        let runtime = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
        let auth_store = Arc::new(AuthStore::new(crate::auth::AuthConfig::default()));
        (runtime, auth_store)
    }

    #[test]
    fn simple_preset_is_default_and_persists_state() {
        let _g = no_auth_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        clear_preset_env();

        let (runtime, auth_store) = fresh_runtime_and_store();
        apply_preset(&runtime, &auth_store).expect("simple preset applies cleanly");

        // No admin was created — `simple` is anonymous-friendly.
        assert!(
            auth_store.needs_bootstrap(),
            "simple preset must not create an admin"
        );

        // Bootstrap state persisted so the next boot is a no-op.
        let store = runtime.db().store();
        let completed = store
            .get_config(BOOTSTRAP_COMPLETED_KEY)
            .expect("completed key persisted");
        assert!(matches!(
            completed,
            crate::storage::schema::Value::Boolean(true)
        ));
        let preset = store
            .get_config(BOOTSTRAP_PRESET_KEY)
            .expect("preset key persisted");
        match preset {
            crate::storage::schema::Value::Text(s) => assert_eq!(s.as_ref(), PRESET_SIMPLE),
            other => panic!("expected Text(simple), got {other:?}"),
        }
        assert!(
            store.get_config(BOOTSTRAP_FIRST_ADMIN_KEY).is_none(),
            "simple preset must not record a first admin"
        );

        clear_preset_env();
    }

    #[test]
    fn production_preset_creates_first_admin_with_allow_all_policy() {
        use crate::auth::policies::{EvalContext, ResourceRef};
        use crate::auth::UserId;

        let _g = no_auth_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        clear_preset_env();
        // SAFETY: env serialised by `no_auth_env_lock`.
        unsafe {
            std::env::set_var(PRESET_ENV, PRESET_PRODUCTION);
            std::env::set_var("REDDB_USERNAME", "ops");
            std::env::set_var("REDDB_PASSWORD", "hunter2");
        }

        let (runtime, auth_store) = fresh_runtime_and_store();
        apply_preset(&runtime, &auth_store).expect("production preset applies cleanly");

        // Admin exists and the auth store is sealed.
        assert!(
            !auth_store.needs_bootstrap(),
            "production preset must seal bootstrap"
        );
        let users = auth_store.list_users();
        assert_eq!(users.len(), 1);
        let admin = &users[0];
        assert_eq!(admin.username, "ops");
        assert!(
            admin.system_owned,
            "first admin must be system-owned to pass the managed-config gate"
        );
        assert!(
            admin.tenant_id.is_none(),
            "first admin must be platform-scoped (tenant=None)"
        );

        // Allow-all policy was installed and attached to the first admin.
        let policy = auth_store
            .get_policy(FIRST_ADMIN_ALLOW_ALL_POLICY)
            .expect("allow-all policy installed");
        assert!(!policy.statements.is_empty());

        // Verify policy-derived authority via the policy evaluator —
        // not a bypass. Any action on any resource must Allow.
        let actor = UserId::platform("ops");
        let ctx = EvalContext {
            principal_tenant: None,
            current_tenant: None,
            peer_ip: None,
            mfa_present: false,
            now_ms: 1_700_000_000_000,
            principal_is_admin_role: true,
            principal_is_system_owned: true,
            principal_is_platform_scoped: true,
        };
        let arbitrary_resource = ResourceRef::new("config", "red.config.audit.enabled");
        assert!(
            auth_store.check_policy_authz(&actor, "config:write", &arbitrary_resource, &ctx),
            "allow-all policy must grant arbitrary actions via the evaluator"
        );

        // Persisted state records the first admin id.
        let store = runtime.db().store();
        match store
            .get_config(BOOTSTRAP_FIRST_ADMIN_KEY)
            .expect("first_admin_id persisted")
        {
            crate::storage::schema::Value::Text(s) => assert_eq!(s.as_ref(), "ops"),
            other => panic!("expected Text(ops), got {other:?}"),
        }
        match store.get_config(BOOTSTRAP_PRESET_KEY).unwrap() {
            crate::storage::schema::Value::Text(s) => assert_eq!(s.as_ref(), PRESET_PRODUCTION),
            other => panic!("expected Text(production), got {other:?}"),
        }

        clear_preset_env();
    }

    #[test]
    fn regulated_preset_enables_query_audit_infrastructure_without_rules() {
        let _g = no_auth_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        clear_preset_env();
        // SAFETY: env serialised by `no_auth_env_lock`.
        unsafe {
            std::env::set_var(PRESET_ENV, PRESET_REGULATED);
        }

        let (runtime, auth_store) = fresh_runtime_and_store();
        apply_preset(&runtime, &auth_store).expect("regulated preset applies cleanly");

        assert!(runtime.query_audit().is_enabled());
        assert!(runtime.query_audit().rules().is_empty());
        assert!(
            runtime
                .db()
                .store()
                .get_collection(crate::runtime::query_audit::QUERY_AUDIT_COLLECTION)
                .is_some(),
            "regulated preset should create the query-audit stream"
        );

        runtime
            .execute_query("CREATE TABLE docs (id INT)")
            .expect("create table");
        runtime
            .execute_query("INSERT INTO docs (id) VALUES (1)")
            .expect("insert");
        runtime.execute_query("SELECT * FROM docs").expect("select");
        let rows = runtime
            .db()
            .store()
            .get_collection(crate::runtime::query_audit::QUERY_AUDIT_COLLECTION)
            .expect("query audit collection")
            .query_all(|_| true);
        assert!(
            rows.is_empty(),
            "regulated preset must not globally audit every query"
        );

        clear_preset_env();
    }

    #[test]
    fn managed_backup_env_rejects_primary_replica_single_file_storage() {
        let _g = no_auth_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        clear_backup_env();
        // SAFETY: env serialised by `no_auth_env_lock`.
        unsafe {
            std::env::set_var("REDDB_BACKUP_S3_ENDPOINT", "https://s3.example.test");
            std::env::set_var("REDDB_BACKUP_S3_BUCKET", "reddb");
            std::env::set_var("REDDB_BACKUP_S3_PREFIX", "clusters/prod");
            std::env::set_var("REDDB_BACKUP_S3_ACCESS_KEY_ID", "AK");
            std::env::set_var("REDDB_BACKUP_S3_SECRET_ACCESS_KEY", "SK");
        }

        let mut config = no_auth_test_config(false);
        config.role = "primary".to_string();
        config.storage_profile = crate::storage::StorageDeployPreset::PrimaryReplicaDev.selection();

        let err = config.to_db_options().unwrap_err();
        assert!(err.contains("managed backup"), "got: {err}");
        assert!(err.contains("operational-directory"), "got: {err}");

        clear_backup_env();
    }

    #[test]
    fn regulated_preset_installs_managed_evidence_guardrails_end_to_end() {
        use crate::auth::policies::{EvalContext, Policy, ResourceRef};
        use crate::auth::store::PrincipalRef;
        use crate::auth::{Role, UserId};
        use crate::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
        use crate::storage::schema::Value;

        let _g = no_auth_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        clear_preset_env();
        // SAFETY: env serialised by `no_auth_env_lock`.
        unsafe {
            std::env::set_var(PRESET_ENV, PRESET_REGULATED);
        }

        let options = no_auth_test_config(false)
            .to_db_options()
            .expect("regulated options");
        assert!(
            options.control_events.compliance_mode,
            "regulated preset must enable fail-closed control evidence before runtime boot"
        );
        assert!(
            options.query_audit.enabled && options.query_audit.rules.is_empty(),
            "regulated preset must enable query-audit infrastructure without global rules"
        );

        let runtime = RedDBRuntime::with_options(options).expect("runtime");
        let auth_store = Arc::new(AuthStore::new(crate::auth::AuthConfig::default()));
        apply_preset(&runtime, &auth_store).expect("regulated preset applies cleanly");
        runtime.set_auth_store(Arc::clone(&auth_store));

        assert!(runtime.control_events_require_persistence());
        assert!(runtime.query_audit().is_enabled());
        assert!(runtime.query_audit().rules().is_empty());
        assert!(auth_store
            .get_policy(REGULATED_PROTECT_MANAGED_POLICY)
            .is_some());

        let managed_policy = runtime
            .config_registry()
            .get_active(REGULATED_PROTECT_MANAGED_POLICY)
            .expect("regulated managed policy registry entry");
        assert!(managed_policy.managed);
        assert_eq!(managed_policy.resource_type, "policy");
        assert!(
            runtime
                .config_registry()
                .get_active(REGULATED_AUDIT_CONFIG_NAMESPACE)
                .expect("regulated audit config namespace")
                .managed
        );

        let registry_rows = runtime
            .execute_query(&format!(
                "SELECT id, managed FROM red.registry WHERE id = '{}'",
                REGULATED_PROTECT_MANAGED_POLICY
            ))
            .expect("red.registry query");
        assert_eq!(registry_rows.result.records.len(), 1);
        assert_eq!(
            registry_rows.result.records[0].get("managed"),
            Some(&Value::Boolean(true))
        );

        let managed_policy_rows = runtime
            .execute_query(&format!(
                "SELECT policy_id FROM red.managed_policies WHERE policy_id = '{}'",
                REGULATED_PROTECT_MANAGED_POLICY
            ))
            .expect("red.managed_policies query");
        assert_eq!(managed_policy_rows.result.records.len(), 1);

        let capability_rows = runtime
            .execute_query(
                "SELECT action FROM red.control_capabilities WHERE action = 'evidence:export'",
            )
            .expect("red.control_capabilities query");
        assert_eq!(capability_rows.result.records.len(), 1);

        auth_store
            .create_user("alice", "p", Role::Admin)
            .expect("create ordinary admin");
        let allow_all = Policy::from_json_str(
            r#"{
                "id": "alice-allow-all",
                "version": 1,
                "statements": [{
                    "effect": "allow",
                    "actions": ["*"],
                    "resources": ["*"]
                }]
            }"#,
        )
        .expect("allow-all policy");
        auth_store.put_policy(allow_all).expect("install allow-all");
        auth_store
            .attach_policy(
                PrincipalRef::User(UserId::platform("alice")),
                "alice-allow-all",
            )
            .expect("attach allow-all");
        let ctx = EvalContext {
            principal_tenant: None,
            current_tenant: None,
            peer_ip: None,
            mfa_present: false,
            now_ms: 1_700_000_000_000,
            principal_is_admin_role: true,
            principal_is_system_owned: false,
            principal_is_platform_scoped: true,
        };
        assert!(
            auth_store.check_policy_authz(
                &UserId::platform("alice"),
                "policy:drop",
                &ResourceRef::new("policy", REGULATED_PROTECT_MANAGED_POLICY),
                &ctx,
            ),
            "ordinary allow-all policy should be broad enough that only the managed guardrail blocks"
        );

        set_current_auth_identity("alice".to_string(), Role::Admin);
        let denied = runtime.execute_query(&format!(
            "DROP POLICY '{}'",
            REGULATED_PROTECT_MANAGED_POLICY
        ));
        clear_current_auth_identity();
        let err = denied.expect_err("managed policy guardrail must deny ordinary admin");
        assert!(
            err.to_string().contains("managed policy"),
            "error should name the managed guardrail: {err}"
        );
        assert!(
            auth_store
                .get_policy(REGULATED_PROTECT_MANAGED_POLICY)
                .is_some(),
            "denied mutation must leave managed policy installed"
        );

        let denied_events = runtime
            .execute_query(&format!(
                "SELECT action, resource, outcome FROM red.control_events \
                 WHERE action = 'policy:drop' AND resource = 'policy:{}'",
                REGULATED_PROTECT_MANAGED_POLICY
            ))
            .expect("red.control_events denied policy drop");
        assert_eq!(denied_events.result.records.len(), 1);
        assert_eq!(
            denied_events.result.records[0].get("outcome"),
            Some(&Value::text("denied"))
        );

        set_current_auth_identity("alice".to_string(), Role::Admin);
        let config_denied = runtime.execute_query("SET CONFIG red.config.audit.enabled = true");
        clear_current_auth_identity();
        let err = config_denied.expect_err("managed config guardrail must deny ordinary admin");
        assert!(
            err.to_string().contains("managed config"),
            "error should name the managed config guardrail: {err}"
        );

        let denied_config_events = runtime
            .execute_query(
                "SELECT action, resource, outcome FROM red.control_events \
                 WHERE action = 'config:write' AND resource = 'config:red.config.audit.enabled'",
            )
            .expect("red.control_events denied config write");
        assert_eq!(denied_config_events.result.records.len(), 1);
        assert_eq!(
            denied_config_events.result.records[0].get("outcome"),
            Some(&Value::text("denied"))
        );

        runtime
            .execute_query("CREATE TABLE regulated_docs (id INT)")
            .expect("create user table");
        runtime
            .execute_query("SELECT * FROM regulated_docs")
            .expect("select user table");
        let audit_rows = runtime
            .db()
            .store()
            .get_collection(crate::runtime::query_audit::QUERY_AUDIT_COLLECTION)
            .expect("query audit collection")
            .query_all(|_| true);
        assert!(
            audit_rows.is_empty(),
            "regulated preset must not globally audit data-plane queries"
        );

        clear_preset_env();
    }

    #[test]
    fn bootstrap_manifest_installs_initial_users_policies_guardrails_and_config() {
        use crate::auth::policies::{EvalContext, ResourceRef};
        use crate::auth::UserId;
        use crate::storage::schema::Value;

        let _g = no_auth_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        clear_preset_env();

        let manifest_dir = std::env::current_dir()
            .expect("current dir")
            .join(".red/tmp/bootstrap-manifest-tests");
        std::fs::create_dir_all(&manifest_dir).expect("create manifest test dir");
        let manifest_path = manifest_dir.join(format!(
            "reddb-bootstrap-manifest-{}-{}.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
        ));
        std::fs::write(
            &manifest_path,
            r#"{
                "users": [
                    {
                        "username": "ops",
                        "password": "hunter2",
                        "role": "admin",
                        "system_owned": true
                    }
                ],
                "policies": [
                    {
                        "id": "bootstrap-registry-admin",
                        "version": 1,
                        "statements": [
                            {
                                "effect": "allow",
                                "actions": ["red.registry:*", "policy:*", "config:write", "select"],
                                "resources": ["registry:*", "policy:*", "config:*", "collection:docs"]
                            }
                        ]
                    }
                ],
                "managed_policies": [
                    {
                        "id": "managed-deny-drop",
                        "version": 1,
                        "statements": [
                            {
                                "effect": "deny",
                                "actions": ["policy:drop"],
                                "resources": ["policy:managed-deny-drop"]
                            }
                        ],
                        "required_resource": "policy:managed-deny-drop",
                        "evidence": "full"
                    }
                ],
                "attachments": [
                    {"user": "ops", "policy": "bootstrap-registry-admin"}
                ],
                "managed_config_namespaces": [
                    {
                        "id": "red.ai",
                        "required_action": "config:write",
                        "required_resource": "config:red.ai.*",
                        "evidence": "metadata"
                    }
                ],
                "config": [
                    {"key": "red.ai.default.provider", "value": "openai"},
                    {
                        "key": "red.ai.openai.default.secret_ref",
                        "secret_ref": {"collection": "red.vault", "key": "openai"}
                    }
                ],
                "actor": "ops"
            }"#,
        )
        .expect("write manifest");
        // SAFETY: env serialised by `no_auth_env_lock`.
        unsafe {
            std::env::set_var("REDDB_BOOTSTRAP_MANIFEST", &manifest_path);
        }

        let (runtime, auth_store) = fresh_runtime_and_store();
        apply_preset(&runtime, &auth_store).expect("manifest applies cleanly");

        let users = auth_store.list_users();
        assert_eq!(users.len(), 1);
        assert_eq!(users[0].username, "ops");
        assert!(users[0].system_owned);

        let actor = UserId::platform("ops");
        let ctx = EvalContext {
            principal_tenant: None,
            current_tenant: None,
            peer_ip: None,
            mfa_present: false,
            now_ms: 1_700_000_000_000,
            principal_is_admin_role: true,
            principal_is_system_owned: true,
            principal_is_platform_scoped: true,
        };
        // Manifest fixture pins a canonical data-plane read action.
        assert!(auth_store.check_policy_authz(
            &actor,
            "select",
            &ResourceRef::new("collection", "docs"),
            &ctx
        ));

        let managed_policy = runtime
            .config_registry()
            .get_active("managed-deny-drop")
            .expect("managed policy registry entry");
        assert!(managed_policy.managed);
        assert_eq!(managed_policy.resource_type, "policy");
        let managed_config = runtime
            .config_registry()
            .get_active("red.ai")
            .expect("managed config namespace registry entry");
        assert!(managed_config.managed);
        assert_eq!(managed_config.resource_type, "config_namespace");

        let store = runtime.db().store();
        match store
            .get_config("red.ai.default.provider")
            .expect("plain config persisted")
        {
            Value::Text(s) => assert_eq!(s.as_ref(), "openai"),
            other => panic!("expected provider text, got {other:?}"),
        }
        let Value::Json(bytes) = store
            .get_config("red.ai.openai.default.secret_ref")
            .expect("secret ref config persisted")
        else {
            panic!("secret ref must be stored as structured JSON");
        };
        let reference: crate::serde_json::Value =
            crate::serde_json::from_slice(&bytes).expect("secret ref json");
        assert_eq!(
            reference.get("type").and_then(|v| v.as_str()),
            Some("secret_ref")
        );
        assert!(
            !String::from_utf8_lossy(&bytes).contains("hunter2"),
            "manifest password must not leak into secret ref config"
        );

        let completed = store
            .get_config(BOOTSTRAP_COMPLETED_KEY)
            .expect("bootstrap completion persisted");
        assert!(matches!(completed, Value::Boolean(true)));
        assert!(
            store
                .get_config("system.bootstrap.manifest.registry_entries")
                .is_some(),
            "managed registry entries must be persisted internally"
        );

        std::fs::remove_file(&manifest_path).expect("remove manifest after first boot");
        let restored_registry = Arc::new(crate::auth::registry::ConfigRegistry::new());
        crate::cli::bootstrap_manifest::rehydrate_manifest_registry(&runtime, &restored_registry)
            .expect("registry rehydrates without manifest file");
        assert!(restored_registry.get_active("managed-deny-drop").is_some());
        assert!(restored_registry.get_active("red.ai").is_some());

        let fresh = Arc::new(AuthStore::new(crate::auth::AuthConfig::default()));
        apply_preset(&runtime, &fresh).expect("re-run must not need manifest file");
        assert!(fresh.needs_bootstrap());

        clear_preset_env();
    }

    #[test]
    fn production_preset_refuses_to_start_without_password() {
        let _g = no_auth_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        clear_preset_env();
        // SAFETY: env serialised by `no_auth_env_lock`.
        unsafe {
            std::env::set_var(PRESET_ENV, PRESET_PRODUCTION);
            std::env::set_var("REDDB_USERNAME", "ops");
        }

        let (runtime, auth_store) = fresh_runtime_and_store();
        let err = apply_preset(&runtime, &auth_store).expect_err("must reject missing password");
        assert!(
            err.contains("REDDB_PASSWORD"),
            "error must name the missing env: {err}"
        );

        // Nothing was persisted; nothing was created.
        assert!(auth_store.needs_bootstrap());
        assert!(runtime
            .db()
            .store()
            .get_config(BOOTSTRAP_COMPLETED_KEY)
            .is_none());

        clear_preset_env();
    }

    #[test]
    fn re_running_production_after_first_boot_is_a_silent_skip() {
        let _g = no_auth_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        clear_preset_env();
        // SAFETY: env serialised by `no_auth_env_lock`.
        unsafe {
            std::env::set_var(PRESET_ENV, PRESET_PRODUCTION);
            std::env::set_var("REDDB_USERNAME", "ops");
            std::env::set_var("REDDB_PASSWORD", "hunter2");
        }

        let (runtime, auth_store) = fresh_runtime_and_store();
        apply_preset(&runtime, &auth_store).expect("first apply");
        assert_eq!(auth_store.list_users().len(), 1);

        // Second invocation: same runtime/store, same env. Must be a
        // no-op — no error, no duplicate admin, no duplicate policy.
        // We do NOT reuse `auth_store` because production sealed it; the
        // idempotency hinge is the persisted config key, not the auth
        // store's in-memory seal. Build a fresh `AuthStore` as a restart
        // would and confirm `apply_preset` is a silent skip.
        let fresh = Arc::new(AuthStore::new(crate::auth::AuthConfig::default()));
        apply_preset(&runtime, &fresh).expect("re-run is silent-skip");
        assert!(
            fresh.needs_bootstrap(),
            "re-run must not create a second admin"
        );
        assert!(
            fresh.get_policy(FIRST_ADMIN_ALLOW_ALL_POLICY).is_none(),
            "re-run must not re-install the allow-all policy on the fresh store"
        );

        clear_preset_env();
    }

    #[test]
    fn unrecognised_preset_value_is_rejected() {
        let _g = no_auth_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        clear_preset_env();
        // SAFETY: env serialised by `no_auth_env_lock`.
        unsafe {
            std::env::set_var(PRESET_ENV, "weird");
        }

        let (runtime, auth_store) = fresh_runtime_and_store();
        let err = apply_preset(&runtime, &auth_store).expect_err("must reject unknown preset");
        assert!(err.contains("weird"), "error must echo the value: {err}");
        assert!(auth_store.needs_bootstrap());

        clear_preset_env();
    }

    #[test]
    fn no_auth_short_circuits_preset_entirely() {
        let _g = no_auth_env_lock().lock().unwrap_or_else(|e| e.into_inner());
        clear_preset_env();
        // SAFETY: env serialised by `no_auth_env_lock`. Production creds
        // are set but `--no-auth` must win — no admin, no bootstrap state.
        unsafe {
            std::env::set_var(PRESET_ENV, PRESET_PRODUCTION);
            std::env::set_var("REDDB_USERNAME", "ops");
            std::env::set_var("REDDB_PASSWORD", "hunter2");
        }

        let options = no_auth_test_config(true)
            .to_db_options()
            .expect("to_db_options");
        assert!(no_auth_active(&options));

        // Mirror `build_runtime_with_telemetry`: when no_auth_active,
        // `apply_preset` is never called.
        let (runtime, auth_store) = fresh_runtime_and_store();
        if !no_auth_active(&options) {
            apply_preset(&runtime, &auth_store).expect("would apply preset");
        }

        assert!(
            auth_store.needs_bootstrap(),
            "--no-auth must prevent any admin creation"
        );
        assert!(
            runtime
                .db()
                .store()
                .get_config(BOOTSTRAP_COMPLETED_KEY)
                .is_none(),
            "--no-auth must skip bootstrap-state persistence"
        );

        clear_preset_env();
    }

    #[test]
    fn implicit_bind_collision_degrades() {
        let held = TcpListener::bind("127.0.0.1:0").expect("hold test port");
        let addr = held.local_addr().expect("held addr").to_string();
        let mut readiness = TransportReadiness::default();

        let listener =
            bind_listener_for_startup(&mut readiness, "http", &addr, false).expect("nonfatal");

        assert!(listener.is_none());
        assert_eq!(readiness.active.len(), 0);
        assert_eq!(readiness.failed.len(), 1);
        assert!(!readiness.failed[0].explicit);
        assert_eq!(readiness.failed[0].bind_addr, addr);
    }
}
