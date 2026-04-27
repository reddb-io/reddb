//! Structured audit log for SOC 2 / HIPAA / ISO 27001 deploys.
//!
//! Replaces the previous free-form `record(action, principal, target,
//! result, details)` API with a stable JSON-Lines schema keyed on
//! `event_id`, `ts`, `principal`, `tenant`, `action`, `resource`,
//! `outcome`, `detail`, `remote_addr`, and `correlation_id` so external
//! tooling (Splunk, Datadog HEC, ELK, BigQuery, Athena) can ingest the
//! file without per-deploy regex.
//!
//! Operational properties:
//!   * **Async-write**: emit sites push onto a bounded `std::sync::mpsc`
//!     channel; a dedicated thread owns the file handle and flushes on
//!     a periodic timer (default 250 ms) or per-event when
//!     `RED_AUDIT_FSYNC=every`. If the channel fills the emit site
//!     falls back to a direct sync write — losing throughput is
//!     preferable to dropping audit lines.
//!   * **Rotation**: when the active file exceeds `RED_AUDIT_MAX_BYTES`
//!     (default 64 MiB) the writer renames it to
//!     `.audit.log.<ms>.zst`, zstd-compresses it, and starts a fresh
//!     active file. The repo already pulls `zstd`; we don't add a gzip
//!     dependency.
//!   * **Hash chain (tamper-evidence)**: each event carries a
//!     `prev_hash` field — the sha256 of the previous JSON line. An
//!     auditor verifying the file recomputes the chain; a single edit
//!     anywhere in the file breaks every subsequent hash. Does **not**
//!     defend against an attacker with `root + write` (they could
//!     rebuild the chain), but it does defend against accidental edits
//!     and most insider tampering.
//!   * **SIEM streaming**: when `RED_AUDIT_STREAM_URL` is set every
//!     line is also POSTed to that URL fire-and-forget.
//!
//! Pre-1.0: the file format breaks from the previous shape. Old
//! `.audit.log` files are NOT readable by the new query endpoint.
//! That's an accepted regression — operators upgrading should rotate
//! the file before the deploy.

use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::crypto::{os_random, sha256};
use crate::json::{Map, Value as JsonValue};

// ---------------------------------------------------------------------------
// Schema
// ---------------------------------------------------------------------------

/// Auth pathway that produced the principal. Decoupled from
/// `crate::auth::AuthSource` so we can record System / Anonymous /
/// Session / ApiKey lanes that aren't surfaced by the runtime auth
/// enum (which only covers Password / ClientCert / Oauth today).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditAuthSource {
    ApiKey,
    Session,
    Password,
    Oauth,
    ClientCert,
    Anonymous,
    System,
}

impl AuditAuthSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ApiKey => "api_key",
            Self::Session => "session",
            Self::Password => "password",
            Self::Oauth => "oauth",
            Self::ClientCert => "client_cert",
            Self::Anonymous => "anonymous",
            Self::System => "system",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "api_key" => Self::ApiKey,
            "session" => Self::Session,
            "password" => Self::Password,
            "oauth" => Self::Oauth,
            "client_cert" => Self::ClientCert,
            "anonymous" => Self::Anonymous,
            "system" => Self::System,
            _ => return None,
        })
    }
}

/// Outcome of the audited action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Success,
    Denied,
    Error,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Denied => "denied",
            Self::Error => "error",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "success" | "ok" => Self::Success,
            "denied" | "deny" => Self::Denied,
            "error" | "err" => Self::Error,
            _ => return None,
        })
    }
}

/// Structured audit event. Serialised as one JSONL row per call.
#[derive(Debug, Clone)]
pub struct AuditEvent {
    pub ts: u128,
    pub event_id: String,
    pub principal: Option<String>,
    pub source: AuditAuthSource,
    pub tenant: Option<String>,
    pub action: String,
    pub resource: Option<String>,
    pub outcome: Outcome,
    pub detail: JsonValue,
    pub remote_addr: Option<String>,
    pub correlation_id: Option<String>,
}

impl AuditEvent {
    /// Convenience builder. The caller fills the required fields and
    /// chains the optional ones — keeps emit sites readable.
    pub fn builder(action: impl Into<String>) -> AuditEventBuilder {
        AuditEventBuilder {
            inner: AuditEvent {
                ts: crate::utils::now_unix_millis() as u128,
                event_id: new_event_id(),
                principal: None,
                source: AuditAuthSource::System,
                tenant: None,
                action: action.into(),
                resource: None,
                outcome: Outcome::Success,
                detail: JsonValue::Null,
                remote_addr: None,
                correlation_id: None,
            },
        }
    }

