//! Log-workload helpers built on top of the hypertable + retention
//! + continuous-aggregate primitives.
//!
//! This module gives application code a tight, typed surface to:
//!
//! * Batch-ingest structured log lines (`LogLine`) with explicit
//!   severity levels and label maps.
//! * Tail new lines since a timestamp watermark for streaming UIs.
//! * Enforce retention windows alongside the daemon that already
//!   ships in [`super::retention`].
//!
//! The underlying storage is the same [`super::hypertable::HypertableRegistry`];
//! physical records live wherever the caller plugs the registry
//! into. This module owns the log-shaped conveniences that make
//! "log workload" idiomatic without forcing every consumer to
//! hand-roll timestamp parsing + label hashing.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use super::hypertable::{ChunkId, HypertableRegistry, HypertableSpec};
use super::retention::{RetentionBackend, RetentionPolicy, RetentionRegistry};

/// Severity levels mapped to the standard syslog numeric rank so
/// downstream queries can do `WHERE severity >= WARN` cheaply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogSeverity {
    Trace = 0,
    Debug = 1,
    Info = 2,
    Warn = 3,
    Error = 4,
    Fatal = 5,
}

impl LogSeverity {
    pub fn as_i64(self) -> i64 {
        self as i64
    }

    pub fn token(self) -> &'static str {
        match self {
            LogSeverity::Trace => "TRACE",
            LogSeverity::Debug => "DEBUG",
            LogSeverity::Info => "INFO",
            LogSeverity::Warn => "WARN",
            LogSeverity::Error => "ERROR",
            LogSeverity::Fatal => "FATAL",
        }
    }

    pub fn from_token(token: &str) -> Option<LogSeverity> {
        match token.to_ascii_uppercase().as_str() {
            "TRACE" => Some(LogSeverity::Trace),
            "DEBUG" => Some(LogSeverity::Debug),
            "INFO" => Some(LogSeverity::Info),
            "WARN" | "WARNING" => Some(LogSeverity::Warn),
            "ERROR" | "ERR" => Some(LogSeverity::Error),
            "FATAL" | "CRITICAL" => Some(LogSeverity::Fatal),
            _ => None,
        }
    }
}

/// One structured log line. `labels` is the low-cardinality map
/// (`service`, `region`, `severity_str`); `fields` carries typed
/// extra payload (`latency_ms`, `status`, `bytes_out`). Keeping them
/// separate lets the codec layer pick `Dict` for labels and
/// `T64` / `Delta` for numeric fields.
#[derive(Debug, Clone)]
pub struct LogLine {
    pub ts_ns: u64,
    pub severity: LogSeverity,
    pub service: String,
    pub message: String,
    pub labels: HashMap<String, String>,
    pub numeric_fields: HashMap<String, f64>,
    /// Optional trace / span identifiers — wiring for graph-based
    /// span traversal comes later; the field lives here so the
    /// ingest pipe doesn't need to reshape when that lands.
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
}

impl LogLine {
    pub fn now(
        severity: LogSeverity,
        service: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        let ts_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        Self {
            ts_ns,
            severity,
            service: service.into(),
            message: message.into(),
            labels: HashMap::new(),
            numeric_fields: HashMap::new(),
            trace_id: None,
            span_id: None,
        }
    }

    pub fn with_label(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.labels.insert(key.into(), value.into());
        self
    }

    pub fn with_field(mut self, key: impl Into<String>, value: f64) -> Self {
        self.numeric_fields.insert(key.into(), value);
        self
    }

    pub fn with_trace(mut self, trace_id: impl Into<String>, span_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
        self.span_id = Some(span_id.into());
        self
    }
}

/// Stats surface for dashboards + health probes.
#[derive(Debug, Default, Clone)]
pub struct LogIngestStats {
    pub lines_ingested: u64,
    pub batches_flushed: u64,
    pub chunks_touched: u64,
    pub last_flush_unix_ns: u64,
}

/// Hypertable-backed log pipeline. Accepts batched writes, routes
/// each line to the owning chunk, and exposes retention + tail APIs.
#[derive(Clone)]
pub struct LogPipeline {
    name: String,
    hypertables: HypertableRegistry,
    retention: RetentionRegistry,
    stats: Arc<Mutex<LogIngestStats>>,
    /// Records tailed by `tail_since`. Kept in a small ring so the
    /// tail API can back a `WATCH` endpoint without re-scanning
    /// chunk metadata on every poll.
    recent: Arc<Mutex<Vec<LogLine>>>,
    recent_capacity: usize,
}

