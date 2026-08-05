//! Integration coverage for issue #2133's transport descriptor sets.

use std::net::TcpListener;

use reddb::service_cli::{
    bind_listener_for_startup, transport_set, BootstrapConfig, ServerCommandConfig,
};
use reddb::storage::StorageProfileSelection;
use reddb::transport::{TransportKind, TransportReadiness};

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

#[test]
fn transport_set_shapes_are_declared_once() {
    let mut router = config();
    router.router_bind_addr = Some("127.0.0.1:0".to_string());
    router.http_bind_addr = Some("127.0.0.1:0".to_string());
    assert_eq!(kinds(&router), vec![TransportKind::Router]);

    let mut http = config();
    http.http_bind_addr = Some("127.0.0.1:0".to_string());
    assert_eq!(kinds(&http), vec![TransportKind::Http]);

    let mut grpc = config();
    grpc.grpc_bind_addr = Some("127.0.0.1:0".to_string());
    assert_eq!(kinds(&grpc), vec![TransportKind::Grpc]);

    let mut dual = config();
    dual.http_bind_addr = Some("127.0.0.1:0".to_string());
    dual.grpc_bind_addr = Some("127.0.0.1:0".to_string());
    assert_eq!(
        kinds(&dual),
        vec![TransportKind::Http, TransportKind::Grpc]
    );

    let mut wire = config();
    wire.wire_bind_addr = Some("127.0.0.1:0".to_string());
    assert_eq!(kinds(&wire), vec![TransportKind::Wire]);

    let mut wire_tls = config();
    wire_tls.wire_tls_bind_addr = Some("127.0.0.1:0".to_string());
    assert_eq!(kinds(&wire_tls), vec![TransportKind::WireTls]);

    let mut pg = config();
    pg.pg_bind_addr = Some("127.0.0.1:0".to_string());
    assert_eq!(kinds(&pg), vec![TransportKind::Postgres]);
}

#[test]
fn http_shape_includes_wire_and_postgres_in_readiness() {
    let mut config = config();
    config.http_bind_addr = Some("127.0.0.1:0".to_string());
    config.wire_bind_addr = Some("127.0.0.1:0".to_string());
    config.pg_bind_addr = Some("127.0.0.1:0".to_string());

    let set = transport_set(&config);
    assert_eq!(
        kinds(&config),
        vec![
            TransportKind::Http,
            TransportKind::Wire,
            TransportKind::Postgres,
        ]
    );

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

    assert_eq!(listeners.len(), 3);
    assert_eq!(
        readiness
            .active
            .iter()
            .map(|listener| listener.transport.as_str())
            .collect::<Vec<_>>(),
        vec!["http", "wire", "postgres"]
    );
    assert!(readiness.failed.is_empty());
}