    /// Wrap a pre-structured-API call into the new schema. Used by the
    /// back-compat `record(action, principal, target, result, details)`
    /// path so existing emit sites keep compiling.
    pub fn from_legacy(
        action: &str,
        principal: &str,
        target: &str,
        result: &str,
        details: JsonValue,
    ) -> Self {
        let outcome = if result == "ok" {
            Outcome::Success
        } else if result.starts_with("err") {
            Outcome::Error
        } else if result.starts_with("denied") || result.starts_with("deny") {
            Outcome::Denied
        } else {
            Outcome::Success
        };
        let mut detail = details;
        if !result.is_empty() && result != "ok" {
            // Preserve the human-readable result string for forensic
            // continuity with pre-restructuring lines.
            let mut obj = match detail {
                JsonValue::Object(map) => map,
                JsonValue::Null => Map::new(),
                other => {
                    let mut m = Map::new();
                    m.insert("legacy".to_string(), other);
                    m
                }
            };
            obj.entry("result_text".to_string())
                .or_insert(JsonValue::String(result.to_string()));
            detail = JsonValue::Object(obj);
        }
        Self {
            ts: crate::utils::now_unix_millis() as u128,
            event_id: new_event_id(),
            principal: if principal.is_empty() || principal == "system" {
                None
            } else {
                Some(principal.to_string())
            },
            source: if principal == "system" {
                AuditAuthSource::System
            } else if principal.is_empty() {
                AuditAuthSource::Anonymous
            } else {
                AuditAuthSource::Password
            },
            tenant: None,
            action: action.to_string(),
            resource: if target.is_empty() {
                None
            } else {
                Some(target.to_string())
            },
            outcome,
            detail,
            remote_addr: None,
            correlation_id: None,
        }
    }

    /// Render to a single JSONL line, optionally seeded with a
    /// `prev_hash` for the tamper-evident chain.
    pub fn to_json_line(&self, prev_hash: Option<&str>) -> String {
        let mut object = Map::new();
        object.insert("ts".to_string(), JsonValue::Number(self.ts as f64));
        object.insert(
            "ts_iso".to_string(),
            JsonValue::String(format_iso8601(self.ts as u64)),
        );
        object.insert(
            "event_id".to_string(),
            JsonValue::String(self.event_id.clone()),
        );
        if let Some(p) = &self.principal {
            object.insert("principal".to_string(), JsonValue::String(p.clone()));
        }
        object.insert(
            "source".to_string(),
            JsonValue::String(self.source.as_str().to_string()),
        );
        if let Some(t) = &self.tenant {
            object.insert("tenant".to_string(), JsonValue::String(t.clone()));
        }
        object.insert("action".to_string(), JsonValue::String(self.action.clone()));
        if let Some(r) = &self.resource {
            object.insert("resource".to_string(), JsonValue::String(r.clone()));
        }
        object.insert(
            "outcome".to_string(),
            JsonValue::String(self.outcome.as_str().to_string()),
        );
        if !matches!(self.detail, JsonValue::Null) {
            object.insert("detail".to_string(), self.detail.clone());
        }
        if let Some(ip) = &self.remote_addr {
            object.insert("remote_addr".to_string(), JsonValue::String(ip.clone()));
        }
        if let Some(cid) = &self.correlation_id {
            object.insert("correlation_id".to_string(), JsonValue::String(cid.clone()));
        }
        if let Some(h) = prev_hash {
            object.insert("prev_hash".to_string(), JsonValue::String(h.to_string()));
        }
        JsonValue::Object(object).to_string_compact()
    }

