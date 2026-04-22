//! ML job definitions — what a job is, what state it can be in, how
//! to serialize it for persistence.
//!
//! Jobs are the unit of async work. Training, backfill, and bulk
//! inference-audit all flow through the same [`MlJob`] record so the
//! operator can inspect `SELECT * FROM ML_JOBS` and see every
//! long-running piece of ML work in one place.

use std::time::{SystemTime, UNIX_EPOCH};

/// Opaque job identifier. 128-bit so it's collision-free across
/// restarts and replicas without coordination.
pub type MlJobId = u128;

/// Kind of work a job performs. Determines which worker handler is
/// dispatched and how `progress` is interpreted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MlJobKind {
    /// `CREATE MODEL ... WITH (async = true)` — train a classifier,
    /// symbolic regression, etc.
    Train,
    /// `ALTER EMBEDDING COLUMN ... WITH BACKFILL = BACKGROUND` —
    /// re-embed existing rows under a new model.
    Backfill,
    /// `CREATE FEATURE ...` — materialise the bitemporal feature log
    /// from the source query.
    FeatureRefresh,
    /// Post-hoc drift computation over a window of recent writes.
    DriftCompute,
}

impl MlJobKind {
    pub fn token(self) -> &'static str {
        match self {
            MlJobKind::Train => "train",
            MlJobKind::Backfill => "backfill",
            MlJobKind::FeatureRefresh => "feature_refresh",
            MlJobKind::DriftCompute => "drift_compute",
        }
    }

    pub fn from_token(token: &str) -> Option<MlJobKind> {
        match token {
            "train" => Some(MlJobKind::Train),
            "backfill" => Some(MlJobKind::Backfill),
            "feature_refresh" => Some(MlJobKind::FeatureRefresh),
            "drift_compute" => Some(MlJobKind::DriftCompute),
            _ => None,
        }
    }
}

/// State machine for a job. Terminal states are `Completed`,
/// `Failed`, and `Cancelled` — workers must not mutate a record in a
/// terminal state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MlJobStatus {
    Queued,
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl MlJobStatus {
    pub fn token(self) -> &'static str {
        match self {
            MlJobStatus::Queued => "queued",
            MlJobStatus::Running => "running",
            MlJobStatus::Completed => "completed",
            MlJobStatus::Failed => "failed",
            MlJobStatus::Cancelled => "cancelled",
        }
    }

    pub fn from_token(token: &str) -> Option<MlJobStatus> {
        match token {
            "queued" => Some(MlJobStatus::Queued),
            "running" => Some(MlJobStatus::Running),
            "completed" => Some(MlJobStatus::Completed),
            "failed" => Some(MlJobStatus::Failed),
            "cancelled" => Some(MlJobStatus::Cancelled),
            _ => None,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            MlJobStatus::Completed | MlJobStatus::Failed | MlJobStatus::Cancelled
        )
    }
}

/// Persistent state of a single ML job.
///
/// Everything the operator needs to inspect `SELECT * FROM ML_JOBS`
/// lives here. `spec_json` carries kind-specific parameters (which
/// algorithm, which features, which hyperparameters); workers parse
/// it themselves so the registry stays schema-free.
#[derive(Debug, Clone)]
pub struct MlJob {
    pub id: MlJobId,
    pub kind: MlJobKind,
    /// Name of the model / feature / embedding column the job mutates.
    pub target_name: String,
    pub status: MlJobStatus,
    /// 0..=100.
    pub progress: u8,
    /// Epoch millis. `0` when not yet scheduled / finished.
    pub created_at_ms: u64,
    pub started_at_ms: u64,
    pub finished_at_ms: u64,
    /// Populated on `Failed`.
    pub error_message: Option<String>,
    /// Free-form payload describing the job — parsed by the worker.
    pub spec_json: String,
    /// Free-form metrics (accuracy, f1, etc.) — written by the worker
    /// before it transitions to `Completed`.
    pub metrics_json: Option<String>,
}

impl MlJob {
    pub fn new(id: MlJobId, kind: MlJobKind, target_name: String, spec_json: String) -> Self {
        Self {
            id,
            kind,
            target_name,
            status: MlJobStatus::Queued,
            progress: 0,
            created_at_ms: now_ms(),
            started_at_ms: 0,
            finished_at_ms: 0,
            error_message: None,
            spec_json,
            metrics_json: None,
        }
    }

