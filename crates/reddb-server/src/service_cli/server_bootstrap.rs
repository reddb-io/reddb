use super::*;
use crate::transport::{TransportDescriptor, TransportKind, TransportReadiness, TransportSet};

enum PreparedTransport {
    Plain,
    HttpTls(Arc<rustls::ServerConfig>),
    GrpcTls(crate::GrpcTlsOptions),
    WireTls(crate::wire::WireTlsConfig),
    Postgres(Option<crate::wire::WireTlsConfig>),
}

struct BoundTransport {
    descriptor: TransportDescriptor,
    listener: Option<TcpListener>,
    prepared: PreparedTransport,
}

/// A fully initialized node whose transport listeners are already bound.
/// Calling [`BootedNode::run`] attaches every descriptor to the same runtime.
pub struct BootedNode {
    config: ServerCommandConfig,
    runtime: RedDBRuntime,
    auth_store: Arc<AuthStore>,
    tokio_runtime: tokio::runtime::Runtime,
    transports: Vec<BoundTransport>,
    readiness: TransportReadiness,
    rt_config: RuntimeConfig,
    worker_threads: usize,
    _backup_tasks: Option<BackupTasksHandle>,
    _telemetry_guard: Option<crate::telemetry::TelemetryGuard>,
}

impl BootedNode {
    fn boot(config: ServerCommandConfig) -> Result<Self, String> {
        let transport_set = transport_set(&config);
        if transport_set.is_empty() {
            return Err("at least one server bind address must be configured".to_string());
        }

        let mut readiness = TransportReadiness::default();
        let mut transports = Vec::with_capacity(transport_set.descriptors().len());
        for descriptor in transport_set.descriptors() {
            let prepared = match descriptor.kind {
                TransportKind::Https => {
                    let tls = resolve_http_tls_config(&config)?;
                    let server_config = crate::server::tls::build_server_config(&tls)
                        .map_err(|err| format!("HTTP TLS: {err}"))?;
                    PreparedTransport::HttpTls(server_config)
                }
                TransportKind::GrpcTls => match resolve_grpc_tls_options(&config) {
                    Ok(options) => PreparedTransport::GrpcTls(options),
                    Err(err) => {
                        let reason = format!("gRPC TLS config error: {err}");
                        readiness.failed(
                            descriptor.kind.as_str(),
                            &descriptor.bind_addr,
                            descriptor.explicit,
                            reason,
                        );
                        tracing::error!(
                            transport = "grpc-tls",
                            bind = %descriptor.bind_addr,
                            error = %err,
                            "gRPC TLS config error; TLS listener will not start"
                        );
                        continue;
                    }
                },
                TransportKind::WireTls => {
                    PreparedTransport::WireTls(resolve_wire_tls_config(&config)?)
                }
                TransportKind::Postgres => {
                    PreparedTransport::Postgres(resolve_pg_wire_tls(&config))
                }
                _ => PreparedTransport::Plain,
            };

            #[cfg(unix)]
            let is_unix_wire = descriptor.kind == TransportKind::Wire
                && (descriptor.bind_addr.starts_with("unix://")
                    || descriptor.bind_addr.starts_with('/'));
            #[cfg(not(unix))]
            let is_unix_wire = false;

            let listener = if is_unix_wire {
                readiness.active(
                    descriptor.kind.as_str(),
                    &descriptor.bind_addr,
                    descriptor.explicit,
                );
                None
            } else {
                let Some(listener) = bind_listener_for_startup(
                    &mut readiness,
                    descriptor.kind.as_str(),
                    &descriptor.bind_addr,
                    descriptor.explicit,
                )?
                else {
                    continue;
                };
                Some(listener)
            };
            transports.push(BoundTransport {
                descriptor: descriptor.clone(),
                listener,
                prepared,
            });
        }

        if transports.is_empty() {
            return Err("no transport listener started".to_string());
        }

        let db_options = config.to_db_options()?;
        let rt_config = detect_runtime_config();
        let worker_threads = config.workers.unwrap_or(rt_config.suggested_workers);
        let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(worker_threads)
            .thread_stack_size(rt_config.stack_size)
            .build()
            .map_err(|err| format!("tokio runtime: {err}"))?;
        let (runtime, auth_store, telemetry_guard) =
            build_runtime_and_auth_store(&config, db_options.clone())?;
        let backup_tasks = spawn_backup_tasks_if_configured(&db_options, &runtime);

        if transport_set.descriptors().iter().any(|descriptor| {
            matches!(descriptor.kind, TransportKind::Router | TransportKind::Http)
        }) {
            spawn_admin_metrics_listeners(&runtime, &auth_store);
        }

        Ok(Self {
            config,
            runtime,
            auth_store,
            tokio_runtime,
            transports,
            readiness,
            rt_config,
            worker_threads,
            _backup_tasks: backup_tasks,
            _telemetry_guard: telemetry_guard,
        })
    }

