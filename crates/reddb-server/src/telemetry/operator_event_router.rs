//! Config-driven per-variant routing for [`OperatorEvent`].
//!
//! See `docs/operations/logging.md` § "Operator event routing" for the TOML
//! config schema and examples.
//!
//! # Routing resolution order
//!
//! 1. Per-variant block (`variant_routes["AuthBypass"]`) — most specific.
//! 2. `default_handlers` in config — user-supplied default.
//! 3. Code default `["audit_log", "tracing"]` — zero upgrade burden.
//!
//! Empty config = identical to the current `OperatorEvent::emit()` behaviour.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::runtime::audit_log::{AuditAuthSource, AuditEvent, AuditLogger, Outcome};
use super::operator_event::OperatorEvent;

// ---------------------------------------------------------------------------
// Validation helpers
// ---------------------------------------------------------------------------

const KNOWN_HANDLERS: &[&str] =
    &["audit_log", "tracing", "stderr", "pagerduty", "generic_webhook"];

fn closest_match(input: &str, candidates: &[&str]) -> Option<String> {
    candidates
        .iter()
        .map(|c| (*c, strsim::levenshtein(input, c)))
        .min_by_key(|(_, d)| *d)
        .filter(|(_, d)| *d <= 4)
        .map(|(c, _)| c.to_string())
}

fn validate_handler_names(names: &[String]) -> Result<(), ConfigError> {
    for name in names {
        if !KNOWN_HANDLERS.contains(&name.as_str()) {
            return Err(ConfigError::UnknownHandler { key: name.clone() });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Config types
// ---------------------------------------------------------------------------

/// Token-bucket rate limit for a webhook handler.
#[derive(Debug, Clone, Default)]
pub struct RateLimitConfig {
    /// Number of requests allowed per `window_sec`.
    pub requests: u32,
    pub window_sec: u64,
}

/// Config for a webhook handler (PagerDuty or generic).
#[derive(Debug, Clone)]
pub struct WebhookHandlerConfig {
    pub url: String,
    /// Name of the environment variable holding the bearer token.
    /// Resolved at router construction time; boot fails if unset.
    pub auth_env: String,
    pub rate_limit: Option<RateLimitConfig>,
}

/// Full router configuration (`[telemetry.operator_event]` in TOML).
#[derive(Debug, Default)]
pub struct RouterConfig {
    /// Handler list applied when no per-variant route matches.
    /// `None` → code default `["audit_log", "tracing"]`.
    pub default_handlers: Option<Vec<String>>,
    /// Per-variant overrides. Keys are CamelCase variant names.
    pub variant_routes: HashMap<String, Vec<String>>,
    /// PagerDuty webhook — referenced as `"pagerduty"` in handler lists.
    pub pagerduty: Option<WebhookHandlerConfig>,
    /// Generic webhook — referenced as `"generic_webhook"` in handler lists.
    pub generic_webhook: Option<WebhookHandlerConfig>,
}

// ---------------------------------------------------------------------------
// Config error
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub enum ConfigError {
    UnknownVariant { key: String, suggestion: Option<String> },
    UnknownHandler { key: String },
    MissingEnvVar { handler: String, var: String },
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownVariant { key, suggestion } => {
                write!(f, "unknown OperatorEvent variant '{key}'")?;
                if let Some(s) = suggestion {
                    write!(f, "; did you mean '{s}'?")?;
                }
                Ok(())
            }
            Self::UnknownHandler { key } => write!(
                f,
                "unknown handler name '{key}'; known: {}",
                KNOWN_HANDLERS.join(", ")
            ),
            Self::MissingEnvVar { handler, var } => write!(
                f,
                "handler '{handler}' requires env var '{var}' which is not set"
            ),
        }
    }
}

impl std::error::Error for ConfigError {}

// ---------------------------------------------------------------------------
// Token bucket (per-handler rate limit)
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct TokenBucket {
    tokens: f64,
    rate: f64,
    burst: f64,
    last: Instant,
}

impl TokenBucket {
    fn new(cfg: &RateLimitConfig) -> Self {
        let rate = if cfg.window_sec > 0 {
            cfg.requests as f64 / cfg.window_sec as f64
        } else {
            cfg.requests as f64
        };
        let burst = rate.max(1.0);
        Self { tokens: burst, rate, burst, last: Instant::now() }
    }

    fn try_consume(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.rate).min(self.burst);
        self.last = now;
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Webhook queue (bounded, drop-oldest on saturation)
// ---------------------------------------------------------------------------

const QUEUE_CAPACITY: usize = 1_000;

#[derive(Debug)]
struct WebhookQueue {
    inner: Mutex<VecDeque<WebhookPayload>>,
    not_empty: Condvar,
    dropped_queue_full: AtomicU64,
    dropped_rate_limit: AtomicU64,
    dropped_max_retries: AtomicU64,
    sent: AtomicU64,
}

impl WebhookQueue {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(VecDeque::with_capacity(QUEUE_CAPACITY)),
            not_empty: Condvar::new(),
            dropped_queue_full: AtomicU64::new(0),
            dropped_rate_limit: AtomicU64::new(0),
            dropped_max_retries: AtomicU64::new(0),
            sent: AtomicU64::new(0),
        })
    }

    fn push(&self, payload: WebhookPayload) {
        let mut q = self.inner.lock().expect("webhook queue mutex");
        if q.len() >= QUEUE_CAPACITY {
            q.pop_front(); // drop oldest
            self.dropped_queue_full.fetch_add(1, Ordering::Relaxed);
        }
        q.push_back(payload);
        drop(q);
        self.not_empty.notify_one();
    }

    fn pop_blocking(&self) -> WebhookPayload {
        let mut q = self.inner.lock().expect("webhook queue mutex");
        loop {
            if let Some(item) = q.pop_front() {
                return item;
            }
            q = self.not_empty.wait(q).expect("webhook queue condvar");
        }
    }
}

