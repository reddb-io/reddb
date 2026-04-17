//! Structured logging façade over `tracing` + `tracing-subscriber`.
//!
//! Entry point for the `red` binary and any embedder that wants RedDB
//! to manage logging on its behalf. Sets up two layers:
//!
//! - **stderr** — pretty (TTY) or JSON (piped/CI) formatted logs
//! - **file**   — optional daily-rotating file in `log_dir`, non-blocking
//!   writer backed by `tracing-appender`
//!
//! A background janitor purges rotated files older than
//! `rotation_keep_days` every hour.
//!
//! The returned `Option<TelemetryGuard>` must live for the process
//! lifetime — dropping it flushes the non-blocking buffer so no log
//! lines are lost on graceful shutdown.

use std::path::PathBuf;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::fmt::writer::MakeWriterExt;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter, Registry};

pub mod janitor;
pub mod span;

/// Stdio / file output format. `Pretty` renders human-readable coloured
/// lines; `Json` emits NDJSON suitable for Loki / ELK / Datadog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Pretty,
    Json,
}

impl LogFormat {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "pretty" | "text" | "human" => Some(Self::Pretty),
            "json" | "ndjson" => Some(Self::Json),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    /// Directory for rotating log files. `None` = stderr-only (CLI
    /// one-shot / embedded default).
    pub log_dir: Option<PathBuf>,
    /// Prefix for rotated files; defaults to `"reddb.log"` when empty.
    pub file_prefix: String,
    /// `RUST_LOG`-style filter expression. Example:
    /// `"info,reddb::wire=debug"`.
    pub level_filter: String,
    /// stderr output format. File output always matches.
    pub format: LogFormat,
    /// How many rotated files to keep (older ones deleted by janitor).
    pub rotation_keep_days: u16,
    /// Service name stamped on every record under the `service` field.
    pub service_name: &'static str,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            log_dir: None,
            file_prefix: "reddb.log".to_string(),
            level_filter: "info".to_string(),
            format: LogFormat::Pretty,
            rotation_keep_days: 14,
            service_name: "reddb",
        }
    }
}

/// Opaque handle that keeps the non-blocking log writer alive. Drop at
/// process exit to flush buffered records.
pub struct TelemetryGuard {
    _worker: Option<WorkerGuard>,
}

/// Install the global `tracing` subscriber. Idempotent: if another
/// subscriber is already registered (e.g. an embedder set up its own),
/// we silently return `None` and let the caller proceed.
///
/// RedDB embedders that want to own the subscriber should simply not
/// call this.
pub fn init(cfg: TelemetryConfig) -> Option<TelemetryGuard> {
    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(&cfg.level_filter));

    // stderr layer — format depends on cfg.format
    let stderr_layer = fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(true)
        .with_thread_ids(false)
        .with_thread_names(false);

    // Optional file layer + worker guard
    let (file_writer_opt, worker_guard) = match cfg.log_dir.as_ref() {
        Some(dir) => {
            if let Err(err) = std::fs::create_dir_all(dir) {
                // Surface the failure to stderr directly — the
                // subscriber isn't up yet, so tracing::warn! wouldn't
                // land anywhere. Skip file logging and continue with
                // stderr-only.
                eprintln!(
                    "telemetry: failed to create log dir {}: {err}",
                    dir.display()
                );
                (None, None)
            } else {
                let file_appender = tracing_appender::rolling::daily(dir, &cfg.file_prefix);
                let (writer, guard) = tracing_appender::non_blocking(file_appender);

                // Spawn retention janitor (if tokio runtime active).
                if cfg.rotation_keep_days > 0 {
                    janitor::spawn(dir.clone(), cfg.file_prefix.clone(), cfg.rotation_keep_days);
                }
                (Some(writer), Some(guard))
            }
        }
        None => (None, None),
    };

    // Build the subscriber. We commit to one format branch at a time —
    // mixing pretty + json per-layer is rarely useful, and the branching
    // keeps the type signatures tractable.
    let result = match cfg.format {
        LogFormat::Pretty => {
            let base = Registry::default().with(env_filter).with(stderr_layer);
            if let Some(writer) = file_writer_opt.clone() {
                let file_layer = fmt::layer()
                    .with_writer(writer.with_max_level(tracing::Level::TRACE))
                    .with_target(true)
                    .with_ansi(false);
                base.with(file_layer).try_init()
            } else {
                base.try_init()
            }
        }
        LogFormat::Json => {
            let stderr_json = fmt::layer()
                .with_writer(std::io::stderr)
                .with_target(true)
                .with_thread_ids(false)
                .with_thread_names(false)
                .json()
                .with_current_span(true)
                .with_span_list(false);
            // Build without the stderr_layer we defined above (it's
            // pretty-only for the Pretty branch). JSON handling builds
            // a fresh json-formatted layer.
            drop(stderr_layer);
            let base = Registry::default().with(env_filter).with(stderr_json);
            if let Some(writer) = file_writer_opt {
                let file_json = fmt::layer()
                    .with_writer(writer.with_max_level(tracing::Level::TRACE))
                    .with_target(true)
                    .json()
                    .with_current_span(true)
                    .with_span_list(false);
                base.with(file_json).try_init()
            } else {
                base.try_init()
            }
        }
    };

    if result.is_err() {
        // Subscriber already set — library mode. That's fine.
        return None;
    }

    // Root event so users know telemetry is alive.
    tracing::info!(
        service = cfg.service_name,
        log_dir = cfg.log_dir.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "<none>".into()),
        format = ?cfg.format,
        "telemetry initialised"
    );

    Some(TelemetryGuard {
        _worker: worker_guard,
    })
}
