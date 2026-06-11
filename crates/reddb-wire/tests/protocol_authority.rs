use std::fs;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crates/reddb-wire has workspace root two levels up")
        .to_path_buf()
}

fn read(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path.as_ref())
        .unwrap_or_else(|err| panic!("read {}: {err}", path.as_ref().display()))
}

fn rust_files_under(path: impl AsRef<Path>) -> Vec<PathBuf> {
    let path = path.as_ref();
    let mut files = Vec::new();
    for entry in
        fs::read_dir(path).unwrap_or_else(|err| panic!("read dir {}: {err}", path.display()))
    {
        let entry = entry.unwrap_or_else(|err| panic!("read dir entry {}: {err}", path.display()));
        let entry_path = entry.path();
        if entry_path.is_dir() {
            files.extend(rust_files_under(&entry_path));
        } else if entry_path
            .extension()
            .is_some_and(|extension| extension == "rs")
        {
            files.push(entry_path);
        }
    }
    files
}

fn non_test_source(text: &str) -> &str {
    text.split("#[cfg(test)]").next().unwrap_or(text)
}

#[test]
fn redwire_frame_contracts_live_only_in_reddb_wire() {
    let root = repo_root();
    let files = [
        "crates/reddb-server/src/wire/redwire/mod.rs",
        "crates/reddb-server/src/wire/redwire/session.rs",
        "crates/reddb-client/src/redwire/mod.rs",
        "crates/reddb-client/src/connector/redwire.rs",
    ];

    for file in files {
        let text = read(root.join(file));
        for forbidden in [
            "pub struct Frame {",
            "struct Frame {",
            "pub enum MessageKind",
            "enum MessageKind",
            "pub fn encode_frame(",
            "fn encode_frame(",
            "pub fn decode_frame(",
            "fn decode_frame(",
        ] {
            assert!(
                !text.contains(forbidden),
                "{file} must import RedWire frame contracts from reddb-wire, found {forbidden:?}"
            );
        }
    }
}

#[test]
fn redwire_stream_payload_contracts_live_only_in_reddb_wire() {
    let root = repo_root();
    let server_input = read(root.join("crates/reddb-server/src/wire/redwire/input_stream.rs"));
    let server_output = read(root.join("crates/reddb-server/src/wire/redwire/output_stream.rs"));
    let server_non_test = non_test_source(&server_input);
    let server_output_non_test = non_test_source(&server_output);
    let wire_stream = read(root.join("crates/reddb-wire/src/redwire/stream.rs"));

    for (file, text) in [
        ("input_stream.rs", server_non_test),
        ("output_stream.rs", server_output_non_test),
    ] {
        for forbidden in [
            "pub struct InputChunk",
            "struct InputChunk",
            "pub enum ChunkParseError",
            "enum ChunkParseError",
            "pub struct OpenInputRequest",
            "struct OpenInputRequest",
            "pub enum OpenInputParseError",
            "enum OpenInputParseError",
            "pub struct OpenStreamRequest",
            "struct OpenStreamRequest",
            "pub enum OpenStreamParseError",
            "enum OpenStreamParseError",
            "pub struct StreamCancelRequest",
            "struct StreamCancelRequest",
            "build_stream_chunk_payload",
            "build_stream_error_payload",
            "build_stream_end_payload",
            "build_input_stream_end_payload",
            "build_input_stream_error_payload",
        ] {
            assert!(
                !text.contains(forbidden),
                "{file} RedWire stream payload contract must live in reddb-wire, found {forbidden:?}"
            );
        }
    }

    for required in [
        "pub struct OpenStreamRequest",
        "pub enum OpenStreamParseError",
        "pub struct StreamCancelRequest",
        "pub fn parse_open_stream",
        "pub fn parse_stream_cancel",
        "pub fn build_open_stream_payload",
        "pub fn build_open_stream_frame",
        "pub fn build_stream_chunk_payload",
        "pub fn build_stream_error_payload",
        "pub fn build_stream_end_payload",
    ] {
        assert!(
            wire_stream.contains(required),
            "reddb-wire should own output stream payload contract {required}"
        );
    }

    for required in [
        "pub struct InputChunk",
        "pub enum ChunkParseError",
        "pub struct OpenInputRequest",
        "pub enum OpenInputParseError",
        "pub fn parse_input_chunk",
        "pub fn parse_input_chunk_json",
        "pub fn build_input_stream_end_payload",
        "pub fn build_input_stream_error_payload",
    ] {
        assert!(
            wire_stream.contains(required),
            "reddb-wire should own stream payload contract {required}"
        );
    }

    for required in [
        "reddb_wire::redwire::stream::parse_input_chunk_json",
        "reddb_wire::redwire::stream::build_input_stream_error_frame",
        "reddb_wire::redwire::stream::build_input_stream_end_frame",
    ] {
        assert!(
            server_non_test.contains(required),
            "server RedWire input-stream runtime should delegate through {required}"
        );
    }
    for required in [
        "reddb_wire::redwire::stream::parse_open_stream",
        "reddb_wire::redwire::stream::parse_stream_cancel",
        "reddb_wire::redwire::stream::build_open_ack_frame",
        "reddb_wire::redwire::stream::build_stream_error_frame",
        "reddb_wire::redwire::stream::build_stream_chunk_frame_from_json_bytes",
        "reddb_wire::redwire::stream::build_stream_end_frame",
    ] {
        assert!(
            server_output_non_test.contains(required),
            "server RedWire output-stream runtime should delegate through {required}"
        );
    }
}

#[test]
fn redwire_queue_payload_contracts_live_only_in_reddb_wire() {
    let root = repo_root();
    let server_queue = read(root.join("crates/reddb-server/src/wire/redwire/queue_wait.rs"));
    let server_non_test = non_test_source(&server_queue);
    let wire_queue = read(root.join("crates/reddb-wire/src/redwire/queue.rs"));

    for forbidden in [
        "pub struct QueueWaitOpenRequest",
        "struct QueueWaitOpenRequest",
        "pub enum QueueWaitParseError",
        "enum QueueWaitParseError",
        "build_event_push_payload",
        "build_queue_wait_timeout_payload",
        "build_queue_wait_error_payload",
    ] {
        assert!(
            !server_non_test.contains(forbidden),
            "server RedWire queue-wait payload contract must live in reddb-wire, found {forbidden:?}"
        );
    }

    for required in [
        "pub struct QueueWaitOpenRequest",
        "pub enum QueueWaitParseError",
        "pub fn parse_queue_wait_open",
        "pub fn build_queue_wait_open_payload",
        "pub fn build_queue_wait_open_frame",
        "pub fn build_event_push_payload",
        "pub fn build_queue_wait_timeout_payload",
        "pub fn build_queue_wait_error_payload",
        "pub const WAIT_CANCELLED_CODE",
        "pub const WAIT_EXCEEDS_CAP_CODE",
        "pub const WAIT_FAILED_CODE",
    ] {
        assert!(
            wire_queue.contains(required),
            "reddb-wire should own queue-wait payload contract {required}"
        );
    }

    for required in [
        "reddb_wire::redwire::queue::parse_queue_wait_open",
        "reddb_wire::redwire::queue::build_queue_event_push_frame_from_json_bytes",
        "reddb_wire::redwire::queue::build_queue_wait_timeout_frame",
        "reddb_wire::redwire::queue::build_queue_wait_error_frame",
    ] {
        assert!(
            server_non_test.contains(required),
            "server RedWire queue-wait runtime should delegate through {required}"
        );
    }
}

