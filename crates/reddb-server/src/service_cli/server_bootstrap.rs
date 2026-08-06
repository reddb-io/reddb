//! The single server bootstrap path (issue #2133).
//!
//! `red server` / `red replica` used to fan out into eight near-identical
//! `run_*_server` functions. They collapse here into one shape detection, one
//! [`TransportSet`] of listener descriptors, and one [`BootedNode`] that binds
//! and serves them. The per-shape differences that were observable to an
//! operator stay observable, but as data on the descriptors instead of eight
//! code paths: which sibling listeners a shape attaches, which listener is
//! fatal, and when each port opens relative to the DB.

use std::future::Future;
use std::pin::Pin;

use super::*;
use crate::transport::{
    TransportDescriptor, TransportKind, TransportReadiness, TransportRole, TransportSet,
};

/// The server shapes `red server` can be asked for. The shape decides which
/// listeners exist, which one is fatal, and the OS thread name — exactly the
/// split the eight `run_*_server` functions encoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServerShape {
    Router,
    Dual,
    Grpc,
    Http,
    Wire,
    WireTls,
    Postgres,
}

impl ServerShape {
    fn detect(config: &ServerCommandConfig) -> Option<Self> {
        if config.router_bind_addr.is_some() {
            return Some(Self::Router);
        }
        match (
            config.grpc_bind_addr.is_some(),
            config.http_bind_addr.is_some(),
        ) {
            (true, true) => Some(Self::Dual),
            (true, false) => Some(Self::Grpc),
            (false, true) => Some(Self::Http),
            (false, false) if config.wire_bind_addr.is_some() => Some(Self::Wire),
            (false, false) if config.wire_tls_bind_addr.is_some() => Some(Self::WireTls),
            (false, false) if config.pg_bind_addr.is_some() => Some(Self::Postgres),
            (false, false) => None,
        }
    }

    const fn thread_name(self) -> &'static str {
        match self {
            Self::Router => "red-server-router",
            Self::Dual => "red-server-dual",
            Self::Grpc => "red-server-grpc",
            Self::Http => "red-server-http",
            Self::Wire => "red-server-wire",
            Self::WireTls => "red-server-wire-tls",
            Self::Postgres => "red-server-pg-wire",
        }
    }
}

/// How a failed bind is treated, per the pre-#2133 runner that owned it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BindPolicy {
    /// Losing the port ends the boot whether or not the operator named it.
    Fatal,
    /// Issue #545: fatal when the operator named the port, degraded when the
    /// address came from a default.
    ExplicitFatal,
    /// Sibling listeners that used to bind inside a detached task, where a
    /// contested port was logged and the node kept serving.
    Degrade,
}

/// Per-transport TLS material resolved before the bind.
enum PreparedTransport {
    Plain,
    HttpTls {
        server_config: Arc<rustls::ServerConfig>,
        mtls: bool,
    },
    GrpcTls(crate::GrpcTlsOptions),
    WireTls(crate::wire::WireTlsConfig),
    Postgres(Option<crate::wire::WireTlsConfig>),
}

struct BoundTransport {
    descriptor: TransportDescriptor,
    /// `None` only for the Unix-socket RedWire descriptor, whose listener
    /// binds the socket itself.
    listener: Option<TcpListener>,
    prepared: PreparedTransport,
}

/// Everything a transport's serve future needs from the booted node.
struct ServeContext {
    config: ServerCommandConfig,
    runtime: RedDBRuntime,
    auth_store: Arc<AuthStore>,
    readiness: TransportReadiness,
    rt_config: RuntimeConfig,
    worker_threads: usize,
}

type TransportFuture = Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;

