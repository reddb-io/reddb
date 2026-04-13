//! Actions Segment - Unified storage for all tool, playbook, and manual actions.
//!
//! This segment implements the unified intelligence layer, providing a single
//! data format (ActionRecord) that all components produce and consume.
//!
//! ## Submodules
//!
//! - `types` - Core enums: ActionSource, Target, ActionType, ActionOutcome
//! - `payloads` - Data structures for different action results (PortScan, DNS, TLS, etc.)
//! - `record` - ActionRecord universal envelope with encoding
//! - `trace` - Execution trace types (Attempt, TimingInfo, ActionTrace)
//! - `segment` - ActionSegment indexed storage
//! - `trace_segment` - TraceSegment storage and Tracer helper
//! - `insights` - Trace analysis and optimization suggestions

pub mod insights;
pub mod payloads;
pub mod record;
pub mod segment;
pub mod trace;
pub mod trace_segment;
pub mod types;

// Re-export main types for convenience
pub use insights::{FailurePattern, PerformanceInsight, PlaybookInsight, TargetProfile};
pub use payloads::{
    DnsData, FingerprintData, HttpData, PingData, PortScanData, RecordPayload, TlsData, VulnData,
    WhoisData,
};
pub use record::{current_timestamp, generate_id, ActionRecord, Confidence, IntoActionRecord};
pub use segment::ActionSegment;
pub use trace::{ActionTrace, Attempt, AttemptOutcome, TimingInfo};
pub use trace_segment::{TraceSegment, Tracer};
pub use types::{ActionOutcome, ActionSource, ActionType, Target};

