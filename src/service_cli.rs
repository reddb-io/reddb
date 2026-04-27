use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::auth::store::AuthStore;
use crate::replication::ReplicationConfig;
use crate::runtime::RedDBRuntime;
use crate::service_router::{serve_tcp_router, TcpProtocolRouterConfig};
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
            Self::Grpc => "127.0.0.1:50051",
            Self::Http => "127.0.0.1:8080",
            Self::Wire => "127.0.0.1:5050",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServerCommandConfig {
    pub path: Option<PathBuf>,
    pub router_bind_addr: Option<String>,
    pub grpc_bind_addr: Option<String>,
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
    pub vault: bool,
    /// Override worker thread count (None = auto-detect from CPUs)
    pub workers: Option<usize>,
    /// Telemetry config (Phase 6 logging). `None` falls back to the
    /// built-in default derived from `path` + stderr-only.
    pub telemetry: Option<crate::telemetry::TelemetryConfig>,
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

impl ServerCommandConfig {
    fn to_db_options(&self) -> RedDBOptions {
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
                    .unwrap_or_else(|| "http://127.0.0.1:50051".to_string());
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

        if self.vault {
            options.auth.vault_enabled = true;
        }

        configure_remote_backend_from_env(&mut options);

        options
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
        || config.wire_bind_addr.is_some();
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
            (false, false) => "red-server-wire",
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
            // Wire-only mode
            if let Some(wire_addr) = config.wire_bind_addr.clone() {
                run_wire_only_server(config, wire_addr)
            } else {
                Err("at least one server bind address must be configured".to_string())
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
    runtime.audit_log().record(
        "config/sighup_reload",
        "system",
        "secrets",
        "ok",
        crate::json::Value::Null,
    );
}

#[inline(never)]
fn run_routed_server(config: ServerCommandConfig, router_bind_addr: String) -> Result<(), String> {
    let workers = config.workers;
    let cli_telemetry = config.telemetry.clone();
    let db_options = config.to_db_options();
    let rt_config = detect_runtime_config();
    let worker_threads = workers.unwrap_or(rt_config.suggested_workers);
    let (runtime, auth_store, _telemetry_guard) =
        build_runtime_and_auth_store(db_options, cli_telemetry)?;

    spawn_admin_metrics_listeners(&runtime, &auth_store);

    let http_listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|err| format!("bind internal HTTP listener: {err}"))?;
    let http_backend = http_listener
        .local_addr()
        .map_err(|err| format!("inspect internal HTTP listener: {err}"))?;
    let http_server = build_http_server(
        runtime.clone(),
        auth_store.clone(),
        http_backend.to_string(),
    );
    let http_handle = http_server.serve_in_background_on(http_listener);

    thread::sleep(Duration::from_millis(100));
    if http_handle.is_finished() {
        return match http_handle.join() {
            Ok(Ok(())) => Err("HTTP backend exited unexpectedly".to_string()),
            Ok(Err(err)) => Err(err.to_string()),
            Err(_) => Err("HTTP backend thread panicked".to_string()),
        };
    }

    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(worker_threads)
        .thread_stack_size(rt_config.stack_size)
        .build()
        .map_err(|err| format!("tokio runtime: {err}"))?;

    let signal_runtime = runtime.clone();
    tokio_runtime.block_on(async move {
        spawn_lifecycle_signal_handler(signal_runtime).await;
        let grpc_listener = std::net::TcpListener::bind("127.0.0.1:0")
            .map_err(|err| format!("bind internal gRPC listener: {err}"))?;
        let grpc_backend = grpc_listener
            .local_addr()
            .map_err(|err| format!("inspect internal gRPC listener: {err}"))?;
        let grpc_server = RedDBGrpcServer::with_options(
            runtime.clone(),
            GrpcServerOptions {
                bind_addr: grpc_backend.to_string(),
                tls: None,
            },
            auth_store,
        );
        tokio::spawn(async move {
            if let Err(err) = grpc_server.serve_on(grpc_listener).await {
                tracing::error!(err = %err, "gRPC backend error");
            }
        });

        let wire_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|err| format!("bind internal wire listener: {err}"))?;
        let wire_backend = wire_listener
            .local_addr()
            .map_err(|err| format!("inspect internal wire listener: {err}"))?;
        let wire_rt = Arc::new(runtime);
        tokio::spawn(async move {
            if let Err(err) =
                crate::wire::redwire::listener::start_redwire_listener_on(wire_listener, wire_rt)
                    .await
            {
                tracing::error!(err = %err, "redwire backend error");
            }
        });

        tracing::info!(
            bind = %router_bind_addr,
            cpus = rt_config.available_cpus,
            workers = worker_threads,
            "router bootstrapping"
        );
        serve_tcp_router(TcpProtocolRouterConfig {
            bind_addr: router_bind_addr,
            grpc_backend,
            http_backend,
            wire_backend,
        })
        .await
        .map_err(|err| err.to_string())
    })
}

/// Spawn RedWire listeners (plaintext + TLS) as background tokio tasks.
fn spawn_wire_listeners(config: &ServerCommandConfig, runtime: &RedDBRuntime) {
    // Plaintext RedWire — TCP or Unix socket
    if let Some(wire_addr) = config.wire_bind_addr.clone() {
        let wire_rt = Arc::new(runtime.clone());
        tokio::spawn(async move {
            // Address starting with `unix://` or an absolute filesystem path
            // switches to Unix domain sockets.
            #[cfg(unix)]
            {
                if wire_addr.starts_with("unix://") || wire_addr.starts_with('/') {
                    if let Err(e) = crate::wire::redwire::listener::start_redwire_unix_listener(
                        &wire_addr, wire_rt,
                    )
                    .await
                    {
                        tracing::error!(err = %e, "redwire unix listener error");
                    }
                    return;
                }
            }
            let cfg = crate::wire::RedWireConfig {
                bind_addr: wire_addr,
                auth_store: None,
                oauth: None,
            };
            if let Err(e) = crate::wire::start_redwire_listener(cfg, wire_rt).await {
                tracing::error!(err = %e, "redwire listener error");
            }
        });
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
    let db_options = config.to_db_options();

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
        build_runtime_and_auth_store(db_options, cli_telemetry)?;
    let signal_runtime = runtime.clone();
    tokio_runtime.block_on(async move {
        spawn_lifecycle_signal_handler(signal_runtime).await;
        let wire_rt = Arc::new(runtime);
        let cfg = crate::wire::RedWireConfig {
            bind_addr: wire_addr,
            auth_store: None,
            oauth: None,
        };
        crate::wire::start_redwire_listener(cfg, wire_rt)
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
    let runtime = RedDBRuntime::with_options(db_options.clone()).map_err(|err| err.to_string())?;

    // PLAN.md Phase 5 / W6 — opt into serverless writer-lease fencing
    // when `RED_LEASE_REQUIRED=true`. Failure here aborts boot: the
    // operator asked for a fence; running unfenced would silently
    // expose split-brain risk.
    crate::runtime::lease_loop::start_lease_loop_if_required(&runtime)
        .map_err(|err| err.to_string())?;

    // Phase 6 logging: merge red_config overrides onto the CLI-built
    // telemetry config, then install the global subscriber.
    let merged = merge_telemetry_with_config(
        cli_telemetry
            .unwrap_or_else(|| default_telemetry_for_path(db_options.data_path.as_deref())),
        &runtime,
    );
    let telemetry_guard = crate::telemetry::init(merged);

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
    auth_store.bootstrap_from_env();

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
    RedDBServer::with_options(
        runtime,
        ServerOptions {
            bind_addr,
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
    let (runtime, auth_store, _telemetry_guard) =
        build_runtime_and_auth_store(config.to_db_options(), cli_telemetry)?;
    spawn_admin_metrics_listeners(&runtime, &auth_store);
    spawn_http_tls_listener(&config, &runtime, &auth_store)?;
    let server = build_http_server(runtime, auth_store, bind_addr.clone());
    tracing::info!(transport = "http", bind = %bind_addr, "listener online");
    server.serve().map_err(|err| err.to_string())
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
    let db_options = config.to_db_options();
    let rt_config = detect_runtime_config();

    let worker_threads = workers.unwrap_or(rt_config.suggested_workers);

    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(worker_threads)
        .thread_stack_size(rt_config.stack_size)
        .build()
        .map_err(|err| format!("tokio runtime: {err}"))?;

    // Guard lives on the outer stack so it outlives the tokio runtime.
    let (runtime, auth_store, _telemetry_guard) =
        build_runtime_and_auth_store(db_options, cli_telemetry)?;
    let signal_runtime = runtime.clone();
    tokio_runtime.block_on(async move {
        spawn_lifecycle_signal_handler(signal_runtime).await;
        // Start wire protocol listeners (plaintext + TLS)
        spawn_wire_listeners(&config, &runtime);

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
        server.serve().await.map_err(|err| err.to_string())
    })
}

#[inline(never)]
fn run_dual_server(
    config: ServerCommandConfig,
    grpc_bind_addr: String,
    http_bind_addr: String,
) -> Result<(), String> {
    let workers = config.workers;
    let wire_bind_addr = config.wire_bind_addr.clone();
    let cli_telemetry = config.telemetry.clone();
    let db_options = config.to_db_options();
    let rt_config = detect_runtime_config();
    let worker_threads = workers.unwrap_or(rt_config.suggested_workers);
    let (runtime, auth_store, _telemetry_guard) =
        build_runtime_and_auth_store(db_options, cli_telemetry)?;

    spawn_admin_metrics_listeners(&runtime, &auth_store);
    spawn_http_tls_listener(&config, &runtime, &auth_store)?;

    let http_server =
        build_http_server(runtime.clone(), auth_store.clone(), http_bind_addr.clone());
    let http_handle = http_server.serve_in_background();

    thread::sleep(Duration::from_millis(150));
    if http_handle.is_finished() {
        return match http_handle.join() {
            Ok(Ok(())) => Err("HTTP server exited unexpectedly".to_string()),
            Ok(Err(err)) => Err(err.to_string()),
            Err(_) => Err("HTTP server thread panicked".to_string()),
        };
    }

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
        spawn_wire_listeners(&config, &runtime);

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
        server.serve().await.map_err(|err| err.to_string())
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
            grpc_bind_addr: Some("0.0.0.0:50051".to_string()),
            http_bind_addr: None,
        };

        let unit = render_systemd_unit(&config);
        assert!(unit.contains("ExecStart=/usr/local/bin/red server --path /var/lib/reddb/data.rdb --grpc-bind 0.0.0.0:50051"));
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
            http_bind_addr: Some("127.0.0.1:8080".to_string()),
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
            grpc_bind_addr: Some("0.0.0.0:50051".to_string()),
            http_bind_addr: Some("0.0.0.0:8080".to_string()),
        };

        let unit = render_systemd_unit(&config);
        assert!(unit.contains("--grpc-bind 0.0.0.0:50051"));
        assert!(unit.contains("--http-bind 0.0.0.0:8080"));
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
}