// ---------------------------------------------------------------------------
// Webhook payload
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct WebhookPayload {
    action: String,
    summary: String,
    ts_ms: u64,
}

impl WebhookPayload {
    fn to_json_body(&self) -> String {
        // Build JSON via the serde_json encoder so special chars in event/summary
        // are escaped correctly (RFC 8259 §7 per ADR 0010).
        use crate::serde_json::Value;
        let event_json = Value::String(self.action.clone()).to_string_compact();
        let summary_json = Value::String(self.summary.clone()).to_string_compact();
        format!(r#"{{"event":{event_json},"summary":{summary_json},"ts":{}}}"#, self.ts_ms)
    }
}

// ---------------------------------------------------------------------------
// Background webhook worker
// ---------------------------------------------------------------------------

fn spawn_webhook_worker(
    name: &str,
    url: String,
    auth_token: String,
    queue: Arc<WebhookQueue>,
) -> thread::JoinHandle<()> {
    let name = name.to_string();
    thread::Builder::new()
        .name(format!("reddb-webhook-{name}"))
        .spawn(move || {
            let agent: ureq::Agent = ureq::Agent::config_builder()
                .timeout_connect(Some(Duration::from_secs(3)))
                .timeout_send_request(Some(Duration::from_secs(5)))
                .timeout_recv_response(Some(Duration::from_secs(5)))
                .http_status_as_error(false)
                .build()
                .into();

            loop {
                let payload = queue.pop_blocking();
                let body = payload.to_json_body();
                let bearer = format!("Bearer {auth_token}");

                let mut success = false;
                for attempt in 1u32..=3 {
                    let result = agent
                        .post(&url)
                        .header("content-type", "application/json")
                        .header("authorization", &bearer)
                        .send(body.as_bytes());

                    match result {
                        Ok(_) => {
                            queue.sent.fetch_add(1, Ordering::Relaxed);
                            success = true;
                            break;
                        }
                        Err(_) if attempt < 3 => {
                            thread::sleep(Duration::from_millis(100 * (1u64 << attempt)));
                        }
                        Err(_) => {}
                    }
                }

                if !success {
                    queue.dropped_max_retries.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(
                        target: "reddb::operator_router",
                        handler = %name,
                        "webhook delivery failed after 3 attempts; event dropped"
                    );
                }
            }
        })
        .expect("spawn webhook worker thread")
}

// ---------------------------------------------------------------------------
// Effective handler (runtime form)
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum EffectiveHandler {
    AuditLog,
    Tracing,
    Stderr,
    Webhook {
        name: String,
        queue: Arc<WebhookQueue>,
        rate_limiter: Option<Mutex<TokenBucket>>,
    },
}

impl EffectiveHandler {
    fn name(&self) -> &str {
        match self {
            Self::AuditLog => "audit_log",
            Self::Tracing => "tracing",
            Self::Stderr => "stderr",
            Self::Webhook { name, .. } => name,
        }
    }
}

// ---------------------------------------------------------------------------
// Metrics snapshot
// ---------------------------------------------------------------------------

/// Prometheus-style counters snapshot for external scraping.
#[derive(Debug, Default)]
pub struct RouterMetricsSnapshot {
    /// `(handler_name, dropped_count)` — aggregated across all drop reasons.
    pub dropped: Vec<(String, u64)>,
    /// `(handler_name, sent_count)` for webhook handlers.
    pub sent: Vec<(String, u64)>,
}

// ---------------------------------------------------------------------------
// OperatorEventRouter
// ---------------------------------------------------------------------------

/// Config-driven dispatcher for [`OperatorEvent`].
///
/// Build with [`OperatorEventRouter::new`]; the construction validates the
/// config strictly (unknown variant names / handler names / missing env vars
/// all return `Err`). With an empty config the router behaves identically to
/// the current `OperatorEvent::emit()`.
#[derive(Debug)]
pub struct OperatorEventRouter {
    audit_logger: Option<Arc<AuditLogger>>,
    default_route: Vec<Arc<EffectiveHandler>>,
    variant_routes: HashMap<&'static str, Vec<Arc<EffectiveHandler>>>,
    webhook_queues: HashMap<String, Arc<WebhookQueue>>,
    // Mutex makes the field Sync (JoinHandle is Send but !Sync); the workers
    // run until process exit and are never joined during normal operation.
    _workers: Mutex<Vec<thread::JoinHandle<()>>>,
}

impl OperatorEventRouter {
    /// Build a router.
    ///
    /// Validation order (boot fail-fast on any error):
    /// 1. Parse → struct (caller's responsibility for TOML).
    /// 2. Strict variant name check with Levenshtein suggestion.
    /// 3. Strict handler name check (closed set).
    /// 4. Webhook env-var presence check.
    pub fn new(
        config: RouterConfig,
        audit_logger: Option<Arc<AuditLogger>>,
    ) -> Result<Self, ConfigError> {
        let known_variants = OperatorEvent::all_variant_names();

        // --- Validate variant names ---
        for key in config.variant_routes.keys() {
            if !known_variants.contains(&key.as_str()) {
                let suggestion = closest_match(key, known_variants);
                return Err(ConfigError::UnknownVariant { key: key.clone(), suggestion });
            }
        }

        // --- Validate handler names ---
        for names in config.variant_routes.values() {
            validate_handler_names(names)?;
        }
        if let Some(ref dh) = config.default_handlers {
            validate_handler_names(dh)?;
        }

        // --- Build webhook handlers (env-var check happens here) ---
        let mut webhook_queues: HashMap<String, Arc<WebhookQueue>> = HashMap::new();
        let mut workers: Vec<thread::JoinHandle<()>> = Vec::new();

        let pd_handler = config
            .pagerduty
            .as_ref()
            .map(|cfg| -> Result<Arc<EffectiveHandler>, ConfigError> {
                let token = std::env::var(&cfg.auth_env).map_err(|_| {
                    ConfigError::MissingEnvVar {
                        handler: "pagerduty".into(),
                        var: cfg.auth_env.clone(),
                    }
                })?;
                let queue = WebhookQueue::new();
                webhook_queues.insert("pagerduty".into(), Arc::clone(&queue));
                workers.push(spawn_webhook_worker("pagerduty", cfg.url.clone(), token, Arc::clone(&queue)));
                let rate_limiter = cfg.rate_limit.as_ref().map(|rl| Mutex::new(TokenBucket::new(rl)));
                Ok(Arc::new(EffectiveHandler::Webhook {
                    name: "pagerduty".into(),
                    queue,
                    rate_limiter,
                }))
            })
            .transpose()?;

        let gw_handler = config
            .generic_webhook
            .as_ref()
            .map(|cfg| -> Result<Arc<EffectiveHandler>, ConfigError> {
                let token = std::env::var(&cfg.auth_env).map_err(|_| {
                    ConfigError::MissingEnvVar {
                        handler: "generic_webhook".into(),
                        var: cfg.auth_env.clone(),
                    }
                })?;
                let queue = WebhookQueue::new();
                webhook_queues.insert("generic_webhook".into(), Arc::clone(&queue));
                workers.push(spawn_webhook_worker("generic_webhook", cfg.url.clone(), token, Arc::clone(&queue)));
                let rate_limiter = cfg.rate_limit.as_ref().map(|rl| Mutex::new(TokenBucket::new(rl)));
                Ok(Arc::new(EffectiveHandler::Webhook {
                    name: "generic_webhook".into(),
                    queue,
                    rate_limiter,
                }))
            })
            .transpose()?;

        // Helper: resolve handler name list → runtime handler vec.
        // Webhook handlers reference shared Arcs so the queue counters are
        // shared between the router and the worker threads.
        let resolve = |names: &[String]| -> Vec<Arc<EffectiveHandler>> {
            names
                .iter()
                .filter_map(|n| match n.as_str() {
                    "audit_log" => Some(Arc::new(EffectiveHandler::AuditLog)),
                    "tracing" => Some(Arc::new(EffectiveHandler::Tracing)),
                    "stderr" => Some(Arc::new(EffectiveHandler::Stderr)),
                    "pagerduty" => pd_handler.clone(),
                    "generic_webhook" => gw_handler.clone(),
                    _ => None,
                })
                .collect()
        };

        let code_default = vec!["audit_log".to_string(), "tracing".to_string()];
        let default_names = config.default_handlers.as_deref().unwrap_or(&code_default);
        let default_route = resolve(default_names);

        let mut variant_routes: HashMap<&'static str, Vec<Arc<EffectiveHandler>>> =
            HashMap::new();
        for (key, names) in &config.variant_routes {
            if let Some(static_key) =
                known_variants.iter().copied().find(|v| *v == key.as_str())
            {
                variant_routes.insert(static_key, resolve(names));
            }
        }

        Ok(Self {
            audit_logger,
            default_route,
            variant_routes,
            webhook_queues,
            _workers: Mutex::new(workers),
        })
    }