/// A fully initialized node whose HTTP / gRPC listeners are already bound.
/// Calling [`BootedNode::run`] opens the remaining listeners and serves them
/// all on the same runtime.
pub struct BootedNode {
    config: ServerCommandConfig,
    runtime: RedDBRuntime,
    auth_store: Arc<AuthStore>,
    tokio_runtime: tokio::runtime::Runtime,
    /// Listeners bound before the DB opened.
    front: Vec<BoundTransport>,
    /// Descriptors the pre-#2133 runners only bound once the DB was up.
    deferred: Vec<TransportDescriptor>,
    readiness: TransportReadiness,
    rt_config: RuntimeConfig,
    worker_threads: usize,
    _backup_tasks: Option<BackupTasksHandle>,
    _telemetry_guard: Option<crate::telemetry::TelemetryGuard>,
}

impl BootedNode {
    fn boot(config: ServerCommandConfig) -> Result<Self, String> {
        let Some(shape) = ServerShape::detect(&config) else {
            return Err("at least one server bind address must be configured".to_string());
        };
        let transport_set = transport_set(&config);

        // Phase 1 — the HTTP and gRPC listeners the pre-#2133 runners bound
        // before opening the DB, so an explicit port collision fails the boot
        // before any recovery work starts.
        let mut readiness = TransportReadiness::default();
        let mut front = Vec::new();
        let mut deferred = Vec::new();
        for descriptor in transport_set.descriptors() {
            if !matches!(descriptor.kind, TransportKind::Http | TransportKind::Grpc) {
                deferred.push(descriptor.clone());
                continue;
            }
            let listener = bind_listener_for_startup(
                &mut readiness,
                descriptor.kind.as_str(),
                &descriptor.bind_addr,
                descriptor.explicit,
            )?;
            front.push(BoundTransport {
                descriptor: descriptor.clone(),
                listener,
                prepared: PreparedTransport::Plain,
            });
        }
        resolve_degraded_front_binds(shape, &config, &mut front)?;

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

        // `RED_ADMIN_BIND` / `RED_METRICS_BIND` serve an HTTP surface, so only
        // the shapes that already owned one honoured them.
        if matches!(
            shape,
            ServerShape::Router | ServerShape::Dual | ServerShape::Http
        ) {
            spawn_admin_metrics_listeners(&runtime, &auth_store);
        }

        Ok(Self {
            config,
            runtime,
            auth_store,
            tokio_runtime,
            front,
            deferred,
            readiness,
            rt_config,
            worker_threads,
            _backup_tasks: backup_tasks,
            _telemetry_guard: telemetry_guard,
        })
    }

    /// Open the deferred listeners, attach every transport to the node's tokio
    /// runtime, and park on the shape's primary transport.
    pub fn run(mut self) -> Result<(), String> {
        let signal_runtime = self.runtime.clone();
        let front = std::mem::take(&mut self.front);
        let deferred = std::mem::take(&mut self.deferred);
        let mut readiness = std::mem::take(&mut self.readiness);
        let mut context = ServeContext {
            config: self.config.clone(),
            runtime: self.runtime.clone(),
            auth_store: self.auth_store.clone(),
            readiness: TransportReadiness::default(),
            rt_config: self.rt_config.clone(),
            worker_threads: self.worker_threads,
        };

        self.tokio_runtime.block_on(async move {
            spawn_lifecycle_signal_handler(signal_runtime).await;

            // Phase 2 — the listeners the pre-#2133 runners opened only once
            // the DB was up, so an orchestration probe cannot flip ready off a
            // SYN backlog while recovery is still running.
            let mut transports = front;
            for descriptor in deferred {
                if let Some(bound) =
                    bind_deferred_transport(&context.config, &mut readiness, descriptor)?
                {
                    transports.push(bound);
                }
            }

            // Every listener is accounted for now, so the HTTP surfaces built
            // below snapshot a complete `/health` transport view.
            context.readiness = readiness;
            let mut primary: Option<TransportFuture> = None;
            for bound in transports {
                let kind = bound.descriptor.kind;
                let role = bound.descriptor.role;
                let future = serve_transport(bound, &context)?;
                match role {
                    TransportRole::Primary => primary = Some(future),
                    TransportRole::Secondary => {
                        tokio::spawn(async move {
                            if let Err(err) = future.await {
                                log_listener_exit(kind, &err);
                            }
                        });
                    }
                }
            }

            let Some(primary) = primary else {
                return Err("no server transport started".to_string());
            };
            primary.await
        })
    }
}

