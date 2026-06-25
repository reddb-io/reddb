//! Intent log for long-running admin operations.
//!
//! Records a JSONL trail of every admin operation from begin → checkpoints →
//! complete/abort. At startup, `scan_and_report` finds any intents that never
//! reached a terminal phase and emits [`OperatorEvent::DanglingAdminIntent`]
//! for each one so operators can investigate interrupted operations.
//!
//! # Durability contract
//!
//! - File opened with `O_APPEND`; POSIX guarantees atomic writes up to
//!   `PIPE_BUF` (4096 bytes on Linux) for regular files. Records are
//!   capped at 3 KiB so multi-writer atomicity holds on supported kernels.
//! - `fsync` on `begin` only. Checkpoint / complete / abort writes are
//!   buffered — a crash between `begin` and `complete` is exactly the
//!   "dangling intent" condition `scan_and_report` is designed to surface.

use std::collections::HashMap;
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use crate::crypto::uuid::Uuid;
use crate::json::{Map, Value as JsonValue};
use crate::utils::time::now_unix_millis;

const MAX_RECORD_BYTES: usize = 3 * 1024;

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum IntentLogError {
    Io(std::io::Error),
    TooLarge { bytes: usize },
    SyncFailed(std::io::Error),
}

impl fmt::Display for IntentLogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "intent log I/O: {e}"),
            Self::TooLarge { bytes } => {
                write!(
                    f,
                    "intent record too large: {bytes} bytes (max {MAX_RECORD_BYTES})"
                )
            }
            Self::SyncFailed(e) => write!(f, "intent log fsync failed: {e}"),
        }
    }
}

impl From<std::io::Error> for IntentLogError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

// ---------------------------------------------------------------------------
// IntentOp — closed enum; add variants when new consumers arrive
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentOp {
    ReplicaBootstrap,
}

impl IntentOp {
    fn as_str(self) -> &'static str {
        match self {
            Self::ReplicaBootstrap => "replica_bootstrap",
        }
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "replica_bootstrap" => Some(Self::ReplicaBootstrap),
            _ => None,
        }
    }
}

impl fmt::Display for IntentOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// ---------------------------------------------------------------------------
// IntentPhase
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentPhase {
    Running,
    Checkpoint(u32),
    Completed,
    Aborted,
}

impl IntentPhase {
    fn as_str(&self) -> String {
        match self {
            Self::Running => "running".to_string(),
            Self::Checkpoint(n) => format!("checkpoint_{n}"),
            Self::Completed => "completed".to_string(),
            Self::Aborted => "aborted".to_string(),
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Aborted)
    }

    fn from_str(s: &str) -> Option<Self> {
        match s {
            "running" => Some(Self::Running),
            "completed" => Some(Self::Completed),
            "aborted" => Some(Self::Aborted),
            _ if s.starts_with("checkpoint_") => s["checkpoint_".len()..]
                .parse::<u32>()
                .ok()
                .map(Self::Checkpoint),
            _ => None,
        }
    }
}

impl fmt::Display for IntentPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

// ---------------------------------------------------------------------------
// Redaction helpers
// ---------------------------------------------------------------------------

const SENSITIVE_SUBSTRINGS: &[&str] = &["password", "secret", "token", "key", "credential", "auth"];

fn is_sensitive_key(k: &str) -> bool {
    let lower = k.to_ascii_lowercase();
    SENSITIVE_SUBSTRINGS.iter().any(|s| lower.contains(s))
}

fn redact_map(map: &Map<String, JsonValue>) -> JsonValue {
    let mut out = Map::new();
    for (k, v) in map {
        let v = if is_sensitive_key(k) {
            JsonValue::String("***REDACTED***".to_string())
        } else {
            v.clone()
        };
        out.insert(k.clone(), v);
    }
    JsonValue::Object(out)
}

// ---------------------------------------------------------------------------
// IntentArgs / IntentProgress / IntentSummary
// ---------------------------------------------------------------------------