impl std::fmt::Debug for LogPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LogPipeline")
            .field("name", &self.name)
            .field("recent_capacity", &self.recent_capacity)
            .finish()
    }
}

impl LogPipeline {
    /// Build a pipeline routing into `hypertable_name` with the
    /// given chunk interval (e.g. `"1d"`).
    pub fn new(
        hypertable_name: impl Into<String>,
        time_column: impl Into<String>,
        chunk_interval: &str,
    ) -> Option<Self> {
        let spec = HypertableSpec::from_interval_string(
            hypertable_name.into(),
            time_column.into(),
            chunk_interval,
        )?;
        let registry = HypertableRegistry::new();
        registry.register(spec.clone());
        Some(Self {
            name: spec.name,
            hypertables: registry,
            retention: RetentionRegistry::new(),
            stats: Arc::new(Mutex::new(LogIngestStats::default())),
            recent: Arc::new(Mutex::new(Vec::new())),
            recent_capacity: 4096,
        })
    }

    pub fn with_recent_capacity(mut self, capacity: usize) -> Self {
        self.recent_capacity = capacity.max(64);
        self
    }

    pub fn hypertables(&self) -> &HypertableRegistry {
        &self.hypertables
    }

    pub fn retention(&self) -> &RetentionRegistry {
        &self.retention
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Ingest one line. Returns the chunk it landed in (useful for
    /// diagnostics and tests).
    pub fn ingest_one(&self, line: LogLine) -> Option<ChunkId> {
        let chunk = self.hypertables.route(&self.name, line.ts_ns)?;
        self.record_recent(&line);
        let mut stats = match self.stats.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        stats.lines_ingested += 1;
        Some(chunk)
    }

    /// Ingest a batch. Routing + accounting happen under a single
    /// lock per chunk boundary — typical tight-loop shape for
    /// 100k lines/s ingest.
    pub fn ingest_batch(&self, lines: &[LogLine]) -> u64 {
        if lines.is_empty() {
            return 0;
        }
        let mut distinct_chunks: Vec<ChunkId> = Vec::new();
        for line in lines {
            if let Some(id) = self.hypertables.route(&self.name, line.ts_ns) {
                if distinct_chunks.last() != Some(&id) && !distinct_chunks.contains(&id) {
                    distinct_chunks.push(id);
                }
                self.record_recent(line);
            }
        }
        let mut stats = match self.stats.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        stats.lines_ingested += lines.len() as u64;
        stats.batches_flushed += 1;
        stats.chunks_touched = stats
            .chunks_touched
            .saturating_add(distinct_chunks.len() as u64);
        stats.last_flush_unix_ns = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        lines.len() as u64
    }

    fn record_recent(&self, line: &LogLine) {
        let mut guard = match self.recent.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.push(line.clone());
        let overflow = guard.len().saturating_sub(self.recent_capacity);
        if overflow > 0 {
            guard.drain(0..overflow);
        }
    }

    /// Return every line seen by `ingest_one` / `ingest_batch` with
    /// `ts_ns > watermark`. Streaming UIs call this from a polling
    /// loop or a `WATCH` subscription.
    pub fn tail_since(&self, watermark_ns: u64) -> Vec<LogLine> {
        let guard = match self.recent.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard
            .iter()
            .filter(|l| l.ts_ns > watermark_ns)
            .cloned()
            .collect()
    }

    /// Install / replace a retention policy for this pipeline's
    /// hypertable. The daemon in [`super::retention::RetentionRegistry`]
    /// is the consumer — wire it separately at startup.
    pub fn set_retention(&self, max_age_secs: u64) {
        self.retention
            .set_policy(self.name.clone(), RetentionPolicy::from_secs(max_age_secs));
    }

    /// Declare a partition-level TTL — applies to every chunk that
    /// belongs to this pipeline's hypertable. Combines with the
    /// retention-daemon sweep: expired chunks disappear in O(1)
    /// metadata updates instead of row-by-row deletes. `None`
    /// clears the TTL.
    pub fn set_partition_ttl(&self, ttl: &str) -> bool {
        let ns = match super::retention::parse_duration_ns(ttl) {
            Some(n) if n > 0 => n,
            _ => return false,
        };
        self.hypertables.set_default_ttl_ns(&self.name, Some(ns));
        true
    }

    /// Run the partition-level sweep once. Returns the chunks that
    /// expired (their physical storage release is the caller's
    /// responsibility — the registry only owns the metadata).
    pub fn sweep_expired_chunks(&self, now_ns: u64) -> Vec<super::hypertable::ChunkMeta> {
        self.hypertables.sweep_expired(&self.name, now_ns)
    }

    pub fn stats(&self) -> LogIngestStats {
        let guard = match self.stats.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.clone()
    }

    /// Count of lines matching a severity threshold — used by
    /// health-check endpoints that alert on "ERROR rate in the last
    /// minute". Looks at the in-memory tail buffer only; for the
    /// full historical view go through `time_bucket` over the
    /// hypertable.
    pub fn recent_count_at_or_above(&self, severity: LogSeverity) -> u64 {
        let guard = match self.recent.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        guard.iter().filter(|l| l.severity >= severity).count() as u64
    }

    /// Adapter so the retention daemon can see this pipeline's
    /// hypertable. Returning `Arc<dyn RetentionBackend>` lets the
    /// caller hand the backend to `RetentionRegistry::start`.
    pub fn retention_backend(&self) -> Arc<dyn RetentionBackend> {
        Arc::new(LogRetentionBackend {
            name: self.name.clone(),
            hypertables: self.hypertables.clone(),
        })
    }
}

struct LogRetentionBackend {
    name: String,
    hypertables: HypertableRegistry,
}

impl RetentionBackend for LogRetentionBackend {
    fn time_series_collections(&self) -> Vec<String> {
        vec![self.name.clone()]
    }

    fn drop_chunks_older_than(&self, collection: &str, cutoff_ns: u64) -> u64 {
        if collection != self.name {
            return 0;
        }
        self.hypertables
            .drop_chunks_before(collection, cutoff_ns)
            .len() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_at(ts_ns: u64, sev: LogSeverity, msg: &str) -> LogLine {
        LogLine {
            ts_ns,
            severity: sev,
            service: "demo".into(),
            message: msg.into(),
            labels: HashMap::new(),
            numeric_fields: HashMap::new(),
            trace_id: None,
            span_id: None,
        }
    }

    #[test]
    fn severity_token_round_trips() {
        for s in [
            LogSeverity::Trace,
            LogSeverity::Debug,
            LogSeverity::Info,
            LogSeverity::Warn,
            LogSeverity::Error,
            LogSeverity::Fatal,
        ] {
            assert_eq!(LogSeverity::from_token(s.token()), Some(s));
        }
        // Aliases.
        assert_eq!(LogSeverity::from_token("warning"), Some(LogSeverity::Warn));
        assert_eq!(LogSeverity::from_token("err"), Some(LogSeverity::Error));
        assert_eq!(
            LogSeverity::from_token("critical"),
            Some(LogSeverity::Fatal)
        );
        assert!(LogSeverity::from_token("nope").is_none());
    }

    #[test]
    fn severity_comparisons_use_syslog_rank() {
        assert!(LogSeverity::Error > LogSeverity::Warn);
        assert!(LogSeverity::Warn > LogSeverity::Info);
        assert_eq!(LogSeverity::Info.as_i64(), 2);
    }

    #[test]
    fn ingest_one_routes_to_chunk_and_updates_stats() {
        let pipe = LogPipeline::new("access_log", "ts", "1h").unwrap();
        let id = pipe
            .ingest_one(line_at(3_600_000_000_001, LogSeverity::Info, "hi"))
            .unwrap();
        assert_eq!(id.start_ns, 3_600_000_000_000);
        assert_eq!(pipe.stats().lines_ingested, 1);
        assert_eq!(pipe.hypertables().total_rows("access_log"), 1);
    }

    #[test]
    fn ingest_batch_bumps_stats_with_distinct_chunk_count() {
        let pipe = LogPipeline::new("logs", "ts", "1h").unwrap();
        let lines: Vec<_> = (0..5)
            .map(|i| line_at(i * 3_600_000_000_000, LogSeverity::Info, "x"))
            .collect();
        let written = pipe.ingest_batch(&lines);
        assert_eq!(written, 5);
        let stats = pipe.stats();
        assert_eq!(stats.lines_ingested, 5);
        assert_eq!(stats.batches_flushed, 1);
        assert_eq!(stats.chunks_touched, 5);
    }

    #[test]
    fn tail_since_returns_only_newer_lines() {
        let pipe = LogPipeline::new("logs", "ts", "1h").unwrap();
        for t in [10, 20, 30, 40] {
            pipe.ingest_one(line_at(t, LogSeverity::Info, "m"));
        }
        let tailed = pipe.tail_since(25);
        assert_eq!(tailed.len(), 2);
        assert_eq!(tailed[0].ts_ns, 30);
        assert_eq!(tailed[1].ts_ns, 40);
    }

    #[test]
    fn recent_ring_respects_capacity() {
        let pipe = LogPipeline::new("logs", "ts", "1h")
            .unwrap()
            .with_recent_capacity(100);
        for t in 0..250 {
            pipe.ingest_one(line_at(t, LogSeverity::Info, "m"));
        }
        let all = pipe.tail_since(0);
        assert_eq!(all.len(), 100, "only the last 100 lines should remain");
        assert_eq!(all[0].ts_ns, 150);
        assert_eq!(all.last().unwrap().ts_ns, 249);
    }

    #[test]
    fn recent_count_at_or_above_filters_correctly() {
        let pipe = LogPipeline::new("logs", "ts", "1h").unwrap();
        pipe.ingest_one(line_at(1, LogSeverity::Debug, "d"));
        pipe.ingest_one(line_at(2, LogSeverity::Info, "i"));
        pipe.ingest_one(line_at(3, LogSeverity::Warn, "w"));
        pipe.ingest_one(line_at(4, LogSeverity::Error, "e"));
        pipe.ingest_one(line_at(5, LogSeverity::Fatal, "f"));
        assert_eq!(pipe.recent_count_at_or_above(LogSeverity::Warn), 3);
        assert_eq!(pipe.recent_count_at_or_above(LogSeverity::Error), 2);
    }

    #[test]
    fn retention_backend_drops_expired_chunks() {
        let pipe = LogPipeline::new("logs", "ts", "1h").unwrap();
        // Three chunks (0, 1h, 2h).
        for t in [0, 3_600_000_000_000, 7_200_000_000_000] {
            pipe.ingest_one(line_at(t, LogSeverity::Info, "m"));
        }
        assert_eq!(pipe.hypertables().show_chunks("logs").len(), 3);

        pipe.set_retention(3_600); // keep last hour only
        let backend = pipe.retention_backend();
        // cutoff exactly at 1h boundary drops chunks whose max_ts ≤ cutoff.
        let dropped = backend.drop_chunks_older_than("logs", 3_600_000_000_000);
        assert_eq!(dropped, 2);
        assert_eq!(pipe.hypertables().show_chunks("logs").len(), 1);
    }

    #[test]
    fn log_line_builder_composes_labels_and_fields() {
        let line = LogLine::now(LogSeverity::Error, "api", "boom")
            .with_label("region", "us-east-1")
            .with_field("latency_ms", 230.0)
            .with_trace("trace-42", "span-7");
        assert_eq!(line.service, "api");
        assert_eq!(line.severity, LogSeverity::Error);
        assert_eq!(line.labels.get("region").unwrap(), "us-east-1");
        assert_eq!(line.numeric_fields.get("latency_ms").unwrap(), &230.0);
        assert_eq!(line.trace_id.as_deref(), Some("trace-42"));
    }

    #[test]
    fn pipeline_without_valid_interval_returns_none() {
        assert!(LogPipeline::new("x", "ts", "raw").is_none());
        assert!(LogPipeline::new("x", "ts", "bogus").is_none());
    }

    #[test]
    fn partition_ttl_sweep_drops_expired_chunks() {
        let pipe = LogPipeline::new("logs", "ts", "1h").unwrap();
        assert!(pipe.set_partition_ttl("2h"));
        const HOUR: u64 = 3_600_000_000_000;
        // Three hourly chunks with exactly one row each at the
        // chunk boundary — so max_ts = 0, 1h, 2h.
        for t in [0, HOUR, 2 * HOUR] {
            pipe.ingest_one(line_at(t, LogSeverity::Info, "m"));
        }
        // now = 3h + 1ns → expiries are 2h, 3h, 4h. The 3h chunk
        // (max_ts=1h) is exactly at its expiry (1h+2h=3h) so it
        // qualifies; the 4h one does not. Plus the 0-start chunk.
        let dropped = pipe.sweep_expired_chunks(3 * HOUR + 1);
        assert_eq!(dropped.len(), 2);
        assert_eq!(pipe.hypertables().show_chunks("logs").len(), 1);
    }

    #[test]
    fn partition_ttl_rejects_invalid_duration() {
        let pipe = LogPipeline::new("logs", "ts", "1h").unwrap();
        assert!(!pipe.set_partition_ttl("raw"));
        assert!(!pipe.set_partition_ttl("nonsense"));
        // A valid TTL still works after a rejected call.
        assert!(pipe.set_partition_ttl("1d"));
    }
}