/// Build the canonical descriptor set for one server command.
pub fn transport_set(config: &ServerCommandConfig) -> TransportSet {
    let Some(shape) = ServerShape::detect(config) else {
        return TransportSet::default();
    };

    let mut descriptors = Vec::new();
    match shape {
        // Issue #933: the router demuxes HTTP, gRPC and RedWire off one port,
        // so no sibling listener is attached to this shape.
        ServerShape::Router => push(
            &mut descriptors,
            TransportKind::Router,
            &config.router_bind_addr,
            config.router_bind_explicit,
            TransportRole::Primary,
        ),
        ServerShape::Dual => {
            push(
                &mut descriptors,
                TransportKind::Http,
                &config.http_bind_addr,
                config.http_bind_explicit,
                TransportRole::Secondary,
            );
            push(
                &mut descriptors,
                TransportKind::Grpc,
                &config.grpc_bind_addr,
                config.grpc_bind_explicit,
                TransportRole::Primary,
            );
            push_wire_family(&mut descriptors, config);
            push(
                &mut descriptors,
                TransportKind::Https,
                &config.http_tls_bind_addr,
                true,
                TransportRole::Secondary,
            );
            push(
                &mut descriptors,
                TransportKind::GrpcTls,
                &config.grpc_tls_bind_addr,
                true,
                TransportRole::Secondary,
            );
        }
        // The gRPC-only shape never looked at `--http-tls-bind`.
        ServerShape::Grpc => {
            push(
                &mut descriptors,
                TransportKind::Grpc,
                &config.grpc_bind_addr,
                config.grpc_bind_explicit,
                TransportRole::Primary,
            );
            push_wire_family(&mut descriptors, config);
            push(
                &mut descriptors,
                TransportKind::GrpcTls,
                &config.grpc_tls_bind_addr,
                true,
                TransportRole::Secondary,
            );
        }
        // The HTTP-only shape never looked at `--grpc-tls-bind`.
        ServerShape::Http => {
            push(
                &mut descriptors,
                TransportKind::Http,
                &config.http_bind_addr,
                config.http_bind_explicit,
                TransportRole::Primary,
            );
            push_wire_family(&mut descriptors, config);
            push(
                &mut descriptors,
                TransportKind::Https,
                &config.http_tls_bind_addr,
                true,
                TransportRole::Secondary,
            );
        }
        // The wire shapes carried the PG sibling only. `--http-tls-bind` and
        // `--grpc-tls-bind` were ignored outright, validation included, and
        // the plaintext / TLS wire flags never coexisted.
        ServerShape::Wire => {
            push(
                &mut descriptors,
                TransportKind::Wire,
                &config.wire_bind_addr,
                config.wire_bind_explicit,
                TransportRole::Primary,
            );
            push(
                &mut descriptors,
                TransportKind::Postgres,
                &config.pg_bind_addr,
                true,
                TransportRole::Secondary,
            );
        }
        ServerShape::WireTls => {
            push(
                &mut descriptors,
                TransportKind::WireTls,
                &config.wire_tls_bind_addr,
                true,
                TransportRole::Primary,
            );
            push(
                &mut descriptors,
                TransportKind::Postgres,
                &config.pg_bind_addr,
                true,
                TransportRole::Secondary,
            );
        }
        ServerShape::Postgres => push(
            &mut descriptors,
            TransportKind::Postgres,
            &config.pg_bind_addr,
            true,
            TransportRole::Primary,
        ),
    }
    TransportSet::new(descriptors)
}

/// Initialize one node and bind its front transports before serving.
pub fn bootstrap_server(config: ServerCommandConfig) -> Result<BootedNode, String> {
    BootedNode::boot(config)
}

