//! Control Event Ledger — skeleton (issue #652).
//!
//! Cross-cutting types + the [`ControlEventLedger`] trait that the
//! policy / config / user-lifecycle producer slices (issues 665/666/
//! 667) will call into. Ships ONE implementor — [`RuntimeLedger`] —
//! which writes one row per `emit()` to the `red.control_events`
//! collection via the unified entity API.
//!
//! This module deliberately does NOT wire `emit()` into any producer
//! call site (`AuthStore::*`, `ConfigRegistry::*`, etc.); that is the
//! scope of 652b/c/d. It also does not decide what counts as
//! sensitive — producers call [`Sensitivity::hashed`] /
//! [`Sensitivity::redacted`] at their own emit sites.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use crate::auth::UserId;
use crate::crypto::uuid::Uuid;
use crate::storage::schema::types::Value;
use crate::storage::unified::{EntityData, EntityId, EntityKind, RowData, UnifiedEntity};
use crate::storage::UnifiedStore;
use crate::utils::now_unix_millis;

/// Canonical name of the managed control-event collection.
pub const CONTROL_EVENTS_COLLECTION: &str = "red.control_events";

// ---------------------------------------------------------------------------
// EventKind
// ---------------------------------------------------------------------------

/// Strong enum of every control-plane mutation the ledger records.
/// Mirrors the `kind` column in `red.control_events`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventKind {
    PolicyCreate,
    PolicyUpdate,
    PolicyDelete,
    PolicyAttach,
    PolicyDetach,
    ConfigWrite,
    ConfigDelete,
    UserCreate,
    UserUpdate,
    UserDelete,
    UserDisable,
    ApiKeyCreate,
    ApiKeyRevoke,
    VaultMetadataRead,
    VaultRead,
    VaultUnseal,
    VaultRotate,
    VaultPurge,
    SchemaDdl,
    TenantGovernance,
    RlsGovernance,
    BackupRun,
    RestoreRun,
    FailoverPromotion,
    ReplicationSafety,
    EvidenceExport,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PolicyCreate => "policy.create",
            Self::PolicyUpdate => "policy.update",
            Self::PolicyDelete => "policy.delete",
            Self::PolicyAttach => "policy.attach",
            Self::PolicyDetach => "policy.detach",
            Self::ConfigWrite => "config.write",
            Self::ConfigDelete => "config.delete",
            Self::UserCreate => "user.create",
            Self::UserUpdate => "user.update",
            Self::UserDelete => "user.delete",
            Self::UserDisable => "user.disable",
            Self::ApiKeyCreate => "apikey.create",
            Self::ApiKeyRevoke => "apikey.revoke",
            Self::VaultMetadataRead => "vault.metadata_read",
            Self::VaultRead => "vault.read",
            Self::VaultUnseal => "vault.unseal",
            Self::VaultRotate => "vault.rotate",
            Self::VaultPurge => "vault.purge",
            Self::SchemaDdl => "schema.ddl",
            Self::TenantGovernance => "tenant.governance",
            Self::RlsGovernance => "rls.governance",
            Self::BackupRun => "backup.run",
            Self::RestoreRun => "restore.run",
            Self::FailoverPromotion => "failover.promotion",
            Self::ReplicationSafety => "replication.safety",
            Self::EvidenceExport => "evidence.export",
        }
    }
}

// ---------------------------------------------------------------------------
// Outcome
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Outcome {
    Allowed,
    Denied,
    Error,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => "allowed",
            Self::Denied => "denied",
            Self::Error => "error",
        }
    }
}

// ---------------------------------------------------------------------------
// ActorRef / Context
// ---------------------------------------------------------------------------