#[test]
fn server_public_reexport_describes_reddb_wire_as_protocol_authority() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/lib.rs"));

    for forbidden in [
        "connection-string parser today",
        "RedWire frames in a follow-up",
        "future slice",
        "future slices",
    ] {
        assert!(
            !text.contains(forbidden),
            "server reexport docs must describe reddb-wire as the current protocol authority, found {forbidden:?}"
        );
    }

    for required in [
        "connection strings",
        "audit-safe sanitizers",
        "RedWire frames/codecs",
        "payloads",
        "topology",
        "replication wire messages",
    ] {
        assert!(
            text.contains(required),
            "server reexport docs should mention reddb-wire ownership of {required}"
        );
    }
}

#[test]
fn context_declares_reddb_wire_as_protocol_authority() {
    let root = repo_root();
    let persistence = read(root.join(".red/context/persistence.md"));
    let wire = read(root.join("crates/reddb-wire/src/lib.rs"));

    for required in [
        "File/protocol ownership boundary",
        "`reddb-wire` is the authority for communication contracts",
        "frames, codecs, payloads, topology, connection strings, sanitizers, and replication wire messages",
        "must not introduce new persistent file formats or protocol payload formats directly",
    ] {
        assert!(
            persistence.contains(required),
            "persistence context must declare reddb-wire authority: {required}"
        );
    }

    for required in [
        "connection-string parser",
        "audit-safe sanitizers",
        "RedWire frame layout and codec",
        "handshake payloads",
        "topology payloads",
        "replication wire",
    ] {
        assert!(
            wire.contains(required),
            "reddb-wire crate docs should advertise ownership of {required}"
        );
    }
}

#[test]
fn client_connection_string_vocabulary_lives_in_reddb_wire() {
    let root = repo_root();
    let connect = read(root.join("crates/reddb-client/src/connect.rs"));
    let red_client = read(root.join("crates/reddb-client/src/bin/red_client.rs"));
    let wire = read(root.join("crates/reddb-wire/src/conn_string.rs"));

    for forbidden in [
        "fn is_embedded_uri",
        "\"red://\" | \"red:\"",
        "trimmed.starts_with(\"red:///\")",
        "Url::parse",
        "split_once(\"://\")",
    ] {
        assert!(
            !red_client.contains(forbidden),
            "red_client connection-string vocabulary belongs in reddb-wire, found {forbidden:?}"
        );
    }

    for forbidden in ["Url::parse", "split_once(\"://\")"] {
        assert!(
            !connect.contains(forbidden),
            "reddb-client connect parser should delegate to reddb-wire, found {forbidden:?}"
        );
    }

    for required in [
        "use reddb_wire::{parse as wire_parse",
        "is_embedded_connection_uri",
    ] {
        assert!(
            connect.contains(required) || red_client.contains(required),
            "client connection handling should route through reddb-wire helper {required}"
        );
    }
    assert!(
        wire.contains("pub fn is_embedded_connection_uri"),
        "reddb-wire should own embedded connection URI aliases"
    );
}

#[test]
fn connection_string_and_sanitizer_contracts_live_only_in_reddb_wire() {
    let root = repo_root();
    let server_root = root.join("crates/reddb-server/src");
    let client_root = root.join("crates/reddb-client/src");
    let connector_root = root.join("crates/reddb-client-connector/src");
    let wire_lib = read(root.join("crates/reddb-wire/src/lib.rs"));
    let wire_conn = read(root.join("crates/reddb-wire/src/conn_string.rs"));
    let wire_sanitizer = read(root.join("crates/reddb-wire/src/sanitizer.rs"));

    for path in rust_files_under(&server_root)
        .into_iter()
        .chain(rust_files_under(&client_root))
        .chain(rust_files_under(&connector_root))
    {
        let text = read(&path);
        let rel = path.strip_prefix(&root).unwrap_or(&path).display();

        for forbidden in [
            "pub enum ConnectionTarget",
            "enum ConnectionTarget",
            "pub struct ConnStringLimits",
            "struct ConnStringLimits",
            "pub struct ConnStringSanitizer",
            "struct ConnStringSanitizer",
            "pub struct ParsedConnString",
            "struct ParsedConnString",
            "pub struct Tainted",
            "struct Tainted",
            "pub struct TaintedRef",
            "struct TaintedRef",
            "pub enum TaintedTarget",
            "enum TaintedTarget",
            "pub enum Boundary",
            "enum Boundary",
            "Url::parse",
            "url::Url",
        ] {
            assert!(
                !text.contains(forbidden),
                "{rel} must not own connection-string or sanitizer contracts; found {forbidden:?}"
            );
        }
    }

    for required in [
        "pub enum ConnectionTarget",
        "pub struct ConnStringLimits",
        "pub fn parse(",
        "pub fn parse_with_limits(",
        "pub fn is_embedded_connection_uri",
    ] {
        assert!(
            wire_conn.contains(required),
            "reddb-wire conn_string module should own {required}"
        );
    }

    for required in [
        "pub enum Boundary",
        "pub struct Tainted",
        "pub struct ConnStringSanitizer",
        "pub struct ParsedConnString",
        "pub struct TaintedRef",
        "pub enum TaintedTarget",
        "pub fn audit_safe_log_field",
    ] {
        assert!(
            wire_sanitizer.contains(required),
            "reddb-wire sanitizer module should own {required}"
        );
    }

    for required in [
        "parse_with_limits",
        "ConnStringLimits",
        "ConnectionTarget",
        "ConnStringSanitizer",
        "ParsedConnString",
        "Tainted",
        "TaintedRef",
        "TaintedTarget",
        "Boundary",
        "audit_safe_log_field",
    ] {
        assert!(
            wire_lib.contains(required),
            "reddb-wire lib should export connection-string/sanitizer contract {required}"
        );
    }
}

#[test]
fn client_auth_wire_vocabulary_lives_in_reddb_wire() {
    let root = repo_root();
    let connector = read(root.join("crates/reddb-client-connector/src/lib.rs"));
    let http = read(root.join("crates/reddb-client/src/http.rs"));
    let bin_http = read(root.join("crates/reddb-client/src/connector/http.rs"));
    let wire = read(root.join("crates/reddb-wire/src/auth.rs"));

    for (file, text) in [
        (
            "crates/reddb-client-connector/src/lib.rs",
            connector.as_str(),
        ),
        ("crates/reddb-client/src/http.rs", http.as_str()),
        (
            "crates/reddb-client/src/connector/http.rs",
            bin_http.as_str(),
        ),
    ] {
        for forbidden in [
            "format!(\"Bearer",
            "\"authorization\"",
            "{{\\\"username\\\":\\\"{}\\\",\\\"password\\\":\\\"{}\\\"}}",
        ] {
            assert!(
                !text.contains(forbidden),
                "{file} auth wire vocabulary belongs in reddb-wire, found {forbidden:?}"
            );
        }
    }

    for required in [
        "pub const AUTHORIZATION_HEADER",
        "pub const BEARER_AUTH_SCHEME",
        "pub fn bearer_authorization_value",
        "pub fn login_payload_json",
    ] {
        assert!(
            wire.contains(required),
            "reddb-wire should own auth wire helper {required}"
        );
    }
}