/// The only process-level server run path.
pub fn run_server(config: ServerCommandConfig) -> Result<(), String> {
    let Some(shape) = ServerShape::detect(&config) else {
        return Err("at least one server bind address must be configured".to_string());
    };
    let handle = thread::Builder::new()
        .name(shape.thread_name().to_string())
        .stack_size(8 * 1024 * 1024)
        .spawn(move || bootstrap_server(config)?.run())
        .map_err(|err| format!("failed to spawn server thread: {err}"))?;

    match handle.join() {
        Ok(result) => result,
        Err(_) => Err("server thread panicked".to_string()),
    }
}

fn push(
    descriptors: &mut Vec<TransportDescriptor>,
    kind: TransportKind,
    bind_addr: &Option<String>,
    explicit: bool,
    role: TransportRole,
) {
    if let Some(bind_addr) = bind_addr.clone() {
        descriptors.push(TransportDescriptor::new(kind, bind_addr, explicit, role));
    }
}

/// The RedWire and PG-Wire listeners every HTTP/gRPC shape attached to its
/// node alongside its own transport.
fn push_wire_family(descriptors: &mut Vec<TransportDescriptor>, config: &ServerCommandConfig) {
    push(
        descriptors,
        TransportKind::Wire,
        &config.wire_bind_addr,
        config.wire_bind_explicit,
        TransportRole::Secondary,
    );
    push(
        descriptors,
        TransportKind::WireTls,
        &config.wire_tls_bind_addr,
        true,
        TransportRole::Secondary,
    );
    push(
        descriptors,
        TransportKind::Postgres,
        &config.pg_bind_addr,
        true,
        TransportRole::Secondary,
    );
}

/// Apply the pre-#2133 rules for an HTTP/gRPC front bind that degraded away: a
/// shape whose own listener is gone cannot boot, and the dual shape falls back
/// to parking on HTTP when only gRPC lost its port.
fn resolve_degraded_front_binds(
    shape: ServerShape,
    config: &ServerCommandConfig,
    front: &mut Vec<BoundTransport>,
) -> Result<(), String> {
    let bound = |kind: TransportKind| {
        front
            .iter()
            .any(|transport| transport.descriptor.kind == kind && transport.listener.is_some())
    };
    let http_bound = bound(TransportKind::Http);
    let grpc_bound = bound(TransportKind::Grpc);

    match shape {
        ServerShape::Http if !http_bound => {
            return Err(format!(
                "no HTTP listener started; implicit bind {} failed",
                config.http_bind_addr.as_deref().unwrap_or_default()
            ));
        }
        ServerShape::Grpc if !grpc_bound => {
            return Err(format!(
                "no gRPC listener started; implicit bind {} failed",
                config.grpc_bind_addr.as_deref().unwrap_or_default()
            ));
        }
        ServerShape::Dual if !http_bound && !grpc_bound => {
            return Err("no listener started; implicit HTTP and gRPC binds failed".to_string());
        }
        ServerShape::Dual if !grpc_bound => {
            for transport in front.iter_mut() {
                if transport.descriptor.kind == TransportKind::Http {
                    transport.descriptor.role = TransportRole::Primary;
                }
            }
        }
        _ => {}
    }

    front.retain(|transport| transport.listener.is_some());
    Ok(())
}

const fn bind_policy(descriptor: &TransportDescriptor) -> BindPolicy {
    if matches!(descriptor.role, TransportRole::Primary) {
        return BindPolicy::Fatal;
    }
    match descriptor.kind {
        // reddb-io/rio-lair#255: an explicit `--wire-tls-bind` must never
        // degrade into "healthy HTTP, dead TLS".
        TransportKind::WireTls => BindPolicy::Fatal,
        TransportKind::Wire => BindPolicy::ExplicitFatal,
        _ => BindPolicy::Degrade,
    }
}

