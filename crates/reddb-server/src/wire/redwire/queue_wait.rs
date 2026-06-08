//! RedWire live queue-wait dispatch (issue #917, PRD #915).
//!
//! Carries the wire-side envelopes for the live queue-wait happy path:
//!   - `QueueWaitOpen`  (client→server) — open a wait on a queue. The
//!     awaiting session parks on the queue-wait registry's async wake
//!     head (no blocking OS thread) and re-probes the normal delivery
//!     path on each wake.
//!   - `QueueEventPush` (server→client) — the delivered message,
//!     pushed the instant one becomes deliverable on that queue.
//!
//! Distinct from the `OpenStream`/`StreamChunk` output-stream family in
//! [`super::output_stream`], which stays query-result pull. These
//! envelopes carry queue delivery and reuse the frame's `stream_id` for
//! multiplexing so a wait can coexist with other streams on the same
//! connection.

use crate::serde_json::{self, Value as JsonValue};
pub use reddb_wire::redwire::queue::{
    QueueWaitOpenRequest, QueueWaitParseError, WAIT_CANCELLED_CODE, WAIT_EXCEEDS_CAP_CODE,
    WAIT_FAILED_CODE,
};
use reddb_wire::redwire::Frame;

pub fn parse_queue_wait_open(payload: &[u8]) -> Result<QueueWaitOpenRequest, QueueWaitParseError> {
    reddb_wire::redwire::queue::parse_queue_wait_open(payload)
}

/// Build the `QueueEventPush` payload for one delivered message. The
/// `message` value is the JSON object rendered by the runtime
/// (`message_id` / `payload` / `consumer` / `delivery_count`).
pub fn build_event_push_payload(message: &JsonValue) -> Vec<u8> {
    let bytes = serde_json::to_vec(message).unwrap_or_default();
    reddb_wire::redwire::queue::build_event_push_payload_from_json_bytes(&bytes)
}

/// Build a `QueueEventPush` frame echoing the open request's
/// `correlation_id` and `stream_id` so the client pairs the push with
/// the wait it opened.
pub fn build_event_push_frame(
    correlation_id: u64,
    stream_id: u16,
    message: &JsonValue,
) -> Result<Frame, super::BuildError> {
    let bytes = serde_json::to_vec(message).unwrap_or_default();
    reddb_wire::redwire::queue::build_queue_event_push_frame_from_json_bytes(
        correlation_id,
        stream_id,
        &bytes,
    )
}

/// Build a `QueueWaitTimeout` frame for an elapsed wait (issue #919).
///
/// A distinct frame kind — not a `QueueEventPush` (which always carries
/// a delivered message) and not a `StreamError` (reserved for parse
/// failures, cancellation, and runtime errors) — so the client can tell
/// "your wait budget elapsed with nothing deliverable" apart from a
/// delivery and apart from a cancellation purely from the frame kind.
/// Echoes the open's `correlation_id` + `stream_id` so the client pairs
/// the timeout with the wait it opened; the small JSON body restates the
/// queue and the budget that elapsed for client-side logging.
pub fn build_queue_wait_timeout_frame(
    correlation_id: u64,
    stream_id: u16,
    queue: &str,
    wait_ms: u64,
) -> Result<Frame, super::BuildError> {
    reddb_wire::redwire::queue::build_queue_wait_timeout_frame(
        correlation_id,
        stream_id,
        queue,
        wait_ms,
    )
}

/// Build a `StreamError` frame carrying a queue-wait parse/validation
/// failure for a specific `stream_id`. Non-fatal at the connection
/// level — the session keeps reading other frames.
pub fn build_queue_wait_error_frame(
    correlation_id: u64,
    stream_id: u16,
    code: &str,
    message: &str,
) -> Result<Frame, super::BuildError> {
    reddb_wire::redwire::queue::build_queue_wait_error_frame(
        correlation_id,
        stream_id,
        code,
        message,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use reddb_wire::redwire::MessageKind;

    #[test]
    fn parse_minimal_request_applies_defaults() {
        let req = parse_queue_wait_open(br#"{"queue":"jobs","consumer":"w1"}"#).unwrap();
        assert_eq!(req.queue, "jobs");
        assert_eq!(req.consumer, "w1");
        assert_eq!(req.group, None);
        assert_eq!(req.count, 1);
        assert_eq!(req.wait_ms, 0);
    }

    #[test]
    fn parse_full_request() {
        let req = parse_queue_wait_open(
            br#"{"queue":"jobs","group":"g","consumer":"w1","count":3,"wait_ms":5000}"#,
        )
        .unwrap();
        assert_eq!(req.group.as_deref(), Some("g"));
        assert_eq!(req.count, 3);
        assert_eq!(req.wait_ms, 5000);
    }

    #[test]
    fn parse_rejects_missing_queue_and_consumer() {
        assert_eq!(
            parse_queue_wait_open(br#"{"consumer":"w1"}"#).unwrap_err(),
            QueueWaitParseError::MissingQueue
        );
        assert_eq!(
            parse_queue_wait_open(br#"{"queue":"jobs"}"#).unwrap_err(),
            QueueWaitParseError::MissingConsumer
        );
    }

    #[test]
    fn parse_rejects_non_json() {
        assert_eq!(
            parse_queue_wait_open(b"not json").unwrap_err(),
            QueueWaitParseError::NotJson
        );
    }

    #[test]
    fn event_push_frame_echoes_correlation_and_stream() {
        let mut obj = serde_json::Map::new();
        obj.insert("message_id".to_string(), JsonValue::String("42".into()));
        let frame = build_event_push_frame(99, 7, &JsonValue::Object(obj)).unwrap();
        assert_eq!(frame.kind, MessageKind::QueueEventPush);
        assert_eq!(frame.correlation_id, 99);
        assert_eq!(frame.stream_id, 7);
    }

    #[test]
    fn timeout_frame_is_distinct_kind_echoing_open() {
        let frame = build_queue_wait_timeout_frame(99, 7, "jobs", 5000).unwrap();
        // Distinct kind — not QueueEventPush (delivery) or StreamError
        // (cancellation / failure) — so the outcome is unambiguous on
        // the wire (AC #1, AC #2).
        assert_eq!(frame.kind, MessageKind::QueueWaitTimeout);
        assert_ne!(frame.kind, MessageKind::QueueEventPush);
        assert_ne!(frame.kind, MessageKind::StreamError);
        assert_eq!(frame.correlation_id, 99, "echoes the open correlation");
        assert_eq!(frame.stream_id, 7, "echoes the open stream_id");
        let body: JsonValue = serde_json::from_slice(&frame.payload).unwrap();
        assert_eq!(body["outcome"], JsonValue::String("timeout".into()));
        assert_eq!(body["queue"], JsonValue::String("jobs".into()));
        assert_eq!(body["wait_ms"], JsonValue::Number(5000.0));
    }

    #[test]
    fn cancellation_and_cap_codes_are_distinct() {
        // The three non-delivery StreamError-bearing outcomes must not
        // alias one another (AC #2 distinguishability extends to the
        // error codes the client switches on).
        assert_ne!(WAIT_CANCELLED_CODE, WAIT_FAILED_CODE);
        assert_ne!(WAIT_CANCELLED_CODE, WAIT_EXCEEDS_CAP_CODE);
        assert_ne!(WAIT_EXCEEDS_CAP_CODE, WAIT_FAILED_CODE);
    }
}