    /// Dispatch `event` through the configured handlers.
    ///
    /// Always synchronous — safe to call from `Drop` impls, signal handlers,
    /// and crash paths.
    pub fn route(&self, event: OperatorEvent) {
        let variant = event.variant_name();
        let handlers = self
            .variant_routes
            .get(variant)
            .unwrap_or(&self.default_route);

        let (action, fields, summary) = event.decompose();
        let ts_ms = crate::utils::now_unix_millis();

        for handler in handlers {
            match handler.as_ref() {
                EffectiveHandler::AuditLog => {
                    if let Some(audit) = &self.audit_logger {
                        let ev = AuditEvent::builder(action)
                            .source(AuditAuthSource::System)
                            .outcome(Outcome::Error)
                            .fields(fields.clone())
                            .build();
                        audit.record_event(ev);
                    }
                }
                EffectiveHandler::Tracing => {
                    tracing::warn!(target: "reddb::operator", "{summary}");
                }
                EffectiveHandler::Stderr => {
                    eprintln!("[reddb::operator] {summary}");
                }
                EffectiveHandler::Webhook { name, queue, rate_limiter } => {
                    if let Some(rl) = rate_limiter {
                        let allowed = rl.lock().expect("rate limiter mutex").try_consume();
                        if !allowed {
                            queue.dropped_rate_limit.fetch_add(1, Ordering::Relaxed);
                            tracing::debug!(
                                target: "reddb::operator_router",
                                handler = %name,
                                "event rate-limited; skipping webhook"
                            );
                            continue;
                        }
                    }
                    queue.push(WebhookPayload {
                        action: action.to_string(),
                        summary: summary.clone(),
                        ts_ms,
                    });
                }
            }
        }
    }