/// Resolve a descriptor's TLS material and open its listener, inside the tokio
/// runtime and after the DB is up.
fn bind_deferred_transport(
    config: &ServerCommandConfig,
    readiness: &mut TransportReadiness,
    descriptor: TransportDescriptor,
) -> Result<Option<BoundTransport>, String> {
    let transport = descriptor.kind.as_str();
    let prepared = match descriptor.kind {
        TransportKind::Https => {
            let tls = resolve_http_tls_config(config)?;
            let server_config = crate::server::tls::build_server_config(&tls)
                .map_err(|err| format!("HTTP TLS: {err}"))?;
            PreparedTransport::HttpTls {
                server_config,
                mtls: tls.client_ca_path.is_some(),
            }
        }
        TransportKind::GrpcTls => match resolve_grpc_tls_options(config) {
            Ok(options) => PreparedTransport::GrpcTls(options),
            Err(err) => {
                let reason = format!("gRPC TLS config error: {err}");
                readiness.failed(
                    transport,
                    &descriptor.bind_addr,
                    descriptor.explicit,
                    reason,
                );
                tracing::error!(
                    target: "reddb::security",
                    transport = "grpc",
                    err = %err,
                    "gRPC TLS config error; TLS listener will not start"
                );
                return Ok(None);
            }
        },
        TransportKind::WireTls => match resolve_wire_tls_config(config) {
            Ok(tls) => PreparedTransport::WireTls(tls),
            Err(err) => {
                let reason = format!("redwire TLS config error: {err}");
                readiness.failed(
                    transport,
                    &descriptor.bind_addr,
                    descriptor.explicit,
                    reason.clone(),
                );
                tracing::error!(
                    transport = "wire-tls",
                    bind = %descriptor.bind_addr,
                    error = %err,
                    "fatal explicit redwire TLS config error"
                );
                return Err(format!("explicit {reason}"));
            }
        },
        TransportKind::Postgres => PreparedTransport::Postgres(resolve_pg_wire_tls(config)),
        _ => PreparedTransport::Plain,
    };

    // A `unix://` or absolute-path RedWire address is a Unix socket; that
    // listener binds the socket itself.
    #[cfg(unix)]
    if descriptor.kind == TransportKind::Wire
        && (descriptor.bind_addr.starts_with("unix://") || descriptor.bind_addr.starts_with('/'))
    {
        readiness.active(transport, &descriptor.bind_addr, descriptor.explicit);
        return Ok(Some(BoundTransport {
            descriptor,
            listener: None,
            prepared,
        }));
    }

    let listener = match bind_policy(&descriptor) {
        BindPolicy::Fatal => Some(bind_fatal_listener(readiness, &descriptor)?),
        BindPolicy::ExplicitFatal => bind_listener_for_startup(
            readiness,
            transport,
            &descriptor.bind_addr,
            descriptor.explicit,
        )?,
        BindPolicy::Degrade => bind_degrading_listener(readiness, &descriptor),
    };
    let Some(listener) = listener else {
        return Ok(None);
    };
    Ok(Some(BoundTransport {
        descriptor,
        listener: Some(listener),
        prepared,
    }))
}

/// Bind a listener whose loss ends the boot whether or not the operator named
/// the port — the shape has nothing left to serve without it.
fn bind_fatal_listener(
    readiness: &mut TransportReadiness,
    descriptor: &TransportDescriptor,
) -> Result<TcpListener, String> {
    match bind_listener_for_startup(
        readiness,
        descriptor.kind.as_str(),
        &descriptor.bind_addr,
        descriptor.explicit,
    ) {
        Ok(Some(listener)) => Ok(listener),
        // An implicit default that degraded away: promote the reason the #545
        // recorder just captured into the fatal error.
        Ok(None) => Err(readiness
            .failed
            .last()
            .map(|failure| failure.reason.clone())
            .unwrap_or_else(|| {
                format!(
                    "{} listener bind {} failed",
                    descriptor.kind.as_str(),
                    descriptor.bind_addr
                )
            })),
        Err(err) => Err(err),
    }
}

