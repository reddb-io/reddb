use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::auth::store::AuthStore;
use crate::replication::ReplicationConfig;
use crate::runtime::RedDBRuntime;
use crate::{
    GrpcServerOptions, RedDBGrpcServer, RedDBOptions, RedDBServer, ServerOptions, StorageMode,
};

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
            Self::Wire => "127.0.0.1:50052",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServerCommandConfig {
    pub path: Option<PathBuf>,
    pub grpc_bind_addr: Option<String>,
    pub http_bind_addr: Option<String>,
    pub wire_bind_addr: Option<String>,
    /// TLS-encrypted wire protocol bind address
    pub wire_tls_bind_addr: Option<String>,
    /// Path to TLS cert PEM (if None + wire_tls_bind, auto-generate)
    pub wire_tls_cert: Option<PathBuf>,
    /// Path to TLS key PEM
    pub wire_tls_key: Option<PathBuf>,
    pub create_if_missing: bool,
    pub read_only: bool,
    pub role: String,
    pub primary_addr: Option<String>,
    pub vault: bool,
    /// Override worker thread count (None = auto-detect from CPUs)
    pub workers: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct SystemdServiceConfig {
    pub service_name: String,
    pub binary_path: PathBuf,
    pub run_user: String,
    pub run_group: String,
    pub data_path: PathBuf,
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

impl ServerCommandConfig {
    fn to_db_options(&self) -> RedDBOptions {
        let mut options = match &self.path {
            Some(path) => RedDBOptions::persistent(path),
            None => RedDBOptions::in_memory(),
        };

        options.mode = if self.path.is_some() {
            StorageMode::Persistent
        } else {
            StorageMode::InMemory
        };
        options.create_if_missing = self.create_if_missing;
        options.read_only = self.read_only;

        options.replication = match self.role.as_str() {
            "primary" => ReplicationConfig::primary(),
            "replica" => {
                let primary_addr = self
                    .primary_addr
                    .clone()
                    .unwrap_or_else(|| "http://127.0.0.1:50051".to_string());
                options.read_only = true;
                ReplicationConfig::replica(primary_addr)
            }
            _ => ReplicationConfig::standalone(),
        };

        if self.vault {
            options.auth.vault_enabled = true;
        }

        options
    }

    pub fn enabled_transports(&self) -> Vec<ServerTransport> {
        let mut transports = Vec::with_capacity(3);
        if self.grpc_bind_addr.is_some() {
            transports.push(ServerTransport::Grpc);
        }
        if self.http_bind_addr.is_some() {
            transports.push(ServerTransport::Http);
        }
        if self.wire_bind_addr.is_some() {
            transports.push(ServerTransport::Wire);
        }
        transports
    }
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

fn command_success<const N: usize>(program: &str, args: [&str; N]) -> Result<bool, String> {
    Command::new(program)
        .args(args)
        .status()
        .map(|status| status.success())
        .map_err(|err| format!("failed to run {program}: {err}"))
}

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
    let has_any = config.grpc_bind_addr.is_some()
        || config.http_bind_addr.is_some()
        || config.wire_bind_addr.is_some();
    if !has_any {
        return Err("at least one server bind address must be configured".into());
    }
    let thread_name = match (
        config.grpc_bind_addr.is_some(),
        config.http_bind_addr.is_some(),
    ) {
        (true, true) => "red-server-dual",
        (true, false) => "red-server-grpc",
        (false, true) => "red-server-http",
        (false, false) => "red-server-wire",
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

    if let Some(bind_addr) = &config.grpc_bind_addr {
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

/// Spawn wire protocol listeners (plaintext + TLS) as background tokio tasks.
fn spawn_wire_listeners(config: &ServerCommandConfig, runtime: &RedDBRuntime) {
    // Plaintext wire
    if let Some(wire_addr) = config.wire_bind_addr.clone() {
        let wire_rt = Arc::new(runtime.clone());
        tokio::spawn(async move {
            if let Err(e) = crate::wire::start_wire_listener(&wire_addr, wire_rt).await {
                eprintln!("wire listener error: {e}");
            }
        });
    }

    // TLS wire
    if let Some(wire_tls_addr) = config.wire_tls_bind_addr.clone() {
        let tls_config = resolve_wire_tls_config(config);
        match tls_config {
            Ok(tls_cfg) => {
                let wire_rt = Arc::new(runtime.clone());
                tokio::spawn(async move {
                    if let Err(e) =
                        crate::wire::start_wire_tls_listener(&wire_tls_addr, wire_rt, &tls_cfg)
                            .await
                    {
                        eprintln!("wire+tls listener error: {e}");
                    }
                });
            }
            Err(e) => eprintln!("wire TLS config error: {e}"),
        }
    }
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
    let db_options = config.to_db_options();

    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(workers)
        .thread_stack_size(rt_config.stack_size)
        .build()
        .map_err(|err| format!("tokio runtime: {err}"))?;

    tokio_runtime.block_on(async move {
        let (runtime, _auth_store) = build_runtime_and_auth_store(db_options)?;
        let wire_rt = Arc::new(runtime);
        crate::wire::start_wire_listener(&wire_addr, wire_rt)
            .await
            .map_err(|e| e.to_string())
    })
}

#[inline(never)]
fn build_runtime_and_auth_store(
    db_options: RedDBOptions,
) -> Result<(RedDBRuntime, Arc<AuthStore>), String> {
    let runtime = RedDBRuntime::with_options(db_options.clone()).map_err(|err| err.to_string())?;
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

    Ok((runtime, auth_store))
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

#[inline(never)]
fn run_http_server(config: ServerCommandConfig, bind_addr: String) -> Result<(), String> {
    let (runtime, auth_store) = build_runtime_and_auth_store(config.to_db_options())?;
    let server = build_http_server(runtime, auth_store, bind_addr.clone());
    eprintln!("red server (HTTP) listening on {bind_addr}");
    server.serve().map_err(|err| err.to_string())
}

#[inline(never)]
fn run_grpc_server(config: ServerCommandConfig, bind_addr: String) -> Result<(), String> {
    let workers = config.workers;
    let db_options = config.to_db_options();
    let rt_config = detect_runtime_config();

    let worker_threads = workers.unwrap_or(rt_config.suggested_workers);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(worker_threads)
        .thread_stack_size(rt_config.stack_size)
        .build()
        .map_err(|err| format!("tokio runtime: {err}"))?;

    runtime.block_on(async move {
        let (runtime, auth_store) = build_runtime_and_auth_store(db_options)?;

        // Start wire protocol listeners (plaintext + TLS)
        spawn_wire_listeners(&config, &runtime);

        let server = RedDBGrpcServer::with_options(
            runtime,
            GrpcServerOptions {
                bind_addr: bind_addr.clone(),
            },
            auth_store,
        );

        eprintln!(
            "red server (gRPC) listening on {} [cpus={}, workers={}]",
            bind_addr, rt_config.available_cpus, worker_threads
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
    let db_options = config.to_db_options();
    let rt_config = detect_runtime_config();
    let worker_threads = workers.unwrap_or(rt_config.suggested_workers);
    let (runtime, auth_store) = build_runtime_and_auth_store(db_options)?;

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

    tokio_runtime.block_on(async move {
        // Start wire protocol listeners (plaintext + TLS)
        spawn_wire_listeners(&config, &runtime);

        let server = RedDBGrpcServer::with_options(
            runtime,
            GrpcServerOptions {
                bind_addr: grpc_bind_addr.clone(),
            },
            auth_store,
        );

        eprintln!("red server (HTTP) listening on {}", http_bind_addr);
        eprintln!(
            "red server (gRPC) listening on {} [cpus={}, workers={}]",
            grpc_bind_addr, rt_config.available_cpus, worker_threads
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
            grpc_bind_addr: Some("0.0.0.0:50051".to_string()),
            http_bind_addr: Some("0.0.0.0:8080".to_string()),
        };

        let unit = render_systemd_unit(&config);
        assert!(unit.contains("--grpc-bind 0.0.0.0:50051"));
        assert!(unit.contains("--http-bind 0.0.0.0:8080"));
    }
}