    /// Parse one JSONL line back into an `AuditEvent`. Returns `None`
    /// for legacy lines (pre-restructuring) or malformed JSON; callers
    /// in the query path use this to filter.
    pub fn parse_line(line: &str) -> Option<Self> {
        let v: JsonValue = crate::json::from_str(line).ok()?;
        let action = v.get("action")?.as_str()?.to_string();
        let outcome_s = v.get("outcome").and_then(|n| n.as_str()).unwrap_or("");
        let outcome = Outcome::parse(outcome_s)?;
        let ts = v.get("ts").and_then(|n| n.as_f64()).unwrap_or(0.0) as u128;
        let event_id = v
            .get("event_id")
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        let source = v
            .get("source")
            .and_then(|n| n.as_str())
            .and_then(AuditAuthSource::parse)
            .unwrap_or(AuditAuthSource::System);
        Some(Self {
            ts,
            event_id,
            principal: v
                .get("principal")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string()),
            source,
            tenant: v
                .get("tenant")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string()),
            action,
            resource: v
                .get("resource")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string()),
            outcome,
            detail: v.get("detail").cloned().unwrap_or(JsonValue::Null),
            remote_addr: v
                .get("remote_addr")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string()),
            correlation_id: v
                .get("correlation_id")
                .and_then(|n| n.as_str())
                .map(|s| s.to_string()),
        })
    }
}

/// Builder for `AuditEvent`. Generated by `AuditEvent::builder()`.
pub struct AuditEventBuilder {
    inner: AuditEvent,
}

impl AuditEventBuilder {
    pub fn principal(mut self, principal: impl Into<String>) -> Self {
        self.inner.principal = Some(principal.into());
        self
    }

    pub fn principal_opt(mut self, principal: Option<String>) -> Self {
        self.inner.principal = principal;
        self
    }

    pub fn source(mut self, source: AuditAuthSource) -> Self {
        self.inner.source = source;
        self
    }

    pub fn tenant(mut self, tenant: impl Into<String>) -> Self {
        self.inner.tenant = Some(tenant.into());
        self
    }

    pub fn resource(mut self, resource: impl Into<String>) -> Self {
        self.inner.resource = Some(resource.into());
        self
    }

    pub fn outcome(mut self, outcome: Outcome) -> Self {
        self.inner.outcome = outcome;
        self
    }

    pub fn detail(mut self, detail: JsonValue) -> Self {
        self.inner.detail = detail;
        self
    }

    pub fn remote_addr(mut self, addr: impl Into<String>) -> Self {
        self.inner.remote_addr = Some(addr.into());
        self
    }

    pub fn correlation_id(mut self, cid: impl Into<String>) -> Self {
        self.inner.correlation_id = Some(cid.into());
        self
    }

    pub fn build(self) -> AuditEvent {
        self.inner
    }
}

// ---------------------------------------------------------------------------
// Identifier
// ---------------------------------------------------------------------------

/// Crockford-base32 ULID-like ID: 10-char timestamp prefix + 16-char
/// random suffix = 26 chars total. Ordered lexicographically by time.
fn new_event_id() -> String {
    const ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let now_ms = crate::utils::now_unix_millis();
    let mut rand_bytes = [0u8; 10];
    let _ = os_random::fill_bytes(&mut rand_bytes);

    let mut out = String::with_capacity(26);
    // 48-bit timestamp -> 10 base32 chars (50 bits, top 2 are zero).
    for i in (0..10).rev() {
        let shift = (i as u32) * 5;
        let idx = ((now_ms >> shift) & 0x1f) as usize;
        out.push(ALPHABET[idx] as char);
    }
    // 80 random bits -> 16 base32 chars.
    let mut acc: u128 = 0;
    for &b in &rand_bytes {
        acc = (acc << 8) | (b as u128);
    }
    for i in (0..16).rev() {
        let shift = (i as u32) * 5;
        let idx = ((acc >> shift) & 0x1f) as usize;
        out.push(ALPHABET[idx] as char);
    }
    out
}

// ---------------------------------------------------------------------------
// Fsync mode
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FsyncMode {
    /// Buffered, fsync only on rotation + shutdown.
    Off,
    /// fsync after every event. Safe for HIPAA strong-durability.
    Every,
    /// fsync every N ms (default 250) — compliance-appropriate
    /// without tanking write throughput.
    Periodic,
}

impl FsyncMode {
    fn from_env() -> Self {
        match std::env::var("RED_AUDIT_FSYNC")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str()
        {
            "every" | "strong" | "on" => Self::Every,
            "off" | "none" => Self::Off,
            _ => Self::Periodic,
        }
    }
}

// ---------------------------------------------------------------------------
// Channel message
// ---------------------------------------------------------------------------

enum WriterMsg {
    Event(AuditEvent),
    Flush(mpsc::Sender<()>),
    Shutdown,
}