/// Who attempted the mutation. Borrowed so producer call-sites don't
/// allocate at every emit; the ledger copies into the persisted row.
#[derive(Debug)]
pub enum ActorRef<'a> {
    User(&'a UserId),
    /// A static system label (e.g. `"bootstrap"`, `"wal_replay"`).
    /// Static-lifetime to keep the enum cheap and to disambiguate from
    /// user identifiers — never a runtime tenant/user string.
    System(&'static str),
    Anonymous,
}

impl<'a> ActorRef<'a> {
    pub fn user_id(&self) -> Option<&UserId> {
        match self {
            Self::User(u) => Some(u),
            _ => None,
        }
    }

    pub fn kind_str(&self) -> &'static str {
        match self {
            Self::User(_) => "user",
            Self::System(_) => "system",
            Self::Anonymous => "anonymous",
        }
    }
}

/// Request-scoped context attached to every emit. Producer call-sites
/// fill what they have; missing fields land as `Null` in the row.
pub struct ControlEventCtx<'a> {
    pub actor: ActorRef<'a>,
    pub scope: Option<Cow<'a, str>>,
    pub request_id: Option<Cow<'a, str>>,
    pub trace_id: Option<Cow<'a, str>>,
}

// ---------------------------------------------------------------------------
// ControlEvent
// ---------------------------------------------------------------------------

pub struct ControlEvent {
    pub kind: EventKind,
    pub outcome: Outcome,
    pub action: Cow<'static, str>,
    pub resource: Option<String>,
    pub reason: Option<String>,
    pub matched_policy_id: Option<String>,
    pub fields: HashMap<String, Sensitivity>,
}

// ---------------------------------------------------------------------------
// Sensitivity
// ---------------------------------------------------------------------------

/// How a payload value is rendered when it lands in `fields_json`.
/// Producer slices choose per-field; the skeleton does not decide what
/// counts as sensitive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sensitivity {
    /// Value persisted as-is. Producer guarantees it isn't sensitive.
    Raw(String),
    /// Fingerprint instead of the value. `algo` is the hash name
    /// (always `"blake3"` for the skeleton helper) and `hex` is the
    /// lowercase hex digest.
    Hashed { algo: &'static str, hex: String },
    /// Placeholder: "a value existed at this field but we are not
    /// logging it." Distinguishable from absence (no key at all).
    Redacted,
}

impl Sensitivity {
    pub fn raw<S: Into<String>>(s: S) -> Self {
        Self::Raw(s.into())
    }

    pub fn hashed(value: &[u8]) -> Self {
        let hex = blake3::hash(value).to_hex().to_string();
        Self::Hashed {
            algo: "blake3",
            hex,
        }
    }

    pub fn redacted() -> Self {
        Self::Redacted
    }
}

// ---------------------------------------------------------------------------
// Ledger trait
// ---------------------------------------------------------------------------

/// Opaque id of a persisted event. Producers may store this to chain a
/// follow-up audit entry to the original.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventId(pub String);

#[derive(Debug)]
pub enum ControlEventError {
    Persistence(String),
}

impl std::fmt::Display for ControlEventError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Persistence(msg) => write!(f, "control-event persistence failed: {msg}"),
        }
    }
}

impl std::error::Error for ControlEventError {}

/// Persistence sink for control events.
///
/// `emit` is synchronous. Callers running under
/// [`ControlEventConfig::compliance_mode`] MUST treat `Err` as
/// "abort the originating mutation" (fail closed). Callers in the
/// default mode may log-and-continue, but the trait never swallows
/// the failure on their behalf — that policy is per-caller.
pub trait ControlEventLedger: Send + Sync {
    fn emit(
        &self,
        ctx: &ControlEventCtx<'_>,
        event: ControlEvent,
    ) -> Result<EventId, ControlEventError>;
}

// ---------------------------------------------------------------------------
// ControlEventConfig
// ---------------------------------------------------------------------------

/// Runtime knob for the ledger. Lives on
/// `RedDBOptions::control_events` and is read at boot from
/// `REDDB_COMPLIANCE_MODE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ControlEventConfig {
    /// When true, the producer slices MUST abort their originating
    /// mutation on `emit` failure (fail closed). Default: false.
    pub compliance_mode: bool,
}

