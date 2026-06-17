use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use reddb_server::auth::policies::{EvalContext, Policy};
use reddb_server::auth::registry::{
    ConfigRegistry, ConfigRegistryControl, ConfigRegistryDraft, EvidenceRequirement, Mutability,
    Sensitivity as RegistrySensitivity,
};
use reddb_server::auth::store::{AuthStore, PrincipalRef};
use reddb_server::auth::{AuthConfig, Role, UserId};
use reddb_server::runtime::control_events::{
    ActorRef, ControlEvent, ControlEventCtx, ControlEventError, ControlEventLedger, EventId,
    EventKind, Outcome, Sensitivity,
};
use reddb_server::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
use reddb_server::storage::schema::Value;
use reddb_server::{RedDBOptions, RedDBRuntime};

#[derive(Debug, Clone)]
struct CapturedEvent {
    actor_kind: String,
    kind: EventKind,
    outcome: Outcome,
    action: String,
    resource: Option<String>,
    reason: Option<String>,
    fields: HashMap<String, Sensitivity>,
}

#[derive(Default)]
struct CapturingLedger {
    events: Mutex<Vec<CapturedEvent>>,
}

impl ControlEventLedger for CapturingLedger {
    fn emit(
        &self,
        ctx: &ControlEventCtx<'_>,
        event: ControlEvent,
    ) -> Result<EventId, ControlEventError> {
        self.events.lock().unwrap().push(CapturedEvent {
            actor_kind: ctx.actor.kind_str().to_string(),
            kind: event.kind,
            outcome: event.outcome,
            action: event.action.to_string(),
            resource: event.resource,
            reason: event.reason,
            fields: event.fields,
        });
        Ok(EventId("captured".to_string()))
    }
}

struct FailingLedger;

impl ControlEventLedger for FailingLedger {
    fn emit(
        &self,
        _ctx: &ControlEventCtx<'_>,
        _event: ControlEvent,
    ) -> Result<EventId, ControlEventError> {
        Err(ControlEventError::Persistence("ledger down".to_string()))
    }
}

fn runtime_with_ledger(ledger: Arc<dyn ControlEventLedger>) -> RedDBRuntime {
    let rt = RedDBRuntime::in_memory().expect("runtime");
    rt.replace_control_event_ledger_for_tests(ledger);
    rt
}

fn compliance_runtime_with_ledger(ledger: Arc<dyn ControlEventLedger>) -> RedDBRuntime {
    let mut options = RedDBOptions::in_memory();
    options.control_events.compliance_mode = true;
    let rt = RedDBRuntime::with_options(options).expect("runtime");
    rt.replace_control_event_ledger_for_tests(ledger);
    rt
}

fn allow_registry_policy(id: &str) -> Policy {
    Policy::from_json_str(&format!(
        r#"{{
            "id":"{id}",
            "version":1,
            "statements":[{{
                "effect":"allow",
                "actions":["red.registry:*"],
                "resources":["registry:*"]
            }}]
        }}"#
    ))
    .unwrap()
}

fn attach_policy(auth: &AuthStore, user: &UserId, policy: Policy) {
    let id = policy.id.clone();
    auth.put_policy(policy).expect("put policy");
    auth.attach_policy(PrincipalRef::User(user.clone()), &id)
        .expect("attach policy");
}

fn registry_ctx() -> EvalContext {
    EvalContext {
        principal_is_admin_role: true,
        principal_is_platform_scoped: true,
        ..EvalContext::default()
    }
}

fn managed_config_draft(id: &str) -> ConfigRegistryDraft {
    ConfigRegistryDraft {
        id: id.to_string(),
        resource_type: reddb_server::auth::managed_config::RESOURCE_TYPE_CONFIG_KEY.to_string(),
        schema: "bool".to_string(),
        mutability: Mutability::MutableViaGovernance,
        sensitivity: RegistrySensitivity::Secret,
        managed: true,
        required_action: "config:write".to_string(),
        required_resource: format!("config:{id}"),
        evidence_requirement: EvidenceRequirement::Metadata,
    }
}

fn as_user<T>(name: &str, role: Role, f: impl FnOnce() -> T) -> T {
    set_current_auth_identity(name.to_string(), role);
    let out = f();
    clear_current_auth_identity();
    out
}

fn last_event(ledger: &CapturingLedger) -> CapturedEvent {
    ledger.events.lock().unwrap().last().unwrap().clone()
}

fn captured_config_events(ledger: &CapturingLedger) -> Vec<CapturedEvent> {
    ledger
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|event| event.action.starts_with("config:"))
        .cloned()
        .collect()
}

fn assert_raw_field(event: &CapturedEvent, name: &str, expected: &str) {
    match event.fields.get(name) {
        Some(Sensitivity::Raw(value)) => assert_eq!(value, expected),
        other => panic!("expected raw {name}={expected}, got {other:?}"),
    }
}

#[test]
fn put_and_delete_config_emit_allowed_events_with_hashed_payload() {
    let ledger = Arc::new(CapturingLedger::default());
    let rt = runtime_with_ledger(ledger.clone());

    rt.execute_query("PUT CONFIG app_settings api_key = 'hunter2'")
        .expect("put config");
    rt.execute_query("DELETE CONFIG app_settings api_key")
        .expect("delete config");

    let events: Vec<_> = captured_config_events(&ledger)
        .into_iter()
        .filter(|event| event.resource.as_deref() == Some("config:app_settings.api_key"))
        .collect();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, EventKind::ConfigWrite);
    assert_eq!(events[0].outcome, Outcome::Allowed);
    assert_eq!(events[0].action, "config:write");
    assert_eq!(
        events[0].resource.as_deref(),
        Some("config:app_settings.api_key")
    );
    assert_raw_field(&events[0], "resource_type", "config_key");
    assert_raw_field(&events[0], "id", "api_key");
    assert_raw_field(&events[0], "managed", "false");
    assert!(matches!(
        events[0].fields.get("payload"),
        Some(Sensitivity::Hashed { .. })
    ));

    assert_eq!(events[1].kind, EventKind::ConfigDelete);
    assert_eq!(events[1].outcome, Outcome::Allowed);
    assert_eq!(events[1].action, "config:delete");
    assert!(matches!(
        events[1].fields.get("payload"),
        Some(Sensitivity::Hashed { .. })
    ));
}

