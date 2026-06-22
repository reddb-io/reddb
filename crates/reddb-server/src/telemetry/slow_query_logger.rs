//! Dedicated slow-query sink — writes structured JSON lines to `red-slow.log`.
//!
//! Below-threshold calls pay only one relaxed atomic load and return
//! immediately with zero allocations. Above-threshold calls go through
//! a deterministic counter-based sampler before writing to the
//! `NonBlocking` writer, which pushes the bytes onto a channel and
//! returns without waiting for the disk write.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use tracing_appender::non_blocking::{NonBlocking, NonBlockingBuilder, WorkerGuard};

use crate::runtime::EffectiveScope;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

pub struct SlowQueryOpts {
    pub log_dir: PathBuf,
    pub threshold_ms: u64,
    /// 0..=100; values > 100 are clamped to 100.
    pub sample_pct: u8,
}

/// Closed enum of query kinds emitted in the slow-query log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryKind {
    Select,
    Insert,
    Update,
    Delete,
    Bulk,
    Aggregate,
    DDL,
    Internal,
}

impl QueryKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Select => "select",
            Self::Insert => "insert",
            Self::Update => "update",
            Self::Delete => "delete",
            Self::Bulk => "bulk",
            Self::Aggregate => "aggregate",
            Self::DDL => "ddl",
            Self::Internal => "internal",
        }
    }
}

// ---------------------------------------------------------------------------
// SlowQueryLogger
// ---------------------------------------------------------------------------

pub struct SlowQueryLogger {
    /// Interior-mutable because `Write::write_all` takes `&mut self`.
    writer: Mutex<NonBlocking>,
    /// Keeps the background writer thread alive for the process lifetime.
    _guard: WorkerGuard,
    threshold_ms: AtomicU64,
    /// 0..100; 100 means "emit every above-threshold query".
    sample_pct: AtomicU8,
    /// Monotonic counter across above-threshold calls — drives round-robin
    /// sampling so exactly `sample_pct`% of above-threshold calls emit.
    above_count: AtomicU64,
    /// Durable telemetry substrate (ADR 0060). When attached, above-threshold
    /// events are dual-written: file sink (existing behavior) + ring store.
    store: std::sync::OnceLock<std::sync::Arc<super::slow_query_store::SlowQueryStore>>,
}

impl SlowQueryLogger {
    pub fn new(opts: SlowQueryOpts) -> Arc<Self> {
        let path = reddb_file::layout::legacy_slow_query_log_path(&opts.log_dir);
        Self::open_at(path, opts.threshold_ms, opts.sample_pct)
    }

    /// Resolve a [`crate::storage::layout::LogDestination`] into a concrete
    /// slow-query sink. `File(p)` writes to that exact path; `Stderr` and
    /// `Syslog` fall back to `<fallback_log_dir>/red-slow.log` until the
    /// dedicated sinks are wired (ADR 0018).
    pub fn for_destination(
        dest: &crate::storage::layout::LogDestination,
        fallback_log_dir: &std::path::Path,
        threshold_ms: u64,
        sample_pct: u8,
    ) -> Arc<Self> {
        use crate::storage::layout::LogDestination;
        let path = match dest {
            LogDestination::File(p) => p.clone(),
            LogDestination::Stderr => {
                reddb_file::layout::legacy_slow_query_log_path(fallback_log_dir)
            }
            LogDestination::Syslog => {
                tracing::warn!(
                    target: "reddb::slow",
                    "slow-query LogDestination::Syslog requested; sink not implemented, falling back to file"
                );
                reddb_file::layout::legacy_slow_query_log_path(fallback_log_dir)
            }
        };
        Self::open_at(path, threshold_ms, sample_pct)
    }

    fn open_at(path: PathBuf, threshold_ms: u64, sample_pct: u8) -> Arc<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap_or_else(|e| panic!("SlowQueryLogger: cannot open {}: {e}", path.display()));

        let (writer, guard) = NonBlockingBuilder::default()
            .buffered_lines_limit(65_536)
            .lossy(true)
            .finish(file);

