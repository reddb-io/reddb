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

use tracing_appender::non_blocking::{NonBlockingBuilder, WorkerGuard};
use tracing_subscriber::fmt::writer::MakeWriterExt;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter, Registry};

/// Non-blocking buffer size — higher than the `tracing-appender`
/// default of 128k because under log-heavy bursts (bulk imports,
/// CDC storms) 128k drops lines silently via `DropCurrent`.
///
/// 1M entries × ~200 bytes per event ≈ 200 MB worst-case RAM —
/// fine for a server process, and dropped log lines are far more
/// painful than the memory.
const LOG_BUFFER_LINES: usize = 1_000_000;

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

/// Opaque handle that keeps the non-blocking log writers alive.
/// Drop at process exit to flush the buffered records for stderr
/// AND the rotating file sink. Both writers run on their own
/// dedicated background threads — the hot path only pushes onto
/// an MPSC channel, never touches stdio syscalls directly.
pub struct TelemetryGuard {
    _stderr_worker: Option<WorkerGuard>,
    _file_worker: Option<WorkerGuard>,
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

    // stderr — wrapped in `non_blocking` so the hot path never
    // blocks on `write(2)` syscalls. A dedicated thread owns the
    // stderr file descriptor and drains an MPSC channel; on a
    // full buffer we'd rather drop a line than stall a request.
    let (stderr_writer, stderr_worker) = NonBlockingBuilder::default()
        .buffered_lines_limit(LOG_BUFFER_LINES)
        .lossy(true)
        .finish(std::io::stderr());

    // Optional file layer + worker guard
    let (file_writer_opt, file_worker) = match cfg.log_dir.as_ref() {
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
                let (writer, guard) = NonBlockingBuilder::default()
                    .buffered_lines_limit(LOG_BUFFER_LINES)
                    .lossy(true)
                    .finish(file_appender);

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
            let stderr_layer = fmt::layer()
                .with_writer(stderr_writer.clone())
                .with_target(true)
                .with_thread_ids(false)
                .with_thread_names(false);
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
                .with_writer(stderr_writer.clone())
                .with_target(true)
                .with_thread_ids(false)
                .with_thread_names(false)
                .json()
                .with_current_span(true)
                .with_span_list(false);
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
        _stderr_worker: Some(stderr_worker),
        _file_worker: file_worker,
    })
}