#[test]
fn managed_config_gate_denial_emits_denied_event() {
    let ledger = Arc::new(CapturingLedger::default());
    let rt = runtime_with_ledger(ledger.clone());
    let auth = Arc::new(AuthStore::new(AuthConfig::default()));
    let seeder = UserId::platform("seeder");
    auth.create_admin_user("seeder", "p", Role::Admin, None)
        .unwrap();
    attach_policy(&auth, &seeder, allow_registry_policy("p-registry"));
    auth.create_user("alice", "p", Role::Admin).unwrap();
    rt.set_auth_store(auth.clone());
    rt.config_registry()
        .register(
            &auth,
            &seeder,
            &registry_ctx(),
            managed_config_draft("managed_flag"),
            1_000,
        )
        .expect("register managed config");

    let err = as_user("alice", Role::Admin, || {
        rt.execute_query("PUT CONFIG app_settings managed_flag = true")
    })
    .expect_err("managed config write must be denied");
    assert!(
        err.to_string().contains("managed config mutation blocked"),
        "{err}"
    );

    let event = captured_config_events(&ledger)
        .into_iter()
        .find(|event| {
            event.outcome == Outcome::Denied
                && event
                    .reason
                    .as_deref()
                    .unwrap_or_default()
                    .contains("managed config mutation blocked")
        })
        .expect("managed denial event");
    assert_eq!(event.kind, EventKind::ConfigWrite);
    assert_eq!(event.outcome, Outcome::Denied);
    assert!(event
        .reason
        .as_ref()
        .unwrap()
        .contains("required policy permission was denied"));
    assert_raw_field(&event, "id", "managed_flag");
    assert_raw_field(&event, "managed", "true");
    assert!(matches!(
        event.fields.get("payload"),
        Some(Sensitivity::Hashed { .. })
    ));
}

#[test]
fn config_write_validation_error_emits_error_event() {
    let ledger = Arc::new(CapturingLedger::default());
    let rt = runtime_with_ledger(ledger.clone());

    let err = rt
        .execute_query("PUT CONFIG app_settings callback = 'not-a-url' WITH TYPE url")
        .expect_err("invalid url should fail");
    assert!(err.to_string().contains("CONFIG value type mismatch"));

    let event = last_event(&ledger);
    assert_eq!(event.kind, EventKind::ConfigWrite);
    assert_eq!(event.outcome, Outcome::Error);
    assert_eq!(event.action, "config:write");
    assert!(event.reason.unwrap().contains("CONFIG value type mismatch"));
}

#[test]
fn compliance_mode_rolls_back_config_write_when_allowed_event_fails() {
    let rt = compliance_runtime_with_ledger(Arc::new(FailingLedger));

    let err = rt
        .execute_query("PUT CONFIG app_settings api_key = 'hunter2'")
        .expect_err("failing ledger aborts write");
    assert!(err.to_string().contains("control-event persistence failed"));

    let missing = rt
        .execute_query("GET CONFIG app_settings api_key")
        .expect("empty config read should succeed");
    assert_eq!(
        missing.result.records[0].get("version"),
        Some(&Value::Null),
        "config write should roll back"
    );
}

#[test]
fn registry_supersede_emits_allowed_event_and_bootstrap_actor_is_system() {
    let store = AuthStore::new(AuthConfig::default());
    let registry = ConfigRegistry::new();
    let ledger = CapturingLedger::default();
    let actor = UserId::platform("bootstrap");
    store
        .create_admin_user("bootstrap", "p", Role::Admin, None)
        .unwrap();
    attach_policy(&store, &actor, allow_registry_policy("p-registry"));
    let event_ctx = ControlEventCtx {
        actor: ActorRef::System("bootstrap"),
        scope: None,
        request_id: Some(Cow::Borrowed("bootstrap-request")),
        trace_id: None,
    };
    let control = ConfigRegistryControl {
        ctx: &event_ctx,
        ledger: &ledger,
        config: Default::default(),
    };

    registry
        .register_with_control_events(
            &store,
            &actor,
            &registry_ctx(),
            managed_config_draft("red.config.audit.enabled"),
            1_000,
            &control,
        )
        .expect("register");
    let mut next = managed_config_draft("red.config.audit.enabled");
    next.schema = "bool-v2".to_string();
    registry
        .supersede_with_control_events(
            &store,
            &actor,
            &registry_ctx(),
            next,
            "tighten config schema",
            2_000,
            &control,
        )
        .expect("supersede");

    let events = ledger.events.lock().unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].actor_kind, "system");
    assert_eq!(events[0].outcome, Outcome::Allowed);
    assert_eq!(events[0].action, "red.registry:register");
    assert_eq!(events[1].actor_kind, "system");
    assert_eq!(events[1].outcome, Outcome::Allowed);
    assert_eq!(events[1].action, "red.registry:supersede");
    assert!(matches!(
        events[1].fields.get("payload"),
        Some(Sensitivity::Hashed { .. })
    ));
}
