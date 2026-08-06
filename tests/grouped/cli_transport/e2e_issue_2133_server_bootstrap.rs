//! Integration coverage for issue #2133's transport descriptor sets.
//!
//! One `transport_set` replaced the eight `run_*_server` functions, so these
//! pin what each shape asks for: which listeners it attaches, which one is its
//! primary, and that every descriptor it emits actually binds and lands in the
//! shared `TransportReadiness`. The boot path that consumes these descriptors
//! is covered separately by
//! `crates/reddb-server/tests/server_bootstrap_boot_path.rs`.

use std::net::TcpListener;

use reddb::service_cli::{
    bind_listener_for_startup, transport_set, BootstrapConfig, ServerCommandConfig,
};
use reddb::storage::StorageProfileSelection;
use reddb::transport::{TransportKind, TransportReadiness, TransportRole};

fn config() -> ServerCommandConfig {
    ServerCommandConfig {
        path: None,
        router_bind_addr: None,
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
        auth: false,
        require_auth: false,
        vault: false,
        no_auth: false,
        workers: Some(1),
        telemetry: None,
        http_limits_cli: reddb::server::HttpLimitsCliInput::default(),
        ui: false,
        ui_dir: None,
        bootstrap: BootstrapConfig::default(),
    }
}

fn kinds(config: &ServerCommandConfig) -> Vec<TransportKind> {
    transport_set(config)
        .descriptors()
        .iter()
        .map(|descriptor| descriptor.kind)
        .collect()
}

fn primary(config: &ServerCommandConfig) -> Option<TransportKind> {
    transport_set(config)
        .descriptors()
        .iter()
        .find(|descriptor| descriptor.role == TransportRole::Primary)
        .map(|descriptor| descriptor.kind)
}

/// Every descriptor the shape emits binds, and each one is recorded in the
/// readiness state the HTTP `/health` snapshot serves.
fn assert_shape(config: &ServerCommandConfig, expected: &[TransportKind]) {
    let set = transport_set(config);
    assert_eq!(kinds(config), expected);
    assert_eq!(primary(config), expected.first().copied());

    let mut readiness = TransportReadiness::default();
    let listeners: Vec<TcpListener> = set
        .descriptors()
        .iter()
        .map(|descriptor| {
            bind_listener_for_startup(
                &mut readiness,
                descriptor.kind.as_str(),
                &descriptor.bind_addr,
                descriptor.explicit,
            )
            .expect("descriptor bind")
            .expect("free loopback address")
        })
        .collect();

    assert_eq!(listeners.len(), expected.len());
    assert_eq!(
        readiness
            .active
            .iter()
            .map(|listener| listener.transport.as_str())
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|kind| kind.as_str())
            .collect::<Vec<_>>()
    );
    assert!(readiness.failed.is_empty());
}

/// Issue #933: the router demuxes every protocol off one port, so it attaches
/// no sibling listener even when other bind flags are set.
#[test]
fn router_transport_set_signals_readiness() {
    let mut router = config();
    router.router_bind_addr = Some("127.0.0.1:0".to_string());
    router.http_bind_addr = Some("127.0.0.1:0".to_string());
    router.pg_bind_addr = Some("127.0.0.1:0".to_string());
    assert_shape(&router, &[TransportKind::Router]);
}

#[test]
fn http_transport_set_signals_readiness() {
    let mut http = config();
    http.http_bind_addr = Some("127.0.0.1:0".to_string());
    assert_shape(&http, &[TransportKind::Http]);
}

#[test]
fn grpc_transport_set_signals_readiness() {
    let mut grpc = config();
    grpc.grpc_bind_addr = Some("127.0.0.1:0".to_string());
    assert_shape(&grpc, &[TransportKind::Grpc]);
}

/// The dual shape parks on gRPC — HTTP is served from a background thread
/// whose exit never tore the node down.
#[test]
fn dual_transport_set_signals_readiness() {
    let mut dual = config();
    dual.http_bind_addr = Some("127.0.0.1:0".to_string());
    dual.grpc_bind_addr = Some("127.0.0.1:0".to_string());

    assert_eq!(
        kinds(&dual),
        vec![TransportKind::Http, TransportKind::Grpc],
        "descriptor order is HTTP then gRPC"
    );
    assert_eq!(primary(&dual), Some(TransportKind::Grpc));
}

#[test]
fn wire_transport_set_signals_readiness() {
    let mut wire = config();
    wire.wire_bind_addr = Some("127.0.0.1:0".to_string());
    assert_shape(&wire, &[TransportKind::Wire]);
}