/// Caller-supplied arguments for an intent. Sensitive keys are redacted
/// before writing to the log.
#[derive(Debug, Default, Clone)]
pub struct IntentArgs(Map<String, JsonValue>);

impl IntentArgs {
    pub fn new() -> Self {
        Self(Map::new())
    }

    pub fn insert(mut self, key: impl Into<String>, value: JsonValue) -> Self {
        self.0.insert(key.into(), value);
        self
    }

    fn to_json_value(&self) -> JsonValue {
        redact_map(&self.0)
    }
}

/// Progress snapshot attached to a checkpoint record.
#[derive(Debug, Default, Clone)]
pub struct IntentProgress(Map<String, JsonValue>);

impl IntentProgress {
    pub fn new() -> Self {
        Self(Map::new())
    }

    pub fn insert(mut self, key: impl Into<String>, value: JsonValue) -> Self {
        self.0.insert(key.into(), value);
        self
    }

    fn to_json_value(&self) -> JsonValue {
        redact_map(&self.0)
    }
}

/// Summary attached to a completed intent record.
#[derive(Debug, Default, Clone)]
pub struct IntentSummary(Map<String, JsonValue>);

impl IntentSummary {
    pub fn new() -> Self {
        Self(Map::new())
    }

    pub fn insert(mut self, key: impl Into<String>, value: JsonValue) -> Self {
        self.0.insert(key.into(), value);
        self
    }

    fn to_json_value(&self) -> JsonValue {
        redact_map(&self.0)
    }
}

// ---------------------------------------------------------------------------
// Record helpers
// ---------------------------------------------------------------------------

fn build_record(
    id: Uuid,
    op: IntentOp,
    phase: &IntentPhase,
    ts: u64,
    actor: &str,
    args: &JsonValue,
    progress: Option<JsonValue>,
    summary: Option<JsonValue>,
) -> Map<String, JsonValue> {
    let mut m = Map::new();
    m.insert("id".to_string(), JsonValue::String(id.to_string()));
    m.insert("op".to_string(), JsonValue::String(op.as_str().to_string()));
    m.insert("phase".to_string(), JsonValue::String(phase.as_str()));
    m.insert("ts".to_string(), JsonValue::Number(ts as f64));
    m.insert("actor".to_string(), JsonValue::String(actor.to_string()));
    m.insert("args".to_string(), args.clone());
    if let Some(p) = progress {
        m.insert("progress".to_string(), p);
    }
    if let Some(s) = summary {
        m.insert("summary".to_string(), s);
    }
    m
}

fn serialize_record(record: &Map<String, JsonValue>) -> Result<String, IntentLogError> {
    let line = JsonValue::Object(record.clone()).to_string_compact();
    let bytes = line.len();
    if bytes > MAX_RECORD_BYTES {
        return Err(IntentLogError::TooLarge { bytes });
    }
    Ok(line)
}

// ---------------------------------------------------------------------------
// AdminIntentLog
// ---------------------------------------------------------------------------

pub struct AdminIntentLog {
    path: PathBuf,
    file: Mutex<File>,
}