    /// True once the job has reached a terminal status.
    pub fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }

    /// Duration between `started_at` and `finished_at`, if both are
    /// set. `None` while the job is still running or never started.
    pub fn duration_ms(&self) -> Option<u64> {
        if self.started_at_ms == 0 || self.finished_at_ms == 0 {
            return None;
        }
        self.finished_at_ms.checked_sub(self.started_at_ms)
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---- JSON serialisation --------------------------------------------------
//
// Jobs are persisted as a small JSON object per row. The schema is:
//
// {
//   "id":           "0xdeadbeef..."  (hex, 32 chars),
//   "kind":         "train" | "backfill" | ...,
//   "target":       "<name>",
//   "status":       "queued" | "running" | ...,
//   "progress":     0..=100,
//   "created_at":   <u64 ms>,
//   "started_at":   <u64 ms>,
//   "finished_at":  <u64 ms>,
//   "error":        "<msg>" | null,
//   "spec":         "<json string, opaque>",
//   "metrics":      "<json string, opaque>" | null
// }
//
// `spec` and `metrics` are quoted JSON strings (not nested objects)
// so the registry layer stays schema-free — the worker owns the
// payload shape.

use crate::json::{Map, Value as JsonValue};

impl MlJob {
    /// Encode as a compact JSON object suitable for KV storage.
    pub fn to_json(&self) -> String {
        let mut obj = Map::new();
        obj.insert(
            "id".to_string(),
            JsonValue::String(format!("{:032x}", self.id)),
        );
        obj.insert(
            "kind".to_string(),
            JsonValue::String(self.kind.token().to_string()),
        );
        obj.insert(
            "target".to_string(),
            JsonValue::String(self.target_name.clone()),
        );
        obj.insert(
            "status".to_string(),
            JsonValue::String(self.status.token().to_string()),
        );
        obj.insert(
            "progress".to_string(),
            JsonValue::Number(self.progress as f64),
        );
        obj.insert(
            "created_at".to_string(),
            JsonValue::Number(self.created_at_ms as f64),
        );
        obj.insert(
            "started_at".to_string(),
            JsonValue::Number(self.started_at_ms as f64),
        );
        obj.insert(
            "finished_at".to_string(),
            JsonValue::Number(self.finished_at_ms as f64),
        );
        obj.insert(
            "error".to_string(),
            match &self.error_message {
                Some(s) => JsonValue::String(s.clone()),
                None => JsonValue::Null,
            },
        );
        obj.insert(
            "spec".to_string(),
            JsonValue::String(self.spec_json.clone()),
        );
        obj.insert(
            "metrics".to_string(),
            match &self.metrics_json {
                Some(s) => JsonValue::String(s.clone()),
                None => JsonValue::Null,
            },
        );
        JsonValue::Object(obj).to_string_compact()
    }

    /// Inverse of [`Self::to_json`]. Returns `None` on any field
    /// mismatch — callers either skip the record or surface a
    /// persistence-corruption error.
    pub fn from_json(raw: &str) -> Option<Self> {
        let parsed = crate::json::parse_json(raw).ok()?;
        let value = JsonValue::from(parsed);
        let obj = value.as_object()?;
        let id_hex = obj.get("id")?.as_str()?;
        if id_hex.len() != 32 {
            return None;
        }
        let id = u128::from_str_radix(id_hex, 16).ok()?;
        let kind = MlJobKind::from_token(obj.get("kind")?.as_str()?)?;
        let target = obj.get("target")?.as_str()?.to_string();
        let status = MlJobStatus::from_token(obj.get("status")?.as_str()?)?;
        let progress = obj.get("progress")?.as_i64()? as u8;
        let created_at = obj.get("created_at")?.as_i64()? as u64;
        let started_at = obj.get("started_at")?.as_i64()? as u64;
        let finished_at = obj.get("finished_at")?.as_i64()? as u64;
        let error_message = match obj.get("error") {
            Some(JsonValue::String(s)) => Some(s.clone()),
            _ => None,
        };
        let spec_json = obj.get("spec")?.as_str()?.to_string();
        let metrics_json = match obj.get("metrics") {
            Some(JsonValue::String(s)) => Some(s.clone()),
            _ => None,
        };
        Some(MlJob {
            id,
            kind,
            target_name: target,
            status,
            progress: progress.min(100),
            created_at_ms: created_at,
            started_at_ms: started_at,
            finished_at_ms: finished_at,
            error_message,
            spec_json,
            metrics_json,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_token_round_trips() {
        for s in [
            MlJobStatus::Queued,
            MlJobStatus::Running,
            MlJobStatus::Completed,
            MlJobStatus::Failed,
            MlJobStatus::Cancelled,
        ] {
            assert_eq!(MlJobStatus::from_token(s.token()), Some(s));
        }
    }

    #[test]
    fn kind_token_round_trips() {
        for k in [
            MlJobKind::Train,
            MlJobKind::Backfill,
            MlJobKind::FeatureRefresh,
            MlJobKind::DriftCompute,
        ] {
            assert_eq!(MlJobKind::from_token(k.token()), Some(k));
        }
    }

    #[test]
    fn only_completed_failed_cancelled_are_terminal() {
        assert!(!MlJobStatus::Queued.is_terminal());
        assert!(!MlJobStatus::Running.is_terminal());
        assert!(MlJobStatus::Completed.is_terminal());
        assert!(MlJobStatus::Failed.is_terminal());
        assert!(MlJobStatus::Cancelled.is_terminal());
    }

    #[test]
    fn new_job_is_queued_with_zero_timestamps() {
        let job = MlJob::new(1, MlJobKind::Train, "spam".into(), "{}".into());
        assert_eq!(job.status, MlJobStatus::Queued);
        assert_eq!(job.progress, 0);
        assert_eq!(job.started_at_ms, 0);
        assert_eq!(job.finished_at_ms, 0);
        assert!(job.duration_ms().is_none());
    }

    #[test]
    fn duration_requires_both_timestamps() {
        let mut job = MlJob::new(1, MlJobKind::Train, "spam".into(), "{}".into());
        job.started_at_ms = 1000;
        assert!(job.duration_ms().is_none());
        job.finished_at_ms = 1250;
        assert_eq!(job.duration_ms(), Some(250));
    }
}
