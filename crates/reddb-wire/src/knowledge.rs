//! Agent-facing connection and topology knowledge reference.
//!
//! The volatile facts in this module are generated from the shared wire crate
//! authorities: connection-string defaults from [`crate::conn_string`] and
//! topology payload version/header details from [`crate::topology`]. The same
//! generated content is served as the `reddb://knowledge/connections` MCP
//! resource and embedded into `docs/llms.txt`.

use crate::{
    ConnStringLimits, DEFAULT_PORT_GRPC, DEFAULT_PORT_GRPCS, DEFAULT_PORT_RED, DEFAULT_PORT_WS,
    DEFAULT_PORT_WSS, MAX_KNOWN_TOPOLOGY_VERSION, TOPOLOGY_HEADER_SIZE, TOPOLOGY_WIRE_VERSION_V1,
};

/// Canonical URI for the connection/topology knowledge resource served over MCP.
pub const RESOURCE_URI: &str = "reddb://knowledge/connections";

/// Short human title for the connection/topology knowledge resource.
pub const RESOURCE_TITLE: &str = "RedDB Connection & Topology Reference";

/// One-line description of the connection/topology knowledge resource.
pub const RESOURCE_DESCRIPTION: &str =
    "Generated connection-string and topology reference from reddb-io-wire authorities.";

/// Markers delimiting the generated connection block inside `docs/llms.txt`.
pub const LLMS_BEGIN_MARKER: &str = "<!-- BEGIN GENERATED: connections -->";
/// Closing marker for the generated connection block in `docs/llms.txt`.
pub const LLMS_END_MARKER: &str = "<!-- END GENERATED: connections -->";

struct TransportSpec {
    pattern: &'static str,
    target: &'static str,
    default_port: Option<u16>,
    tls: &'static str,
    parser_authority: &'static str,
}

fn transport_specs() -> Vec<TransportSpec> {
    vec![
        TransportSpec {
            pattern: "red://, red://:memory:, red:///path/to/data.rdb",
            target: "Embedded memory or file target",
            default_port: None,
            tls: "n/a",
            parser_authority: "is_embedded_connection_uri",
        },
        TransportSpec {
            pattern: "red://host[:port]",
            target: "RedWire TCP",
            default_port: Some(DEFAULT_PORT_RED),
            tls: "plain",
            parser_authority: "parse -> ConnectionTarget::RedWire",
        },
        TransportSpec {
            pattern: "reds://host[:port]",
            target: "RedWire TLS",
            default_port: Some(DEFAULT_PORT_RED),
            tls: "TLS",
            parser_authority: "parse -> ConnectionTarget::RedWire",
        },
        TransportSpec {
            pattern: "grpc://host[:port]",
            target: "gRPC plain endpoint",
            default_port: Some(DEFAULT_PORT_GRPC),
            tls: "plain",
            parser_authority: "parse -> ConnectionTarget::Grpc",
        },
        TransportSpec {
            pattern: "grpcs://host[:port]",
            target: "gRPC TLS endpoint",
            default_port: Some(DEFAULT_PORT_GRPCS),
            tls: "TLS",
            parser_authority: "parse -> ConnectionTarget::Grpc",
        },
        TransportSpec {
            pattern: "http://host[:port]",
            target: "HTTP REST endpoint",
            default_port: Some(80),
            tls: "plain",
            parser_authority: "parse -> ConnectionTarget::Http",
        },
        TransportSpec {
            pattern: "https://host[:port]",
            target: "HTTPS REST endpoint",
            default_port: Some(443),
            tls: "TLS",
            parser_authority: "parse -> ConnectionTarget::Http",
        },
        TransportSpec {
            pattern: "red+ws://host[:port]",
            target: "Browser-native WebSocket",
            default_port: Some(DEFAULT_PORT_WS),
            tls: "plain",
            parser_authority: "parse -> ConnectionTarget::WsNative",
        },
        TransportSpec {
            pattern: "red+wss://host[:port]",
            target: "Browser-native secure WebSocket",
            default_port: Some(DEFAULT_PORT_WSS),
            tls: "TLS",
            parser_authority: "parse -> ConnectionTarget::WsNative",
        },
    ]
}

fn default_port_label(port: Option<u16>) -> String {
    port.map(|p| p.to_string())
        .unwrap_or_else(|| "n/a".to_string())
}

fn render_transport_table(out: &mut String) {
    out.push_str("| Pattern | Target | Default port | TLS | Parser authority |\n");
    out.push_str("|---------|--------|--------------|-----|------------------|\n");
    for spec in transport_specs() {
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | `{}` |\n",
            spec.pattern,
            spec.target,
            default_port_label(spec.default_port),
            spec.tls,
            spec.parser_authority
        ));
    }
}

fn render_examples(out: &mut String) {
    out.push_str("## Routing examples\n\n");
    out.push_str(
        "- `red://` and `red:///data/prod.rdb` are embedded aliases; callers gate them with `is_embedded_connection_uri` before remote transport parsing.\n",
    );
    out.push_str(&format!(
        "- `red://primary.svc` parses as RedWire TCP on port `{DEFAULT_PORT_RED}`.\n"
    ));
    out.push_str(&format!(
        "- `reds://primary.svc` parses as RedWire TLS on port `{DEFAULT_PORT_RED}`.\n"
    ));
    out.push_str(&format!(
        "- `grpc://primary.svc` parses as a gRPC endpoint on port `{DEFAULT_PORT_GRPC}`.\n"
    ));
    out.push_str(
        "- `grpc://primary.svc,replica-a.svc,replica-b.svc?route=primary` parses as a cluster target with the first host as primary, later hosts as replicas, and `force_primary=true`.\n",
    );
    out.push('\n');
}

