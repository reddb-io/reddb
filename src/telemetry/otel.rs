//! OpenTelemetry tracing scaffold (PLAN.md Phase 5.3).
//!
//! This module is gated on `--features otel` so the default build
//! stays lean (no OTLP / opentelemetry crate dependency tree).
//! Operators who want OTLP export to Jaeger / Tempo / Honeycomb /
//! Datadog / NewRelic compile the engine with `--features otel` and
//! configure via the standard W3C / OTel env vars:
//!
//!   * `OTEL_EXPORTER_OTLP_ENDPOINT` (default `http://localhost:4317`)
//!   * `OTEL_EXPORTER_OTLP_PROTOCOL` (`grpc` or `http/protobuf`)
//!   * `OTEL_SERVICE_NAME` (default `reddb`)
//!   * `OTEL_RESOURCE_ATTRIBUTES` (comma-separated `k=v` list)
//!
//! ## Why a foundation module instead of full integration
//!
//! Pulling `opentelemetry`, `opentelemetry-otlp`, `tonic`, and the
//! tracing-subscriber bridge layers in adds ~30 transitive crates
//! and changes the binary size envelope materially. The PLAN.md
//! release gate (`< 30 MB static`) needs that decision validated
//! against the static-binary build first. This module documents
//! the contract; the live OTLP exporter wiring lands in a separate
//! commit that includes the dep-tree audit + size measurement.
//!
//! ## What's wired today
//!
//! - `OtelConfig::from_env()` reads the standard env vars and
//!   returns the parsed config.
//! - `init()` is a no-op stub that logs the parsed config so
//!   operators can confirm the engine *would* export to the right
//!   place once the integration ships.
//!
//! Code paths that already use the `tracing` crate continue to
//! work unchanged. Span context propagation through gRPC / HTTP
//! headers (`traceparent`) is the next deliverable.

use std::collections::BTreeMap;

#[derive(Debug, Clone)]
pub struct OtelConfig {
    pub endpoint: String,
    pub protocol: OtelProtocol,
    pub service_name: String,
    pub resource_attributes: BTreeMap<String, String>,
    pub sample_ratio: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtelProtocol {
    Grpc,
    HttpProtobuf,
}

impl OtelProtocol {
    pub fn label(self) -> &'static str {
        match self {
            Self::Grpc => "grpc",
            Self::HttpProtobuf => "http/protobuf",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "grpc" | "" => Some(Self::Grpc),
            "http/protobuf" | "http" | "protobuf" => Some(Self::HttpProtobuf),
            _ => None,
        }
    }
}

impl OtelConfig {
    pub fn from_env() -> Self {
        let endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
            .unwrap_or_else(|_| "http://localhost:4317".to_string());
        let protocol = std::env::var("OTEL_EXPORTER_OTLP_PROTOCOL")
            .ok()
            .and_then(|raw| OtelProtocol::parse(&raw))
            .unwrap_or(OtelProtocol::Grpc);
        let service_name =
            std::env::var("OTEL_SERVICE_NAME").unwrap_or_else(|_| "reddb".to_string());
        let resource_attributes = parse_resource_attributes(
            &std::env::var("OTEL_RESOURCE_ATTRIBUTES").unwrap_or_default(),
        );
        let sample_ratio = std::env::var("OTEL_TRACES_SAMPLER_ARG")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| (0.0..=1.0).contains(v))
            .unwrap_or(1.0);
        Self {
            endpoint,
            protocol,
            service_name,
            resource_attributes,
            sample_ratio,
        }
    }
}

fn parse_resource_attributes(raw: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for kv in raw.split(',') {
        let kv = kv.trim();
        if kv.is_empty() {
            continue;
        }
        if let Some((k, v)) = kv.split_once('=') {
            out.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    out
}

/// Initialise OTLP export. **Stub** today — logs the parsed config
/// so the operator can verify their env vars are correct, but does
/// not yet ship spans. The live exporter wires here once the
/// dep-tree review passes.
pub fn init(cfg: &OtelConfig) {
    tracing::info!(
        target: "reddb::telemetry::otel",
        endpoint = %cfg.endpoint,
        protocol = cfg.protocol.label(),
        service_name = %cfg.service_name,
        sample_ratio = cfg.sample_ratio,
        attrs = ?cfg.resource_attributes,
        "otel feature enabled — config parsed; OTLP exporter wiring is a follow-up commit"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_parse_known_values() {
        assert_eq!(OtelProtocol::parse("grpc"), Some(OtelProtocol::Grpc));
        assert_eq!(OtelProtocol::parse("GRPC"), Some(OtelProtocol::Grpc));
        assert_eq!(OtelProtocol::parse(""), Some(OtelProtocol::Grpc));
        assert_eq!(
            OtelProtocol::parse("http/protobuf"),
            Some(OtelProtocol::HttpProtobuf)
        );
        assert_eq!(OtelProtocol::parse("http"), Some(OtelProtocol::HttpProtobuf));
        assert_eq!(OtelProtocol::parse("nope"), None);
    }

    #[test]
    fn resource_attributes_parsed() {
        let attrs = parse_resource_attributes("region=us-east-1,tenant=acme,env=prod");
        assert_eq!(attrs.get("region"), Some(&"us-east-1".to_string()));
        assert_eq!(attrs.get("tenant"), Some(&"acme".to_string()));
        assert_eq!(attrs.get("env"), Some(&"prod".to_string()));
    }

    #[test]
    fn resource_attributes_skip_empty_segments() {
        let attrs = parse_resource_attributes(",,k=v,,");
        assert_eq!(attrs.len(), 1);
        assert_eq!(attrs.get("k"), Some(&"v".to_string()));
    }

    #[test]
    fn sample_ratio_clamps_out_of_range() {
        unsafe {
            std::env::set_var("OTEL_TRACES_SAMPLER_ARG", "2.0");
        }
        let cfg = OtelConfig::from_env();
        unsafe {
            std::env::remove_var("OTEL_TRACES_SAMPLER_ARG");
        }
        assert_eq!(cfg.sample_ratio, 1.0);
    }
}
