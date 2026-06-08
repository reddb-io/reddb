//! RedWire live queue-wait payload contracts.

use serde_json::Value as JsonValue;

use super::{BuildError, Frame, FrameBuilder, MessageKind};

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
    let count = obj
        .get("count")
        .and_then(|x| x.as_u64())
        .map(|n| (n as usize).max(1))
        .unwrap_or(1);
    let wait_ms = obj.get("wait_ms").and_then(|x| x.as_u64()).unwrap_or(0);
    Ok(QueueWaitOpenRequest {
        queue,
        group,
        consumer,
        count,
        wait_ms,
    })
}

pub fn build_event_push_payload(message: &JsonValue) -> Vec<u8> {
    serde_json::to_vec(message).unwrap_or_default()
}

pub fn build_event_push_payload_from_json_bytes(message: &[u8]) -> Vec<u8> {
    let value = serde_json::from_slice(message).unwrap_or(JsonValue::Null);
    build_event_push_payload(&value)
}

pub fn build_queue_event_push_frame_from_json_bytes(
    correlation_id: u64,
    stream_id: u16,
    message: &[u8],
) -> Result<Frame, BuildError> {
    FrameBuilder::reply_to(correlation_id)
        .kind(MessageKind::QueueEventPush)
        .stream_id(stream_id)
        .payload(build_event_push_payload_from_json_bytes(message))
        .build()
}

pub fn build_queue_wait_timeout_payload(queue: &str, wait_ms: u64) -> Vec<u8> {
    let mut obj = serde_json::Map::new();
    obj.insert(
        "outcome".to_string(),
        JsonValue::String("timeout".to_string()),
    );
    obj.insert("queue".to_string(), JsonValue::String(queue.to_string()));
    obj.insert("wait_ms".to_string(), JsonValue::Number(wait_ms.into()));
    serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
}

pub fn build_queue_wait_timeout_frame(
    correlation_id: u64,
    stream_id: u16,
    queue: &str,
    wait_ms: u64,
) -> Result<Frame, BuildError> {
    FrameBuilder::reply_to(correlation_id)
        .kind(MessageKind::QueueWaitTimeout)
        .stream_id(stream_id)
        .payload(build_queue_wait_timeout_payload(queue, wait_ms))
        .build()
}

pub fn build_queue_wait_error_payload(code: &str, message: &str) -> Vec<u8> {
    let mut obj = serde_json::Map::new();
    obj.insert("code".to_string(), JsonValue::String(code.to_string()));
    obj.insert(
        "message".to_string(),
        JsonValue::String(message.to_string()),
    );
    serde_json::to_vec(&JsonValue::Object(obj)).unwrap_or_default()
}

pub fn build_queue_wait_error_frame(
    correlation_id: u64,
    stream_id: u16,
    code: &str,
    message: &str,
) -> Result<Frame, BuildError> {
    FrameBuilder::reply_to(correlation_id)
        .kind(MessageKind::StreamError)
        .stream_id(stream_id)
        .payload(build_queue_wait_error_payload(code, message))
        .build()
}

pub const WAIT_CANCELLED_CODE: &str = "queue_wait_cancelled";
pub const WAIT_EXCEEDS_CAP_CODE: &str = "queue_wait_exceeds_cap";
pub const WAIT_FAILED_CODE: &str = "queue_wait_failed";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_wait_open_applies_defaults() {
        let req = parse_queue_wait_open(br#"{"queue":"jobs","consumer":"w1"}"#).unwrap();
        assert_eq!(req.queue, "jobs");
        assert_eq!(req.consumer, "w1");
        assert_eq!(req.group, None);
        assert_eq!(req.count, 1);
        assert_eq!(req.wait_ms, 0);
    }

    #[test]
    fn queue_wait_open_rejects_missing_required_fields() {
        assert_eq!(
            parse_queue_wait_open(br#"{"consumer":"w1"}"#),
            Err(QueueWaitParseError::MissingQueue)
        );
        assert_eq!(
            parse_queue_wait_open(br#"{"queue":"jobs"}"#),
            Err(QueueWaitParseError::MissingConsumer)
        );
    }

    #[test]
    fn timeout_payload_has_distinct_outcome() {
        let bytes = build_queue_wait_timeout_payload("jobs", 5000);
        let value: JsonValue = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["outcome"], "timeout");
        assert_eq!(value["queue"], "jobs");
        assert_eq!(value["wait_ms"], 5000);
    }

    #[test]
    fn queue_wait_frames_echo_open_stream() {
        let event =
            build_queue_event_push_frame_from_json_bytes(99, 7, br#"{"message_id":"42"}"#).unwrap();
        assert_eq!(event.kind, MessageKind::QueueEventPush);
        assert_eq!(event.correlation_id, 99);
        assert_eq!(event.stream_id, 7);

        let timeout = build_queue_wait_timeout_frame(99, 7, "jobs", 5000).unwrap();
        assert_eq!(timeout.kind, MessageKind::QueueWaitTimeout);
        assert_eq!(timeout.correlation_id, 99);
        assert_eq!(timeout.stream_id, 7);

        let error = build_queue_wait_error_frame(99, 7, WAIT_CANCELLED_CODE, "cancelled").unwrap();
        assert_eq!(error.kind, MessageKind::StreamError);
        assert_eq!(error.correlation_id, 99);
        assert_eq!(error.stream_id, 7);
    }
}