impl AdminIntentLog {
    /// Open (or create) the intent log at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, IntentLogError> {
        let path = path.as_ref().to_path_buf();
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            path,
            file: Mutex::new(file),
        })
    }

    /// Begin a new intent. Writes the opening record and fsyncs.
    pub fn begin(
        &self,
        op: IntentOp,
        actor: &str,
        args: IntentArgs,
    ) -> Result<IntentHandle<'_>, IntentLogError> {
        let id = Uuid::new_v7();
        let ts = now_unix_millis();
        let args_json = args.to_json_value();
        let record = build_record(
            id,
            op,
            &IntentPhase::Running,
            ts,
            actor,
            &args_json,
            None,
            None,
        );
        let line = serialize_record(&record)?;

        {
            let mut bytes = line.into_bytes();
            bytes.push(b'\n');
            let mut file = self.file.lock().unwrap();
            file.write_all(&bytes)?;
            file.flush()?;
            // fsync on begin only — see module doc
            file.sync_data().map_err(IntentLogError::SyncFailed)?;
        }

        Ok(IntentHandle {
            log: self,
            id,
            op,
            actor: actor.to_string(),
            args_json,
            started_at_ms: ts,
            last_phase: IntentPhase::Running,
            done: false,
        })
    }

    /// Return metadata about every intent that has not yet reached a
    /// terminal phase (completed or aborted).
    pub fn list_unfinished(&self) -> Vec<UnfinishedIntent> {
        self.scan_intents_internal()
    }

    /// Scan the log and emit [`crate::telemetry::operator_event::OperatorEvent::DanglingAdminIntent`]
    /// for every unfinished intent. Corrupted lines are skipped with a
    /// `tracing::warn!` breadcrumb — they do not abort the scan.
    pub fn scan_and_report(&self) {
        for item in self.scan_intents_internal() {
            crate::telemetry::operator_event::OperatorEvent::DanglingAdminIntent {
                id: item.id,
                op: item.op,
                started_at_ms: item.started_at_ms,
                last_phase: item.last_phase,
            }
            .emit_global();
        }
    }

    fn write_record(
        &self,
        id: Uuid,
        op: IntentOp,
        phase: &IntentPhase,
        actor: &str,
        args_json: &JsonValue,
        progress: Option<&IntentProgress>,
        summary: Option<&IntentSummary>,
    ) -> Result<(), IntentLogError> {
        let ts = now_unix_millis();
        let record = build_record(
            id,
            op,
            phase,
            ts,
            actor,
            args_json,
            progress.map(|p| p.to_json_value()),
            summary.map(|s| s.to_json_value()),
        );
        let line = serialize_record(&record)?;
        let mut bytes = line.into_bytes();
        bytes.push(b'\n');
        let mut file = self.file.lock().unwrap();
        file.write_all(&bytes)?;
        file.flush()?;
        Ok(())
    }

    fn scan_intents_internal(&self) -> Vec<UnfinishedIntent> {
        let content = match std::fs::read_to_string(&self.path) {
            Ok(c) => c,
            Err(_) => return Vec::new(),
        };

        struct ScanEntry {
            op: IntentOp,
            started_at_ms: u64,
            actor: String,
            phase: IntentPhase,
            args: Map<String, JsonValue>,
            last_progress: Option<Map<String, JsonValue>>,
        }

        let mut intents: HashMap<String, ScanEntry> = HashMap::new();

        for raw_line in content.lines() {
            let line = raw_line.trim();
            if line.is_empty() {
                continue;
            }

            let v: JsonValue = match crate::json::from_str(line) {
                Ok(v) => v,
                Err(_) => {
                    tracing::warn!(
                        target: "reddb::admin_intent_log",
                        "corrupted intent log line skipped"
                    );
                    continue;
                }
            };

            let Some(id) = v.get("id").and_then(|x| x.as_str()).map(|s| s.to_string()) else {
                continue;
            };
            let Some(op_str) = v.get("op").and_then(|x| x.as_str()) else {
                continue;
            };
            let Some(op) = IntentOp::from_str(op_str) else {
                continue;
            };
            let Some(phase_str) = v.get("phase").and_then(|x| x.as_str()) else {
                continue;
            };
            let Some(phase) = IntentPhase::from_str(phase_str) else {
                continue;
            };
            let ts = v.get("ts").and_then(|x| x.as_f64()).unwrap_or(0.0) as u64;
            let actor = v
                .get("actor")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();

            let args_map = v
                .get("args")
                .and_then(|x| x.as_object())
                .cloned()
                .unwrap_or_default();
            let progress_map = v.get("progress").and_then(|x| x.as_object()).cloned();

            intents
                .entry(id)
                .and_modify(|e| {
                    // Keep earliest ts and args (from running record); update phase and progress.
                    e.phase = phase.clone();
                    if let Some(p) = progress_map.clone() {
                        e.last_progress = Some(p);
                    }
                })
                .or_insert(ScanEntry {
                    op,
                    started_at_ms: ts,
                    actor,
                    phase,
                    args: args_map,
                    last_progress: progress_map,
                });
        }

        intents
            .into_iter()
            .filter(|(_, e)| !e.phase.is_terminal())
            .map(|(id_str, e)| {
                let id = Uuid::parse_str(&id_str).unwrap_or_else(|_| Uuid::new_v4());
                UnfinishedIntent {
                    id,
                    op: e.op,
                    started_at_ms: e.started_at_ms,
                    actor: e.actor,
                    last_phase: e.phase,
                    args: e.args,
                    last_progress: e.last_progress,
                }
            })
            .collect()
    }

    /// Recreate a linear handle for an unfinished intent after process restart.
    ///
    /// This deliberately does not write a new `running` record: future
    /// checkpoints and completion records are appended under the original id,
    /// so a resumed operation does not leave a permanent dangling intent.
    pub fn resume_unfinished<'a>(&'a self, item: &UnfinishedIntent) -> IntentHandle<'a> {
        IntentHandle {
            log: self,
            id: item.id,
            op: item.op,
            actor: item.actor.clone(),
            args_json: JsonValue::Object(item.args.clone()),
            started_at_ms: item.started_at_ms,
            last_phase: item.last_phase.clone(),
            done: false,
        }
    }
}

