//! Transport-neutral overload vocabulary.
//!
//! Human prose is deliberately not the contract here. Callers branch on the
//! stable `error_code`, while the remaining fields explain which bounded
//! resource refused the operation and expose a point-in-time admission
//! snapshot for operators.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::json::{Map, Value as JsonValue};

static INCIDENT_SEQUENCE: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OverloadReason {
    GlobalInflightExhausted,
    HealthReserveExhausted,
    ExecutionQueueFull,
    ExecutionQueueTimeout,
    HandlerDeadlineExceeded,
}

impl OverloadReason {
    pub(crate) const fn code(self) -> &'static str {
        match self {
            Self::GlobalInflightExhausted => "global_inflight_exhausted",
            Self::HealthReserveExhausted => "health_reserve_exhausted",
            Self::ExecutionQueueFull => "execution_queue_full",
            Self::ExecutionQueueTimeout => "execution_queue_timeout",
            Self::HandlerDeadlineExceeded => "handler_deadline_exceeded",
        }
    }

    const fn stage(self) -> &'static str {
        match self {
            Self::GlobalInflightExhausted | Self::HealthReserveExhausted => "request_admission",
            Self::ExecutionQueueFull | Self::ExecutionQueueTimeout => "execution_queue",
            Self::HandlerDeadlineExceeded => "engine_execution",
        }
    }

    const fn resource(self) -> &'static str {
        match self {
            Self::GlobalInflightExhausted => "http_inflight_requests",
            Self::HealthReserveExhausted => "health_inflight_requests",
            Self::ExecutionQueueFull | Self::ExecutionQueueTimeout => "engine_execution_slots",
            Self::HandlerDeadlineExceeded => "handler_deadline_ms",
        }
    }

    const fn detail(self) -> &'static str {
        match self {
            Self::GlobalInflightExhausted => "global in-flight request limit reached",
            Self::HealthReserveExhausted => "health-probe reserve is full",
            Self::ExecutionQueueFull => "engine execution queue is full",
            Self::ExecutionQueueTimeout => "request deadline expired while waiting to execute",
            Self::HandlerDeadlineExceeded => "request deadline expired during engine execution",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OverloadSnapshot {
    pub(crate) limit: usize,
    pub(crate) current: usize,
    pub(crate) execution_active_limit: usize,
    pub(crate) execution_active: usize,
    pub(crate) execution_queue_limit: usize,
    pub(crate) execution_queued: usize,
}

/// Build the additive HTTP problem body used by the REST edge.
///
/// `error` and `retry_after_secs` retain the pre-existing contract. New
/// clients should branch on `error_code`; all other fields are diagnostic.
pub(crate) fn problem_json(
    reason: OverloadReason,
    snapshot: OverloadSnapshot,
    retry_after_secs: u64,
) -> String {
    let mut execution = Map::new();
    insert_usize(
        &mut execution,
        "active_limit",
        snapshot.execution_active_limit,
    );
    insert_usize(&mut execution, "active", snapshot.execution_active);
    insert_usize(
        &mut execution,
        "queue_limit",
        snapshot.execution_queue_limit,
    );
    insert_usize(&mut execution, "queued", snapshot.execution_queued);

    let sequence = INCIDENT_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let mut root = Map::new();
    root.insert("ok".to_string(), JsonValue::Bool(false));
    root.insert(
        "type".to_string(),
        JsonValue::String(format!(
            "https://reddb.io/problems/overload/{}",
            reason.code()
        )),
    );
    root.insert(
        "title".to_string(),
        JsonValue::String("RedDB request refused".to_string()),
    );
    root.insert("status".to_string(), JsonValue::Number(503.0));
    root.insert(
        "detail".to_string(),
        JsonValue::String(reason.detail().to_string()),
    );
    root.insert(
        "error".to_string(),
        JsonValue::String("server at capacity".to_string()),
    );
    root.insert(
        "error_code".to_string(),
        JsonValue::String(reason.code().to_string()),
    );
    root.insert(
        "stage".to_string(),
        JsonValue::String(reason.stage().to_string()),
    );
    root.insert(
        "resource".to_string(),
        JsonValue::String(reason.resource().to_string()),
    );
    insert_usize(&mut root, "limit", snapshot.limit);
    insert_usize(&mut root, "current", snapshot.current);
    root.insert("execution".to_string(), JsonValue::Object(execution));
    root.insert("retryable".to_string(), JsonValue::Bool(true));
    root.insert(
        "retry_after_secs".to_string(),
        JsonValue::Number(retry_after_secs as f64),
    );
    root.insert(
        "retry_after_ms".to_string(),
        JsonValue::Number(retry_after_secs.saturating_mul(1_000) as f64),
    );
    root.insert(
        "incident_id".to_string(),
        JsonValue::String(format!("overload-{}-{sequence}", std::process::id())),
    );

    crate::json::to_string(&JsonValue::Object(root))
        .unwrap_or_else(|_| "{\"error\":\"server at capacity\"}".to_string())
}

fn insert_usize(map: &mut Map<String, JsonValue>, key: &str, value: usize) {
    map.insert(key.to_string(), JsonValue::Number(value as f64));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn problem_body_keeps_legacy_fields_and_exposes_stable_reason() {
        let body = problem_json(
            OverloadReason::ExecutionQueueFull,
            OverloadSnapshot {
                limit: 8,
                current: 8,
                execution_active_limit: 2,
                execution_active: 2,
                execution_queue_limit: 6,
                execution_queued: 6,
            },
            5,
        );
        let parsed: JsonValue = crate::json::from_str(&body).expect("valid problem JSON");
        assert_eq!(
            parsed["error"],
            JsonValue::String("server at capacity".to_string())
        );
        assert_eq!(
            parsed["error_code"],
            JsonValue::String("execution_queue_full".to_string())
        );
        assert_eq!(parsed["retry_after_secs"].as_i64(), Some(5));
        assert_eq!(parsed["execution"]["active"].as_i64(), Some(2));
        assert_eq!(parsed["execution"]["queued"].as_i64(), Some(6));
    }
}
