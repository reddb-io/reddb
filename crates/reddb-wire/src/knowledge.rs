//! Agent-facing connection-model knowledge reference.
//!
//! Supported URI schemes are generated from the connection layer's
//! [`crate::conn_string::SUPPORTED_SCHEMES`] catalog. The deployment topology
//! guidance is hand-authored narrative per ADR 0061.

use crate::conn_string::{ConnectionScheme, SUPPORTED_SCHEMES};

/// Canonical URI for the connection-model knowledge resource served over MCP.
pub const RESOURCE_URI: &str = "reddb://knowledge/connections";

/// Short human title for the connection knowledge resource.
pub const RESOURCE_TITLE: &str = "RedDB Connection Model";

/// One-line description of the connection knowledge resource.
pub const RESOURCE_DESCRIPTION: &str =
    "Generated URI scheme/transport catalog plus the RedDB deployment topology guide.";

/// Markers delimiting the generated connection block inside `docs/llms.txt`.
pub const LLMS_BEGIN_MARKER: &str = "<!-- BEGIN GENERATED: connections -->";
/// Closing marker for the generated connection block in `docs/llms.txt`.
pub const LLMS_END_MARKER: &str = "<!-- END GENERATED: connections -->";

struct Topology {
    name: &'static str,
    summary: &'static str,
}

const TOPOLOGIES: &[Topology] = &[
    Topology {
        name: "Embedded / standalone",
        summary: "`memory://` and `file://` run the engine in the caller's process; \
a standalone server exposes the same engine over RedWire, gRPC, and HTTP.",
    },
    Topology {
        name: "Serverless",
        summary: "Serverless callers usually keep no local state and connect to a managed \
endpoint. Prefer `reds://` when RedWire is reachable; use HTTPS/gRPC only when the platform \
requires those transports.",
    },
    Topology {
        name: "Primary-replica",
        summary: "A primary accepts writes while replicas serve read traffic after replication. \
Multi-host remote URIs express primary-first routing, with `?route=primary` available when reads \
must stay on the primary.",
    },
    Topology {
        name: "Cluster",
        summary: "Cluster deployments coordinate multiple RedDB nodes behind the same logical \
database. RedWire (`red://` / `reds://`) is the principal data-plane transport; gRPC and HTTP are \
compatibility/admin surfaces, not the only way to connect.",
    },
];

/// Full Markdown body served by the MCP resource.
pub fn connection_reference_markdown() -> String {
    let mut out = String::new();
    push_intro(&mut out);
    push_supported_schemes(&mut out);
    push_topologies(&mut out);
    out
}

/// The connection block as embedded in `docs/llms.txt`.
pub fn connection_llms_section() -> String {
    format!(
        "{LLMS_BEGIN_MARKER}\n{}\n{LLMS_END_MARKER}",
        connection_reference_markdown().trim_end()
    )
}

fn push_intro(out: &mut String) {
    out.push_str("# RedDB Connection Model\n\n");
    out.push_str(
        "RedWire (`red://` / `reds://`) is RedDB's principal transport. \
gRPC, HTTP(S), browser WebSocket variants, `memory://`, and `file://` are supported connection \
surfaces, but agents should not collapse the model to \"only gRPC\" or \"only WebSocket\".\n\n",
    );
}

fn push_supported_schemes(out: &mut String) {
    out.push_str("## Supported URI Schemes and Transports\n\n");
    out.push_str("| URI scheme | Transport | Mode | Example | Notes |\n");
    out.push_str("|---|---|---|---|---|\n");
    for scheme in SUPPORTED_SCHEMES {
        push_scheme_row(out, *scheme);
    }
    out.push('\n');
}

fn push_scheme_row(out: &mut String, scheme: ConnectionScheme) {
    out.push_str("| `");
    out.push_str(scheme.uri_prefix());
    out.push_str("` | ");
    out.push_str(scheme.transport());
    out.push_str(" | ");
    out.push_str(scheme.mode());
    out.push_str(" | `");
    out.push_str(scheme.example());
    out.push_str("` | ");
    out.push_str(scheme.notes());
    out.push_str(" |\n");
}

fn push_topologies(out: &mut String) {
    out.push_str("## Deployment Topologies\n\n");
    for topology in TOPOLOGIES {
        out.push_str("### ");
        out.push_str(topology.name);
        out.push_str("\n\n");
        out.push_str(topology.summary);
        out.push_str("\n\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_scheme_examples_parse() {
        for scheme in SUPPORTED_SCHEMES {
            crate::conn_string::parse(scheme.example()).unwrap_or_else(|err| {
                panic!("{} example did not parse: {err}", scheme.uri_prefix())
            });
        }
    }

    #[test]
    fn reference_lists_supported_schemes_from_connection_layer() {
        let markdown = connection_reference_markdown();
        for scheme in SUPPORTED_SCHEMES {
            let needle = format!("`{}`", scheme.uri_prefix());
            assert!(markdown.contains(&needle), "missing {needle}");
        }
    }

    #[test]
    fn reference_covers_principal_transport_and_topologies() {
        let markdown = connection_reference_markdown();
        assert!(markdown.contains("principal transport"));
        assert!(markdown.contains("RedWire (`red://` / `reds://`)"));
        for topology in TOPOLOGIES {
            assert!(
                markdown.contains(topology.name),
                "missing {}",
                topology.name
            );
        }
    }

    #[test]
    fn llms_section_wraps_reference() {
        let section = connection_llms_section();
        assert!(section.starts_with(LLMS_BEGIN_MARKER));
        assert!(section.ends_with(LLMS_END_MARKER));
        let reference = connection_reference_markdown();
        assert!(section.contains(reference.trim_end()));
    }
}