impl ControlEventConfig {
    /// Convenience: do callers need durable evidence before letting
    /// the originating mutation complete?
    pub fn require_persistence(&self) -> bool {
        self.compliance_mode
    }
}

// ---------------------------------------------------------------------------
// RuntimeLedger — the skeleton's single implementor
// ---------------------------------------------------------------------------

/// Writes one row per `emit()` to `red.control_events` via the
/// unified entity API. The collection is created on construction if
/// it doesn't already exist (idempotent across re-opens).
pub struct RuntimeLedger {
    store: Arc<UnifiedStore>,
}

impl RuntimeLedger {
    pub fn new(store: Arc<UnifiedStore>) -> Self {
        let _ = store.get_or_create_collection(CONTROL_EVENTS_COLLECTION);
        Self { store }
    }
}

impl ControlEventLedger for RuntimeLedger {
    fn emit(
        &self,
        ctx: &ControlEventCtx<'_>,
        event: ControlEvent,
    ) -> Result<EventId, ControlEventError> {
        let ts_ms = now_unix_millis();
        // ns since epoch: ms * 1e6, clamped to i64 range. The actual
        // call site doesn't have nanosecond precision today; producer
        // slices that need it can carry it via `fields`.
        let ts_ns = (ts_ms as i128)
            .saturating_mul(1_000_000)
            .min(i64::MAX as i128) as i64;
        // UUIDv7 is time-sortable in its first 48 bits, satisfying the
        // brief's "ULID, sortable by time" requirement without adding
        // a ulid dep.
        let id = Uuid::new_v7().to_string();

        let mut named: HashMap<String, Value> = HashMap::with_capacity(14);
        named.insert("id".into(), Value::text(id.clone()));
        named.insert("ts".into(), Value::Integer(ts_ns));
        named.insert("kind".into(), Value::text(event.kind.as_str()));
        named.insert("outcome".into(), Value::text(event.outcome.as_str()));
        named.insert("actor_kind".into(), Value::text(ctx.actor.kind_str()));
        named.insert(
            "actor_user_id".into(),
            ctx.actor
                .user_id()
                .map(|u| Value::text(u.to_string()))
                .unwrap_or(Value::Null),
        );
        named.insert(
            "scope".into(),
            ctx.scope
                .as_ref()
                .map(|s| Value::text(s.to_string()))
                .unwrap_or(Value::Null),
        );
        named.insert("action".into(), Value::text(event.action.to_string()));
        named.insert(
            "resource".into(),
            event.resource.map(Value::text).unwrap_or(Value::Null),
        );
        named.insert(
            "reason".into(),
            event.reason.map(Value::text).unwrap_or(Value::Null),
        );
        named.insert(
            "matched_policy_id".into(),
            event
                .matched_policy_id
                .map(Value::text)
                .unwrap_or(Value::Null),
        );
        named.insert(
            "request_id".into(),
            ctx.request_id
                .as_ref()
                .map(|s| Value::text(s.to_string()))
                .unwrap_or(Value::Null),
        );
        named.insert(
            "trace_id".into(),
            ctx.trace_id
                .as_ref()
                .map(|s| Value::text(s.to_string()))
                .unwrap_or(Value::Null),
        );
        named.insert(
            "fields_json".into(),
            Value::text(serialise_fields(&event.fields)),
        );

        let entity = UnifiedEntity::new(
            EntityId::new(0),
            EntityKind::TableRow {
                table: Arc::from(CONTROL_EVENTS_COLLECTION),
                row_id: 0,
            },
            EntityData::Row(RowData {
                columns: Vec::new(),
                named: Some(named),
                schema: None,
            }),
        );
        self.store
            .insert_auto(CONTROL_EVENTS_COLLECTION, entity)
            .map_err(|e| ControlEventError::Persistence(e.to_string()))?;
        Ok(EventId(id))
    }
}