#[test]
fn server_replication_basebackup_payload_fields_live_in_reddb_wire() {
    let root = repo_root();
    let replica = read(root.join("crates/reddb-server/src/replication/replica.rs"));
    let wire = read(root.join("crates/reddb-wire/src/replication/basebackup.rs"));
    let non_test = non_test_source(&replica);

    for forbidden in [
        "\"basebackup_manifest_hex\"",
        "\"basebackup_chunk_ordinal\"",
        "\"basebackup_chunk_hex\"",
        "\"basebackup_chunk_ordinal/basebackup_chunk_hex\"",
    ] {
        assert!(
            !non_test.contains(forbidden),
            "server replica should import basebackup payload field contracts from reddb-wire, found {forbidden:?}"
        );
    }

    for required in [
        "BASEBACKUP_MANIFEST_HEX_FIELD",
        "BASEBACKUP_CHUNK_ORDINAL_FIELD",
        "BASEBACKUP_CHUNK_HEX_FIELD",
        "BASEBACKUP_CHUNK_PAIR_FIELD",
        "required_basebackup_manifest",
        "basebackup_chunk_part",
    ] {
        assert!(
            wire.contains(required),
            "reddb-wire should own basebackup payload contract {required}"
        );
    }
}

#[test]
fn server_replication_wal_ack_payloads_live_in_reddb_wire() {
    let root = repo_root();
    let grpc = read(root.join("crates/reddb-server/src/grpc/service_impl.rs"));
    let non_test = non_test_source(&grpc);
    let wire = read(root.join("crates/reddb-wire/src/replication/wal_stream.rs"));

    for forbidden in [
        "reply.insert(\"replica_id\"",
        "reply.insert(\"applied_lsn\"",
        "reply.insert(\"durable_lsn\"",
        "reply.insert(\"apply_errors_total\"",
        "reply.insert(\"divergence_total\"",
        "\"apply_errors_total\".into()",
        "\"divergence_total\".into()",
    ] {
        assert!(
            !non_test.contains(forbidden),
            "server ack_replica_lsn should not own WAL ack reply payload fields, found {forbidden:?}"
        );
    }

    for required in [
        "WalStreamAck::decode_json",
        "WalStreamAckReply::from_ack",
        "reply.encode_json()",
    ] {
        assert!(
            non_test.contains(required),
            "server ack_replica_lsn should route WAL ack payload through reddb-wire {required}"
        );
    }

    for required in [
        "pub struct WalStreamAck",
        "pub struct WalStreamAckReply",
        "pub fn encode_json(&self) -> Vec<u8>",
        "pub fn decode_json(bytes: &[u8]) -> Result<Self>",
    ] {
        assert!(
            wire.contains(required),
            "reddb-wire should own WAL ack payload contract {required}"
        );
    }
}

#[test]
fn server_rejoin_rewind_confirmation_payload_lives_in_reddb_wire() {
    let root = repo_root();
    let handler = read(root.join("crates/reddb-server/src/server/handlers_replication.rs"));
    let non_test = non_test_source(&handler);
    let wire = read(root.join("crates/reddb-wire/src/replication/timeline.rs"));

    for forbidden in [
        "serde_json::from_slice::<crate::serde_json::Value>(&body)",
        "payload.get(\"target_timeline\")",
        "payload.get(\"rewind_to_lsn\")",
        "object.insert(\"target_timeline\"",
        "object.insert(\"rewind_to_lsn\"",
        "object.insert(\"next_step\"",
    ] {
        assert!(
            !non_test.contains(forbidden),
            "server rejoin rewind confirmation payload must live in reddb-wire, found {forbidden:?}"
        );
    }

    for required in [
        "RejoinRewindConfirmation::decode_json",
        "RejoinRewindConfirmationReply::confirmed",
    ] {
        assert!(
            non_test.contains(required),
            "server rejoin rewind confirmation should route through reddb-wire {required}"
        );
    }

    for required in [
        "pub struct RejoinRewindConfirmation",
        "pub struct RejoinRewindConfirmationReply",
        "pub fn decode_json(bytes: &[u8]) -> Result<Self>",
        "pub fn encode_json(&self) -> Vec<u8>",
    ] {
        assert!(
            wire.contains(required),
            "reddb-wire should own rejoin rewind confirmation payload contract {required}"
        );
    }

    for required in ["RejoinRewindConfirmation {", ".encode_json()"] {
        assert!(
            handler.contains(required),
            "server rejoin rewind tests should use reddb-wire request encoder {required}"
        );
    }

    for forbidden in [
        r#"{"target_timeline":3,"rewind_to_lsn":42}"#,
        r#"{"target_timeline":3,"rewind_to_lsn":41}"#,
    ] {
        assert!(
            !handler.contains(forbidden),
            "server rejoin rewind tests should use reddb-wire request encoders, found {forbidden:?}"
        );
    }
}

#[test]
fn server_failover_promotion_payload_lives_in_reddb_wire() {
    let root = repo_root();
    let handler = read(root.join("crates/reddb-server/src/server/handlers_failover.rs"));
    let non_test = non_test_source(&handler);
    let wire = read(root.join("crates/reddb-wire/src/replication/timeline.rs"));

    for forbidden in [
        "serde_json::from_slice::<crate::serde_json::Value>(&body)",
        ".get(\"holder_id\")",
        ".get(\"ttl_ms\")",
        "object.insert(\"holder_id\"",
        "object.insert(\"generation\"",
        "object.insert(\"acquired_at_ms\"",
        "object.insert(\"expires_at_ms\"",
        "object.insert(\"timeline\"",
        "object.insert(\"applied_lsn\"",
        "object.insert(\"next_step\"",
    ] {
        assert!(
            !non_test.contains(forbidden),
            "server failover promotion payload must live in reddb-wire, found {forbidden:?}"
        );
    }

    for required in [
        "FailoverPromotionRequest::decode_json",
        "FailoverPromotionReply::promoted",
    ] {
        assert!(
            non_test.contains(required),
            "server failover promotion should route through reddb-wire {required}"
        );
    }

    for required in ["FailoverPromotionRequest {", ".encode_json()"] {
        assert!(
            handler.contains(required),
            "server failover promotion tests should use reddb-wire request encoder {required}"
        );
    }

    for required in [
        "pub struct FailoverPromotionRequest",
        "pub struct FailoverPromotionReply",
        "pub fn decode_json(bytes: &[u8]) -> Result<Self>",
        "pub fn encode_json(&self) -> Vec<u8>",
    ] {
        assert!(
            wire.contains(required),
            "reddb-wire should own failover promotion payload contract {required}"
        );
    }

    assert!(
        !handler.contains(r#"{"holder_id":"replica-a","ttl_ms":30000}"#),
        "server failover promotion tests should use reddb-wire request encoders"
    );
}

#[test]
fn server_redwire_frame_header_length_routes_through_reddb_wire() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/wire/redwire/session.rs"));

    for forbidden in [
        "u32::from_le_bytes([buf[0]",
        "u32::from_le_bytes([header[0]",
        "length < FRAME_HEADER_SIZE",
        "length > MAX_FRAME_SIZE",
        "frame_len_from_header",
        "decode_frame_parts",
        "FRAME_HEADER_SIZE",
    ] {
        assert!(
            !text.contains(forbidden),
            "server RedWire frame length validation should route through reddb-wire, found {forbidden:?}"
        );
    }
    assert!(
        text.contains("read_frame_async"),
        "server RedWire session should delegate frame reads to reddb_wire::redwire::read_frame_async"
    );
}

#[test]
fn client_connector_redwire_is_compatibility_adapter_only() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-client/src/connector/redwire.rs"));
    let code_lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//!"))
        .collect::<Vec<_>>();

    for forbidden in [
        "tokio::net::TcpStream",
        "AsyncReadExt",
        "AsyncWriteExt",
        "pub enum Auth",
        "pub enum RedWireError",
        "pub struct RedWireClient",
        "fn from_client_error",
        "build_hello_payload",
        "build_auth_response",
        "fn handshake",
        "fn read_frame",
        "decode_frame",
        "encode_frame",
    ] {
        assert!(
            !text.contains(forbidden),
            "connector::redwire should delegate to crate::redwire, found {forbidden:?}"
        );
    }
    assert!(
        text.contains("pub use crate::redwire::{Auth, ConnectOptions, RedWireClient};"),
        "connector::redwire should be reexports over the canonical client redwire module"
    );
    assert_eq!(
        code_lines,
        vec![
            "pub use crate::error::{ClientError as RedWireError, Result};",
            "pub use crate::redwire::{Auth, ConnectOptions, RedWireClient};",
        ],
        "connector::redwire should stay a compatibility re-export only"
    );
}