        Arc::new(Self {
            writer: Mutex::new(writer),
            _guard: guard,
            threshold_ms: AtomicU64::new(threshold_ms),
            sample_pct: AtomicU8::new(sample_pct.min(100)),
            above_count: AtomicU64::new(0),
            store: std::sync::OnceLock::new(),
        })
    }

    /// Attach the operational telemetry substrate store (ADR 0060).
    ///
    /// Called once at runtime startup. Above-threshold, sampled events will
    /// be written to both the file sink and the ring store. A second call
    /// is a no-op (first registration wins).
    pub fn attach_store(&self, store: std::sync::Arc<super::slow_query_store::SlowQueryStore>) {
        let _ = self.store.set(store);
    }

    /// Record a completed query. Below-threshold: single relaxed load, no
    /// allocation. Above-threshold + sampled: emit a JSON line to the sink.
    pub fn record(
        &self,
        kind: QueryKind,
        duration_ms: u64,
        sql_redacted: String,
        scope: &EffectiveScope,
    ) {
        // Hot path: threshold gate — single relaxed atomic load.
        if duration_ms < self.threshold_ms.load(Ordering::Relaxed) {
            return;
        }

        // Sampling: deterministic round-robin counter.
        // `above_count % 100 < sample_pct` gives exactly sample_pct% over
        // long runs without any floating-point or RNG overhead.
        let pct = u64::from(self.sample_pct.load(Ordering::Relaxed));
        if pct < 100 {
            let n = self.above_count.fetch_add(1, Ordering::Relaxed);
            if (n % 100) >= pct {
                return;
            }
        }

        self.emit(kind, duration_ms, sql_redacted, scope);
    }

    fn emit(
        &self,
        kind: QueryKind,
        duration_ms: u64,
        sql_redacted: String,
        scope: &EffectiveScope,
    ) {
        let ts_ms = crate::utils::now_unix_millis();
        let tenant = scope.tenant.as_deref().unwrap_or("").to_string();
        let identity = scope
            .identity
            .as_ref()
            .map(|(u, _)| u.as_str())
            .unwrap_or("")
            .to_string();

        // Durable substrate (ADR 0060): push to the ring store with hashed
        // tenant/identity before writing the file line. Store is optional;
        // missing store → file-only behavior (backward-compatible).
        if let Some(store) = self.store.get() {
            store.push(super::slow_query_store::SlowQueryEvent {
                ts_ms,
                kind: kind.as_str(),
                duration_ms,
                sql_redacted: sql_redacted.clone(),
                tenant_hash: super::slow_query_store::hash_label(&tenant),
                identity_hash: super::slow_query_store::hash_label(&identity),
            });
        }

        let mut map = std::collections::BTreeMap::new();
        map.insert(
            "ts_ms".to_string(),
            crate::json::Value::Number(ts_ms as f64),
        );
        map.insert(
            "kind".to_string(),
            crate::json::Value::String(kind.as_str().to_string()),
        );
        map.insert(
            "duration_ms".to_string(),
            crate::json::Value::Number(duration_ms as f64),
        );
        map.insert("sql".to_string(), crate::json::Value::String(sql_redacted));
        map.insert("tenant".to_string(), crate::json::Value::String(tenant));
        map.insert("identity".to_string(), crate::json::Value::String(identity));

        let obj = crate::json::Value::Object(map);
        if let Ok(mut line) = crate::json::to_string(&obj) {
            line.push('\n');
            if let Ok(mut w) = self.writer.lock() {
                let _ = w.write_all(line.as_bytes());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::time::Instant;

    use super::*;
    use crate::runtime::EffectiveScope;
    use crate::storage::transaction::snapshot::Snapshot;

    fn tmp_dir() -> PathBuf {
        let mut d = std::env::temp_dir();
        d.push(format!(
            "reddb-slow-{}-{}",
            std::process::id(),
            crate::utils::now_unix_nanos()
        ));
        d
    }

    fn logger(dir: &PathBuf, threshold_ms: u64, sample_pct: u8) -> Arc<SlowQueryLogger> {
        SlowQueryLogger::new(SlowQueryOpts {
            log_dir: dir.clone(),
            threshold_ms,
            sample_pct,
        })
    }

    fn empty_scope() -> EffectiveScope {
        EffectiveScope {
            tenant: None,
            identity: None,
            snapshot: Snapshot {
                xid: 0,
                in_progress: HashSet::new(),
            },
            visible_collections: None,
        }
    }

    fn flush(_log: &Arc<SlowQueryLogger>) {
        // NonBlocking sends bytes to a background thread; sleep gives the
        // worker time to drain the channel and write to disk.
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    fn read_log_lines(dir: &PathBuf) -> Vec<crate::json::Value> {
        let path = reddb_file::layout::legacy_slow_query_log_path(dir);
        let body = std::fs::read_to_string(&path).unwrap_or_default();
        body.lines()
            .filter(|l| !l.is_empty())
            .map(|l| crate::json::from_str::<crate::json::Value>(l).expect("valid JSON"))
            .collect()
    }

    // -----------------------------------------------------------------------
    // Below-threshold: zero writes, fast
    // -----------------------------------------------------------------------

    #[test]
    fn below_threshold_no_file_writes() {
        let dir = tmp_dir();
        let log = logger(&dir, 1000, 100);
        let scope = empty_scope();

        for _ in 0..10_000 {
            log.record(QueryKind::Select, 5, "SELECT 1".into(), &scope);
        }

        flush(&log);
        let lines = read_log_lines(&dir);
        assert!(
            lines.is_empty(),
            "expected zero writes, got {}",
            lines.len()
        );
    }

    #[test]
    fn below_threshold_wall_time_under_10ms() {
        let dir = tmp_dir();
        let log = logger(&dir, 1000, 100);
        let scope = empty_scope();

        let start = Instant::now();
        for _ in 0..10_000 {
            log.record(QueryKind::Select, 5, "SELECT 1".into(), &scope);
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed.as_millis() < 10,
            "10k below-threshold calls took {}ms (>10ms budget)",
            elapsed.as_millis()
        );
    }

    // -----------------------------------------------------------------------
    // Above-threshold: structured JSON line, parseable fields
    // -----------------------------------------------------------------------

    #[test]
    fn above_threshold_emits_json_line() {
        let dir = tmp_dir();
        let log = logger(&dir, 10, 100);
        let scope = empty_scope();

        log.record(QueryKind::Select, 100, "SELECT * FROM t".into(), &scope);
        flush(&log);

        let lines = read_log_lines(&dir);
        assert_eq!(lines.len(), 1, "expected 1 line");
        let v = &lines[0];
        assert_eq!(v.get("kind").and_then(|x| x.as_str()), Some("select"));
        assert_eq!(v.get("duration_ms").and_then(|x| x.as_i64()), Some(100));
        let sql = v.get("sql").and_then(|x| x.as_str());
        assert_eq!(sql, Some("SELECT * FROM t"));
    }

    #[test]
    fn json_line_has_all_required_fields() {
        let dir = tmp_dir();
        let log = logger(&dir, 0, 100);
        let scope = empty_scope();

        log.record(
            QueryKind::Insert,
            42,
            "INSERT INTO t VALUES (1)".into(),
            &scope,
        );
        flush(&log);

        let lines = read_log_lines(&dir);
        assert_eq!(lines.len(), 1);
        let v = &lines[0];
        assert!(v.get("ts_ms").is_some(), "missing ts_ms");
        assert!(v.get("kind").is_some(), "missing kind");
        assert!(v.get("duration_ms").is_some(), "missing duration_ms");
        assert!(v.get("sql").is_some(), "missing sql");
        assert!(v.get("tenant").is_some(), "missing tenant");
        assert!(v.get("identity").is_some(), "missing identity");
    }

    #[test]
    fn all_query_kinds_serialise() {
        let kinds = [
            QueryKind::Select,
            QueryKind::Insert,
            QueryKind::Update,
            QueryKind::Delete,
            QueryKind::Bulk,
            QueryKind::Aggregate,
            QueryKind::DDL,
            QueryKind::Internal,
        ];
        for k in kinds {
            assert!(!k.as_str().is_empty());
        }
    }

    // -----------------------------------------------------------------------
    // Sampling property test: sample_pct=10 → ~10% (±2pp) over 10_000 calls
    // -----------------------------------------------------------------------

    #[test]
    fn sampling_property_10pct() {
        let dir = tmp_dir();
        // threshold=0 so every call is above-threshold
        let log = logger(&dir, 0, 10);
        let scope = empty_scope();

        let calls = 10_000u64;
        for i in 0..calls {
            log.record(QueryKind::Select, 1, format!("SELECT {i}"), &scope);
        }
        flush(&log);

        let lines = read_log_lines(&dir);
        let emitted = lines.len() as u64;
        // Expected: 1000 ± 200 (2pp of 10_000)
        assert!(
            emitted >= 800 && emitted <= 1200,
            "sample_pct=10 over {calls} calls emitted {emitted} (expected 800..=1200)"
        );
    }

    // -----------------------------------------------------------------------
    // Adversarial SQL: CRLF / NUL / quote → escape-safe JSON
    // -----------------------------------------------------------------------

    #[test]
    fn adversarial_sql_is_escape_safe() {
        let payloads: &[(&str, &str)] = &[
            ("crlf", "SELECT 1\r\nDROP TABLE t--"),
            ("nul", "SELECT '\x00'"),
            ("quote", r#"SELECT "secret" FROM t"#),
            ("json_inject", r#"SELECT 1},"pwned":true,{"x":"#),
            ("low_ctrl", "SELECT \x01\x02\x07\x1f"),
            ("backslash", "SELECT 'C:\\path\\file'"),
        ];

        for (label, sql) in payloads {
            let dir = tmp_dir();
            let log = logger(&dir, 0, 100);
            let scope = empty_scope();

            log.record(QueryKind::Select, 1, sql.to_string(), &scope);
            flush(&log);

            let path = reddb_file::layout::legacy_slow_query_log_path(&dir);
            let body = std::fs::read_to_string(&path).unwrap_or_default();
            let line = body
                .lines()
                .find(|l| !l.is_empty())
                .unwrap_or_else(|| panic!("{label}: no line emitted"));

            // Must be a single JSONL row — no embedded newline.
            assert!(
                !line.contains('\n'),
                "{label}: embedded newline in JSONL row"
            );

            // Must parse as valid JSON.
            let v: crate::json::Value = crate::json::from_str(line)
                .unwrap_or_else(|e| panic!("{label}: not valid JSON: {e}\n{line:?}"));

            // SQL must round-trip correctly.
            let recovered = v.get("sql").and_then(|x| x.as_str()).unwrap_or("");
            assert_eq!(recovered, *sql, "{label}: SQL round-trip mismatch");
        }
    }

    // -----------------------------------------------------------------------
    // Exact-threshold boundary: duration == threshold emits
    // -----------------------------------------------------------------------

    #[test]
    fn at_threshold_boundary_emits() {
        let dir = tmp_dir();
        let log = logger(&dir, 50, 100);
        let scope = empty_scope();

        // Exactly at threshold: duration_ms < threshold_ms is false → emits.
        log.record(QueryKind::Select, 50, "SELECT 1".into(), &scope);
        flush(&log);
        let lines = read_log_lines(&dir);
        assert_eq!(lines.len(), 1, "duration == threshold should emit");
    }

    #[test]
    fn just_below_threshold_does_not_emit() {
        let dir = tmp_dir();
        let log = logger(&dir, 50, 100);
        let scope = empty_scope();

        log.record(QueryKind::Select, 49, "SELECT 1".into(), &scope);
        flush(&log);
        let lines = read_log_lines(&dir);
        assert!(lines.is_empty(), "duration < threshold must not emit");
    }
}
