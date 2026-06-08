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
fn server_redwire_frame_header_length_routes_through_reddb_wire() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-server/src/wire/redwire/session.rs"));

    for forbidden in [
        "u32::from_le_bytes([buf[0]",
        "u32::from_le_bytes([header[0]",
        "length < FRAME_HEADER_SIZE",
        "length > MAX_FRAME_SIZE",
    ] {
        assert!(
            !text.contains(forbidden),
            "server RedWire frame length validation should route through reddb-wire, found {forbidden:?}"
        );
    }
    assert!(
        text.contains("frame_len_from_header"),
        "server RedWire session should call reddb_wire::redwire::frame_len_from_header"
    );
    assert!(
        text.contains("decode_frame_parts"),
        "server RedWire session should assemble split header/payload reads through reddb_wire::redwire::decode_frame_parts"
    );
}

#[test]
fn client_connector_redwire_is_compatibility_adapter_only() {
    let root = repo_root();
    let text = read(root.join("crates/reddb-client/src/connector/redwire.rs"));

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
}

#[test]
fn client_redwire_has_single_frame_io_adapter() {
    let root = repo_root();
    let client = read(root.join("crates/reddb-client/src/redwire/mod.rs"));
    let handshake = read(root.join("crates/reddb-client/src/redwire/handshake.rs"));
    let io = read(root.join("crates/reddb-client/src/redwire/io.rs"));

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
        "u32::from_le_bytes([header[0]",
    ] {
        assert!(
            !client.contains(forbidden),
            "client redwire module should use its single frame I/O adapter, found {forbidden:?}"
        );
    }

    for required in [
        "frame_len_from_header",
        "decode_frame_parts",
        "encode_frame",
        "pub(super) async fn read_frame",
        "pub(super) async fn write_frame",
    ] {
        assert!(
            io.contains(required),
            "client frame I/O adapter should route through reddb-wire {required}"
        );
    }

    assert!(
        handshake.contains("build_client_hello_payload"),
        "client handshake should let reddb-wire choose advertised Hello minor versions"
    );
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

    for forbidden in [
        "write_frame_header(&mut resp",
        "write_frame_header(&mut resp, MSG_RESULT",
        "write_frame_header(&mut resp, MSG_ERROR",
    ] {
        assert!(
            !listener.contains(forbidden),
            "legacy result/error envelope construction belongs in reddb-wire, found {forbidden:?}"
        );
        assert!(
            !query_direct.contains(forbidden),
            "query_direct legacy envelope construction belongs in reddb-wire, found {forbidden:?}"
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
        "decode_bulk_ok_payload",
        "decode_delete_ok_affected",
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
        "encode_bulk_ok_payload_from_json_ids_bytes",
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
    let wire = read(root.join("crates/reddb-wire/src/redwire/builder.rs"));

    for forbidden in [
        "fn build_error_frame_lossy",
        "fn build_dispatch_reply_frame",
        "FrameBuilder::reply_to(correlation_id)",
        "kind(MessageKind::Error)",
    ] {
        assert!(
            !session.contains(forbidden),
            "generic RedWire reply/error frame construction belongs in reddb-wire, found {forbidden:?}"
        );
    }

    for required in [
        "build_reply_frame",
        "build_error_frame_lossy",
        "build_dispatch_reply_frame",
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
    ] {
        assert!(
            !session.contains(forbidden),
            "server session must not own RedWire handshake frame construction, found {forbidden:?}"
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
