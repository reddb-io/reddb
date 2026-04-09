use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use crate::replication::ReplicationConfig;
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
        stack_size: 16 * 1024 * 1024, // 16MB default
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
}

impl ServerTransport {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Grpc => "gRPC",
            Self::Http => "HTTP",
        }
    }

    pub const fn default_bind_addr(self) -> &'static str {
        match self {
            Self::Grpc => "127.0.0.1:50051",
            Self::Http => "127.0.0.1:8080",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServerCommandConfig {
    pub transport: ServerTransport,
    pub path: Option<PathBuf>,
    pub bind_addr: String,
    pub create_if_missing: bool,
    pub read_only: bool,
    pub role: String,
    pub primary_addr: Option<String>,
    pub vault: bool,
    /// Override worker thread count (None = auto-detect from CPUs)
    pub workers: Option<usize>,
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

        if self.vault || self.path.is_some() {
            options.auth.vault_enabled = true;
        }

        options
    }
}

pub fn run_server_with_large_stack(config: ServerCommandConfig) -> Result<(), String> {
    let thread_name = match config.transport {
        ServerTransport::Grpc => "red-server-grpc",
        ServerTransport::Http => "red-server-http",
    };

    let handle = thread::Builder::new()
        .name(thread_name.into())
        .stack_size(16 * 1024 * 1024)
        .spawn(move || match config.transport {
            ServerTransport::Grpc => run_grpc_server(config),
            ServerTransport::Http => run_http_server(config),
        })
        .map_err(|err| format!("failed to spawn server thread: {err}"))?;

    match handle.join() {
        Ok(result) => result,
        Err(_) => Err("server thread panicked".to_string()),
    }
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

fn run_http_server(config: ServerCommandConfig) -> Result<(), String> {
    let bind_addr = config.bind_addr.clone();
    let server = RedDBServer::from_database_options(
        config.to_db_options(),
        ServerOptions {
            bind_addr: bind_addr.clone(),
            ..ServerOptions::default()
        },
    )
    .map_err(|err| err.to_string())?;

    eprintln!("red server (HTTP) listening on {bind_addr}");
    server.serve().map_err(|err| err.to_string())
}

fn run_grpc_server(config: ServerCommandConfig) -> Result<(), String> {
    let bind_addr = config.bind_addr.clone();
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
        let server = RedDBGrpcServer::from_database_options(
            db_options,
            GrpcServerOptions {
                bind_addr: bind_addr.clone(),
            },
        )
        .map_err(|err| err.to_string())?;

        eprintln!(
            "red server (gRPC) listening on {} [cpus={}, workers={}]",
            bind_addr, rt_config.available_cpus, worker_threads
        );
        server.serve().await.map_err(|err| err.to_string())
    })
}
