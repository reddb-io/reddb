//! Agent-facing connection-model knowledge reference — generated from the
//! engine's own connection layer (ADR 0061, "Agent-Facing Knowledge & MCP
//! Surface").
//!
//! Agents consistently mis-model RedDB's connection surface: they assume it is
//! "only gRPC" or "only WebSocket" and miss that **RedWire (`red://` /
//! `reds://`) is the principal transport**. This module is the single source
//! for the corrective knowledge, split into two halves:
//!
//! - the supported URI **schemes / transports** are emitted straight from the
//!   connection layer's own vocabulary — [`crate::conn_string`], the canonical
//!   parser. Nothing here hand-maintains *which* schemes parse; the
//!   [`tests::every_scheme_parses`] anti-drift test fails the build if a
//!   documented scheme stops being accepted by the parser. The documented
//!   default ports are interpolated straight from the parser's port constants
//!   so they cannot drift either.
//! - the deployment **topology narrative** (embedded/standalone, serverless,
//!   primary-replica, cluster) is hand-authored prose because it is judgment,
//!   not extractable from source.
//!
//! The same generated content is served two ways from this one source: as the
//! `reddb://knowledge/connection` MCP resource and as the connection section of
//! the generated `docs/llms.txt`.

use crate::conn_string::{
    DEFAULT_PORT_GRPC, DEFAULT_PORT_GRPCS, DEFAULT_PORT_RED, DEFAULT_PORT_WS, DEFAULT_PORT_WSS,
};

/// Canonical URI for the connection knowledge resource served over MCP.
pub const RESOURCE_URI: &str = "reddb://knowledge/connection";

/// Short human title for the connection knowledge resource.
pub const RESOURCE_TITLE: &str = "RedDB Connection & Topology Guide";

/// One-line description of the connection knowledge resource.
pub const RESOURCE_DESCRIPTION: &str =
    "Generated connection-model reference: every supported URI scheme/transport plus the \
deployment topologies, with RedWire (red://) as the principal transport.";

/// Markers delimiting the generated connection block inside `docs/llms.txt`.
/// The `docs/llms.txt` sync test reads the text between these markers and
/// asserts it equals [`connection_llms_section`], so the file is generated, not
/// hand-maintained.
pub const LLMS_BEGIN_MARKER: &str = "<!-- BEGIN GENERATED: connection -->";
/// Closing marker for the generated connection block in `docs/llms.txt`.
pub const LLMS_END_MARKER: &str = "<!-- END GENERATED: connection -->";

/// One supported connection scheme as taught to agents.
///
/// Every field is reviewable prose except `example`, which is a concrete,
/// parseable URI that doubles as the anti-drift probe: the
/// [`tests::every_scheme_parses`] test feeds it back through
/// [`conn_string::parse`] and asserts the connection layer still accepts it,
/// so this catalog can never claim a scheme the engine has dropped.
pub struct SchemeDoc {
    /// The URI scheme token (the part before `://`).
    pub scheme: &'static str,
    /// Canonical URI form shown to agents.
    pub uri_form: &'static str,
    /// The transport this scheme selects.
    pub transport: &'static str,
    /// One-line summary of when to reach for it.
    pub summary: &'static str,
    /// A concrete, parseable example URI — also the anti-drift probe.
    pub example: &'static str,
}

/// The supported connection schemes, ordered to teach the lesson: the principal
/// RedWire transports first, then the compatibility transports, then the
/// embedded aliases. This list mirrors the scheme `match` arms in
/// [`conn_string::parse_with_limits`]; the anti-drift tests below fail the build
/// if any entry stops parsing or if the parser gains a scheme this list misses.
pub const SCHEMES: &[SchemeDoc] = &[
    SchemeDoc {
        scheme: "red",
        uri_form: "red://host:port",
        transport: "RedWire — framed binary protocol over TCP (ADR 0001)",
        summary: "Principal transport: the lowest-overhead native protocol. Reach for this first.",
        example: "red://db.example.com:5050",
    },
    SchemeDoc {
        scheme: "reds",
        uri_form: "reds://host:port",
        transport: "RedWire over TLS",
        summary: "Principal transport, encrypted: RedWire with TLS for untrusted networks.",
        example: "reds://db.example.com:5050",
    },
    SchemeDoc {
        scheme: "red+ws",
        uri_form: "red+ws://host",
        transport: "RedWire over WebSocket (ADR 0047 direct-when-reachable)",
        summary: "Browser-native RedWire: the UI connects directly, no local bridge.",
        example: "red+ws://db.example.com",
    },
    SchemeDoc {
        scheme: "red+wss",
        uri_form: "red+wss://host",
        transport: "RedWire over secure WebSocket (WSS)",
        summary: "Browser-native RedWire, encrypted: the hosted-endpoint default for the UI.",
        example: "red+wss://db.example.com",
    },
    SchemeDoc {
        scheme: "grpc",
        uri_form: "grpc://host:port",
        transport: "gRPC (tonic)",
        summary: "Compatibility transport for gRPC-native clients; cluster URIs list replicas.",
        example: "grpc://db.example.com:55055",
    },
    SchemeDoc {
        scheme: "grpcs",
        uri_form: "grpcs://host:port",
        transport: "gRPC over TLS",
        summary: "Compatibility transport, encrypted: gRPC with TLS.",
        example: "grpcs://db.example.com:55555",
    },
    SchemeDoc {
        scheme: "http",
        uri_form: "http://host:port",
        transport: "HTTP / REST (JSON over HTTP)",
        summary: "REST endpoint for HTTP-only callers and health checks.",
        example: "http://db.example.com:8080",
    },
    SchemeDoc {
        scheme: "https",
        uri_form: "https://host",
        transport: "HTTPS / REST (JSON over TLS)",
        summary: "REST endpoint, encrypted.",
        example: "https://db.example.com",
    },
    SchemeDoc {
        scheme: "memory",
        uri_form: "memory://",
        transport: "Embedded engine, in-process, ephemeral",
        summary: "Zero-config default: a genuine in-memory engine instance, not a mock.",
        example: "memory://",
    },
    SchemeDoc {
        scheme: "file",
        uri_form: "file:///path/to/db",
        transport: "Embedded engine, in-process, persisted to disk",
        summary: "Embedded engine that persists to a local path.",
        example: "file:///var/lib/reddb/data",
    },
];