#[test]
fn client_redwire_client_type_has_single_definition() {
    let root = repo_root();
    let mut definitions = Vec::new();
    for path in rust_files_under(&root.join("crates/reddb-client/src")) {
        let text = read(&path);
        if text.contains("pub struct RedWireClient") {
            definitions.push(path.strip_prefix(&root).unwrap_or(&path).to_path_buf());
        }
    }

    assert_eq!(
        definitions,
        vec![PathBuf::from("crates/reddb-client/src/redwire/mod.rs")],
        "reddb-client should expose exactly one RedWireClient implementation"
    );
}

#[test]
fn server_redwire_tests_build_fixtures_through_reddb_wire() {
    let root = repo_root();
    for path in rust_files_under(&root.join("crates/reddb-server/tests")) {
        let text = read(&path);
        for forbidden in [
            "Frame::new(",
            "FrameBuilder::",
            "decode_frame(",
            "encode_frame(",
            "FRAME_HEADER_SIZE",
        ] {
            assert!(
                !text.contains(forbidden),
                "{} RedWire fixtures should use reddb-wire builders and frame I/O, found {forbidden:?}",
                path.display()
            );
        }
    }

    for (file, required) in [
        (
            "crates/reddb-server/tests/queue_read_wait_redwire_smoke.rs",
            "build_queue_wait_open_frame",
        ),
        (
            "crates/reddb-server/tests/queue_read_wait_redwire_smoke.rs",
            "read_frame_async",
        ),
        (
            "crates/reddb-server/tests/e2e_issue_936_browser_credential_layer.rs",
            "build_open_stream_frame",
        ),
        (
            "crates/reddb-server/tests/e2e_issue_936_browser_credential_layer.rs",
            "write_frame_async",
        ),
    ] {
        let text = read(root.join(file));
        assert!(
            text.contains(required),
            "{file} should route RedWire fixtures through reddb-wire {required}"
        );
    }
}

#[test]
fn client_redwire_has_single_frame_io_adapter() {
    let root = repo_root();
    let client = read(root.join("crates/reddb-client/src/redwire/mod.rs"));
    let handshake = read(root.join("crates/reddb-client/src/redwire/handshake.rs"));
    let io = read(root.join("crates/reddb-client/src/redwire/io.rs"));
    let wire_io = read(root.join("crates/reddb-wire/src/redwire/io.rs"));

    for forbidden in [
        "FRAME_HEADER_SIZE",
        "decode_frame",
        "MAX_KNOWN_MINOR_VERSION",
        "build_hello_payload",
        "u32::from_le_bytes([header[0]",
        "async fn read_frame",
    ] {
        assert!(
            !handshake.contains(forbidden),
            "client handshake should use the canonical redwire::io adapter, found {forbidden:?}"
        );
    }

    for forbidden in [
        "FRAME_HEADER_SIZE",
        "decode_frame",
        "decode_frame_parts",
        "encode_frame",
        "frame_len_from_header",
        "u32::from_le_bytes([header[0]",
    ] {
        assert!(
            !client.contains(forbidden),
            "client redwire module should use its single frame I/O adapter, found {forbidden:?}"
        );
    }

    for forbidden in [
        "frame_len_from_header",
        "decode_frame_parts",
        "encode_frame",
        "read_exact",
        "write_all",
    ] {
        assert!(
            !io.contains(forbidden),
            "client frame I/O adapter should delegate to reddb-wire, found {forbidden:?}"
        );
    }

    for required in [
        "read_frame_async",
        "write_frame_async",
        "pub(super) async fn read_frame",
        "pub(super) async fn write_frame",
    ] {
        assert!(
            io.contains(required),
            "client frame I/O adapter should route through reddb-wire {required}"
        );
    }

    for required in [
        "pub async fn read_frame_async",
        "pub async fn write_frame_async",
        "frame_len_from_header",
        "decode_frame_parts",
        "encode_frame",
    ] {
        assert!(
            wire_io.contains(required),
            "reddb-wire should own async frame I/O contract {required}"
        );
    }

    assert!(
        handshake.contains("build_client_hello_frame"),
        "client handshake should let reddb-wire build the Hello frame and choose advertised minor versions"
    );
}

#[test]
fn client_redwire_request_frames_live_in_reddb_wire() {
    let root = repo_root();
    let client = read(root.join("crates/reddb-client/src/redwire/mod.rs"));
    let handshake = read(root.join("crates/reddb-client/src/redwire/handshake.rs"));
    let builder = read(root.join("crates/reddb-wire/src/redwire/builder.rs"));
    let wire_handshake = read(root.join("crates/reddb-wire/src/redwire/handshake.rs"));

    for file in [&client, &handshake] {
        assert!(
            !file.contains("Frame::new("),
            "client RedWire request frame construction belongs in reddb-wire"
        );
    }

    for required in [
        "build_query_frame",
        "build_query_with_params_frame",
        "build_bulk_insert_frame",
        "build_get_frame",
        "build_delete_frame",
        "build_bulk_insert_binary_frame",
        "build_ping_frame",
        "build_bye_frame",
    ] {
        assert!(
            client.contains(required),
            "client should route request frame construction through {required}"
        );
        assert!(
            builder.contains(&format!("pub fn {required}")),
            "reddb-wire should own client request frame builder {required}"
        );
    }

    for required in ["build_client_hello_frame", "build_auth_response_frame"] {
        assert!(
            handshake.contains(required),
            "client handshake should route frame construction through {required}"
        );
        assert!(
            wire_handshake.contains(&format!("pub fn {required}")),
            "reddb-wire should own handshake frame builder {required}"
        );
    }
}

#[test]
fn server_redwire_test_request_frames_use_reddb_wire_builders() {
    let root = repo_root();
    let files = [
        "crates/reddb-server/src/wire/redwire/session.rs",
        "crates/reddb-server/src/wire/redwire/session/session_bulk_stream_tests.rs",
    ];

    for file in files {
        let text = read(root.join(file));
        assert!(
            !text.contains("Frame::new("),
            "{file} should build RedWire test frames through reddb-wire builders"
        );
    }

    let session = read(root.join("crates/reddb-server/src/wire/redwire/session.rs"));
    let bulk_stream = read(
        root.join("crates/reddb-server/src/wire/redwire/session/session_bulk_stream_tests.rs"),
    );

    assert!(
        session.contains("build_bulk_insert_frame"),
        "session RedWire tests should use reddb-wire bulk insert builders"
    );
    for required in [
        "build_request_frame",
        "build_client_hello_frame",
        "build_auth_response_frame",
    ] {
        assert!(
            bulk_stream.contains(required),
            "bulk stream RedWire tests should use reddb-wire {required}"
        );
    }
}

#[test]
fn server_and_client_do_not_construct_redwire_frames_inline() {
    let root = repo_root();
    for source_root in [
        root.join("crates/reddb-server/src"),
        root.join("crates/reddb-client/src"),
    ] {
        for file in rust_files_under(&source_root) {
            let text = read(&file);
            for forbidden in ["Frame::new(", "FrameBuilder::"] {
                assert!(
                    !text.contains(forbidden),
                    "{} should delegate RedWire frame construction to reddb-wire helpers; found {forbidden:?}",
                    file.display()
                );
            }
        }
    }
}

