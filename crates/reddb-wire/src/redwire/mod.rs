//! RedWire — RedDB's binary TCP/TLS wire protocol.
//!
//! ADR 0001 (`.red/adr/0001-redwire-tcp-protocol.md`) is the
//! normative spec. This module owns the frame layout, message-kind
//! discriminator, flags, encode/decode codec, and generic async
//! frame I/O over byte streams. Server-side dispatch, auth policy,
//! session loop, and listener accept stay in `reddb` and depend on
//! these types.

pub mod builder;
pub mod bulk_binary;
pub mod bulk_json;
pub mod bulk_stream;
pub mod codec;
pub mod cursor;
pub mod frame;
pub mod handshake;
pub mod io;
pub mod operations;
pub mod prepared;
pub mod queue;
pub mod stream;

pub use builder::{
    build_bulk_insert_binary_frame, build_bulk_insert_frame, build_bye_frame, build_delete_frame,
    build_dispatch_reply_frame, build_error_frame, build_error_frame_lossy, build_get_frame,
    build_ping_frame, build_query_frame, build_query_with_params_frame, build_reply_frame,
    build_request_frame, BuildError, FrameBuilder,
};
pub use bulk_binary::{
    decode_bulk_binary_payload, encode_bulk_binary_payload, BulkBinaryError, BulkBinaryFlavor,
    BulkBinaryPayload,
};
pub use bulk_json::{
    decode_bulk_json_payload, encode_bulk_json_payload, BulkJsonError, BulkJsonPayload,
};
pub use bulk_stream::{
    decode_bulk_stream_rows_payload, decode_bulk_stream_start_payload,
    encode_bulk_stream_rows_payload, encode_bulk_stream_start_payload, BulkStreamError,
    BulkStreamRowsPayload, BulkStreamStartPayload,
};
pub use codec::{
    decode_frame, decode_frame_parts, encode_frame, frame_len_from_header, FrameError,
};
pub use cursor::{
    decode_close_cursor_payload, decode_declare_cursor_payload, decode_fetch_payload,
    encode_close_cursor_payload, encode_cursor_batch_payload, encode_cursor_ok_payload,
    encode_declare_cursor_payload, encode_fetch_payload, CloseCursorPayload, CursorPayloadError,
    DeclareCursorPayload, FetchPayload,
};
pub use frame::{
    Flags, Frame, MessageClass, MessageDirection, MessageKind, FRAME_HEADER_SIZE, MAX_FRAME_SIZE,
};
pub use handshake::{
    build_auth_fail_frame, build_auth_fail_payload, build_auth_ok_frame_from_payload,
    build_auth_ok_payload, build_auth_response_anonymous_payload,
    build_auth_response_bearer_payload, build_auth_response_frame,
    build_auth_response_oauth_jwt_payload, build_client_hello_frame, build_client_hello_payload,
    build_hello_ack, build_hello_ack_frame, build_hello_payload, choose_hello_minor_version,
    AuthFail, AuthOk, Hello, HelloAck, SUPPORTED_METHODS,
};
pub use io::{read_frame_async, write_frame_async, RedWireIoError};
pub use operations::{
    decode_bulk_ok_count_payload, decode_bulk_ok_payload, decode_delete_ok_affected,
    decode_delete_payload, decode_error_payload, decode_get_payload, decode_get_result_payload,
    decode_insert_dispatch_payload, decode_query_result_payload, decode_text_payload,
    encode_bulk_insert_payload, encode_bulk_ok_count_payload, encode_bulk_ok_payload,
    encode_bulk_ok_payload_from_json_id_literals, encode_bulk_ok_payload_from_json_ids_bytes,
    encode_delete_ok_payload, encode_get_result_payload, encode_insert_payload, encode_key_payload,
    encode_query_result_summary_payload, expect_bulk_ok_or_error, expect_delete_ok_or_error,
    expect_pong_reply, expect_result_or_error, BulkOkPayload, InsertDispatchPayload, KeyPayload,
    OperationPayloadError, OperationReplyError,
};
pub use prepared::{
    decode_deallocate_payload, decode_execute_prepared_payload, decode_prepare_payload,
    encode_deallocate_payload, encode_execute_prepared_payload, encode_prepare_payload,
    encode_prepared_ok_payload, DeallocatePayload, ExecutePreparedPayload, PreparePayload,
    PreparedOkPayload, PreparedPayloadError,
};
pub use queue::{
    build_event_push_payload, build_event_push_payload_from_json_bytes,
    build_queue_event_push_frame_from_json_bytes, build_queue_wait_error_frame,
    build_queue_wait_error_payload, build_queue_wait_timeout_frame,
    build_queue_wait_timeout_payload, parse_queue_wait_open, QueueWaitOpenRequest,
    QueueWaitParseError, WAIT_CANCELLED_CODE, WAIT_EXCEEDS_CAP_CODE, WAIT_FAILED_CODE,
};
pub use stream::{
    build_input_stream_end_frame, build_input_stream_end_payload, build_input_stream_error_frame,
    build_input_stream_error_payload, build_open_ack_frame, build_open_ack_payload,
    build_stream_chunk_frame_from_json_bytes, build_stream_chunk_payload,
    build_stream_chunk_payload_from_json_bytes, build_stream_end_frame, build_stream_end_payload,
    build_stream_error_frame, build_stream_error_payload, open_stream_is_input, parse_input_chunk,
    parse_input_chunk_json, parse_open_input, parse_open_stream, parse_stream_cancel,
    ChunkParseError, InputChunk, InputChunkJson, OpenInputParseError, OpenInputRequest,
    OpenStreamParseError, OpenStreamRequest, StreamCancelRequest,
};