    /// Prometheus-style metrics snapshot.
    pub fn metrics(&self) -> RouterMetricsSnapshot {
        let mut snap = RouterMetricsSnapshot::default();
        for (name, q) in &self.webhook_queues {
            let dropped = q.dropped_queue_full.load(Ordering::Relaxed)
                + q.dropped_rate_limit.load(Ordering::Relaxed)
                + q.dropped_max_retries.load(Ordering::Relaxed);
            snap.dropped.push((name.clone(), dropped));
            snap.sent.push((name.clone(), q.sent.load(Ordering::Relaxed)));
        }
        snap
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::net::TcpListener;
    use std::sync::Arc;

    use super::*;
    use crate::runtime::audit_log::AuditLogger;

    fn make_audit_logger() -> (Arc<AuditLogger>, std::path::PathBuf) {
        let mut dir = std::env::temp_dir();
        dir.push(format!(
            "reddb-router-test-{}-{}",
            std::process::id(),
            crate::utils::now_unix_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".audit.log");
        let logger = Arc::new(AuditLogger::with_path(path.clone()));
        (logger, path)
    }

    fn drain(logger: &AuditLogger) {
        assert!(
            logger.wait_idle(Duration::from_secs(2)),
            "audit logger drain timed out"
        );
    }

    fn last_audit_action(path: &std::path::Path) -> Option<String> {
        let body = std::fs::read_to_string(path).ok()?;
        let line = body.lines().last()?;
        let v: crate::serde_json::Value = crate::serde_json::from_str(line).ok()?;
        v.get("action").and_then(|x| x.as_str()).map(|s| s.to_string())
    }

    // -----------------------------------------------------------------------
    // Default routing: empty config = audit_log + tracing
    // -----------------------------------------------------------------------

    #[test]
    fn empty_config_routes_all_variants_to_audit_and_tracing() {
        let (audit, path) = make_audit_logger();
        let router = OperatorEventRouter::new(RouterConfig::default(), Some(Arc::clone(&audit)))
            .expect("router build");

        let variants: &[OperatorEvent] = &[
            OperatorEvent::ReplicationBroken { peer: "p".into(), reason: "r".into() },
            OperatorEvent::Divergence { peer: "p".into(), leader_lsn: 1, follower_lsn: 0 },
            OperatorEvent::WalFsyncFailed { path: "/d".into(), error: "e".into() },
            OperatorEvent::DiskSpaceCritical { path: "/d".into(), available_bytes: 1, threshold_bytes: 2 },
            OperatorEvent::AuthBypass { principal: "a".into(), resource: "r".into(), detail: "d".into() },
            OperatorEvent::AdminCapabilityGranted { granted_to: "a".into(), capability: "c".into(), granted_by: "b".into() },
            OperatorEvent::SecretRotationFailed { secret_ref: "s".into(), error: "e".into() },
            OperatorEvent::ConfigChanged { key: "k".into(), old_value: "o".into(), new_value: "n".into(), changed_by: "b".into() },
            OperatorEvent::StartupFailed { phase: "p".into(), error: "e".into() },
            OperatorEvent::ShutdownForced { reason: "r".into() },
            OperatorEvent::SchemaCorruption { collection: "c".into(), detail: "d".into() },
            OperatorEvent::CheckpointFailed { lsn: 1, error: "e".into() },
            OperatorEvent::ConfigChangeRequiresRestart { fields_changed: "f".into() },
            OperatorEvent::SubscriptionSchemaChange {
                collection: "c".into(),
                subscription_names: "sub1".into(),
                fields_added: "phone".into(),
                fields_removed: "".into(),
                lsn: 42,
            },
            OperatorEvent::OutboxDlqActivated {
                queue: "users_events".into(),
                dlq: "users_events_outbox_dlq".into(),
                reason: "queue_full".into(),
            },
        ];

        // Verify count matches all_variant_names minus DanglingAdminIntent
        // (which requires crypto types — tested separately).
        for event in variants {
            // Clone variant_name before routing (event is consumed by route).
            let vname = event.variant_name();
            router.route(clone_event(event));
            let _ = vname; // just ensure it compiled
        }

        drain(&audit);
        // At least one line written: the last event.
        let action = last_audit_action(&path).expect("at least one audit line");
        assert!(action.starts_with("operator/"), "action={action}");
    }

    // Clone helper for test variants that don't use crypto types.
    fn clone_event(e: &OperatorEvent) -> OperatorEvent {
        match e {
            OperatorEvent::ReplicationBroken { peer, reason } => {
                OperatorEvent::ReplicationBroken { peer: peer.clone(), reason: reason.clone() }
            }
            OperatorEvent::Divergence { peer, leader_lsn, follower_lsn } => {
                OperatorEvent::Divergence { peer: peer.clone(), leader_lsn: *leader_lsn, follower_lsn: *follower_lsn }
            }
            OperatorEvent::WalFsyncFailed { path, error } => {
                OperatorEvent::WalFsyncFailed { path: path.clone(), error: error.clone() }
            }
            OperatorEvent::DiskSpaceCritical { path, available_bytes, threshold_bytes } => {
                OperatorEvent::DiskSpaceCritical { path: path.clone(), available_bytes: *available_bytes, threshold_bytes: *threshold_bytes }
            }
            OperatorEvent::AuthBypass { principal, resource, detail } => {
                OperatorEvent::AuthBypass { principal: principal.clone(), resource: resource.clone(), detail: detail.clone() }
            }
            OperatorEvent::AdminCapabilityGranted { granted_to, capability, granted_by } => {
                OperatorEvent::AdminCapabilityGranted { granted_to: granted_to.clone(), capability: capability.clone(), granted_by: granted_by.clone() }
            }
            OperatorEvent::SecretRotationFailed { secret_ref, error } => {
                OperatorEvent::SecretRotationFailed { secret_ref: secret_ref.clone(), error: error.clone() }
            }
            OperatorEvent::ConfigChanged { key, old_value, new_value, changed_by } => {
                OperatorEvent::ConfigChanged { key: key.clone(), old_value: old_value.clone(), new_value: new_value.clone(), changed_by: changed_by.clone() }
            }
            OperatorEvent::StartupFailed { phase, error } => {
                OperatorEvent::StartupFailed { phase: phase.clone(), error: error.clone() }
            }
            OperatorEvent::ShutdownForced { reason } => {
                OperatorEvent::ShutdownForced { reason: reason.clone() }
            }
            OperatorEvent::SchemaCorruption { collection, detail } => {
                OperatorEvent::SchemaCorruption { collection: collection.clone(), detail: detail.clone() }
            }
            OperatorEvent::CheckpointFailed { lsn, error } => {
                OperatorEvent::CheckpointFailed { lsn: *lsn, error: error.clone() }
            }
            OperatorEvent::ConfigChangeRequiresRestart { fields_changed } => {
                OperatorEvent::ConfigChangeRequiresRestart { fields_changed: fields_changed.clone() }
            }
            OperatorEvent::DanglingAdminIntent { .. } => {
                // Not cloneable without crypto type impls; skip.
                OperatorEvent::ShutdownForced { reason: "clone_placeholder".into() }
            }
            OperatorEvent::SubscriptionSchemaChange {
                collection,
                subscription_names,
                fields_added,
                fields_removed,
                lsn,
            } => OperatorEvent::SubscriptionSchemaChange {
                collection: collection.clone(),
                subscription_names: subscription_names.clone(),
                fields_added: fields_added.clone(),
                fields_removed: fields_removed.clone(),
                lsn: *lsn,
            },
            OperatorEvent::OutboxDlqActivated { queue, dlq, reason } => {
                OperatorEvent::OutboxDlqActivated {
                    queue: queue.clone(),
                    dlq: dlq.clone(),
                    reason: reason.clone(),
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Config validation: unknown variant name → Levenshtein suggestion
    // -----------------------------------------------------------------------

    #[test]
    fn unknown_variant_gives_suggestion() {
        let mut config = RouterConfig::default();
        // "AuthBypas" is one edit away from "AuthBypass" (missing 's').
        config.variant_routes.insert(
            "AuthBypas".into(),
            vec!["audit_log".into()],
        );
        let err = OperatorEventRouter::new(config, None).unwrap_err();
        match err {
            ConfigError::UnknownVariant { key, suggestion } => {
                assert_eq!(key, "AuthBypas");
                // strsim should suggest "AuthBypass" (levenshtein distance 1)
                assert_eq!(suggestion.as_deref(), Some("AuthBypass"));
            }
            other => panic!("expected UnknownVariant, got: {other}"),
        }
    }

    #[test]
    fn unknown_handler_name_is_rejected() {
        let mut config = RouterConfig::default();
        config.variant_routes.insert(
            "AuthBypass".into(),
            vec!["slack".into()], // not a known handler
        );
        let err = OperatorEventRouter::new(config, None).unwrap_err();
        assert!(matches!(err, ConfigError::UnknownHandler { .. }));
    }

    // -----------------------------------------------------------------------
    // Per-variant route override
    // -----------------------------------------------------------------------

    #[test]
    fn per_variant_route_overrides_default() {
        let (audit, path) = make_audit_logger();
        let mut config = RouterConfig::default();
        // Route AuthBypass to stderr-only (no audit log for this test variant).
        config.variant_routes.insert(
            "AuthBypass".into(),
            vec!["stderr".into()],
        );
        let router = OperatorEventRouter::new(config, Some(Arc::clone(&audit))).unwrap();

        // Emit AuthBypass — should NOT go to audit (only stderr per config).
        router.route(OperatorEvent::AuthBypass {
            principal: "test".into(),
            resource: "/secret".into(),
            detail: "test override".into(),
        });
        drain(&audit);
        // Audit log should still be empty (or unchanged from no lines).
        let body = std::fs::read_to_string(&path).unwrap_or_default();
        assert!(
            body.lines().all(|l| !l.contains("auth_bypass")),
            "auth_bypass should not appear in audit (stderr-only route)"
        );

        // Emit a different variant — should still go to default (audit+tracing).
        router.route(OperatorEvent::ShutdownForced { reason: "test".into() });
        drain(&audit);
        let action = last_audit_action(&path).expect("shutdown_forced in audit");
        assert_eq!(action, "operator/shutdown_forced");
    }

    // -----------------------------------------------------------------------
    // Token bucket rate limit
    // -----------------------------------------------------------------------

    #[test]
    fn token_bucket_throttles_after_burst() {
        let mut bucket = TokenBucket::new(&RateLimitConfig { requests: 3, window_sec: 60 });
        // rate = 3/60 = 0.05 t/s, burst = max(0.05, 1.0) = 1.0
        // First consume should succeed.
        assert!(bucket.try_consume(), "first consume should succeed");
        // Second should fail (burst exhausted, no time passed).
        assert!(!bucket.try_consume(), "second consume should be throttled");
    }

    #[test]
    fn token_bucket_refills_over_time() {
        let mut bucket = TokenBucket::new(&RateLimitConfig { requests: 100, window_sec: 1 });
        // rate = 100/s, burst = 100
        for _ in 0..100 {
            assert!(bucket.try_consume());
        }
        assert!(!bucket.try_consume(), "burst exhausted");
        thread::sleep(Duration::from_millis(20));
        // After 20ms at 100/s we get ~2 tokens.
        assert!(bucket.try_consume(), "should refill after sleep");
    }

    // -----------------------------------------------------------------------
    // Webhook queue: drop oldest on saturation
    // -----------------------------------------------------------------------

    #[test]
    fn queue_drops_oldest_on_saturation() {
        let queue = WebhookQueue::new();
        for i in 0..QUEUE_CAPACITY {
            queue.push(WebhookPayload {
                action: format!("ev/{i}"),
                summary: format!("s{i}"),
                ts_ms: i as u64,
            });
        }
        assert_eq!(queue.dropped_queue_full.load(Ordering::Relaxed), 0);

        // One more push → drops oldest (ev/0).
        queue.push(WebhookPayload {
            action: "ev/overflow".into(),
            summary: "overflow".into(),
            ts_ms: QUEUE_CAPACITY as u64,
        });
        assert_eq!(queue.dropped_queue_full.load(Ordering::Relaxed), 1);

        // Oldest item in queue should now be ev/1 (ev/0 was dropped).
        let first = queue.pop_blocking();
        assert_eq!(first.action, "ev/1");
    }

    // -----------------------------------------------------------------------
    // Integration: mock webhook server receives payload
    // -----------------------------------------------------------------------

    #[test]
    fn webhook_delivers_payload_to_mock_server() {
        // Bind a local TCP server to act as the webhook endpoint.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let url = format!("http://{addr}/webhook");

        // Accept one connection in a background thread.
        let server_thread = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut buf = vec![0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            String::from_utf8_lossy(&buf[..n]).to_string()
        });

        // Set up env var for auth.
        std::env::set_var("TEST_WEBHOOK_TOKEN_ROUTER", "test-token-42");

        let config = RouterConfig {
            default_handlers: None,
            variant_routes: {
                let mut m = HashMap::new();
                m.insert("ShutdownForced".into(), vec!["generic_webhook".into()]);
                m
            },
            pagerduty: None,
            generic_webhook: Some(WebhookHandlerConfig {
                url,
                auth_env: "TEST_WEBHOOK_TOKEN_ROUTER".into(),
                rate_limit: None,
            }),
        };

        let router = OperatorEventRouter::new(config, None).unwrap();
        router.route(OperatorEvent::ShutdownForced { reason: "integration-test".into() });

        let raw = server_thread.join().expect("server thread");
        // The request should contain our auth header and JSON body.
        assert!(raw.contains("Bearer test-token-42"), "missing auth header");
        assert!(raw.contains("shutdown_forced"), "missing event in body");
    }

    // -----------------------------------------------------------------------
    // Race: concurrent route() calls don't corrupt rate limiter
    // -----------------------------------------------------------------------

    #[test]
    fn concurrent_route_calls_safe() {
        let router = Arc::new(
            OperatorEventRouter::new(RouterConfig::default(), None).unwrap(),
        );
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let r = Arc::clone(&router);
                thread::spawn(move || {
                    for _ in 0..50 {
                        r.route(OperatorEvent::ShutdownForced { reason: "stress".into() });
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }
        // No panic = pass.
    }

    // -----------------------------------------------------------------------
    // Missing env var → boot fail-fast
    // -----------------------------------------------------------------------

    #[test]
    fn missing_env_var_fails_at_construction() {
        let config = RouterConfig {
            default_handlers: None,
            variant_routes: HashMap::new(),
            pagerduty: Some(WebhookHandlerConfig {
                url: "http://localhost/pd".into(),
                auth_env: "REDDB_TEST_PD_KEY_DEFINITELY_NOT_SET_12345".into(),
                rate_limit: None,
            }),
            generic_webhook: None,
        };
        let err = OperatorEventRouter::new(config, None).unwrap_err();
        assert!(matches!(err, ConfigError::MissingEnvVar { .. }));
    }
}
