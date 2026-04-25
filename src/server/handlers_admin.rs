//! Lifecycle / admin HTTP endpoints (PLAN.md Phase 1).
//!
//! Universal contract surface consumed by orchestrators (K8s preStop,
//! Fly autostop, ECS drain, systemd, custom).
//!
//! - `POST /admin/shutdown` — flush + checkpoint + optional backup,
//!   200 only when safe to die. Idempotent.
//! - `POST /admin/drain` — stop accepting new writes, in-flight finish,
//!   200 once drain complete. Soft pre-shutdown step.
//! - `GET  /health/live` — process responsive (always cheap).
//! - `GET  /health/ready` — accepts queries (WAL replay + restore done).
//! - `GET  /health/startup` — same logic as ready, K8s-style longer
//!   timeout window.

use super::*;
use crate::runtime::lifecycle::Phase;

impl RedDBServer {
    /// `POST /admin/shutdown` — graceful shutdown coordinator.
    /// Returns 200 with the shutdown report when complete; 200 with
    /// the cached report when already shut down (idempotent); 500
    /// on flush failure (process should still exit afterwards).
    ///
    /// The HTTP layer does not own process exit — that's the
    /// signal-handler / `run_server` driver. This handler reports
    /// state; orchestrators that posted SIGTERM separately will see
    /// the process die when their grace window elapses.
    pub(crate) fn handle_admin_shutdown(&self) -> HttpResponse {
        let backup_on_shutdown = std::env::var("RED_BACKUP_ON_SHUTDOWN")
            .ok()
            .map(|v| matches!(v.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"))
            .unwrap_or(true);

        match self.runtime.graceful_shutdown(backup_on_shutdown) {
            Ok(report) => {
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert(
                    "phase".to_string(),
                    JsonValue::String(self.runtime.lifecycle().phase().as_str().to_string()),
                );
                object.insert(
                    "flushed_wal".to_string(),
                    JsonValue::Bool(report.flushed_wal),
                );
                object.insert(
                    "final_checkpoint".to_string(),
                    JsonValue::Bool(report.final_checkpoint),
                );
                object.insert(
                    "backup_uploaded".to_string(),
                    JsonValue::Bool(report.backup_uploaded),
                );
                object.insert(
                    "duration_ms".to_string(),
                    JsonValue::Number(report.duration_ms as f64),
                );
                json_response(200, JsonValue::Object(object))
            }
            Err(err) => json_error(500, err.to_string()),
        }
    }

    /// `POST /admin/drain` — flip to Draining phase. Subsequent
    /// `WriteGate`-checked writes will be rejected until shutdown
    /// completes or another phase override re-enables Ready.
    /// Idempotent.
    pub(crate) fn handle_admin_drain(&self) -> HttpResponse {
        self.runtime.lifecycle().mark_draining();
        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert(
            "phase".to_string(),
            JsonValue::String(self.runtime.lifecycle().phase().as_str().to_string()),
        );
        json_response(200, JsonValue::Object(object))
    }

    /// `GET /health/live` — process is alive and responsive. Always
    /// 200 once the runtime is constructed; 503 only after Stopped.
    /// Never touches I/O.
    pub(crate) fn handle_health_live(&self) -> HttpResponse {
        let phase = self.runtime.lifecycle().phase();
        let alive = !matches!(phase, Phase::Stopped);
        let status = if alive { 200 } else { 503 };
        let mut object = Map::new();
        object.insert(
            "status".to_string(),
            JsonValue::String(if alive { "alive" } else { "stopped" }.to_string()),
        );
        object.insert(
            "phase".to_string(),
            JsonValue::String(phase.as_str().to_string()),
        );
        json_response(status, JsonValue::Object(object))
    }

    /// `GET /health/ready` — runtime is fully past WAL replay /
    /// restore-from-remote and accepts queries.
    pub(crate) fn handle_health_ready(&self) -> HttpResponse {
        self.health_ready_response("ready")
    }

    /// `GET /health/startup` — Kubernetes startup probe variant.
    /// Same readiness logic as `/health/ready`; orchestrator gives
    /// it a longer grace window before failing the pod.
    pub(crate) fn handle_health_startup(&self) -> HttpResponse {
        self.health_ready_response("startup")
    }

    fn health_ready_response(&self, probe: &str) -> HttpResponse {
        let lifecycle = self.runtime.lifecycle();
        let phase = lifecycle.phase();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        let started_at = lifecycle.started_at_ms();
        let since_secs = (now.saturating_sub(started_at) as f64) / 1000.0;
        let mut object = Map::new();
        object.insert("probe".to_string(), JsonValue::String(probe.to_string()));
        object.insert(
            "phase".to_string(),
            JsonValue::String(phase.as_str().to_string()),
        );
        object.insert(
            "since_secs".to_string(),
            JsonValue::Number((since_secs * 1000.0).round() / 1000.0),
        );
        if let Some(ready_at) = lifecycle.ready_at_ms() {
            object.insert(
                "ready_at_unix_ms".to_string(),
                JsonValue::Number(ready_at as f64),
            );
        }

        if phase.accepts_queries() {
            object.insert("status".to_string(), JsonValue::String("ready".to_string()));
            json_response(200, JsonValue::Object(object))
        } else {
            object.insert(
                "status".to_string(),
                JsonValue::String(phase.as_str().to_string()),
            );
            if let Some(reason) = lifecycle.not_ready_reason() {
                object.insert("reason".to_string(), JsonValue::String(reason));
            } else {
                object.insert(
                    "reason".to_string(),
                    JsonValue::String(match phase {
                        Phase::Starting => "starting".to_string(),
                        Phase::ShuttingDown => "shutting_down".to_string(),
                        Phase::Stopped => "stopped".to_string(),
                        Phase::Draining => "draining".to_string(),
                        Phase::Ready => "ready".to_string(),
                    }),
                );
            }
            json_response(503, JsonValue::Object(object))
        }
    }
}
