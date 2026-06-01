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
use reddb_wire::redwire::frame::{Frame, MessageKind};

use super::FrameBuilder;

/// Parsed `QueueWaitOpen` payload. Shape:
///
/// ```json
/// { "queue": "jobs", "group": "g?", "consumer": "w1",
///   "count": 1, "wait_ms": 5000 }
/// ```
///
/// `group` is optional (the runtime resolves the default work / fanout
/// group when absent, matching the SQL `QUEUE READ` path). `count`
/// defaults to 1 and `wait_ms` to 0 (a single re-probe of current
/// state) when omitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueWaitOpenRequest {
    pub queue: String,
    pub group: Option<String>,
    pub consumer: String,
    pub count: usize,
    pub wait_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueueWaitParseError {
    NotJson,
    NotObject,
    MissingQueue,
    MissingConsumer,
}

impl QueueWaitParseError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotJson | Self::NotObject => "queue_wait_invalid_payload",
            Self::MissingQueue => "queue_wait_missing_queue",
            Self::MissingConsumer => "queue_wait_missing_consumer",
        }
    }
    pub fn message(&self) -> &'static str {
        match self {
            Self::NotJson => "QueueWaitOpen payload must be JSON",
            Self::NotObject => "QueueWaitOpen payload must be a JSON object",
            Self::MissingQueue => "QueueWaitOpen payload missing 'queue' string field",
            Self::MissingConsumer => "QueueWaitOpen payload missing 'consumer' string field",
        }
    }
}

pub fn parse_queue_wait_open(payload: &[u8]) -> Result<QueueWaitOpenRequest, QueueWaitParseError> {
    let v: JsonValue = serde_json::from_slice(payload).map_err(|_| QueueWaitParseError::NotJson)?;
    let obj = v.as_object().ok_or(QueueWaitParseError::NotObject)?;
    let queue = obj
        .get("queue")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .ok_or(QueueWaitParseError::MissingQueue)?
        .to_string();
    let consumer = obj
        .get("consumer")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .ok_or(QueueWaitParseError::MissingConsumer)?
        .to_string();
    let group = obj
        .get("group")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string());
    // `count` defaults to 1; clamp to at least 1 so a wait always asks
    // for a deliverable message.
    let count = obj
        .get("count")
        .and_then(|x| x.as_f64())
        .map(|n| (n as usize).max(1))
        .unwrap_or(1);
    let wait_ms = obj
        .get("wait_ms")
        .and_then(|x| x.as_f64())
        .map(|n| n.max(0.0) as u64)
        .unwrap_or(0);
    Ok(QueueWaitOpenRequest {
        queue,
        group,
        consumer,
        count,
        wait_ms,
    })
}

/// Build the `QueueEventPush` payload for one delivered message. The
/// `message` value is the JSON object rendered by the runtime
/// (`message_id` / `payload` / `consumer` / `delivery_count`).
pub fn build_event_push_payload(message: &JsonValue) -> Vec<u8> {
    serde_json::to_vec(message).unwrap_or_default()
}

/// Build a `QueueEventPush` frame echoing the open request's
/// `correlation_id` and `stream_id` so the client pairs the push with
/// the wait it opened.
pub fn build_event_push_frame(
    correlation_id: u64,
    stream_id: u16,
    message: &JsonValue,
) -> Result<Frame, super::BuildError> {
    FrameBuilder::reply_to(correlation_id)
        .kind(MessageKind::QueueEventPush)
        .stream_id(stream_id)
        .payload(build_event_push_payload(message))
        .build()
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
    let mut obj = serde_json::Map::new();
    obj.insert("code".to_string(), JsonValue::String(code.to_string()));
    obj.insert(
        "message".to_string(),
        JsonValue::String(message.to_string()),
    );
    FrameBuilder::reply_to(correlation_id)
        .kind(MessageKind::StreamError)
        .stream_id(stream_id)
        .payload(serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default())
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