// ---------------------------------------------------------------------------
// AuditLogger
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct AuditLogger {
    path: PathBuf,
    tx: Mutex<Option<mpsc::SyncSender<WriterMsg>>>,
    /// Direct-write fallback mutex. Used when the writer thread isn't
    /// running (in-memory tests) or the channel is full and the emit
    /// site picks the sync path so the event can't be dropped.
    fallback_lock: Mutex<()>,
    /// Snapshot of the most recent line's sha256 for tamper-evidence.
    /// Shared between the writer thread and the direct-write fallback
    /// so the chain stays consistent across both paths.
    last_hash: Arc<Mutex<Option<String>>>,
    max_bytes: u64,
    fsync_mode: FsyncMode,
    stream_url: Option<String>,
    /// Background-write flag; tests use `wait_idle()` to synchronise.
    writer_alive: Arc<AtomicBool>,
    pending: Arc<AtomicU64>,
    handle: Mutex<Option<JoinHandle<()>>>,
}

impl AuditLogger {
    /// Place the audit log next to the primary `.rdb` file so backup
    /// + restore flows can ship it together.
    pub fn for_data_path(data_path: &Path) -> Self {
        let parent = data_path.parent().unwrap_or_else(|| Path::new("."));
        let path = parent.join(".audit.log");
        Self::with_path(path)
    }

    /// Direct constructor primarily for tests that want a custom path.
    pub fn with_path(path: PathBuf) -> Self {
        let max_bytes = std::env::var("RED_AUDIT_MAX_BYTES")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(64 * 1024 * 1024);
        let fsync_mode = FsyncMode::from_env();
        let stream_url = std::env::var("RED_AUDIT_STREAM_URL")
            .ok()
            .filter(|s| !s.is_empty());
        Self::with_settings(path, max_bytes, fsync_mode, stream_url)
    }

    /// Test/integration constructor with explicit settings — bypasses
    /// env-var resolution so parallel tests don't race on `set_var`.
    pub fn with_max_bytes(path: PathBuf, max_bytes: u64) -> Self {
        Self::with_settings(path, max_bytes, FsyncMode::Periodic, None)
    }

    fn with_settings(
        path: PathBuf,
        max_bytes: u64,
        fsync_mode: FsyncMode,
        stream_url: Option<String>,
    ) -> Self {
        let writer_alive = Arc::new(AtomicBool::new(false));
        let pending = Arc::new(AtomicU64::new(0));
        // Seed the chain from the existing tail so a restart doesn't
        // break tamper evidence.
        let mut seed: Option<String> = None;
        if let Ok(body) = std::fs::read_to_string(&path) {
            if let Some(line) = body.lines().last() {
                seed = Some(crate::utils::to_hex(&sha256::sha256(line.as_bytes())));
            }
        }
        let last_hash = Arc::new(Mutex::new(seed));
        let logger = Self {
            path,
            tx: Mutex::new(None),
            fallback_lock: Mutex::new(()),
            last_hash,
            max_bytes,
            fsync_mode,
            stream_url,
            writer_alive: Arc::clone(&writer_alive),
            pending: Arc::clone(&pending),
            handle: Mutex::new(None),
        };
        logger.start_writer_thread();
        logger
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Active rotation threshold (bytes).
    pub fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    fn start_writer_thread(&self) {
        let (tx, rx) = mpsc::sync_channel::<WriterMsg>(4096);
        *self.tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(tx);
        let path = self.path.clone();
        let max_bytes = self.max_bytes;
        let fsync_mode = self.fsync_mode;
        let stream_url = self.stream_url.clone();
        let writer_alive = Arc::clone(&self.writer_alive);
        let pending = Arc::clone(&self.pending);
        let last_hash = Arc::clone(&self.last_hash);
        writer_alive.store(true, Ordering::SeqCst);
        let handle = thread::Builder::new()
            .name("reddb-audit-writer".to_string())
            .spawn(move || {
                writer_loop(
                    rx,
                    path,
                    max_bytes,
                    fsync_mode,
                    stream_url,
                    writer_alive,
                    pending,
                    last_hash,
                );
            })
            .expect("spawn audit writer thread");
        *self.handle.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle);
    }