/// Discriminator byte every RedWire client sends as the very first
/// byte off the wire. The service-router detector keys off this
/// (and so does the standalone listener path).
pub const REDWIRE_MAGIC: u8 = 0xFE;

/// Highest minor version the server supports. Wire-bumped as we
/// add features that change the handshake; data-plane additions
/// flow through `Hello.features` instead.
pub const MAX_KNOWN_MINOR_VERSION: u8 = 0x01;

/// Default port for the RedWire listener.
pub const DEFAULT_REDWIRE_PORT: u16 = 5050;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupError {
    BadMagic { got: u8 },
    UnsupportedMinor { got: u8, max: u8 },
}

impl std::fmt::Display for StartupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadMagic { got } => {
                write!(
                    f,
                    "redwire: client did not present magic byte (got 0x{got:02x})"
                )
            }
            Self::UnsupportedMinor { got, max } => {
                write!(
                    f,
                    "redwire: unsupported minor version {got}; max supported is {max}"
                )
            }
        }
    }
}

impl std::error::Error for StartupError {}

pub fn client_preface(minor: u8) -> [u8; 2] {
    [REDWIRE_MAGIC, minor]
}

pub fn supported_client_preface() -> [u8; 2] {
    client_preface(MAX_KNOWN_MINOR_VERSION)
}

pub fn validate_startup_magic(got: u8) -> Result<(), StartupError> {
    if got == REDWIRE_MAGIC {
        Ok(())
    } else {
        Err(StartupError::BadMagic { got })
    }
}

pub fn validate_minor_version(got: u8) -> Result<(), StartupError> {
    if got <= MAX_KNOWN_MINOR_VERSION {
        Ok(())
    } else {
        Err(StartupError::UnsupportedMinor {
            got,
            max: MAX_KNOWN_MINOR_VERSION,
        })
    }
}

#[cfg(test)]
mod startup_tests {
    use super::*;

    #[test]
    fn preface_uses_magic_and_supported_minor() {
        assert_eq!(supported_client_preface(), [0xfe, MAX_KNOWN_MINOR_VERSION]);
    }

    #[test]
    fn startup_validation_rejects_bad_magic_and_future_minor() {
        assert_eq!(validate_startup_magic(REDWIRE_MAGIC), Ok(()));
        assert!(matches!(
            validate_startup_magic(0),
            Err(StartupError::BadMagic { got: 0 })
        ));
        assert_eq!(validate_minor_version(MAX_KNOWN_MINOR_VERSION), Ok(()));
        assert!(matches!(
            validate_minor_version(MAX_KNOWN_MINOR_VERSION.saturating_add(1)),
            Err(StartupError::UnsupportedMinor { .. })
        ));
    }
}