#[test]
fn redwire_startup_preface_lives_in_reddb_wire() {
    let root = repo_root();
    let client = read(root.join("crates/reddb-client/src/redwire/mod.rs"));
    let listener = read(root.join("crates/reddb-server/src/wire/redwire/listener.rs"));
    let session = read(root.join("crates/reddb-server/src/wire/redwire/session.rs"));
    let wire = read(root.join("crates/reddb-wire/src/redwire/mod.rs"));

    for forbidden in [
        "pub const MAGIC",
        "pub const SUPPORTED_VERSION",
        "write_all(&[MAGIC, SUPPORTED_VERSION])",
        "write_all(&[reddb_wire::redwire::REDWIRE_MAGIC",
    ] {
        assert!(
            !client.contains(forbidden),
            "client must get startup preface bytes from reddb-wire, found {forbidden:?}"
        );
    }

    for forbidden in ["magic[0] != REDWIRE_MAGIC", "got 0x{:02x}"] {
        assert!(
            !listener.contains(forbidden),
            "server listener must validate startup magic through reddb-wire, found {forbidden:?}"
        );
    }

    assert!(
        !session.contains("minor > MAX_KNOWN_MINOR_VERSION"),
        "server session must validate startup minor version through reddb-wire"
    );
    assert!(
        !session.contains("MAX_KNOWN_MINOR_VERSION"),
        "server session must negotiate Hello minor versions through reddb-wire"
    );
    assert!(
        session.contains("choose_hello_minor_version"),
        "server session should call reddb_wire::redwire::choose_hello_minor_version"
    );

    for required in [
        "supported_client_preface",
        "validate_startup_magic",
        "validate_minor_version",
        "choose_hello_minor_version",
        "StartupError",
    ] {
        assert!(
            wire.contains(required),
            "reddb-wire should own startup preface contract {required}"
        );
    }
}

#[test]
fn client_redwire_bulk_binary_value_tags_come_from_reddb_wire() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-client/src/redwire/mod.rs"));

    for forbidden in [
        "TAG_I64", "TAG_F64", "TAG_TEXT", "TAG_BOOL", "TAG_NULL", "VAL_I64", "VAL_F64", "VAL_TEXT",
        "VAL_BOOL", "VAL_NULL",
    ] {
        assert!(
            !text.contains(forbidden),
            "RedWire binary value tags belong in reddb-wire, found {forbidden:?}"
        );
    }
    assert!(
        text.contains("reddb_wire::legacy::WireValue")
            && text.contains("encode_bulk_binary_payload"),
        "client BinaryValue should encode through reddb_wire::redwire::encode_bulk_binary_payload"
    );
}

#[test]
fn legacy_result_and_error_envelopes_live_in_reddb_wire() {
    let root = repo_root();
    let listener = read(root.join("crates/reddb-server/src/wire/listener.rs"));
    let query_direct = read(root.join("crates/reddb-server/src/wire/query_direct.rs"));
    let wire = read(root.join("crates/reddb-wire/src/legacy.rs"));
    let listener_non_test = non_test_source(&listener);
    let query_direct_non_test = non_test_source(&query_direct);

    for forbidden in [
        "write_frame_header(&mut resp",
        "write_frame_header(&mut resp, MSG_RESULT",
        "write_frame_header(&mut resp, MSG_ERROR",
        "encode_column_name",
        "buf.extend_from_slice(&(columns.len() as u16).to_le_bytes())",
        "buf.extend_from_slice(&(records.len() as u32).to_le_bytes())",
        "body[header_nrows_pos..header_nrows_pos + 4].copy_from_slice",
        "let body = [0u8, 0, 0, 0, 0, 0]",
        "body.push(VAL_",
    ] {
        assert!(
            !listener_non_test.contains(forbidden),
            "legacy result/error frame and result-set payload construction belongs in reddb-wire, found {forbidden:?}"
        );
        assert!(
            !query_direct_non_test.contains(forbidden),
            "query_direct legacy frame and result-set payload construction belongs in reddb-wire, found {forbidden:?}"
        );
    }

    for required in ["build_legacy_result_frame", "build_legacy_error_frame"] {
        assert!(
            listener.contains(required),
            "server legacy listener should delegate envelope construction through {required}"
        );
        if required == "build_legacy_result_frame" {
            assert!(
                query_direct.contains(required),
                "query_direct should delegate legacy result envelopes through {required}"
            );
        }
        assert!(
            wire.contains(&format!("pub fn {required}")),
            "reddb-wire should own legacy envelope builder {required}"
        );
    }
    assert!(
        listener.contains("encode_result_payload_header")
            && query_direct.contains("encode_result_payload_header"),
        "server legacy listener should delegate result-set payload headers through reddb-wire"
    );
    assert!(
        wire.contains("pub fn encode_result_payload_header"),
        "reddb-wire should own legacy result-set payload header construction"
    );
    assert!(
        query_direct.contains("set_result_payload_row_count"),
        "query_direct should patch result-set row counts through reddb-wire"
    );
    assert!(
        wire.contains("pub fn set_result_payload_row_count"),
        "reddb-wire should own legacy result-set row-count patching"
    );

    for required in [
        "build_legacy_bulk_ok_frame",
        "build_legacy_bulk_stream_ack_frame",
        "build_legacy_prepared_ok_frame",
        "build_legacy_cursor_ok_frame",
        "build_legacy_cursor_batch_frame",
    ] {
        assert!(
            listener.contains(required),
            "server legacy listener should delegate response envelopes through {required}"
        );
        assert!(
            wire.contains(&format!("pub fn {required}")),
            "reddb-wire should own legacy response envelope builder {required}"
        );
    }
}

#[test]
fn redwire_json_operation_payloads_live_in_reddb_wire() {
    let root = repo_root();
    let client = read(root.join("crates/reddb-client/src/redwire/mod.rs"));
    let server = read(root.join("crates/reddb-server/src/wire/redwire/session.rs"));

    for forbidden in [
        "obj.insert(\n            \"collection\"",
        "obj.insert(\"payload\"",
        "obj.insert(\"payloads\"",
        "obj.insert(\"id\"",
        "bulk_insert_result_from_json",
        "fn json_id_to_string",
        "serde_json::from_slice(raw.as_bytes())",
        "serde_json::from_slice(&resp.payload)",
        "String::from_utf8_lossy(&resp.payload)",
        "match resp.kind",
        "expected Result/Error",
        "expected BulkOk/Error",
        "expected DeleteOk/Error",
        "expected Pong",
    ] {
        assert!(
            !client.contains(forbidden),
            "client RedWire operation payload contracts belong in reddb-wire, found {forbidden:?}"
        );
    }
    for required in [
        "encode_insert_payload",
        "encode_bulk_insert_payload",
        "encode_key_payload",
        "decode_query_result_payload",
        "decode_get_result_payload",
        "decode_bulk_ok_payload",
        "decode_delete_ok_affected",
        "expect_result_or_error",
        "expect_bulk_ok_or_error",
        "expect_delete_ok_or_error",
        "expect_pong_reply",
    ] {
        assert!(
            client.contains(required),
            "client RedWire operations should route through {required}"
        );
    }

    for forbidden in [
        "obj.get(\"collection\")",
        "obj.get(\"payload\")",
        "obj.get(\"payloads\")",
        "obj.get(\"id\")",
        "obj.get(\"idempotency_key\")",
        "obj.get(\"batch\")",
        "obj.insert(\"ok\"",
        "obj.insert(\"statement\"",
        "out.insert(\"affected\"",
        "out.insert(\"ids\"",
        "out.insert(\"ok\"",
        "out.insert(\"found\"",
        "JsonValue::Array(ids)",
        "serde_json::to_vec",
    ] {
        assert!(
            !server.contains(forbidden),
            "server RedWire operation payload contracts belong in reddb-wire, found {forbidden:?}"
        );
    }
    for required in [
        "decode_insert_dispatch_payload",
        "decode_get_payload",
        "decode_delete_payload",
        "encode_query_result_summary_payload",
        "encode_bulk_ok_payload_from_json_id_literals",
        "encode_get_result_payload",
        "encode_delete_ok_payload",
    ] {
        assert!(
            server.contains(required),
            "server RedWire operations should route through {required}"
        );
    }
}