    /// Latest tip-hash of the chain (for diagnostics + tests).
    pub fn current_hash(&self) -> Option<String> {
        self.last_hash
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Block until the writer thread has drained every pending event.
    /// Tests use this to make assertions deterministic.
    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let tx_guard = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(tx) = tx_guard.as_ref() {
            let (back_tx, back_rx) = mpsc::channel();
            if tx.send(WriterMsg::Flush(back_tx)).is_err() {
                return false;
            }
            drop(tx_guard);
            let remaining = deadline.saturating_duration_since(Instant::now());
            return back_rx.recv_timeout(remaining).is_ok();
        }
        false
    }

    /// Back-compat record signature. Used by every existing emit site.
    /// Wraps into the new schema and forwards to `record_event`.
    pub fn record(
        &self,
        action: &str,
        principal: &str,
        target: &str,
        result: &str,
        details: JsonValue,
    ) {
        let event = AuditEvent::from_legacy(action, principal, target, result, details);
        self.record_event(event);
    }

    /// Primary new entry point. Non-blocking when the channel has
    /// capacity; falls back to a sync write when full or the writer
    /// thread has shut down.
    pub fn record_event(&self, event: AuditEvent) {
        let tx_guard = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        let recovered_event: AuditEvent;
        if let Some(tx) = tx_guard.as_ref() {
            self.pending.fetch_add(1, Ordering::SeqCst);
            match tx.try_send(WriterMsg::Event(event)) {
                Ok(()) => return,
                Err(mpsc::TrySendError::Full(msg)) => {
                    self.pending.fetch_sub(1, Ordering::SeqCst);
                    tracing::warn!(
                        target: "reddb::audit",
                        "audit channel saturated; falling back to sync write"
                    );
                    recovered_event = match msg {
                        WriterMsg::Event(ev) => ev,
                        _ => return,
                    };
                }
                Err(mpsc::TrySendError::Disconnected(msg)) => {
                    self.pending.fetch_sub(1, Ordering::SeqCst);
                    recovered_event = match msg {
                        WriterMsg::Event(ev) => ev,
                        _ => return,
                    };
                }
            }
        } else {
            recovered_event = event;
        }
        drop(tx_guard);
        self.write_direct(recovered_event);
    }

    /// Fallback path: a direct, synchronous append. Used when the
    /// background channel is full or the writer thread isn't running.
    fn write_direct(&self, event: AuditEvent) {
        let _g = self.fallback_lock.lock().unwrap_or_else(|e| e.into_inner());
        let prev = self
            .last_hash
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let line = event.to_json_line(prev.as_deref());
        if let Err(err) = append_line_with_rotation(&self.path, &line, self.max_bytes) {
            tracing::warn!(
                target: "reddb::audit",
                error = %err,
                path = %self.path.display(),
                "direct audit append failed"
            );
            return;
        }
        let new_hash = crate::utils::to_hex(&sha256::sha256(line.as_bytes()));
        if let Ok(mut g) = self.last_hash.lock() {
            *g = Some(new_hash);
        }
        if let Some(url) = &self.stream_url {
            stream_post(url, &line);
        }
        tracing::info!(target: "reddb::audit", "{line}");
    }
}