    fn run(mut self) -> Result<(), String> {
        let signal_runtime = self.runtime.clone();
        let runtime = self.runtime.clone();
        let auth_store = self.auth_store.clone();
        let config = self.config.clone();
        let readiness = self.readiness.clone();
        let rt_config = self.rt_config.clone();
        let worker_threads = self.worker_threads;
        let transports = std::mem::take(&mut self.transports);

        self.tokio_runtime.block_on(async move {
            spawn_lifecycle_signal_handler(signal_runtime).await;
            let mut tasks = tokio::task::JoinSet::new();

            for bound in transports {
                let descriptor = bound.descriptor;
                let bind_addr = descriptor.bind_addr.clone();
                match (descriptor.kind, bound.listener, bound.prepared) {
                    (TransportKind::Router, Some(listener), PreparedTransport::Plain) => {
                        let http_server = build_http_server_with_transport_readiness(
                            runtime.clone(),
                            auth_store.clone(),
                            bind_addr.clone(),
                            readiness.clone(),
                        );
                        let http_server = apply_http_limits(http_server, &config, &runtime);
                        let http_server = apply_ui_bundle(http_server, &config)?;
                        let grpc_server = RedDBGrpcServer::with_options(
                            runtime.clone(),
                            GrpcServerOptions {
                                bind_addr: bind_addr.clone(),
                                tls: None,
                            },
                            auth_store.clone(),
                        );
                        let wire_runtime = Arc::new(runtime.clone());
                        tasks.spawn(async move {
                            tracing::info!(
                                transport = "router",
                                bind = %bind_addr,
                                cpus = rt_config.available_cpus,
                                workers = worker_threads,
                                "router bootstrapping"
                            );
                            serve_tcp_router(InProcessRouterConfig {
                                listener,
                                bind_addr,
                                http_server,
                                grpc_server,
                                wire_runtime,
                            })
                            .await
                            .map_err(|err| err.to_string())
                        });
                    }
                    (TransportKind::Http, Some(listener), PreparedTransport::Plain) => {
                        let server = build_http_server_with_transport_readiness(
                            runtime.clone(),
                            auth_store.clone(),
                            bind_addr.clone(),
                            readiness.clone(),
                        );
                        let server = apply_http_limits(server, &config, &runtime);
                        let server = apply_ui_bundle(server, &config)?;
                        tasks.spawn(async move {
                            tracing::info!(transport = "http", bind = %bind_addr, "listener online");
                            tokio::task::spawn_blocking(move || server.serve_on(listener))
                                .await
                                .map_err(|err| format!("HTTP server thread panicked: {err}"))?
                                .map_err(|err| err.to_string())
                        });
                    }
                    (TransportKind::Https, Some(listener), PreparedTransport::HttpTls(tls)) => {
                        let server = build_http_server_with_transport_readiness(
                            runtime.clone(),
                            auth_store.clone(),
                            bind_addr.clone(),
                            readiness.clone(),
                        );
                        let server = apply_http_limits(server, &config, &runtime);
                        tasks.spawn(async move {
                            tracing::info!(transport = "https", bind = %bind_addr, "TLS listener online");
                            tokio::task::spawn_blocking(move || server.serve_tls_on(listener, tls))
                                .await
                                .map_err(|err| format!("HTTPS server thread panicked: {err}"))?
                                .map_err(|err| err.to_string())
                        });
                    }
                    (TransportKind::Grpc, Some(listener), PreparedTransport::Plain) => {
                        let server = RedDBGrpcServer::with_options(
                            runtime.clone(),
                            GrpcServerOptions {
                                bind_addr: bind_addr.clone(),
                                tls: None,
                            },
                            auth_store.clone(),
                        );
                        tasks.spawn(async move {
                            tracing::info!(transport = "grpc", bind = %bind_addr, "listener online");
                            server.serve_on(listener).await.map_err(|err| err.to_string())
                        });
                    }
                    (
                        TransportKind::GrpcTls,
                        Some(listener),
                        PreparedTransport::GrpcTls(tls),
                    ) => {
                        let server = RedDBGrpcServer::with_options(
                            runtime.clone(),
                            GrpcServerOptions {
                                bind_addr: bind_addr.clone(),
                                tls: Some(tls),
                            },
                            auth_store.clone(),
                        );
                        tasks.spawn(async move {
                            tracing::info!(transport = "grpc-tls", bind = %bind_addr, "TLS listener online");
                            server.serve_on(listener).await.map_err(|err| err.to_string())
                        });
                    }
                    (TransportKind::Wire, listener, PreparedTransport::Plain) => {
                        let wire_runtime = Arc::new(runtime.clone());
                        tasks.spawn(async move {
                            #[cfg(unix)]
                            if listener.is_none() {
                                return crate::wire::redwire::listener::start_redwire_unix_listener(
                                    &bind_addr,
                                    wire_runtime,
                                )
                                .await
                                .map_err(|err| err.to_string());
                            }
                            let listener = listener.expect("TCP wire descriptor has a listener");
                            crate::wire::redwire::listener::start_redwire_listener_on(
                                tokio::net::TcpListener::from_std({
                                    listener
                                        .set_nonblocking(true)
                                        .map_err(|err| err.to_string())?;
                                    listener
                                })
                                .map_err(|err| err.to_string())?,
                                wire_runtime,
                            )
                            .await
                            .map_err(|err| err.to_string())
                        });
                    }
                    (
                        TransportKind::WireTls,
                        Some(listener),
                        PreparedTransport::WireTls(tls),
                    ) => {
                        let wire_runtime = Arc::new(runtime.clone());
                        tasks.spawn(async move {
                            listener
                                .set_nonblocking(true)
                                .map_err(|err| err.to_string())?;
                            let listener = tokio::net::TcpListener::from_std(listener)
                                .map_err(|err| err.to_string())?;
                            tracing::info!(transport = "wire-tls", bind = %bind_addr, "TLS listener online");
                            crate::wire::start_redwire_tls_listener_on(
                                listener,
                                wire_runtime,
                                &tls,
                            )
                            .await
                            .map_err(|err| err.to_string())
                        });
                    }
                    (
                        TransportKind::Postgres,
                        Some(listener),
                        PreparedTransport::Postgres(tls),
                    ) => {
                        let pg_runtime = Arc::new(runtime.clone());
                        tasks.spawn(async move {
                            listener
                                .set_nonblocking(true)
                                .map_err(|err| err.to_string())?;
                            let listener = tokio::net::TcpListener::from_std(listener)
                                .map_err(|err| err.to_string())?;
                            let pg_config = crate::wire::PgWireConfig {
                                bind_addr,
                                tls,
                                ..Default::default()
                            };
                            crate::wire::start_pg_wire_listener_on(listener, pg_config, pg_runtime)
                                .await
                                .map_err(|err| err.to_string())
                        });
                    }
                    _ => return Err("transport descriptor preparation mismatch".to_string()),
                }
            }

            match tasks.join_next().await {
                Some(Ok(Ok(()))) => Err("server transport exited unexpectedly".to_string()),
                Some(Ok(Err(err))) => Err(err),
                Some(Err(err)) => Err(format!("server transport task failed: {err}")),
                None => Err("no server transport task started".to_string()),
            }
        })
    }
}