#[test]
fn redwire_binary_bulk_payload_lives_in_reddb_wire() {
    let root = repo_root();
    let client = read(root.join("crates/reddb-client/src/redwire/mod.rs"));
    let server = read(root.join("crates/reddb-server/src/wire/listener.rs"));

    for forbidden in [
        "payload.extend_from_slice(&(collection.len() as u16).to_le_bytes())",
        "payload.extend_from_slice(collection.as_bytes())",
        "payload.extend_from_slice(&(columns.len() as u16).to_le_bytes())",
        "payload.extend_from_slice(&(rows.len() as u32).to_le_bytes())",
        "pub(crate) fn encode(&self, buf: &mut Vec<u8>)",
    ] {
        assert!(
            !client.contains(forbidden),
            "client RedWire binary bulk payload contract belongs in reddb-wire, found {forbidden:?}"
        );
    }
    assert!(
        client.contains("encode_bulk_binary_payload"),
        "client binary bulk should route through reddb-wire"
    );

    for forbidden in [
        "binary bulk: missing collection length",
        "binary bulk: missing column count",
        "binary bulk: missing row count",
        "prevalidated: missing collection length",
        "prevalidated: missing column count",
        "prevalidated: missing row count",
    ] {
        assert!(
            !server.contains(forbidden),
            "server RedWire binary bulk payload contract belongs in reddb-wire, found {forbidden:?}"
        );
    }
    assert!(
        server.contains("decode_bulk_binary_payload"),
        "server binary bulk should route through reddb-wire"
    );
}

#[test]
fn redwire_json_bulk_payload_lives_in_reddb_wire() {
    let root = repo_root();
    let server = read(root.join("crates/reddb-server/src/wire/listener.rs"));

    for forbidden in [
        "bulk insert: payload too short",
        "bulk insert: missing collection length",
        "bulk insert: truncated collection name",
        "bulk insert: missing row count",
        "bulk insert: missing JSON length",
        "bulk insert: truncated JSON payload",
        "bulk insert: invalid JSON payload",
    ] {
        assert!(
            !server.contains(forbidden),
            "server RedWire JSON bulk payload contract belongs in reddb-wire, found {forbidden:?}"
        );
    }

    assert!(
        server.contains("decode_bulk_json_payload"),
        "server JSON bulk should route through reddb-wire"
    );
}

#[test]
fn redwire_bulk_ok_count_payload_lives_in_reddb_wire() {
    let root = repo_root();
    let client = read(root.join("crates/reddb-client/src/redwire/mod.rs"));
    let server = read(root.join("crates/reddb-server/src/wire/listener.rs"));

    for forbidden in [
        "BulkOk truncated: expected 8-byte count",
        "u64::from_le_bytes([\n                    resp.payload",
    ] {
        assert!(
            !client.contains(forbidden),
            "client RedWire BulkOk count payload contract belongs in reddb-wire, found {forbidden:?}"
        );
    }
    assert!(
        client.contains("decode_bulk_ok_count_payload"),
        "client binary BulkOk count should route through reddb-wire"
    );

    for forbidden in [
        "resp.extend_from_slice(&count.to_le_bytes())",
        "resp.extend_from_slice(&(count as u64).to_le_bytes())",
        "resp.extend_from_slice(&state.total_flushed.to_le_bytes())",
    ] {
        assert!(
            !server.contains(forbidden),
            "server RedWire BulkOk count payload contract belongs in reddb-wire, found {forbidden:?}"
        );
    }
    assert!(
        server.contains("encode_bulk_ok_count_payload")
            && server.contains("decode_bulk_ok_count_payload"),
        "server BulkOk count payloads should route through reddb-wire"
    );
}

#[test]
fn redwire_prepared_payloads_live_in_reddb_wire() {
    let root = repo_root();
    let server = read(root.join("crates/reddb-server/src/wire/listener.rs"));

    for forbidden in [
        "truncated prepare stmt_id",
        "truncated prepare sql_len",
        "truncated prepare sql",
        "invalid UTF-8 in prepare sql",
        "truncated execute stmt_id",
        "truncated execute nparams",
        "truncated deallocate stmt_id",
        "p.extend_from_slice(&stmt_id.to_le_bytes())",
        "p.extend_from_slice(&(binds.len() as u16).to_le_bytes())",
    ] {
        assert!(
            !server.contains(forbidden),
            "server RedWire prepared payload contract belongs in reddb-wire, found {forbidden:?}"
        );
    }

    for required in [
        "decode_prepare_payload",
        "decode_execute_prepared_payload",
        "decode_deallocate_payload",
        "encode_prepared_ok_payload",
    ] {
        assert!(
            server.contains(required),
            "server prepared handlers should route through {required}"
        );
    }
}

#[test]
fn redwire_cursor_payloads_live_in_reddb_wire() {
    let root = repo_root();
    let server = read(root.join("crates/reddb-server/src/wire/listener.rs"));

    for forbidden in [
        "truncated declare cursor_id",
        "truncated declare sql_len",
        "truncated declare sql",
        "invalid UTF-8 in declare sql",
        "truncated fetch cursor_id",
        "truncated fetch max_rows",
        "truncated close cursor_id",
        "p.extend_from_slice(&cursor_id.to_le_bytes())",
    ] {
        assert!(
            !server.contains(forbidden),
            "server RedWire cursor payload contract belongs in reddb-wire, found {forbidden:?}"
        );
    }

    for required in [
        "decode_declare_cursor_payload",
        "decode_fetch_payload",
        "decode_close_cursor_payload",
        "encode_cursor_ok_payload",
        "encode_cursor_batch_payload",
    ] {
        assert!(
            server.contains(required),
            "server cursor handlers should route through {required}"
        );
    }
}

#[test]
fn redwire_bulk_stream_payloads_live_in_reddb_wire() {
    let root = repo_root();
    let server = read(root.join("crates/reddb-server/src/wire/listener.rs"));

    for forbidden in [
        "stream start: missing collection length",
        "stream start: missing column count",
        "stream start: missing column name length",
        "stream rows: missing row count",
    ] {
        assert!(
            !server.contains(forbidden),
            "server RedWire bulk-stream payload contract belongs in reddb-wire, found {forbidden:?}"
        );
    }

    for required in [
        "decode_bulk_stream_start_payload",
        "decode_bulk_stream_rows_payload",
    ] {
        assert!(
            server.contains(required),
            "server bulk-stream payloads should route through {required}"
        );
    }
}