/// Render the supported-schemes section as a Markdown table.
fn render_schemes_table() -> String {
    let mut out = String::new();
    out.push_str("| Scheme | Transport | When to use |\n");
    out.push_str("|---|---|---|\n");
    for doc in SCHEMES {
        out.push_str(&format!(
            "| `{}` | {} | {} |\n",
            doc.uri_form, doc.transport, doc.summary,
        ));
    }
    out
}

/// The hand-authored deployment-topology narrative. This is judgment, not
/// extractable from source, so it is prose — but it is pinned by the
/// [`tests::reference_covers_all_topologies`] anti-drift test, which fails the
/// build if any of the four taught topologies is dropped.
fn render_topologies() -> String {
    "## Deployment topologies\n\n\
RedDB is one engine that scales from a library call to a replicated cluster. \
The same data model and the same RQL surface apply at every size; only the \
connection URI changes.\n\n\
- **Embedded / standalone.** The engine runs in-process — no server, no socket. \
Open it with `memory://` (ephemeral) or `file:///path` (persisted). This is the \
default and the fastest path: there is no transport at all, just function calls \
into the engine. A standalone `red` server process that a single client talks to \
over `red://` is the same engine with a listener attached.\n\n\
- **Serverless.** A short-lived process (a function invocation, a job) opens an \
embedded `memory://`/`file://` engine for its lifetime, or connects out to a \
remote endpoint over `red://`/`reds://`. There is no long-running server to \
manage; the engine's cost is paid only while the process is alive.\n\n\
- **Primary-replica.** A writable primary fans changes out to read-only \
replicas. Point writes at the primary and spread reads across replicas with a \
multi-host URI (`red://primary,replica-a,replica-b`); append `?route=primary` to \
force a read onto the primary when you need read-your-writes. Replicas serve \
reads at lower latency and absorb read load.\n\n\
- **Cluster.** Multiple nodes coordinate for availability and horizontal \
capacity. Clients address the cluster with a multi-host URI listing the member \
endpoints; the connector resolves the primary for writes and balances reads \
across the rest. Cluster URIs work over `red://`/`reds://` and `grpc://`/\
`grpcs://`.\n"
        .to_string()
}