/// Bind a sibling listener that pre-#2133 opened inside a detached task: a
/// contested port is logged and recorded, and the node keeps serving.
fn bind_degrading_listener(
    readiness: &mut TransportReadiness,
    descriptor: &TransportDescriptor,
) -> Option<TcpListener> {
    let transport = descriptor.kind.as_str();
    match TcpListener::bind(&descriptor.bind_addr) {
        Ok(listener) => {
            readiness.active(transport, &descriptor.bind_addr, descriptor.explicit);
            Some(listener)
        }
        Err(err) => {
            let reason = format!("{transport} listener bind {}: {err}", descriptor.bind_addr);
            readiness.failed(
                transport,
                &descriptor.bind_addr,
                descriptor.explicit,
                reason,
            );
            tracing::error!(
                transport,
                bind = %descriptor.bind_addr,
                error = %err,
                "non-fatal listener bind failure; listener degraded"
            );
            None
        }
    }
}

/// Build the future that serves one bound transport for the node's lifetime.
fn serve_transport(
    bound: BoundTransport,
    context: &ServeContext,
) -> Result<TransportFuture, String> {
    let ServeContext {
        config,
        runtime,
        auth_store,
        readiness,
        rt_config,
        worker_threads,
    } = context;
    let bind_addr = bound.descriptor.bind_addr.clone();

    match (bound.descriptor.kind, bound.listener, bound.prepared) {
        (TransportKind::Router, Some(listener), PreparedTransport::Plain) => {
            let http_server =
                build_http_server(runtime.clone(), auth_store.clone(), bind_addr.clone());
            let http_server = apply_http_limits(http_server, config, runtime);
            let http_server = apply_ui_bundle(http_server, config)?;
            let grpc_server = RedDBGrpcServer::with_options(
                runtime.clone(),
                GrpcServerOptions {
                    bind_addr: bind_addr.clone(),
                    tls: None,
                },
                auth_store.clone(),
            );
            let wire_runtime = Arc::new(runtime.clone());
            let cpus = rt_config.available_cpus;
            let workers = *worker_threads;
            Ok(Box::pin(async move {
                tracing::info!(
                    bind = %bind_addr,
                    cpus,
                    workers,
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
            }))
        }
        (TransportKind::Http, Some(listener), PreparedTransport::Plain) => {
            let server = build_http_server_with_transport_readiness(
                runtime.clone(),
                auth_store.clone(),
                bind_addr.clone(),
                readiness.clone(),
            );
            let server = apply_http_limits(server, config, runtime);
            let server = apply_ui_bundle(server, config)?;
            Ok(Box::pin(async move {
                tracing::info!(transport = "http", bind = %bind_addr, "listener online");
                join_http_thread(server.serve_in_background_on(listener)).await
            }))
        }
        (
            TransportKind::Https,
            Some(listener),
            PreparedTransport::HttpTls {
                server_config,
                mtls,
            },
        ) => {
            let server = build_http_server(runtime.clone(), auth_store.clone(), bind_addr.clone());
            let server = apply_http_limits(server, config, runtime);
            Ok(Box::pin(async move {
                tracing::info!(
                    transport = "https",
                    bind = %bind_addr,
                    mtls = %mtls,
                    "TLS listener online"
                );
                join_http_thread(server.serve_tls_in_background_on(listener, server_config)).await
            }))
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
            let cpus = rt_config.available_cpus;
            let workers = *worker_threads;
            Ok(Box::pin(async move {
                tracing::info!(
                    transport = "grpc",
                    bind = %bind_addr,
                    cpus,
                    workers,
                    "listener online"
                );
                server
                    .serve_on(listener)
                    .await
                    .map_err(|err| err.to_string())
            }))
        }
        (TransportKind::GrpcTls, Some(listener), PreparedTransport::GrpcTls(tls)) => {
            let server = RedDBGrpcServer::with_options(
                runtime.clone(),
                GrpcServerOptions {
                    bind_addr: bind_addr.clone(),
                    tls: Some(tls),
                },
                auth_store.clone(),
            );
            Ok(Box::pin(async move {
                tracing::info!(transport = "grpc+tls", bind = %bind_addr, "listener online");
                server
                    .serve_on(listener)
                    .await
                    .map_err(|err| err.to_string())
            }))
        }
        (TransportKind::Wire, listener, PreparedTransport::Plain) => {
            let wire_runtime = Arc::new(runtime.clone());
            Ok(Box::pin(async move {
                // `start_redwire_listener_on` never logged itself online; the
                // #545 readiness record is this transport's only signal.
                #[cfg(unix)]
                if listener.is_none() {
                    return crate::wire::redwire::listener::start_redwire_unix_listener(
                        &bind_addr,
                        wire_runtime,
                    )
                    .await
                    .map_err(|err| err.to_string());
                }
                let listener = into_tokio_listener(
                    listener.expect("TCP wire descriptor carries a bound listener"),
                )?;
                crate::wire::redwire::listener::start_redwire_listener_on(listener, wire_runtime)
                    .await
                    .map_err(|err| err.to_string())
            }))
        }
        (TransportKind::WireTls, Some(listener), PreparedTransport::WireTls(tls)) => {
            let wire_runtime = Arc::new(runtime.clone());
            Ok(Box::pin(async move {
                let listener = into_tokio_listener(listener)?;
                // `start_redwire_tls_listener_on` does not log for itself, and
                // issues #1588 / #545 pin this exact line as the wire-TLS
                // readiness probe.
                tracing::info!(transport = "redwire+tls", bind = %bind_addr, "listener online");
                crate::wire::start_redwire_tls_listener_on(listener, wire_runtime, &tls)
                    .await
                    .map_err(|err| err.to_string())
            }))
        }
        (TransportKind::Postgres, Some(listener), PreparedTransport::Postgres(tls)) => {
            let pg_runtime = Arc::new(runtime.clone());
            Ok(Box::pin(async move {
                let listener = into_tokio_listener(listener)?;
                let pg_config = crate::wire::PgWireConfig {
                    bind_addr,
                    tls,
                    ..Default::default()
                };
                crate::wire::start_pg_wire_listener_on(listener, pg_config, pg_runtime)
                    .await
                    .map_err(|err| err.to_string())
            }))
        }
        _ => Err("transport descriptor preparation mismatch".to_string()),
    }
}

/// Hand a listener bound during bootstrap over to tokio.
fn into_tokio_listener(listener: TcpListener) -> Result<tokio::net::TcpListener, String> {
    listener
        .set_nonblocking(true)
        .map_err(|err| err.to_string())?;
    tokio::net::TcpListener::from_std(listener).map_err(|err| err.to_string())
}

/// Park on a background HTTP/HTTPS server thread without holding a tokio
/// worker.
async fn join_http_thread(handle: thread::JoinHandle<std::io::Result<()>>) -> Result<(), String> {
    match tokio::task::spawn_blocking(move || handle.join()).await {
        Ok(Ok(Ok(()))) => Err("HTTP server exited unexpectedly".to_string()),
        Ok(Ok(Err(err))) => Err(err.to_string()),
        Ok(Err(_)) => Err("HTTP server thread panicked".to_string()),
        Err(join_err) => Err(format!("HTTP server monitor task failed: {join_err}")),
    }
}

/// Pre-#2133 wording for a sibling listener whose accept loop exited.
fn log_listener_exit(kind: TransportKind, err: &str) {
    match kind {
        TransportKind::Wire => tracing::error!(err, "redwire listener error"),
        TransportKind::WireTls => tracing::error!(err, "redwire+tls listener error"),
        TransportKind::Postgres => tracing::error!(err, "pg wire listener error"),
        TransportKind::GrpcTls => {
            tracing::error!(transport = "grpc+tls", err, "gRPC TLS listener error")
        }
        _ => tracing::error!(transport = kind.as_str(), err, "listener error"),
    }
}