#[test]
fn causal_bookmark_wire_token_lives_in_reddb_wire() {
    let root = repo_root();
    let server = read(root.join("crates/reddb-server/src/replication/bookmark.rs"));
    let client = read(root.join("crates/reddb-client/src/bookmark_routing.rs"));
    let wire = read(root.join("crates/reddb-wire/src/replication/bookmark.rs"));

    for forbidden in [
        "pub struct CausalBookmark",
        "pub enum BookmarkDecodeError",
        "rbm1.",
        "from_str_radix",
        "InvalidPrefix",
        "InvalidLength",
        "InvalidHex",
    ] {
        assert!(
            !server.contains(forbidden),
            "causal bookmark wire token belongs in reddb-wire, found {forbidden:?}"
        );
    }
    for forbidden in ["pub struct BookmarkTarget", "pub commit_lsn: u64"] {
        assert!(
            !client.contains(forbidden),
            "client causal bookmark target should alias reddb-wire, found {forbidden:?}"
        );
    }

    assert!(
        server.contains("pub use reddb_wire::replication::{BookmarkDecodeError, CausalBookmark};"),
        "server bookmark module should be a compatibility reexport"
    );
    assert!(
        client.contains("pub type BookmarkTarget = CausalBookmark;"),
        "client BookmarkTarget should remain as a compatibility alias over reddb-wire"
    );
    for required in [
        "pub struct CausalBookmark",
        "pub enum BookmarkDecodeError",
        "rbm1.",
        "from_str_radix",
    ] {
        assert!(
            wire.contains(required),
            "reddb-wire should own causal bookmark token contract {required}"
        );
    }
}

#[test]
fn replication_change_record_payload_lives_in_reddb_wire() {
    let root = repo_root();
    let server = read(root.join("crates/reddb-server/src/replication/cdc.rs"));
    let wire = read(root.join("crates/reddb-wire/src/replication/change_record.rs"));

    for forbidden in [
        "pub struct ChangeRecord",
        "pub enum ChangeOperation",
        "entity_bytes_hex",
        "refresh_records_hex",
        "invalid replication operation",
        "hex::encode",
        "hex::decode",
        "crate::json::to_string",
    ] {
        assert!(
            !server.contains(forbidden),
            "replication ChangeRecord payload contract belongs in reddb-wire, found {forbidden:?}"
        );
    }
    for required in [
        "pub use reddb_wire::replication::{public_item_kind, ChangeOperation, ChangeRecord};",
        "pub fn change_record_from_entity(",
    ] {
        assert!(
            server.contains(required),
            "server CDC should reexport protocol types and keep only storage-specific builders"
        );
    }
    for required in [
        "pub struct ChangeRecord",
        "pub enum ChangeOperation",
        "entity_bytes_hex",
        "refresh_records_hex",
        "invalid replication operation",
        "pub fn public_item_kind",
    ] {
        assert!(
            wire.contains(required),
            "reddb-wire should own replication ChangeRecord payload contract {required}"
        );
    }
}

#[test]
fn replication_basebackup_payload_lives_in_reddb_wire() {
    let root = repo_root();
    let grpc = read(root.join("crates/reddb-server/src/grpc/service_impl.rs"));
    let replica = read(root.join("crates/reddb-server/src/replication/replica.rs"));
    let wire = read(root.join("crates/reddb-wire/src/replication/basebackup.rs"));

    for forbidden in [
        "\"basebackup_available\"",
        "\"basebackup_timeline\"",
        "\"basebackup_start_lsn\"",
        "\"basebackup_checkpoint_lsn\"",
        "\"basebackup_snapshot_bytes\"",
        "\"basebackup_snapshot_checksum\"",
        "\"basebackup_manifest_hex\"",
        "\"basebackup_chunks\"",
        "\"basebackup_chunk_ordinal\"",
        "\"basebackup_chunk_hex\"",
        "hex::encode",
    ] {
        assert!(
            !grpc.contains(forbidden),
            "replication basebackup wire payload belongs in reddb-wire, found {forbidden:?}"
        );
    }
    for required in [
        "reddb_wire::replication::BaseBackupChunk::new",
        "reddb_wire::replication::BaseBackupManifestChunk",
        "chunk.encode_json()",
    ] {
        assert!(
            grpc.contains(required),
            "gRPC replication snapshot should route through reddb-wire {required}"
        );
    }
    assert!(
        replica.contains("reddb_wire::replication::BaseBackupChunk"),
        "replica basebackup staging should consume the reddb-wire payload type"
    );
    for required in [
        "pub struct BaseBackupChunk",
        "pub struct BaseBackupManifestChunk",
        "basebackup_manifest_hex",
        "basebackup_chunk_hex",
        "pub fn encode_json",
        "pub fn decode_json",
    ] {
        assert!(
            wire.contains(required),
            "reddb-wire should own basebackup payload contract {required}"
        );
    }
}

#[test]
fn redwire_queue_wait_payload_and_frame_builders_live_in_reddb_wire() {
    let root = repo_root();
    let server = read(root.join("crates/reddb-server/src/wire/redwire/queue_wait.rs"));
    let wire = read(root.join("crates/reddb-wire/src/redwire/queue.rs"));

    for forbidden in [
        "FrameBuilder::reply_to",
        "kind(MessageKind::QueueEventPush)",
        "kind(MessageKind::QueueWaitTimeout)",
        "kind(MessageKind::StreamError)",
        "obj.insert(\"code\"",
        "obj.insert(\"message\"",
        "serde_json::to_vec",
        "pub fn build_event_push_payload",
        "build_event_push_payload_from_json_bytes",
        "build_queue_wait_timeout_payload(queue, wait_ms)",
    ] {
        assert!(
            !server.contains(forbidden),
            "RedWire queue-wait payload/frame contract belongs in reddb-wire, found {forbidden:?}"
        );
    }

    for required in [
        "build_queue_event_push_frame_from_json_bytes",
        "build_queue_wait_timeout_frame",
        "build_queue_wait_error_frame",
    ] {
        assert!(
            server.contains(required),
            "server queue_wait adapter should delegate to reddb-wire {required}"
        );
        assert!(
            wire.contains(required),
            "reddb-wire should own queue-wait frame builder {required}"
        );
    }
}

#[test]
fn redwire_stream_frame_builders_live_in_reddb_wire() {
    let root = repo_root();
    let output = read(root.join("crates/reddb-server/src/wire/redwire/output_stream.rs"));
    let input = read(root.join("crates/reddb-server/src/wire/redwire/input_stream.rs"));
    let session = read(root.join("crates/reddb-server/src/wire/redwire/session.rs"));
    let wire = read(root.join("crates/reddb-wire/src/redwire/stream.rs"));

    for (file, text) in [
        ("output_stream.rs", output.as_str()),
        ("input_stream.rs", input.as_str()),
    ] {
        for forbidden in [
            "FrameBuilder::reply_to",
            "kind(MessageKind::OpenAck)",
            "kind(MessageKind::StreamChunk)",
            "kind(MessageKind::StreamError)",
            "kind(MessageKind::StreamEnd)",
            "pub fn build_open_ack_payload",
            "pub fn build_stream_chunk_payload",
            "pub fn build_stream_error_payload",
            "pub fn build_stream_end_payload",
            "pub fn build_input_stream_end_payload",
            "pub fn build_input_stream_error_payload",
            "serde_json::to_vec",
        ] {
            assert!(
                !text.contains(forbidden),
                "{file} must delegate RedWire stream frame construction to reddb-wire, found {forbidden:?}"
            );
        }
    }

    for required in [
        "build_open_ack_frame",
        "build_stream_chunk_frame_from_json_bytes",
        "build_stream_error_frame",
        "build_stream_end_frame",
        "build_input_stream_error_frame",
        "build_input_stream_end_frame",
    ] {
        assert!(
            wire.contains(required),
            "reddb-wire should own stream frame builder {required}"
        );
    }

    assert!(output.contains("build_open_ack_frame"));
    assert!(output.contains("build_stream_chunk_frame_from_json_bytes"));
    assert!(input.contains("build_input_stream_error_frame"));
    assert!(input.contains("build_input_stream_end_frame"));
    assert!(
        !session.contains("kind(MessageKind::OpenAck)"),
        "session OpenAck frame construction should delegate to reddb-wire"
    );
    assert!(
        session.contains("os::build_open_ack_frame"),
        "session input stream OpenAck should route through reddb-wire"
    );
}