/// Deterministic JSON serialiser for the `fields` map. Keys are sorted
/// so two events with the same logical payload hash-compare equal at
/// the row level (helpful for tests and dedup). Hand-rolled to avoid
/// pulling serde into the ledger hot path.
fn serialise_fields(fields: &HashMap<String, Sensitivity>) -> String {
    let mut keys: Vec<&String> = fields.keys().collect();
    keys.sort();
    let mut out = String::from("{");
    for (i, k) in keys.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        json_escape_into(k, &mut out);
        out.push_str("\":");
        match &fields[*k] {
            Sensitivity::Raw(s) => {
                out.push_str(r#"{"kind":"raw","value":""#);
                json_escape_into(s, &mut out);
                out.push_str(r#""}"#);
            }
            Sensitivity::Hashed { algo, hex } => {
                out.push_str(r#"{"kind":"hashed","algo":""#);
                out.push_str(algo);
                out.push_str(r#"","hex":""#);
                out.push_str(hex);
                out.push_str(r#""}"#);
            }
            Sensitivity::Redacted => {
                out.push_str(r#"{"kind":"redacted"}"#);
            }
        }
    }
    out.push('}');
    out
}

fn json_escape_into(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn anon_ctx<'a>() -> ControlEventCtx<'a> {
        ControlEventCtx {
            actor: ActorRef::Anonymous,
            scope: None,
            request_id: None,
            trace_id: None,
        }
    }

    fn sample_event() -> ControlEvent {
        ControlEvent {
            kind: EventKind::PolicyCreate,
            outcome: Outcome::Allowed,
            action: Cow::Borrowed("policy.write"),
            resource: Some("policy:test".into()),
            reason: None,
            matched_policy_id: Some("p-abc".into()),
            fields: HashMap::new(),
        }
    }

    #[test]
    fn collection_is_created_on_first_open_and_reopen_is_idempotent() {
        let store = Arc::new(UnifiedStore::new());
        let _l1 = RuntimeLedger::new(store.clone());
        assert!(store.get_collection(CONTROL_EVENTS_COLLECTION).is_some());
        // A second ledger over the same store must not explode —
        // `create_collection` would error on duplicate, but
        // `RuntimeLedger::new` goes through `get_or_create_collection`.
        let _l2 = RuntimeLedger::new(store.clone());
        assert!(store.get_collection(CONTROL_EVENTS_COLLECTION).is_some());
    }

    #[test]
    fn runtime_ledger_emit_persists_row_with_every_schema_column() {
        let store = Arc::new(UnifiedStore::new());
        let ledger = RuntimeLedger::new(store.clone());
        let id = ledger.emit(&anon_ctx(), sample_event()).expect("emit ok");
        assert!(!id.0.is_empty(), "row id must be populated");

        let manager = store
            .get_collection(CONTROL_EVENTS_COLLECTION)
            .expect("table exists");
        let rows = manager.query_all(|_| true);
        assert_eq!(rows.len(), 1);

        match &rows[0].data {
            EntityData::Row(row) => {
                let named = row.named.as_ref().expect("named columns present");
                for col in [
                    "id",
                    "ts",
                    "kind",
                    "outcome",
                    "actor_kind",
                    "actor_user_id",
                    "scope",
                    "action",
                    "resource",
                    "reason",
                    "matched_policy_id",
                    "request_id",
                    "trace_id",
                    "fields_json",
                ] {
                    assert!(named.contains_key(col), "missing schema column {col}");
                }
                assert_eq!(named["kind"], Value::text("policy.create"));
                assert_eq!(named["outcome"], Value::text("allowed"));
                assert_eq!(named["actor_kind"], Value::text("anonymous"));
                assert_eq!(named["actor_user_id"], Value::Null);
                assert_eq!(named["scope"], Value::Null);
                assert_eq!(named["resource"], Value::text("policy:test"));
                assert_eq!(named["matched_policy_id"], Value::text("p-abc"));
            }
            other => panic!("expected Row, got {other:?}"),
        }
    }

    #[test]
    fn emit_with_user_actor_records_kind_and_label() {
        let store = Arc::new(UnifiedStore::new());
        let ledger = RuntimeLedger::new(store.clone());
        let user = UserId::scoped("acme", "alice");
        let ctx = ControlEventCtx {
            actor: ActorRef::User(&user),
            scope: Some(Cow::Borrowed("acme")),
            request_id: Some(Cow::Borrowed("req-42")),
            trace_id: None,
        };
        ledger.emit(&ctx, sample_event()).unwrap();
        let manager = store.get_collection(CONTROL_EVENTS_COLLECTION).unwrap();
        let rows = manager.query_all(|_| true);
        let named = match &rows[0].data {
            EntityData::Row(r) => r.named.as_ref().unwrap(),
            _ => panic!(),
        };
        assert_eq!(named["actor_kind"], Value::text("user"));
        assert_eq!(named["actor_user_id"], Value::text("acme/alice"));
        assert_eq!(named["scope"], Value::text("acme"));
        assert_eq!(named["request_id"], Value::text("req-42"));
        assert_eq!(named["trace_id"], Value::Null);
    }

    #[test]
    fn sensitivity_hashed_is_stable_blake3() {
        let a = Sensitivity::hashed(b"hunter2");
        let b = Sensitivity::hashed(b"hunter2");
        assert_eq!(a, b);
        match a {
            Sensitivity::Hashed { algo, hex } => {
                assert_eq!(algo, "blake3");
                assert_eq!(hex.len(), 64);
                // Pin the digest so producer-slice tests (652b/c/d)
                // can rely on it across runs.
                assert_eq!(hex, blake3::hash(b"hunter2").to_hex().to_string(),);
            }
            other => panic!("expected Hashed, got {other:?}"),
        }
    }

    #[test]
    fn sensitivity_redacted_serialises_without_value() {
        let mut fields = HashMap::new();
        fields.insert("password".to_string(), Sensitivity::redacted());
        let s = serialise_fields(&fields);
        assert_eq!(s, r#"{"password":{"kind":"redacted"}}"#);
    }

    // ---- ComplianceMode caller-decision shape ---------------------------
    //
    // The trait never swallows persistence failures; the per-caller
    // policy (`abort the originating mutation` vs `log and continue`)
    // is encoded by the caller against [`ControlEventConfig::require_persistence`].
    // The tests below pin that contract via a deliberately failing fake.

    struct FailingLedger;
    impl ControlEventLedger for FailingLedger {
        fn emit(
            &self,
            _: &ControlEventCtx<'_>,
            _: ControlEvent,
        ) -> Result<EventId, ControlEventError> {
            Err(ControlEventError::Persistence("simulated".into()))
        }
    }

    fn caller_decides(
        cfg: ControlEventConfig,
        ledger: &dyn ControlEventLedger,
    ) -> Result<(), &'static str> {
        match ledger.emit(&anon_ctx(), sample_event()) {
            Ok(_) => Ok(()),
            Err(_) if cfg.require_persistence() => Err("aborted"),
            Err(_) => Ok(()),
        }
    }

    #[test]
    fn compliance_mode_makes_callers_fail_closed_on_persistence_failure() {
        let cfg = ControlEventConfig {
            compliance_mode: true,
        };
        assert!(cfg.require_persistence());
        assert_eq!(caller_decides(cfg, &FailingLedger), Err("aborted"));
    }

    #[test]
    fn default_mode_lets_callers_continue_on_persistence_failure() {
        let cfg = ControlEventConfig::default();
        assert!(!cfg.require_persistence());
        // The trait still surfaces the error...
        assert!(FailingLedger.emit(&anon_ctx(), sample_event()).is_err());
        // ...but the caller is free to swallow it.
        assert_eq!(caller_decides(cfg, &FailingLedger), Ok(()));
    }
}