/// Build the canonical descriptor set for one server command.
pub fn transport_set(config: &ServerCommandConfig) -> TransportSet {
    if let Some(bind_addr) = config.router_bind_addr.clone() {
        return TransportSet::new(vec![TransportDescriptor::new(
            TransportKind::Router,
            bind_addr,
            config.router_bind_explicit,
        )]);
    }

    let mut descriptors = Vec::new();
    let mut push = |kind, bind_addr: &Option<String>, explicit| {
        if let Some(bind_addr) = bind_addr.clone() {
            descriptors.push(TransportDescriptor::new(kind, bind_addr, explicit));
        }
    };
    push(
        TransportKind::Http,
        &config.http_bind_addr,
        config.http_bind_explicit,
    );
    push(
        TransportKind::Grpc,
        &config.grpc_bind_addr,
        config.grpc_bind_explicit,
    );
    push(TransportKind::Https, &config.http_tls_bind_addr, true);
    push(TransportKind::GrpcTls, &config.grpc_tls_bind_addr, true);
    push(
        TransportKind::Wire,
        &config.wire_bind_addr,
        config.wire_bind_explicit,
    );
    push(TransportKind::WireTls, &config.wire_tls_bind_addr, true);
    push(TransportKind::Postgres, &config.pg_bind_addr, true);
    TransportSet::new(descriptors)
}

/// Initialize one node and bind its complete transport set before serving.
pub fn bootstrap_server(config: ServerCommandConfig) -> Result<BootedNode, String> {
    BootedNode::boot(config)
}

/// The only process-level server run path.
pub fn run_server(config: ServerCommandConfig) -> Result<(), String> {
    let thread_name = transport_set(&config)
        .descriptors()
        .first()
        .map(|descriptor| format!("red-server-{}", descriptor.kind.as_str()))
        .unwrap_or_else(|| "red-server".to_string());
    let handle = thread::Builder::new()
        .name(thread_name)
        .stack_size(8 * 1024 * 1024)
        .spawn(move || bootstrap_server(config)?.run())
        .map_err(|err| format!("failed to spawn server thread: {err}"))?;

    match handle.join() {
        Ok(result) => result,
        Err(_) => Err("server thread panicked".to_string()),
    }
}