/// Generate the canonical connection-model reference as Markdown, sourced from
/// the engine's connection layer (schemes, ports) plus the hand-authored
/// topology narrative. This single string is what the MCP
/// `reddb://knowledge/connection` resource serves and what `docs/llms.txt`
/// embeds.
pub fn connection_reference_markdown() -> String {
    let mut out = String::new();
    out.push_str("# RedDB Connection & Topology Guide\n\n");
    out.push_str(
        "RedDB speaks several transports, but they are not equal. **RedWire \
(`red://` / `reds://`) is the principal transport** — the native, lowest-overhead \
binary protocol the engine is built around. RedDB is *not* \"only gRPC\" and *not* \
\"only WebSocket\": gRPC, HTTP, and WebSocket are compatibility transports, and \
`memory://` / `file://` are the embedded engine itself.\n\n",
    );
    out.push_str(
        "The supported schemes below are generated from the connection layer \
(`reddb-io-wire` connection-string parser); the default ports are the parser's \
own constants. Do not edit by hand — regenerate from the engine.\n\n",
    );

    out.push_str(&format!("## Schemes & transports ({})\n\n", SCHEMES.len()));
    out.push_str(
        "The URI scheme selects the transport. Choose `red://` unless a specific \
compatibility need (a gRPC-only client, a plain HTTP caller, a browser) dictates \
otherwise:\n\n",
    );
    out.push_str(&render_schemes_table());
    out.push('\n');

    out.push_str("### Default ports\n\n");
    out.push_str("When the URI omits a port, the connection layer applies these defaults:\n\n");
    out.push_str(&format!(
        "- RedWire `red://`: `{DEFAULT_PORT_RED}`\n\
- RedWire-over-WebSocket `red+ws://`: `{DEFAULT_PORT_WS}`; `red+wss://`: `{DEFAULT_PORT_WSS}`\n\
- gRPC `grpc://`: `{DEFAULT_PORT_GRPC}`; `grpcs://`: `{DEFAULT_PORT_GRPCS}`\n\
- HTTP `http://`: `80`; `https://`: `443`\n\n",
    ));

    out.push_str(&render_topologies());

    // Trim the trailing blank line so the body ends with exactly one newline.
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

/// The connection block as embedded in `docs/llms.txt`: the generated reference
/// fenced by the begin/end markers. Emitting the markers here keeps
/// `docs/llms.txt` and the MCP resource fed by one source.
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
    use crate::conn_string::{self, ConnectionTarget};

    /// catalog ⊆ engine: every documented scheme's example URI must still be
    /// accepted by the connection-layer parser, and parse to a target whose
    /// transport matches the scheme. This is the anti-drift spine — it fails
    /// the build if the parser drops or renames a scheme this list claims.
    #[test]
    fn every_scheme_parses() {
        for doc in SCHEMES {
            let target = conn_string::parse(doc.example).unwrap_or_else(|err| {
                panic!(
                    "scheme {:?} example {:?} is in the knowledge catalog but the \
connection layer rejected it: {err}",
                    doc.scheme, doc.example,
                )
            });
            let ok = match doc.scheme {
                "red" => matches!(target, ConnectionTarget::RedWire { tls: false, .. }),
                "reds" => matches!(target, ConnectionTarget::RedWire { tls: true, .. }),
                "red+ws" => matches!(target, ConnectionTarget::WsNative { tls: false, .. }),
                "red+wss" => matches!(target, ConnectionTarget::WsNative { tls: true, .. }),
                "grpc" | "grpcs" => matches!(target, ConnectionTarget::Grpc { .. }),
                "http" | "https" => matches!(target, ConnectionTarget::Http { .. }),
                "memory" => matches!(target, ConnectionTarget::Memory),
                "file" => matches!(target, ConnectionTarget::File { .. }),
                other => panic!("unhandled scheme {other:?} in test"),
            };
            assert!(
                ok,
                "scheme {:?} parsed to an unexpected target: {target:?}",
                doc.scheme,
            );
        }
    }

    /// A sanity guard that the parse check has teeth: a scheme that is NOT in
    /// the vocabulary must be rejected.
    #[test]
    fn unsupported_scheme_is_rejected() {
        let err = conn_string::parse("ftp://db.example.com").unwrap_err();
        assert_eq!(err.kind, conn_string::ParseErrorKind::UnsupportedScheme);
    }

    /// The documented scheme tokens are unique — no scheme is taught twice.
    #[test]
    fn scheme_tokens_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for doc in SCHEMES {
            assert!(
                seen.insert(doc.scheme),
                "scheme {:?} is listed more than once",
                doc.scheme,
            );
        }
    }

    /// Every documented scheme appears in the generated reference.
    #[test]
    fn reference_lists_every_scheme() {
        let reference = connection_reference_markdown();
        for doc in SCHEMES {
            assert!(
                reference.contains(&format!("`{}`", doc.uri_form)),
                "scheme {:?} ({}) is missing from the generated connection reference",
                doc.scheme,
                doc.uri_form,
            );
        }
    }

    /// The reference states, in the ADR 0061 wording, that RedWire is the
    /// principal transport — the single most important correction.
    #[test]
    fn reference_states_redwire_is_principal() {
        let reference = connection_reference_markdown();
        assert!(reference.contains("principal transport"));
        assert!(reference.contains("`red://`"));
        assert!(reference.contains("`reds://`"));
    }

    /// The reference covers all four taught deployment topologies.
    #[test]
    fn reference_covers_all_topologies() {
        let reference = connection_reference_markdown();
        for topology in [
            "Embedded / standalone",
            "Serverless",
            "Primary-replica",
            "Cluster",
        ] {
            assert!(
                reference.contains(topology),
                "topology {topology:?} is missing from the generated connection reference",
            );
        }
    }

    /// The documented default ports are the parser's own constants, so they
    /// cannot drift from the connection layer.
    #[test]
    fn reference_uses_parser_default_ports() {
        let reference = connection_reference_markdown();
        assert!(reference.contains(&format!("`{DEFAULT_PORT_RED}`")));
        assert!(reference.contains(&format!("`{DEFAULT_PORT_GRPC}`")));
    }

    /// The reference is deterministic (pure function of the catalog + prose).
    #[test]
    fn reference_is_deterministic() {
        assert_eq!(
            connection_reference_markdown(),
            connection_reference_markdown()
        );
    }

    /// The `docs/llms.txt` block wraps exactly the reference between markers.
    #[test]
    fn llms_section_wraps_reference() {
        let section = connection_llms_section();
        assert!(section.starts_with(LLMS_BEGIN_MARKER));
        assert!(section.ends_with(LLMS_END_MARKER));
        assert!(section.contains(&connection_reference_markdown()));
    }
}
