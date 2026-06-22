//! Proptest strategies that emit syntactically valid connection
//! URIs along with the [`ConnectionTarget`] they should parse to.
//!
//! Each strategy returns a `(uri, expected)` pair so the property
//! tests can assert AST-equality round-trips without re-deriving
//! the expected value at the call site.
//!
//! Generators stay deliberately small: enough variation to
//! exercise grammar corners, not so much that shrinking explodes.

use std::path::PathBuf;

use proptest::prelude::*;
use reddb_wire::{
    conn_string::{DEFAULT_PORT_GRPC, DEFAULT_PORT_GRPCS, DEFAULT_PORT_RED},
    ConnectionTarget,
};

/// DNS-style label: alphanumeric + hyphen, length 1..=12. Matches
/// the conservative subset every backend agrees on so the
/// generator never produces hosts that `url::Url` would reject
/// for stricter reasons (e.g. all-numeric labels, leading hyphen).
pub fn host_label() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9-]{0,10}[a-z0-9]".prop_map(|s| s)
}

/// Multi-label hostname: 1..=3 labels joined by `.`.
pub fn hostname() -> impl Strategy<Value = String> {
    proptest::collection::vec(host_label(), 1..=3).prop_map(|parts| parts.join("."))
}

/// Optional explicit port. `None` means "let the parser pick the
/// scheme default".
pub fn opt_port() -> impl Strategy<Value = Option<u16>> {
    prop_oneof![Just(None), (1024u16..65535u16).prop_map(Some)]
}

/// `red://` and `reds://` — RedWire transport. Returns the URI
/// plus the canonical target.
pub fn red_uri() -> impl Strategy<Value = (String, ConnectionTarget)> {
    (any::<bool>(), hostname(), opt_port()).prop_map(|(tls, host, port)| {
        let scheme = if tls { "reds" } else { "red" };
        let resolved_port = port.unwrap_or(DEFAULT_PORT_RED);
        let uri = match port {
            Some(p) => format!("{scheme}://{host}:{p}"),
            None => format!("{scheme}://{host}"),
        };
        let target = ConnectionTarget::RedWire {
            host,
            port: resolved_port,
            tls,
        };
        (uri, target)
    })
}

/// `grpc://` / `grpcs://` single-host. Both forms normalise to a
/// `Grpc { endpoint: http://host:port }` target.
pub fn grpc_uri() -> impl Strategy<Value = (String, ConnectionTarget)> {
    (any::<bool>(), hostname(), opt_port()).prop_map(|(tls, host, port)| {
        let scheme = if tls { "grpcs" } else { "grpc" };
        let resolved = port.unwrap_or(if tls {
            DEFAULT_PORT_GRPCS
        } else {
            DEFAULT_PORT_GRPC
        });
        let uri = match port {
            Some(p) => format!("{scheme}://{host}:{p}"),
            None => format!("{scheme}://{host}"),
        };
        let target = ConnectionTarget::Grpc {
            endpoint: format!("http://{host}:{resolved}"),
        };
        (uri, target)
    })
}

/// `http://` / `https://` — REST endpoint. Default ports differ
/// from the gRPC/RedWire family, so they are spelled out here.
pub fn http_uri() -> impl Strategy<Value = (String, ConnectionTarget)> {
    (any::<bool>(), hostname(), opt_port()).prop_map(|(tls, host, port)| {
        let scheme = if tls { "https" } else { "http" };
        let default = if tls { 443 } else { 80 };
        let resolved = port.unwrap_or(default);
        let uri = match port {
            Some(p) => format!("{scheme}://{host}:{p}"),
            None => format!("{scheme}://{host}"),
        };
        let target = ConnectionTarget::Http {
            base_url: format!("{scheme}://{host}:{resolved}"),
        };
        (uri, target)
    })
}

/// `memory://` / `memory:` — both spellings are valid.
pub fn memory_uri() -> impl Strategy<Value = (String, ConnectionTarget)> {
    prop_oneof![
        Just(("memory://".to_string(), ConnectionTarget::Memory)),
        Just(("memory:".to_string(), ConnectionTarget::Memory)),
    ]
}

/// `file:///abs/path` — embedded backend. Path stays ASCII so the
/// generator never produces strings the parser must percent-decode.
pub fn file_uri() -> impl Strategy<Value = (String, ConnectionTarget)> {
    "/[a-z]{1,8}(/[a-z0-9]{1,8}){0,3}\\.rdb".prop_map(|p| {
        let uri = format!("file://{p}");
        let target = ConnectionTarget::File {
            path: PathBuf::from(p),
        };
        (uri, target)
    })
}

/// `grpc://primary,replica1,replica2` cluster URI. Optional
/// `?route=primary` flips `force_primary`.
pub fn grpc_cluster_uri() -> impl Strategy<Value = (String, ConnectionTarget)> {
    let scheme_strategy = prop_oneof![
        Just(("grpc", DEFAULT_PORT_GRPC)),
        Just(("grpcs", DEFAULT_PORT_GRPCS)),
        Just(("red", DEFAULT_PORT_RED)),
        Just(("reds", DEFAULT_PORT_RED)),
    ];
    (
        scheme_strategy,
        proptest::collection::vec((hostname(), opt_port()), 2..=5),
        any::<bool>(),
    )
        .prop_map(|((scheme, default_port), hosts, force_primary)| {
            let host_part = hosts
                .iter()
                .map(|(h, p)| match p {
                    Some(p) => format!("{h}:{p}"),
                    None => h.clone(),
                })
                .collect::<Vec<_>>()
                .join(",");
            let mut uri = format!("{scheme}://{host_part}");
            if force_primary {
                uri.push_str("?route=primary");
            }
            let mut endpoints = hosts.iter().map(|(h, p)| {
                let port = p.unwrap_or(default_port);
                format!("http://{h}:{port}")
            });
            let primary = endpoints.next().expect("at least 2 hosts");
            let replicas: Vec<String> = endpoints.collect();
            let target = ConnectionTarget::GrpcCluster {
                primary,
                replicas,
                force_primary,
            };
            (uri, target)
        })
}

/// Any documented form. Used by the panic-safety property test
/// where the specific shape doesn't matter, only that arbitrary
/// valid inputs round-trip without panics.
pub fn any_uri() -> impl Strategy<Value = (String, ConnectionTarget)> {
    prop_oneof![
        red_uri(),
        grpc_uri(),
        http_uri(),
        memory_uri(),
        file_uri(),
        grpc_cluster_uri(),
    ]
}
