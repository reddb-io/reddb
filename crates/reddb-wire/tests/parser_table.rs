//! Table-driven coverage for the connection-string parser.
//!
//! Every row pins one expected behaviour from
//! `docs/clients/connection-strings.md` so the documented vocabulary
//! cannot drift silently.

use std::path::PathBuf;

use reddb_wire::{is_embedded_connection_uri, parse, ConnectionTarget, ParseErrorKind};

#[derive(Debug)]
struct OkCase {
    name: &'static str,
    input: &'static str,
    expect: ConnectionTarget,
}

#[derive(Debug)]
struct ErrCase {
    name: &'static str,
    input: &'static str,
    kind: ParseErrorKind,
}

fn ok_cases() -> Vec<OkCase> {
    vec![
        OkCase {
            name: "memory://",
            input: "memory://",
            expect: ConnectionTarget::Memory,
        },
        OkCase {
            name: "memory: alias",
            input: "memory:",
            expect: ConnectionTarget::Memory,
        },
        OkCase {
            name: "file:// absolute path",
            input: "file:///var/lib/reddb/data.rdb",
            expect: ConnectionTarget::File {
                path: PathBuf::from("/var/lib/reddb/data.rdb"),
            },
        },
        OkCase {
            name: "red:// default port 5050",
            input: "red://primary.svc",
            expect: ConnectionTarget::RedWire {
                host: "primary.svc".into(),
                port: 5050,
                tls: false,
            },
        },
        OkCase {
            name: "reds:// default port 5050 with TLS flag",
            input: "reds://host.example",
            expect: ConnectionTarget::RedWire {
                host: "host.example".into(),
                port: 5050,
                tls: true,
            },
        },
        OkCase {
            name: "red:// explicit port",
            input: "red://h:6000",
            expect: ConnectionTarget::RedWire {
                host: "h".into(),
                port: 6000,
                tls: false,
            },
        },
        OkCase {
            name: "grpc:// default port 5055",
            input: "grpc://primary",
            expect: ConnectionTarget::Grpc {
                endpoint: "http://primary:5055".into(),
            },
        },
        OkCase {
            name: "grpcs:// default port 5055",
            input: "grpcs://primary",
            expect: ConnectionTarget::Grpc {
                endpoint: "http://primary:5055".into(),
            },
        },
        OkCase {
            name: "grpc:// explicit port",
            input: "grpc://primary:6000",
            expect: ConnectionTarget::Grpc {
                endpoint: "http://primary:6000".into(),
            },
        },
        OkCase {
            name: "http:// default port 80",
            input: "http://h",
            expect: ConnectionTarget::Http {
                base_url: "http://h:80".into(),
            },
        },
        OkCase {
            name: "https:// default port 443",
            input: "https://h",
            expect: ConnectionTarget::Http {
                base_url: "https://h:443".into(),
            },
        },
        OkCase {
            name: "http:// explicit port",
            input: "http://h:8080",
            expect: ConnectionTarget::Http {
                base_url: "http://h:8080".into(),
            },
        },
        OkCase {
            name: "https:// explicit port",
            input: "https://h:8443",
            expect: ConnectionTarget::Http {
                base_url: "https://h:8443".into(),
            },
        },
        OkCase {
            name: "grpc cluster explicit ports",
            input: "grpc://primary:5055,replica1:5055,replica2:5055",
            expect: ConnectionTarget::GrpcCluster {
                primary: "http://primary:5055".into(),
                replicas: vec!["http://replica1:5055".into(), "http://replica2:5055".into()],
                force_primary: false,
            },
        },
        OkCase {
            name: "grpc cluster inherits scheme default port",
            input: "grpc://a,b",
            expect: ConnectionTarget::GrpcCluster {
                primary: "http://a:5055".into(),
                replicas: vec!["http://b:5055".into()],
                force_primary: false,
            },
        },
        OkCase {
            name: "red cluster uses 5050 default",
            input: "red://a,b",
            expect: ConnectionTarget::GrpcCluster {
                primary: "http://a:5050".into(),
                replicas: vec!["http://b:5050".into()],
                force_primary: false,
            },
        },
        OkCase {
            name: "reds cluster uses 5050 default",
            input: "reds://a,b",
            expect: ConnectionTarget::GrpcCluster {
                primary: "http://a:5050".into(),
                replicas: vec!["http://b:5050".into()],
                force_primary: false,
            },
        },
        OkCase {
            name: "cluster per-host port overrides default",
            input: "grpc://a:7000,b:7001,c",
            expect: ConnectionTarget::GrpcCluster {
                primary: "http://a:7000".into(),
                replicas: vec!["http://b:7001".into(), "http://c:5055".into()],
                force_primary: false,
            },
        },
        OkCase {
            name: "?route=primary forces primary",
            input: "grpc://primary,replica?route=primary",
            expect: ConnectionTarget::GrpcCluster {
                primary: "http://primary:5055".into(),
                replicas: vec!["http://replica:5055".into()],
                force_primary: true,
            },
        },
        OkCase {
            name: "single-host grpc with explicit port routes Grpc not cluster",
            input: "grpc://primary:5055",
            expect: ConnectionTarget::Grpc {
                endpoint: "http://primary:5055".into(),
            },
        },
    ]
}

fn err_cases() -> Vec<ErrCase> {
    vec![
        ErrCase {
            name: "empty input",
            input: "",
            kind: ParseErrorKind::Empty,
        },
        ErrCase {
            name: "file:// without path",
            input: "file://",
            kind: ParseErrorKind::InvalidUri,
        },
        ErrCase {
            name: "unknown scheme",
            input: "mongodb://localhost",
            kind: ParseErrorKind::UnsupportedScheme,
        },
        ErrCase {
            name: "cluster with empty middle host",
            input: "grpc://primary,,replica",
            kind: ParseErrorKind::InvalidUri,
        },
        ErrCase {
            name: "cluster starting with empty host",
            input: "grpc://,b",
            kind: ParseErrorKind::InvalidUri,
        },
        ErrCase {
            name: "cluster with non-numeric port",
            input: "grpc://a:nope,b:5055",
            kind: ParseErrorKind::InvalidUri,
        },
    ]
}

#[test]
fn ok_cases_match_expected_target() {
    for c in ok_cases() {
        let got = parse(c.input)
            .unwrap_or_else(|e| panic!("{}: {input} => err {e}", c.name, input = c.input));
        assert_eq!(
            got, c.expect,
            "{}: {} => unexpected target",
            c.name, c.input
        );
    }
}

#[test]
fn err_cases_match_expected_kind() {
    for c in err_cases() {
        let err = parse(c.input).expect_err(&format!("{}: expected error for {}", c.name, c.input));
        assert_eq!(
            err.kind, c.kind,
            "{}: {} => unexpected error kind ({})",
            c.name, c.input, err.message
        );
    }
}

#[test]
fn embedded_red_uri_aliases_are_centralized() {
    for input in [
        "red://",
        "red:",
        "red:///",
        "red://:memory",
        "red://:memory:",
    ] {
        assert!(
            is_embedded_connection_uri(input),
            "{input} should be an embedded alias"
        );
    }
    assert!(is_embedded_connection_uri(" red:///tmp/demo.rdb "));
    assert!(!is_embedded_connection_uri("red://primary:5050"));
    assert!(!is_embedded_connection_uri("grpc://primary:5055"));
}