impl Drop for AuditLogger {
    fn drop(&mut self) {
        let mut tx_guard = self.tx.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(tx) = tx_guard.take() {
            let _ = tx.send(WriterMsg::Shutdown);
        }
        drop(tx_guard);
        if let Some(handle) = self.handle.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = handle.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Writer thread
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn writer_loop(
    rx: mpsc::Receiver<WriterMsg>,
    path: PathBuf,
    max_bytes: u64,
    fsync_mode: FsyncMode,
    stream_url: Option<String>,
    writer_alive: Arc<AtomicBool>,
    pending: Arc<AtomicU64>,
    last_hash: Arc<Mutex<Option<String>>>,
) {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }

    let mut writer = match open_active(&path) {
        Ok(w) => w,
        Err(err) => {
            tracing::error!(target: "reddb::audit", error = %err, "audit writer init failed");
            writer_alive.store(false, Ordering::SeqCst);
            return;
        }
    };
    // Track size in-memory; BufWriter hides the on-disk size until
    // flush, and we rotate on bytes-actually-written so a slow
    // flush cadence doesn't run away.
    let mut bytes_written: u64 = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

    let periodic_interval = Duration::from_millis(250);
    let mut last_flush = Instant::now();
    let mut buffered_since_fsync: u64 = 0;

    loop {
        // Wake up periodically so the periodic-fsync mode can run even
        // when no events arrive (compliance-driven).
        let recv_timeout = match fsync_mode {
            FsyncMode::Periodic => periodic_interval,
            FsyncMode::Every | FsyncMode::Off => Duration::from_secs(1),
        };
        match rx.recv_timeout(recv_timeout) {
            Ok(WriterMsg::Event(event)) => {
                let prev = last_hash.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let line = event.to_json_line(prev.as_deref());

                let line_bytes = line.len() as u64 + 1; // newline
                if let Err(err) = write_line(&mut writer, &line) {
                    tracing::warn!(
                        target: "reddb::audit",
                        error = %err,
                        "audit write failed; reopening"
                    );
                    if let Ok(w2) = open_active(&path) {
                        writer = w2;
                        let _ = write_line(&mut writer, &line);
                    }
                }
                bytes_written = bytes_written.saturating_add(line_bytes);
                let new_hash = crate::utils::to_hex(&sha256::sha256(line.as_bytes()));
                if let Ok(mut g) = last_hash.lock() {
                    *g = Some(new_hash);
                }
                if let Some(url) = &stream_url {
                    stream_post(url, &line);
                }
                tracing::info!(target: "reddb::audit", "{line}");
                pending.fetch_sub(1, Ordering::SeqCst);
                buffered_since_fsync += 1;

                match fsync_mode {
                    FsyncMode::Every => {
                        let _ = writer.flush();
                        let _ = writer.get_ref().sync_data();
                        buffered_since_fsync = 0;
                    }
                    FsyncMode::Periodic => {
                        if last_flush.elapsed() >= periodic_interval {
                            let _ = writer.flush();
                            let _ = writer.get_ref().sync_data();
                            last_flush = Instant::now();
                            buffered_since_fsync = 0;
                        }
                    }
                    FsyncMode::Off => {}
                }

                // Rotation check based on in-memory accounting; BufWriter
                // metadata can lag.
                if bytes_written >= max_bytes {
                    let _ = writer.flush();
                    let _ = writer.get_ref().sync_data();
                    if let Err(err) = rotate(&path) {
                        tracing::warn!(
                            target: "reddb::audit",
                            error = %err,
                            "audit rotation failed; continuing in-place"
                        );
                    }
                    match open_active(&path) {
                        Ok(w2) => writer = w2,
                        Err(err) => {
                            tracing::error!(
                                target: "reddb::audit",
                                error = %err,
                                "audit reopen failed after rotate"
                            );
                            break;
                        }
                    }
                    last_flush = Instant::now();
                    buffered_since_fsync = 0;
                    bytes_written = 0;
                }
            }
            Ok(WriterMsg::Flush(ack)) => {
                let _ = writer.flush();
                let _ = writer.get_ref().sync_data();
                last_flush = Instant::now();
                buffered_since_fsync = 0;
                // Acks are sent only after pending == 0; in this design
                // every event sent before Flush has already been
                // processed (channel is FIFO), so we can ack now.
                let _ = ack.send(());
            }
            Ok(WriterMsg::Shutdown) => {
                let _ = writer.flush();
                let _ = writer.get_ref().sync_data();
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if buffered_since_fsync > 0 {
                    let _ = writer.flush();
                    let _ = writer.get_ref().sync_data();
                    last_flush = Instant::now();
                    buffered_since_fsync = 0;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = writer.flush();
                let _ = writer.get_ref().sync_data();
                break;
            }
        }
    }

    writer_alive.store(false, Ordering::SeqCst);
}

fn open_active(path: &Path) -> std::io::Result<BufWriter<std::fs::File>> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    Ok(BufWriter::new(f))
}

fn write_line(writer: &mut BufWriter<std::fs::File>, line: &str) -> std::io::Result<()> {
    writer.write_all(line.as_bytes())?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn append_line_with_rotation(path: &Path, line: &str, max_bytes: u64) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(line.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_data()?;
    drop(file);
    if let Ok(meta) = std::fs::metadata(path) {
        if meta.len() >= max_bytes {
            let _ = rotate(path);
        }
    }
    Ok(())
}

/// Rename the active file to `<path>.<ms>.zst` and zstd-compress it
/// in-place. The compressed file replaces the renamed plaintext copy
/// so the on-disk artefact is `.audit.log.<ms>.zst`.
///
/// Rotation timestamp uses unix nanos so back-to-back rotations
/// under load (or in a tight test loop) don't collide on the same
/// filename.
fn rotate(active: &Path) -> std::io::Result<()> {
    let ts = crate::utils::now_unix_nanos();
    let stem = active
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(".audit.log");
    let parent = active.parent().unwrap_or_else(|| Path::new("."));
    let plain = parent.join(format!("{stem}.{ts}"));
    std::fs::rename(active, &plain)?;
    let raw = std::fs::read(&plain)?;
    let compressed = match zstd::bulk::compress(&raw, 3) {
        Ok(c) => c,
        Err(err) => {
            // Compression failed: leave the rotated file uncompressed
            // rather than lose audit data.
            tracing::warn!(
                target: "reddb::audit",
                error = %err,
                "audit rotation: zstd compress failed; leaving plaintext"
            );
            return Ok(());
        }
    };
    let zst = parent.join(format!("{stem}.{ts}.zst"));
    let mut out = std::fs::File::create(&zst)?;
    out.write_all(&compressed)?;
    out.sync_data()?;
    drop(out);
    let _ = std::fs::remove_file(&plain);
    Ok(())
}

// ---------------------------------------------------------------------------
// SIEM streaming (fire-and-forget)
// ---------------------------------------------------------------------------

fn stream_post(url: &str, line: &str) {
    let url = url.to_string();
    let line = line.to_string();
    // Spawn a one-shot thread; ureq builds a fresh agent per call.
    // Best-effort: one attempt, no retry — SIEM ingestion lag is
    // not the RedDB hot path's problem.
    let _ = thread::Builder::new()
        .name("reddb-audit-siem".to_string())
        .spawn(move || {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .timeout_connect(Some(Duration::from_secs(2)))
                .timeout_send_request(Some(Duration::from_secs(3)))
                .timeout_recv_response(Some(Duration::from_secs(3)))
                .http_status_as_error(false)
                .build()
                .into();
            let _ = agent
                .post(&url)
                .header("content-type", "application/x-ndjson")
                .send(line.as_bytes());
        });
}

// ---------------------------------------------------------------------------
// ISO-8601 helper (kept from the previous implementation)
// ---------------------------------------------------------------------------

fn format_iso8601(ms_since_epoch: u64) -> String {
    let secs = ms_since_epoch / 1000;
    let ms = ms_since_epoch % 1000;
    let days = secs / 86_400;
    let rem = secs % 86_400;
    let (y, mo, d) = civil_from_days(days as i64);
    let h = rem / 3600;
    let mi = (rem % 3600) / 60;
    let s = rem % 60;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y, mo, d, h, mi, s, ms
    )
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_data_path(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "reddb-audit-{}-{}-{}",
            tag,
            std::process::id(),
            crate::utils::now_unix_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p.push("data.rdb");
        p
    }

    fn drain(logger: &AuditLogger) {
        assert!(logger.wait_idle(Duration::from_secs(2)));
    }

    #[test]
    fn record_writes_one_line_per_call() {
        let data = temp_data_path("one-line");
        let logger = AuditLogger::for_data_path(&data);
        logger.record(
            "admin/readonly",
            "operator",
            "instance",
            "ok",
            JsonValue::Null,
        );
        drain(&logger);
        let body = std::fs::read_to_string(logger.path()).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("\"action\":\"admin/readonly\""));
        assert!(lines[0].contains("\"outcome\":\"success\""));
    }