// ==================== Tests ====================

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn test_action_record_roundtrip() {
        let record = ActionRecord::new(
            ActionSource::tool("network-ports"),
            Target::Host(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))),
            ActionType::Scan,
            RecordPayload::PortScan(PortScanData {
                open_ports: vec![22, 80, 443],
                closed_ports: vec![],
                filtered_ports: vec![8080],
                duration_ms: 1500,
            }),
            ActionOutcome::Success,
        );

        let encoded = record.encode();
        let decoded = ActionRecord::decode(&encoded).expect("decode failed");

        assert_eq!(record.id, decoded.id);
        assert_eq!(record.timestamp, decoded.timestamp);
        assert_eq!(record.action_type, decoded.action_type);
        assert!(decoded.is_success());

        if let RecordPayload::PortScan(data) = &decoded.payload {
            assert_eq!(data.open_ports, vec![22, 80, 443]);
            assert_eq!(data.filtered_ports, vec![8080]);
            assert_eq!(data.duration_ms, 1500);
        } else {
            panic!("wrong payload type");
        }
    }

    #[test]
    fn test_target_types() {
        let targets = vec![
            Target::Host(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))),
            Target::Network(IpAddr::V4(Ipv4Addr::new(192, 168, 0, 0)), 24),
            Target::Domain("example.com".into()),
            Target::Url("https://example.com/path".into()),
            Target::Port(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 443),
            Target::Service(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)), 22, "ssh".into()),
        ];

        for target in targets {
            assert!(!target.host_str().is_empty());
        }
    }

    #[test]
    fn test_action_outcome_variants() {
        let outcomes = [
            ActionOutcome::Success,
            ActionOutcome::Failed {
                error: "connection refused".into(),
            },
            ActionOutcome::Timeout { after_ms: 5000 },
            ActionOutcome::Partial {
                completed: 50,
                total: 100,
            },
            ActionOutcome::Skipped {
                reason: "already scanned".into(),
            },
        ];

        assert!(outcomes[0].is_success());
        assert!(outcomes[1].is_failure());
        assert!(outcomes[2].is_failure());
        assert!(!outcomes[3].is_success());
        assert!(!outcomes[4].is_success());
    }

    #[test]
    fn test_dns_data_roundtrip() {
        let data = DnsData {
            record_type: "MX".into(),
            records: vec!["mail1.example.com".into(), "mail2.example.com".into()],
            ttl: Some(3600),
        };

        let mut buf = Vec::new();
        data.encode(&mut buf);

        let mut pos = 0;
        let decoded = DnsData::decode(&buf, &mut pos).expect("decode failed");

        assert_eq!(data.record_type, decoded.record_type);
        assert_eq!(data.records, decoded.records);
        assert_eq!(data.ttl, decoded.ttl);
    }

    #[test]
    fn test_playbook_source_roundtrip() {
        let source = ActionSource::Playbook {
            id: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16],
            step: 42,
        };

        let mut buf = Vec::new();
        source.encode(&mut buf);

        let mut pos = 0;
        let decoded = ActionSource::decode(&buf, &mut pos).expect("decode failed");

        if let ActionSource::Playbook { id, step } = decoded {
            assert_eq!(id, [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
            assert_eq!(step, 42);
        } else {
            panic!("wrong source type");
        }
    }

    #[test]
    fn test_action_trace_roundtrip() {
        let action_id = [1u8; 16];
        let mut trace = ActionTrace::new(action_id);

        trace.add_attempt("connect", AttemptOutcome::Success, 50);
        trace.add_attempt("send_request", AttemptOutcome::Success, 100);
        trace.add_attempt(
            "parse_response",
            AttemptOutcome::Failed("invalid json".into()),
            10,
        );
        trace.add_param("timeout", "5000");
        trace.add_param("retries", "3");

        let encoded = trace.encode();
        let decoded = ActionTrace::decode(&encoded).expect("decode failed");

        assert_eq!(decoded.action_id, action_id);
        assert_eq!(decoded.attempts.len(), 3);
        assert_eq!(decoded.parameters.len(), 2);

        assert_eq!(decoded.attempts[0].what, "connect");
        assert_eq!(decoded.attempts[0].outcome, AttemptOutcome::Success);

        assert_eq!(decoded.attempts[2].what, "parse_response");
        assert!(matches!(
            decoded.attempts[2].outcome,
            AttemptOutcome::Failed(_)
        ));

        assert_eq!(decoded.parameters[0], ("timeout".into(), "5000".into()));
    }

    #[test]
    fn test_tracer_helper() {
        let action_id = [2u8; 16];
        let mut tracer = Tracer::new(action_id);

        tracer.param("preset", "common");

        // Simulate successful attempt
        let result: Result<i32, &str> = tracer.attempt("connect", || Ok(42));
        assert_eq!(result, Ok(42));

        // Simulate failed attempt
        let result: Result<i32, &str> = tracer.attempt("auth", || Err("access denied"));
        assert!(result.is_err());

        // Record timeout
        tracer.timeout("wait_response", 5000);

        let trace = tracer.finish();

        assert_eq!(trace.action_id, action_id);
        assert_eq!(trace.attempts.len(), 3);
        assert_eq!(trace.parameters.len(), 1);

        // Check attempt outcomes
        assert!(matches!(trace.attempts[0].outcome, AttemptOutcome::Success));
        assert!(matches!(
            trace.attempts[1].outcome,
            AttemptOutcome::Failed(_)
        ));
        assert!(matches!(trace.attempts[2].outcome, AttemptOutcome::Timeout));
    }

    #[test]
    fn test_timing_info() {
        let mut timing = TimingInfo::new();
        assert!(timing.started_at > 0);

        timing.add_network(100);
        timing.add_network(50);
        timing.add_processing(25);

        assert_eq!(timing.network_ms, 150);
        assert_eq!(timing.processing_ms, 25);

        timing.complete();
        assert!(timing.ended_at >= timing.started_at);
        assert_eq!(
            timing.total_ms,
            timing.ended_at.saturating_sub(timing.started_at)
        );
    }

    #[test]
    fn test_action_segment() {
        let mut segment = ActionSegment::new();

        // Add some records
        let ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        let record1 = ActionRecord::new(
            ActionSource::tool("network-ports"),
            Target::Host(ip),
            ActionType::Scan,
            RecordPayload::PortScan(PortScanData {
                open_ports: vec![22, 80],
                ..Default::default()
            }),
            ActionOutcome::Success,
        );

        let record2 = ActionRecord::new(
            ActionSource::tool("dns-lookup"),
            Target::Domain("example.com".into()),
            ActionType::Resolve,
            RecordPayload::Dns(DnsData {
                record_type: "A".into(),
                records: vec!["93.184.216.34".into()],
                ttl: Some(3600),
            }),
            ActionOutcome::Success,
        );

        let record3 = ActionRecord::new(
            ActionSource::tool("network-ports"),
            Target::Host(ip),
            ActionType::Scan,
            RecordPayload::PortScan(PortScanData::default()),
            ActionOutcome::Failed {
                error: "connection refused".into(),
            },
        );

        segment.add(record1);
        segment.add(record2);
        segment.add(record3);

        assert_eq!(segment.len(), 3);

        // Query by target
        let by_host = segment.by_target(&Target::Host(ip));
        assert_eq!(by_host.len(), 2);

        // Query by type
        let scans = segment.by_type(ActionType::Scan);
        assert_eq!(scans.len(), 2);

        let resolves = segment.by_type(ActionType::Resolve);
        assert_eq!(resolves.len(), 1);

        // Query by outcome
        let successes = segment.successes();
        assert_eq!(successes.len(), 2);

        let failures = segment.failures();
        assert_eq!(failures.len(), 1);
    }

    #[test]
    fn test_action_segment_serialization() {
        let mut segment = ActionSegment::new();

        segment.add(ActionRecord::new(
            ActionSource::tool("test"),
            Target::Domain("test.com".into()),
            ActionType::Resolve,
            RecordPayload::Dns(DnsData::default()),
            ActionOutcome::Success,
        ));

        segment.add(ActionRecord::new(
            ActionSource::Playbook {
                id: [1u8; 16],
                step: 1,
            },
            Target::Host(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))),
            ActionType::Scan,
            RecordPayload::PortScan(PortScanData::default()),
            ActionOutcome::Timeout { after_ms: 5000 },
        ));

        let serialized = segment.serialize();
        let deserialized = ActionSegment::deserialize(&serialized).expect("deserialize failed");

        assert_eq!(deserialized.len(), 2);
        assert_eq!(deserialized.successes().len(), 1);
        assert_eq!(deserialized.timeouts().len(), 1);
    }

    #[test]
    fn test_trace_segment() {
        let mut segment = TraceSegment::new();

        let id1 = [1u8; 16];
        let id2 = [2u8; 16];

        let mut trace1 = ActionTrace::new(id1);
        trace1.add_attempt("connect", AttemptOutcome::Success, 100);
        trace1.add_attempt("scan", AttemptOutcome::Failed("refused".into()), 50);

        let mut trace2 = ActionTrace::new(id2);
        trace2.add_attempt("connect", AttemptOutcome::Timeout, 5000);

        segment.add(trace1);
        segment.add(trace2);

        assert_eq!(segment.len(), 2);

        // Query by action
        let t1 = segment.for_action(&id1).expect("trace1 not found");
        assert_eq!(t1.attempts.len(), 2);

        // Query all failures
        let failures = segment.all_failed_attempts();
        assert_eq!(failures.len(), 1);

        // Query all timeouts
        let timeouts = segment.all_timeouts();
        assert_eq!(timeouts.len(), 1);
    }

    #[test]
    fn test_trace_segment_serialization() {
        let mut segment = TraceSegment::new();

        let mut trace = ActionTrace::new([3u8; 16]);
        trace.add_attempt("test", AttemptOutcome::Success, 10);
        trace.add_param("key", "value");

        segment.add(trace);

        let serialized = segment.serialize();
        let deserialized = TraceSegment::deserialize(&serialized).expect("deserialize failed");

        assert_eq!(deserialized.len(), 1);

        let t = deserialized
            .for_action(&[3u8; 16])
            .expect("trace not found");
        assert_eq!(t.attempts.len(), 1);
        assert_eq!(t.parameters.len(), 1);
    }
}