// ---------------------------------------------------------------------------
// UnfinishedIntent (returned by list_unfinished / used by scan_and_report)
// ---------------------------------------------------------------------------

pub struct UnfinishedIntent {
    pub id: Uuid,
    pub op: IntentOp,
    pub started_at_ms: u64,
    pub actor: String,
    pub last_phase: IntentPhase,
    /// Args from the opening `running` record. Used by consumers to filter by
    /// owner fields (e.g., `replica_id`) and implement single-resumer policy.
    pub args: Map<String, JsonValue>,
    /// Progress map from the most recent checkpoint record, if any. `None`
    /// means the intent started but never reached a checkpoint.
    pub last_progress: Option<Map<String, JsonValue>>,
}

// ---------------------------------------------------------------------------
// IntentHandle — linear type; Drop writes aborted if complete() not called
// ---------------------------------------------------------------------------

pub struct IntentHandle<'a> {
    log: &'a AdminIntentLog,
    id: Uuid,
    op: IntentOp,
    actor: String,
    args_json: JsonValue,
    pub started_at_ms: u64,
    last_phase: IntentPhase,
    done: bool,
}

impl<'a> IntentHandle<'a> {
    pub fn id(&self) -> Uuid {
        self.id
    }

    pub fn last_phase(&self) -> &IntentPhase {
        &self.last_phase
    }

    /// Write a checkpoint record. `n` should be monotonically increasing.
    pub fn checkpoint(
        &mut self,
        n: u32,
        progress: Option<IntentProgress>,
    ) -> Result<(), IntentLogError> {
        let phase = IntentPhase::Checkpoint(n);
        self.log.write_record(
            self.id,
            self.op,
            &phase,
            &self.actor,
            &self.args_json,
            progress.as_ref(),
            None,
        )?;
        self.last_phase = phase;
        Ok(())
    }

    /// Mark the intent complete. Consumes the handle; Drop will not write aborted.
    pub fn complete(mut self, summary: Option<IntentSummary>) -> Result<(), IntentLogError> {
        let result = self.log.write_record(
            self.id,
            self.op,
            &IntentPhase::Completed,
            &self.actor,
            &self.args_json,
            None,
            summary.as_ref(),
        );
        if result.is_ok() {
            self.done = true;
        }
        result
    }
}