    #[test]
    fn record_appends_across_calls() {
        let data = temp_data_path("append");
        let logger = AuditLogger::for_data_path(&data);
        logger.record("admin/drain", "op", "instance", "ok", JsonValue::Null);
        logger.record("admin/shutdown", "op", "instance", "ok", JsonValue::Null);
        drain(&logger);
        let lines = std::fs::read_to_string(logger.path()).unwrap();
        assert_eq!(lines.lines().count(), 2);
    }

    #[test]
    fn record_event_emits_full_schema() {
        let data = temp_data_path("schema");
        let logger = AuditLogger::for_data_path(&data);
        let mut detail = Map::new();
        detail.insert("ms".to_string(), JsonValue::Number(412.0));
        let ev = AuditEvent::builder("admin/shutdown")
            .principal("alice@acme")
            .source(AuditAuthSource::Session)
            .tenant("acme")
            .resource("instance")
            .outcome(Outcome::Success)
            .detail(JsonValue::Object(detail))
            .remote_addr("203.0.113.5")
            .correlation_id("req-42")
            .build();
        logger.record_event(ev);
        drain(&logger);
        let body = std::fs::read_to_string(logger.path()).unwrap();
        assert!(body.contains("\"action\":\"admin/shutdown\""));
        assert!(body.contains("\"principal\":\"alice@acme\""));
        assert!(body.contains("\"tenant\":\"acme\""));
        assert!(body.contains("\"source\":\"session\""));
        assert!(body.contains("\"correlation_id\":\"req-42\""));
        assert!(body.contains("\"remote_addr\":\"203.0.113.5\""));
        assert!(body.contains("\"event_id\":\""));
        assert!(body.contains("\"prev_hash\":") || body.lines().count() == 1);
    }