#[test]
fn wire_tls_transport_set_signals_readiness() {
    let mut wire_tls = config();
    wire_tls.wire_tls_bind_addr = Some("127.0.0.1:0".to_string());
    assert_shape(&wire_tls, &[TransportKind::WireTls]);
}

#[test]
fn postgres_transport_set_signals_readiness() {
    let mut pg = config();
    pg.pg_bind_addr = Some("127.0.0.1:0".to_string());
    assert_shape(&pg, &[TransportKind::Postgres]);
}

/// Issue #2055: the HTTP-only shape attaches the wire family the gRPC and dual
/// shapes always had, so `/health` enumerates them as ready.
#[test]
fn http_shape_includes_wire_and_postgres_in_readiness() {
    let mut config = config();
    config.http_bind_addr = Some("127.0.0.1:0".to_string());
    config.wire_bind_addr = Some("127.0.0.1:0".to_string());
    config.pg_bind_addr = Some("127.0.0.1:0".to_string());

    assert_shape(
        &config,
        &[
            TransportKind::Http,
            TransportKind::Wire,
            TransportKind::Postgres,
        ],
    );
}

/// The gRPC-only shape never looked at `--http-tls-bind`, so a stray value
/// must not become a descriptor — and must not be validated into a fatal boot
/// error either.
#[test]
fn grpc_shape_ignores_http_tls_bind() {
    let mut config = config();
    config.grpc_bind_addr = Some("127.0.0.1:0".to_string());
    config.grpc_tls_bind_addr = Some("127.0.0.1:0".to_string());
    config.http_tls_bind_addr = Some("127.0.0.1:0".to_string());

    assert_eq!(
        kinds(&config),
        vec![TransportKind::Grpc, TransportKind::GrpcTls]
    );
    assert_eq!(primary(&config), Some(TransportKind::Grpc));
}

/// Mirror of the gRPC case: the HTTP-only shape never looked at
/// `--grpc-tls-bind`.
#[test]
fn http_shape_ignores_grpc_tls_bind() {
    let mut config = config();
    config.http_bind_addr = Some("127.0.0.1:0".to_string());
    config.http_tls_bind_addr = Some("127.0.0.1:0".to_string());
    config.grpc_tls_bind_addr = Some("127.0.0.1:0".to_string());

    assert_eq!(
        kinds(&config),
        vec![TransportKind::Http, TransportKind::Https]
    );
    assert_eq!(primary(&config), Some(TransportKind::Http));
}

/// The wire shapes carried the PG sibling only; the TLS binds belonging to
/// other shapes were ignored outright.
#[test]
fn wire_shapes_carry_only_the_postgres_sibling() {
    let mut wire = config();
    wire.wire_bind_addr = Some("127.0.0.1:0".to_string());
    wire.wire_tls_bind_addr = Some("127.0.0.1:0".to_string());
    wire.pg_bind_addr = Some("127.0.0.1:0".to_string());
    wire.http_tls_bind_addr = Some("127.0.0.1:0".to_string());
    wire.grpc_tls_bind_addr = Some("127.0.0.1:0".to_string());
    assert_shape(&wire, &[TransportKind::Wire, TransportKind::Postgres]);

    let mut wire_tls = config();
    wire_tls.wire_tls_bind_addr = Some("127.0.0.1:0".to_string());
    wire_tls.pg_bind_addr = Some("127.0.0.1:0".to_string());
    wire_tls.http_tls_bind_addr = Some("127.0.0.1:0".to_string());
    assert_shape(
        &wire_tls,
        &[TransportKind::WireTls, TransportKind::Postgres],
    );
}

/// Sibling listeners are attached to another shape's node: only the shape's
/// own transport is primary, so only its exit ends the process.
#[test]
fn only_the_shape_transport_is_primary() {
    let mut config = config();
    config.grpc_bind_addr = Some("127.0.0.1:0".to_string());
    config.wire_bind_addr = Some("127.0.0.1:0".to_string());
    config.wire_tls_bind_addr = Some("127.0.0.1:0".to_string());
    config.pg_bind_addr = Some("127.0.0.1:0".to_string());
    config.grpc_tls_bind_addr = Some("127.0.0.1:0".to_string());

    let set = transport_set(&config);
    let primaries: Vec<TransportKind> = set
        .descriptors()
        .iter()
        .filter(|descriptor| descriptor.role == TransportRole::Primary)
        .map(|descriptor| descriptor.kind)
        .collect();
    assert_eq!(primaries, vec![TransportKind::Grpc]);
    assert_eq!(set.descriptors().len(), 5);
}