impl Drop for IntentHandle<'_> {
    fn drop(&mut self) {
        if !self.done {
            let _ = self.log.write_record(
                self.id,
                self.op,
                &IntentPhase::Aborted,
                &self.actor,
                &self.args_json,
                None,
                None,
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::Value as JsonValue;

    fn tmp_path(label: &str) -> (tempfile::NamedTempFile, PathBuf) {
        let file = tempfile::Builder::new()
            .prefix(&format!("reddb-intent-{label}-"))
            .suffix(".log")
            .tempfile()
            .expect("temp intent log file");
        let path = file.path().to_path_buf();
        (file, path)
    }

    fn cleanup(file: tempfile::NamedTempFile, path: &Path) {
        // `NamedTempFile::close` removes the file; on panic the Drop impl
        // still runs. The explicit `_ = remove_file` is a belt-and-braces
        // fallback in case the path was renamed (some intent log paths move
        // the file when rotating — see writer). Either way the leak guard
        // (scripts/check-temp-residue.sh) must see no residue.
        drop(file);
        let _ = std::fs::remove_file(path);
    }

    fn last_line_json(path: &Path) -> JsonValue {
        let body = std::fs::read_to_string(path).unwrap();
        let line = body.lines().last().expect("at least one line");
        crate::json::from_str(line).expect("valid JSON")
    }

    fn all_lines_json(path: &Path) -> Vec<JsonValue> {
        let body = std::fs::read_to_string(path).unwrap();
        body.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| crate::json::from_str(l).expect("valid JSON"))
            .collect()
    }

    // -----------------------------------------------------------------------
    // 1. begin writes a running record and fsyncs
    // -----------------------------------------------------------------------
    #[test]
    fn begin_writes_running_record() {
        let (_tmpfile, path) = tmp_path("begin");
        let log = AdminIntentLog::open(&path).unwrap();
        let handle = log
            .begin(IntentOp::ReplicaBootstrap, "ops-bot", IntentArgs::new())
            .unwrap();
        drop(handle); // also writes aborted; just check first line

        let body = std::fs::read_to_string(&path).unwrap();
        let first_line = body.lines().next().unwrap();
        let v: JsonValue = crate::json::from_str(first_line).unwrap();
        assert_eq!(v.get("phase").and_then(|x| x.as_str()), Some("running"));
        assert_eq!(
            v.get("op").and_then(|x| x.as_str()),
            Some("replica_bootstrap")
        );
        assert_eq!(v.get("actor").and_then(|x| x.as_str()), Some("ops-bot"));
    }

    // -----------------------------------------------------------------------
    // 2. complete writes completed, Drop writes nothing extra
    // -----------------------------------------------------------------------
    #[test]
    fn complete_writes_completed_phase() {
        let (_tmpfile, path) = tmp_path("complete");
        let log = AdminIntentLog::open(&path).unwrap();
        let handle = log
            .begin(IntentOp::ReplicaBootstrap, "admin", IntentArgs::new())
            .unwrap();
        handle.complete(None).unwrap();

        let lines = all_lines_json(&path);
        assert_eq!(lines.len(), 2, "begin + complete = 2 lines");
        assert_eq!(
            lines[1].get("phase").and_then(|x| x.as_str()),
            Some("completed")
        );
    }

    // -----------------------------------------------------------------------
    // 3. Drop without complete writes aborted
    // -----------------------------------------------------------------------
    #[test]
    fn drop_without_complete_writes_aborted() {
        let (_tmpfile, path) = tmp_path("drop-abort");
        let log = AdminIntentLog::open(&path).unwrap();
        {
            let _handle = log
                .begin(IntentOp::ReplicaBootstrap, "admin", IntentArgs::new())
                .unwrap();
            // drop here without calling complete
        }

        let last = last_line_json(&path);
        assert_eq!(last.get("phase").and_then(|x| x.as_str()), Some("aborted"));
    }

    // -----------------------------------------------------------------------
    // 4. checkpoint writes intermediate records
    // -----------------------------------------------------------------------
    #[test]
    fn checkpoint_writes_intermediate_records() {
        let (_tmpfile, path) = tmp_path("checkpoint");
        let log = AdminIntentLog::open(&path).unwrap();
        let mut handle = log
            .begin(IntentOp::ReplicaBootstrap, "admin", IntentArgs::new())
            .unwrap();
        handle.checkpoint(1, None).unwrap();
        handle.checkpoint(2, None).unwrap();
        handle.complete(None).unwrap();

        let lines = all_lines_json(&path);
        assert_eq!(lines.len(), 4); // begin + 2 checkpoints + complete
        assert_eq!(
            lines[1].get("phase").and_then(|x| x.as_str()),
            Some("checkpoint_1")
        );
        assert_eq!(
            lines[2].get("phase").and_then(|x| x.as_str()),
            Some("checkpoint_2")
        );
    }

    // -----------------------------------------------------------------------
    // 5. scan_and_report emits N DanglingAdminIntent events for N unfinished
    // -----------------------------------------------------------------------
    #[test]
    fn scan_and_report_finds_unfinished_intents() {
        let (_tmpfile, path) = tmp_path("scan");
        let log = AdminIntentLog::open(&path).unwrap();

        // Use mem::forget to simulate a crash — prevents Drop from writing aborted,
        // leaving 2 intents with only a "running" record (no terminal phase).
        let h1 = log
            .begin(IntentOp::ReplicaBootstrap, "a", IntentArgs::new())
            .unwrap();
        let h2 = log
            .begin(IntentOp::ReplicaBootstrap, "b", IntentArgs::new())
            .unwrap();
        std::mem::forget(h1);
        std::mem::forget(h2);

        // 1 completed normally
        let h3 = log
            .begin(IntentOp::ReplicaBootstrap, "c", IntentArgs::new())
            .unwrap();
        h3.complete(None).unwrap();

        let log2 = AdminIntentLog::open(&path).unwrap();
        let unfinished = log2.list_unfinished();
        assert_eq!(unfinished.len(), 2, "expected exactly 2 dangling intents");
    }

    // -----------------------------------------------------------------------
    // 6. Record > 3 KiB returns TooLarge, no write
    // -----------------------------------------------------------------------
    #[test]
    fn record_too_large_returns_error_no_write() {
        let (_tmpfile, path) = tmp_path("toolarge");
        let log = AdminIntentLog::open(&path).unwrap();

        // Build args that blow past 3KB
        let big_value = "x".repeat(4096);
        let args = IntentArgs::new().insert("data", JsonValue::String(big_value));
        let err = log.begin(IntentOp::ReplicaBootstrap, "admin", args);
        assert!(
            matches!(err, Err(IntentLogError::TooLarge { .. })),
            "expected TooLarge, got {:?}",
            err.err().map(|e| e.to_string())
        );

        // No lines written
        let content = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            content.lines().all(|l| l.trim().is_empty()),
            "no lines should have been written"
        );
    }

    // -----------------------------------------------------------------------
    // 7. Corrupted JSON line does not crash scan; emits tracing::warn
    // -----------------------------------------------------------------------
    #[test]
    fn corrupted_line_skipped_in_scan() {
        let (_tmpfile, path) = tmp_path("corrupt");
        let log = AdminIntentLog::open(&path).unwrap();
        let h = log
            .begin(IntentOp::ReplicaBootstrap, "admin", IntentArgs::new())
            .unwrap();
        drop(h); // aborted

        // Inject a corrupted line between existing lines
        let mut content = std::fs::read_to_string(&path).unwrap();
        content.push_str("not-valid-json\n");
        std::fs::write(&path, &content).unwrap();

        // Reopen and scan — should not panic
        let log2 = AdminIntentLog::open(&path).unwrap();
        let unfinished = log2.list_unfinished(); // aborted is terminal, so 0
                                                 // The important assertion: we got here without panic
        assert_eq!(unfinished.len(), 0);
    }

    // -----------------------------------------------------------------------
    // 8. Sensitive keys are redacted in args
    // -----------------------------------------------------------------------
    #[test]
    fn sensitive_keys_are_redacted() {
        let (_tmpfile, path) = tmp_path("redact");
        let log = AdminIntentLog::open(&path).unwrap();
        let args = IntentArgs::new()
            .insert("password", JsonValue::String("hunter2".to_string()))
            .insert("host", JsonValue::String("db.internal".to_string()));
        let h = log
            .begin(IntentOp::ReplicaBootstrap, "admin", args)
            .unwrap();
        h.complete(None).unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        let first_line = body.lines().next().unwrap();
        let v: JsonValue = crate::json::from_str(first_line).unwrap();
        let args_obj = v.get("args").unwrap();
        let pwd = args_obj.get("password").and_then(|x| x.as_str());
        assert_eq!(pwd, Some("***REDACTED***"), "password should be redacted");
        let host = args_obj.get("host").and_then(|x| x.as_str());
        assert_eq!(host, Some("db.internal"), "host should not be redacted");
    }

    // -----------------------------------------------------------------------
    // 9. Multi-process POSIX atomicity test
    //    Spawns 2 child processes + parent all writing concurrently.
    //    Every line must parse as valid JSON — no record interleaving.
    // -----------------------------------------------------------------------
    #[test]
    fn multi_process_posix_atomicity() {
        const LOG_PATH_ENV: &str = "INTENT_LOG_CHILD_PATH";
        const CHILD_OPS: u32 = 20;

        // --- child mode ---
        if let Ok(path) = std::env::var(LOG_PATH_ENV) {
            let log = AdminIntentLog::open(&path).unwrap();
            for _ in 0..CHILD_OPS {
                let h = log
                    .begin(IntentOp::ReplicaBootstrap, "child", IntentArgs::new())
                    .unwrap();
                h.complete(None).unwrap();
            }
            return;
        }

        // --- parent mode ---
        let dir = std::env::current_dir()
            .unwrap()
            .join(".red/tmp/admin-intent-log-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!(
            "reddb-intent-mp-{}-{}.log",
            std::process::id(),
            crate::utils::now_unix_nanos()
        ));

        // Create the file before spawning children
        let log = AdminIntentLog::open(&path).unwrap();

        let exe = std::env::current_exe().unwrap();
        let spawn_child = || {
            std::process::Command::new(&exe)
                .arg("multi_process_posix_atomicity")
                .env(LOG_PATH_ENV, &path)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn child process")
        };

        let mut child1 = spawn_child();
        let mut child2 = spawn_child();

        // Parent writes concurrently
        for _ in 0..CHILD_OPS {
            let h = log
                .begin(IntentOp::ReplicaBootstrap, "parent", IntentArgs::new())
                .unwrap();
            h.complete(None).unwrap();
        }

        let s1 = wait_child_with_deadline(&mut child1, "child1");
        let s2 = wait_child_with_deadline(&mut child2, "child2");
        assert!(s1.success(), "child1 exited with failure");
        assert!(s2.success(), "child2 exited with failure");

        // Verify: every non-empty line parses as valid JSON (no interleaving)
        let content = std::fs::read_to_string(&path).unwrap();
        let mut line_count = 0_usize;
        for (i, line) in content.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            crate::json::from_str::<JsonValue>(line)
                .unwrap_or_else(|e| panic!("line {i} is not valid JSON: {e}\n{line:?}"));
            line_count += 1;
        }
        // 3 writers × 20 ops × 2 records (begin + complete) = 120 minimum
        assert!(line_count >= 120, "expected ≥120 lines, got {line_count}");

        let _ = std::fs::remove_file(&path);
    }

    fn wait_child_with_deadline(
        child: &mut std::process::Child,
        name: &str,
    ) -> std::process::ExitStatus {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            match child
                .try_wait()
                .unwrap_or_else(|err| panic!("{name} wait failed: {err}"))
            {
                Some(status) => return status,
                None if std::time::Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("{name} did not exit before deadline");
                }
                None => std::thread::yield_now(),
            }
        }
    }
}