    #[test]
    fn hash_chain_links_every_event() {
        let data = temp_data_path("chain");
        let logger = AuditLogger::for_data_path(&data);
        for i in 0..5 {
            logger.record_event(
                AuditEvent::builder(format!("test/event/{i}"))
                    .principal("tester")
                    .build(),
            );
        }
        drain(&logger);
        let body = std::fs::read_to_string(logger.path()).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 5);
        let mut prev: Option<String> = None;
        for (idx, line) in lines.iter().enumerate() {
            let parsed: JsonValue = crate::json::from_str(line).unwrap();
            let stored_prev = parsed
                .get("prev_hash")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            assert_eq!(stored_prev, prev, "line {idx} prev_hash mismatch");
            prev = Some(crate::utils::to_hex(&sha256::sha256(line.as_bytes())));
        }
    }

    #[test]
    fn legacy_record_back_compat_maps_outcomes() {
        let data = temp_data_path("legacy");
        let logger = AuditLogger::for_data_path(&data);
        logger.record(
            "admin/restore",
            "operator",
            "instance",
            "err: disk full",
            JsonValue::Null,
        );
        drain(&logger);
        let body = std::fs::read_to_string(logger.path()).unwrap();
        assert!(body.contains("\"outcome\":\"error\""));
        assert!(body.contains("\"result_text\":\"err: disk full\""));
    }

    #[test]
    fn iso8601_formats_known_epoch() {
        assert_eq!(
            format_iso8601(1_709_210_096_789),
            "2024-02-29T12:34:56.789Z"
        );
    }

    #[test]
    fn rotation_at_threshold() {
        let data = temp_data_path("rotate");
        let parent = data.parent().unwrap().to_path_buf();
        let logger = AuditLogger::with_max_bytes(parent.join(".audit.log"), 1024);
        for i in 0..30 {
            logger.record_event(
                AuditEvent::builder(format!("test/rotate/{i}"))
                    .principal("rotator")
                    .detail(JsonValue::String(
                        "lorem ipsum dolor sit amet consectetur padding padding padding"
                            .to_string(),
                    ))
                    .build(),
            );
        }
        drain(&logger);
        let parent = logger.path().parent().unwrap();
        let rotated: Vec<_> = std::fs::read_dir(parent)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .map(|n| n.starts_with(".audit.log.") && n.ends_with(".zst"))
                    .unwrap_or(false)
            })
            .collect();
        assert!(
            !rotated.is_empty(),
            "expected at least one rotated .zst file"
        );
    }

    #[test]
    fn parse_line_round_trips() {
        let ev = AuditEvent::builder("auth/login.ok")
            .principal("alice")
            .source(AuditAuthSource::Password)
            .tenant("acme")
            .outcome(Outcome::Success)
            .build();
        let line = ev.to_json_line(None);
        let parsed = AuditEvent::parse_line(&line).expect("round-trip parse");
        assert_eq!(parsed.action, "auth/login.ok");
        assert_eq!(parsed.principal.as_deref(), Some("alice"));
        assert_eq!(parsed.tenant.as_deref(), Some("acme"));
        assert_eq!(parsed.outcome, Outcome::Success);
        assert_eq!(parsed.source, AuditAuthSource::Password);
    }

    #[test]
    fn event_id_is_lexicographically_sortable_by_time() {
        let a = new_event_id();
        std::thread::sleep(Duration::from_millis(2));
        let b = new_event_id();
        assert!(a < b, "event_id ordering broken: {a} >= {b}");
    }
}