fn render_limits(out: &mut String) {
    let limits = ConnStringLimits::default();
    out.push_str("## Parser limits\n\n");
    out.push_str(
        "The parser enforces these unauthenticated-input guardrails before URI routing:\n\n",
    );
    out.push_str(&format!("- `max_uri_bytes`: `{}`\n", limits.max_uri_bytes));
    out.push_str(&format!(
        "- `max_query_params`: `{}`\n",
        limits.max_query_params
    ));
    out.push_str(&format!(
        "- `max_cluster_hosts`: `{}`\n\n",
        limits.max_cluster_hosts
    ));
}

fn render_topology(out: &mut String) {
    out.push_str("## Topology advertisement\n\n");
    out.push_str(
        "The topology payload is the shared read-routing advertisement carried by RedWire HelloAck and the gRPC Topology RPC.\n\n",
    );
    out.push_str(&format!(
        "- `TOPOLOGY_WIRE_VERSION_V1`: `0x{TOPOLOGY_WIRE_VERSION_V1:02x}`\n"
    ));
    out.push_str(&format!(
        "- `MAX_KNOWN_TOPOLOGY_VERSION`: `0x{MAX_KNOWN_TOPOLOGY_VERSION:02x}`\n"
    ));
    out.push_str(&format!(
        "- `TOPOLOGY_HEADER_SIZE`: `{TOPOLOGY_HEADER_SIZE}` bytes (version tag + little-endian body length)\n",
    ));
    out.push_str(
        "- Unknown future version tags are ignored cleanly so clients can fall back to URI-only routing.\n",
    );
    out.push_str(
        "- Version 1 carries `epoch`, `primary.addr`, `primary.region`, and ordered replica records: `addr`, `region`, `healthy`, `lag_ms`, `last_applied_lsn`, `rebootstrapping`.\n\n",
    );
}

/// Generate the canonical connection and topology reference as Markdown.
pub fn connection_reference_markdown() -> String {
    let mut out = String::new();
    out.push_str("# RedDB Connection & Topology Reference\n\n");
    out.push_str(
        "RedDB connection strings and topology payloads are owned by `reddb-io-wire`, the shared wire crate used by the server, client, and language drivers.\n\n",
    );
    out.push_str(
        "This reference is generated from the connection-string parser constants and topology wire constants. Do not edit by hand -- regenerate from the engine.\n\n",
    );
    out.push_str("## Connection targets\n\n");
    render_transport_table(&mut out);
    out.push('\n');
    render_examples(&mut out);
    render_limits(&mut out);
    render_topology(&mut out);

    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

/// The connection block as embedded in `docs/llms.txt`.
pub fn connection_llms_section() -> String {
    format!(
        "{begin}\n{body}\n{end}",
        begin = LLMS_BEGIN_MARKER,
        body = connection_reference_markdown(),
        end = LLMS_END_MARKER,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{is_embedded_connection_uri, parse, ConnectionTarget};

    #[test]
    fn reference_lists_parser_default_ports() {
        let reference = connection_reference_markdown();
        for port in [
            DEFAULT_PORT_RED,
            DEFAULT_PORT_GRPC,
            DEFAULT_PORT_GRPCS,
            DEFAULT_PORT_WS,
            DEFAULT_PORT_WSS,
        ] {
            assert!(
                reference.contains(&port.to_string()),
                "default port {port} missing from generated reference"
            );
        }
    }

    #[test]
    fn reference_examples_match_parser_contract() {
        assert!(is_embedded_connection_uri("red:///data/prod.rdb"));

        match parse("red://primary.svc").expect("redwire target parses") {
            ConnectionTarget::RedWire { port, tls, .. } => {
                assert_eq!(port, DEFAULT_PORT_RED);
                assert!(!tls);
            }
            other => panic!("expected RedWire target, got {other:?}"),
        }

        match parse("grpc://primary.svc").expect("grpc target parses") {
            ConnectionTarget::Grpc { endpoint } => {
                assert_eq!(endpoint, format!("http://primary.svc:{DEFAULT_PORT_GRPC}"));
            }
            other => panic!("expected gRPC target, got {other:?}"),
        }

        match parse("grpc://primary.svc,replica-a.svc?route=primary")
            .expect("cluster target parses")
        {
            ConnectionTarget::GrpcCluster {
                primary,
                replicas,
                force_primary,
            } => {
                assert_eq!(primary, format!("http://primary.svc:{DEFAULT_PORT_GRPC}"));
                assert_eq!(
                    replicas,
                    vec![format!("http://replica-a.svc:{DEFAULT_PORT_GRPC}")]
                );
                assert!(force_primary);
            }
            other => panic!("expected cluster target, got {other:?}"),
        }
    }

    #[test]
    fn llms_section_wraps_reference() {
        let section = connection_llms_section();
        assert!(section.starts_with(LLMS_BEGIN_MARKER));
        assert!(section.ends_with(LLMS_END_MARKER));
        assert!(section.contains(&connection_reference_markdown()));
    }
}