#[test]
fn redwire_generic_reply_frame_builders_live_in_reddb_wire() {
    let root = repo_root();
    let session = read(root.join("crates/reddb-server/src/wire/redwire/session.rs"));
    let session_non_test = non_test_source(&session);
    let wire = read(root.join("crates/reddb-wire/src/redwire/builder.rs"));

    for forbidden in [
        "fn build_error_frame_lossy",
        "fn build_dispatch_reply_frame",
        "fn rewrap_handler_response",
        "FrameBuilder::reply_to(correlation_id)",
        "kind(MessageKind::Error)",
        "Frame::new(",
        "raw_bytes[4]",
        "raw_bytes[5..]",
        "MessageKind::from_u8(kind_byte)",
        "fast-path handler returned a truncated frame",
    ] {
        assert!(
            !session_non_test.contains(forbidden),
            "generic RedWire reply/error frame construction belongs in reddb-wire, found {forbidden:?}"
        );
    }

    for required in [
        "build_reply_frame",
        "build_error_frame_lossy",
        "build_dispatch_reply_frame",
        "rewrap_length_prefixed_handler_response",
    ] {
        assert!(
            session.contains(required),
            "server session should delegate generic frame construction through {required}"
        );
        assert!(
            wire.contains(&format!("pub fn {required}")),
            "reddb-wire should own generic frame builder {required}"
        );
    }
}

#[test]
fn redwire_auth_payload_and_scram_messages_live_in_reddb_wire() {
    let root = repo_root();
    let auth = read(root.join("crates/reddb-server/src/wire/redwire/auth.rs"));
    let session = read(root.join("crates/reddb-server/src/wire/redwire/session.rs"));
    let wire = read(root.join("crates/reddb-wire/src/redwire/handshake.rs"));

    for forbidden in [
        "fn parse_bearer_response",
        "obj.get(\"token\")",
        "JsonValue::String(base64_std(server_signature))",
        "client-first must start with 'n,,'",
        "missing p=<proof>",
        "channel binding must be 'biws'",
        "const B64_ALPHA",
    ] {
        assert!(
            !auth.contains(forbidden),
            "server auth must not own RedWire auth/SCRAM payload contract, found {forbidden:?}"
        );
    }
    for forbidden in [
        "FrameBuilder::reply_to",
        "kind(MessageKind::HelloAck)",
        "kind(MessageKind::AuthOk)",
        "kind(MessageKind::AuthFail)",
        "!= MessageKind::AuthResponse",
        "crate::serde_json::from_slice::<JsonValue>(&resp.payload)",
        "o.get(\"jwt\")",
    ] {
        assert!(
            !session.contains(forbidden),
            "server session must not own RedWire handshake frame/payload contract, found {forbidden:?}"
        );
    }

    for required in [
        "parse_auth_response_bearer_token",
        "build_scram_auth_ok_payload",
        "base64_std(&crate::auth::store::random_bytes(18))",
    ] {
        assert!(
            auth.contains(required),
            "server auth should adapt runtime policy through reddb-wire {required}"
        );
    }
    for required in ["build_hello_ack_frame", "build_auth_ok_frame_from_payload"] {
        assert!(
            session.contains(required),
            "server session handshake should delegate frame construction through {required}"
        );
        assert!(
            wire.contains(&format!("pub fn {required}")),
            "reddb-wire should own handshake frame builder {required}"
        );
    }
    assert!(
        session.contains("expect_auth_response_payload"),
        "server session AuthResponse kind checks should route through reddb-wire"
    );
    assert!(
        wire.contains("pub fn expect_auth_response_payload"),
        "reddb-wire should own AuthResponse kind expectation"
    );
    assert!(
        session.contains("parse_auth_response_oauth_jwt"),
        "server session OAuth AuthResponse parsing should route through reddb-wire"
    );
    assert!(
        wire.contains("pub fn parse_auth_response_oauth_jwt"),
        "reddb-wire should own OAuth AuthResponse payload parsing"
    );

    for required in [
        "parse_scram_client_first",
        "build_scram_server_first",
        "parse_scram_client_final",
    ] {
        assert!(
            session.contains(&format!("reddb_wire::redwire::handshake::{required}")),
            "server RedWire session should call reddb-wire SCRAM message helper {required}"
        );
        assert!(
            wire.contains(&format!("pub fn {required}")),
            "reddb-wire should own SCRAM message helper {required}"
        );
    }
}

/// Issue #1055: the WebSocket edge's protocol-visible contracts — the WS
/// subprotocol/path constants (item 2), the pure upgrade-gate decision
/// (item 3), and the `0xFE` magic detector (item 4) — must be owned by
/// reddb-wire, with the server crate delegating to them.
#[test]
fn redwire_ws_edge_contracts_live_in_reddb_wire() {
    let root = repo_root();
    let wire_mod = read(root.join("crates/reddb-wire/src/redwire/mod.rs"));
    let wire_gate = read(root.join("crates/reddb-wire/src/redwire/ws_gate.rs"));
    let ws_edge = read(root.join("crates/reddb-server/src/server/ws_edge.rs"));
    let detector = read(root.join("crates/reddb-server/src/service_router/detector.rs"));

    // ---- Item 2: WS subprotocol/path constants live in reddb-wire ----
    for required in ["REDWIRE_WS_SUBPROTOCOL", "REDWIRE_WS_PATH"] {
        assert!(
            wire_mod.contains(&format!("pub const {required}")),
            "reddb-wire should own WS edge constant {required}"
        );
    }
    // The subprotocol token is the authority's single source of truth: the
    // literal must appear only in reddb-wire, never inline in server src.
    for file in rust_files_under(root.join("crates/reddb-server/src")) {
        let text = read(&file);
        assert!(
            !non_test_source(&text).contains("\"reddb.redwire.v1\""),
            "{} must import REDWIRE_WS_SUBPROTOCOL from reddb-wire, not inline the token",
            file.display()
        );
    }
    assert!(
        ws_edge.contains("use reddb_wire::redwire::{REDWIRE_WS_PATH, REDWIRE_WS_SUBPROTOCOL}"),
        "ws_edge should source the WS constants from reddb-wire"
    );

    // ---- Item 3: pure WS upgrade-gate decision lives in reddb-wire ----
    for required in ["pub fn evaluate_ws_upgrade", "pub enum WsUpgradeRefusal"] {
        assert!(
            wire_gate.contains(required),
            "reddb-wire should own WS upgrade-gate contract {required}"
        );
    }
    assert!(
        ws_edge.contains("reddb_wire::redwire::evaluate_ws_upgrade"),
        "ws_edge gate must delegate to reddb_wire::redwire::evaluate_ws_upgrade"
    );
    assert!(
        !non_test_source(&ws_edge).contains("allowlist.iter().any(|allowed| allowed == o)"),
        "ws_edge must not re-implement the origin allowlist match inline"
    );

    // ---- Item 4: 0xFE magic detector lives in reddb-wire ----
    for required in ["pub fn redwire_magic_match", "pub enum RedWireMagicMatch"] {
        assert!(
            wire_mod.contains(required),
            "reddb-wire should own RedWire magic detector {required}"
        );
    }
    assert!(
        detector.contains("reddb_wire::redwire::redwire_magic_match"),
        "service_router RedWireDetector must delegate to reddb_wire::redwire::redwire_magic_match"
    );
    assert!(
        !non_test_source(&detector).contains("peek[0] == 0xFE"),
        "service_router must not hard-code the 0xFE magic match inline"
    );
}
