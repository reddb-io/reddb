use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Mutex;

use reddb_server::auth::policies::{
    ActionPattern, Effect, EvalContext, Policy, ResourcePattern, Statement,
};
use reddb_server::auth::registry::{
    ConfigRegistry, ConfigRegistryDraft, EvidenceRequirement, Mutability,
    Sensitivity as RegistrySensitivity,
};
use reddb_server::auth::store::{AuthStore, PolicyMutationControl, PrincipalRef};
use reddb_server::auth::{AuthConfig, UserId};
use reddb_server::runtime::control_events::{
    ActorRef, ControlEvent, ControlEventCtx, ControlEventError, ControlEventLedger, EventId,
    EventKind, Outcome,
};

#[derive(Debug, Clone)]
struct CapturedEvent {
    kind: EventKind,
    outcome: Outcome,
    action: String,
    resource: Option<String>,
    reason: Option<String>,
    matched_policy_id: Option<String>,
    fields: HashMap<String, reddb_server::runtime::control_events::Sensitivity>,
}

#[derive(Default)]
struct CapturingLedger {
    events: Mutex<Vec<CapturedEvent>>,
}

impl ControlEventLedger for CapturingLedger {
    fn emit(
        &self,
        _ctx: &ControlEventCtx<'_>,
        event: ControlEvent,
    ) -> Result<EventId, ControlEventError> {
        self.events.lock().unwrap().push(CapturedEvent {
            kind: event.kind,
            outcome: event.outcome,
            action: event.action.to_string(),
            resource: event.resource,
            reason: event.reason,
            matched_policy_id: event.matched_policy_id,
            fields: event.fields,
        });
        Ok(EventId("test-event".to_string()))
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

fn allow_policy(id: &str, action: &str, resource_kind: &str, resource_name: &str) -> Policy {
    Policy {
        id: id.to_string(),
        version: 1,
        statements: vec![Statement {
            sid: Some("allow".to_string()),
            effect: Effect::Allow,
            actions: vec![ActionPattern::Exact(action.to_string())],
            resources: vec![ResourcePattern::Exact {
                kind: resource_kind.to_string(),
                name: resource_name.to_string(),
            }],
            condition: None,
        }],
        tenant: None,
        created_at: 1_000,
        updated_at: 1_000,
    }
}

fn control<'a>(
    ctx: &'a ControlEventCtx<'a>,
    ledger: &'a dyn ControlEventLedger,
    registry: &'a ConfigRegistry,
    actor: &'a UserId,
    eval_ctx: &'a EvalContext,
) -> PolicyMutationControl<'a> {
    PolicyMutationControl {
        ctx,
        ledger,
        config: Default::default(),
        registry: Some(registry),
        actor,
        eval_ctx,
    }
}

fn compliance_control<'a>(
    ctx: &'a ControlEventCtx<'a>,
    ledger: &'a dyn ControlEventLedger,
    registry: &'a ConfigRegistry,
    actor: &'a UserId,
    eval_ctx: &'a EvalContext,
) -> PolicyMutationControl<'a> {
    PolicyMutationControl {
        ctx,
        ledger,
        config: reddb_server::runtime::control_events::ControlEventConfig {
            compliance_mode: true,
        },
        registry: Some(registry),
        actor,
        eval_ctx,
    }
}

fn actor_ctx<'a>(actor: &'a UserId) -> ControlEventCtx<'a> {
    ControlEventCtx {
        actor: ActorRef::User(actor),
        scope: None,
        request_id: Some(Cow::Borrowed("test-request")),
        trace_id: None,
    }
}

fn register_managed_policy(registry: &ConfigRegistry, store: &AuthStore, policy_id: &str) {
    let seeder = UserId::platform("seeder");
    store
        .put_policy(allow_policy(
            "p-registry-register",
            "red.registry:register",
            "registry",
            policy_id,
        ))
        .expect("registry allow policy");
    store
        .attach_policy(PrincipalRef::User(seeder.clone()), "p-registry-register")
        .expect("attach registry allow policy");
    let mut ctx = EvalContext::default();
    ctx.principal_is_admin_role = true;
    ctx.principal_is_platform_scoped = true;
    registry
        .register(
            store,
            &seeder,
            &ctx,
            ConfigRegistryDraft {
                id: policy_id.to_string(),
                resource_type: reddb_server::auth::managed_policy::RESOURCE_TYPE_POLICY.to_string(),
                schema: "iam-policy-v1".to_string(),
                mutability: Mutability::MutableViaGovernance,
                sensitivity: RegistrySensitivity::Internal,
                managed: true,
                required_action: "policy:put".to_string(),
                required_resource: format!("policy:{policy_id}"),
                evidence_requirement: EvidenceRequirement::Metadata,
            },
            1_000,
        )
        .expect("managed policy registry entry");
}

fn ordinary_eval_ctx() -> EvalContext {
    EvalContext::default()
}

fn last_event(ledger: &CapturingLedger) -> CapturedEvent {
    ledger.events.lock().unwrap().last().unwrap().clone()
}

fn assert_event(
    event: &CapturedEvent,
    kind: EventKind,
    outcome: Outcome,
    action: &str,
    policy_id: &str,
) {
    assert_eq!(event.kind, kind);
    assert_eq!(event.outcome, outcome);
    assert_eq!(event.action, action);
    assert_eq!(
        event.resource.as_deref(),
        Some(format!("policy:{policy_id}").as_str())
    );
}

#[test]
fn create_policy_allowed_emits_control_event() {
    let store = AuthStore::new(AuthConfig::default());
    let registry = ConfigRegistry::new();
    let ledger = CapturingLedger::default();
    let actor = UserId::platform("operator");
    let event_ctx = actor_ctx(&actor);
    let eval_ctx = EvalContext::default();
    let policy = allow_policy("p-read", "select", "table", "orders");

    store
        .put_policy_with_control_events(
            policy,
            &control(&event_ctx, &ledger, &registry, &actor, &eval_ctx),
        )
        .expect("policy create succeeds");

    assert!(store.get_policy("p-read").is_some());
    let events = ledger.events.lock().unwrap();
    assert_eq!(events.len(), 1);
    let event = &events[0];
    assert_eq!(event.kind, EventKind::PolicyCreate);
    assert_eq!(event.outcome, Outcome::Allowed);
    assert_eq!(event.action, "policy:put");
    assert_eq!(event.resource.as_deref(), Some("policy:p-read"));
    assert_eq!(event.reason, None);
    assert_eq!(event.matched_policy_id, None);
    assert!(event.fields.contains_key("policy_id"));
    assert!(event.fields.contains_key("effect"));
    assert!(event.fields.contains_key("action"));
    assert!(event.fields.contains_key("resource"));
}

#[test]
fn create_policy_denied_emits_control_event() {
    let store = AuthStore::new(AuthConfig::default());
    let registry = ConfigRegistry::new();
    register_managed_policy(&registry, &store, "p-managed");
    let ledger = CapturingLedger::default();
    let actor = UserId::platform("ordinary");
    let event_ctx = actor_ctx(&actor);
    let eval_ctx = ordinary_eval_ctx();

    let err = store
        .put_policy_with_control_events(
            allow_policy("p-managed", "select", "table", "orders"),
            &control(&event_ctx, &ledger, &registry, &actor, &eval_ctx),
        )
        .expect_err("managed create is denied");

    assert!(err.to_string().contains("managed policy mutation blocked"));
    assert!(store.get_policy("p-managed").is_none());
    let event = last_event(&ledger);
    assert_event(
        &event,
        EventKind::PolicyCreate,
        Outcome::Denied,
        "policy:put",
        "p-managed",
    );
    assert!(event
        .reason
        .unwrap()
        .contains("required IAM permission was denied"));
    assert_eq!(event.matched_policy_id.as_deref(), Some("p-managed"));
}

#[test]
fn create_policy_error_emits_control_event() {
    let store = AuthStore::new(AuthConfig::default());
    let registry = ConfigRegistry::new();
    let ledger = CapturingLedger::default();
    let actor = UserId::platform("operator");
    let event_ctx = actor_ctx(&actor);
    let eval_ctx = ordinary_eval_ctx();

    let err = store
        .put_policy_with_control_events(
            allow_policy("_grant_reserved", "select", "table", "orders"),
            &control(&event_ctx, &ledger, &registry, &actor, &eval_ctx),
        )
        .expect_err("reserved policy id is rejected");

    assert!(err.to_string().contains("reserved"));
    let event = last_event(&ledger);
    assert_event(
        &event,
        EventKind::PolicyCreate,
        Outcome::Error,
        "policy:put",
        "_grant_reserved",
    );
}

#[test]
fn update_policy_allowed_emits_control_event() {
    let store = AuthStore::new(AuthConfig::default());
    store
        .put_policy(allow_policy("p-read", "select", "table", "orders"))
        .unwrap();
    let registry = ConfigRegistry::new();
    let ledger = CapturingLedger::default();
    let actor = UserId::platform("operator");
    let event_ctx = actor_ctx(&actor);
    let eval_ctx = ordinary_eval_ctx();

    store
        .put_policy_with_control_events(
            allow_policy("p-read", "insert", "table", "orders"),
            &control(&event_ctx, &ledger, &registry, &actor, &eval_ctx),
        )
        .expect("policy update succeeds");

    let event = last_event(&ledger);
    assert_event(
        &event,
        EventKind::PolicyUpdate,
        Outcome::Allowed,
        "policy:put",
        "p-read",
    );
}

#[test]
fn update_policy_denied_emits_control_event() {
    let store = AuthStore::new(AuthConfig::default());
    store
        .put_policy(allow_policy("p-managed", "select", "table", "orders"))
        .unwrap();
    let registry = ConfigRegistry::new();
    register_managed_policy(&registry, &store, "p-managed");
    let ledger = CapturingLedger::default();
    let actor = UserId::platform("ordinary");
    let event_ctx = actor_ctx(&actor);
    let eval_ctx = ordinary_eval_ctx();

    store
        .put_policy_with_control_events(
            allow_policy("p-managed", "insert", "table", "orders"),
            &control(&event_ctx, &ledger, &registry, &actor, &eval_ctx),
        )
        .expect_err("managed update is denied");

    let event = last_event(&ledger);
    assert_event(
        &event,
        EventKind::PolicyUpdate,
        Outcome::Denied,
        "policy:put",
        "p-managed",
    );
}

#[test]
fn update_policy_error_emits_control_event() {
    let store = AuthStore::new(AuthConfig::default());
    store
        .put_policy(allow_policy("p-read", "select", "table", "orders"))
        .unwrap();
    let registry = ConfigRegistry::new();
    let ledger = CapturingLedger::default();
    let actor = UserId::platform("operator");
    let event_ctx = actor_ctx(&actor);
    let eval_ctx = ordinary_eval_ctx();

    store
        .put_policy_with_control_events(
            allow_policy("p-read", "not-an-action", "table", "orders"),
            &control(&event_ctx, &ledger, &registry, &actor, &eval_ctx),
        )
        .expect_err("invalid policy update is rejected");

    let event = last_event(&ledger);
    assert_event(
        &event,
        EventKind::PolicyUpdate,
        Outcome::Error,
        "policy:put",
        "p-read",
    );
}

#[test]
fn delete_policy_allowed_emits_control_event() {
    let store = AuthStore::new(AuthConfig::default());
    store
        .put_policy(allow_policy("p-read", "select", "table", "orders"))
        .unwrap();
    let registry = ConfigRegistry::new();
    let ledger = CapturingLedger::default();
    let actor = UserId::platform("operator");
    let event_ctx = actor_ctx(&actor);
    let eval_ctx = ordinary_eval_ctx();

    store
        .delete_policy_with_control_events(
            "p-read",
            &control(&event_ctx, &ledger, &registry, &actor, &eval_ctx),
        )
        .expect("policy delete succeeds");

    assert!(store.get_policy("p-read").is_none());
    let event = last_event(&ledger);
    assert_event(
        &event,
        EventKind::PolicyDelete,
        Outcome::Allowed,
        "policy:drop",
        "p-read",
    );
}

#[test]
fn delete_policy_denied_emits_control_event() {
    let store = AuthStore::new(AuthConfig::default());
    store
        .put_policy(allow_policy("p-managed", "select", "table", "orders"))
        .unwrap();
    let registry = ConfigRegistry::new();
    register_managed_policy(&registry, &store, "p-managed");
    let ledger = CapturingLedger::default();
    let actor = UserId::platform("ordinary");
    let event_ctx = actor_ctx(&actor);
    let eval_ctx = ordinary_eval_ctx();

    store
        .delete_policy_with_control_events(
            "p-managed",
            &control(&event_ctx, &ledger, &registry, &actor, &eval_ctx),
        )
        .expect_err("managed delete is denied");

    assert!(store.get_policy("p-managed").is_some());
    let event = last_event(&ledger);
    assert_event(
        &event,
        EventKind::PolicyDelete,
        Outcome::Denied,
        "policy:drop",
        "p-managed",
    );
}

#[test]
fn delete_policy_error_emits_control_event() {
    let store = AuthStore::new(AuthConfig::default());
    let registry = ConfigRegistry::new();
    let ledger = CapturingLedger::default();
    let actor = UserId::platform("operator");
    let event_ctx = actor_ctx(&actor);
    let eval_ctx = ordinary_eval_ctx();

    store
        .delete_policy_with_control_events(
            "missing",
            &control(&event_ctx, &ledger, &registry, &actor, &eval_ctx),
        )
        .expect_err("missing policy delete is rejected");

    let event = last_event(&ledger);
    assert_event(
        &event,
        EventKind::PolicyDelete,
        Outcome::Error,
        "policy:drop",
        "missing",
    );
}

#[test]
fn attach_policy_allowed_emits_control_event() {
    let store = AuthStore::new(AuthConfig::default());
    store
        .put_policy(allow_policy("p-read", "select", "table", "orders"))
        .unwrap();
    let registry = ConfigRegistry::new();
    let ledger = CapturingLedger::default();
    let actor = UserId::platform("operator");
    let event_ctx = actor_ctx(&actor);
    let eval_ctx = ordinary_eval_ctx();
    let target = UserId::platform("alice");

    store
        .attach_policy_with_control_events(
            PrincipalRef::User(target.clone()),
            "p-read",
            &control(&event_ctx, &ledger, &registry, &actor, &eval_ctx),
        )
        .expect("policy attach succeeds");

    assert_eq!(store.effective_policies(&target).len(), 1);
    let event = last_event(&ledger);
    assert_event(
        &event,
        EventKind::PolicyAttach,
        Outcome::Allowed,
        "policy:attach",
        "p-read",
    );
}

#[test]
fn attach_policy_denied_emits_control_event() {
    let store = AuthStore::new(AuthConfig::default());
    store
        .put_policy(allow_policy("p-managed", "select", "table", "orders"))
        .unwrap();
    let registry = ConfigRegistry::new();
    register_managed_policy(&registry, &store, "p-managed");
    let ledger = CapturingLedger::default();
    let actor = UserId::platform("ordinary");
    let event_ctx = actor_ctx(&actor);
    let eval_ctx = ordinary_eval_ctx();

    store
        .attach_policy_with_control_events(
            PrincipalRef::User(UserId::platform("alice")),
            "p-managed",
            &control(&event_ctx, &ledger, &registry, &actor, &eval_ctx),
        )
        .expect_err("managed attach is denied");

    let event = last_event(&ledger);
    assert_event(
        &event,
        EventKind::PolicyAttach,
        Outcome::Denied,
        "policy:attach",
        "p-managed",
    );
}

#[test]
fn attach_policy_error_emits_control_event() {
    let store = AuthStore::new(AuthConfig::default());
    let registry = ConfigRegistry::new();
    let ledger = CapturingLedger::default();
    let actor = UserId::platform("operator");
    let event_ctx = actor_ctx(&actor);
    let eval_ctx = ordinary_eval_ctx();

    store
        .attach_policy_with_control_events(
            PrincipalRef::User(UserId::platform("alice")),
            "missing",
            &control(&event_ctx, &ledger, &registry, &actor, &eval_ctx),
        )
        .expect_err("missing policy attach is rejected");

    let event = last_event(&ledger);
    assert_event(
        &event,
        EventKind::PolicyAttach,
        Outcome::Error,
        "policy:attach",
        "missing",
    );
}

#[test]
fn detach_policy_allowed_emits_control_event() {
    let store = AuthStore::new(AuthConfig::default());
    let target = UserId::platform("alice");
    store
        .put_policy(allow_policy("p-read", "select", "table", "orders"))
        .unwrap();
    store
        .attach_policy(PrincipalRef::User(target.clone()), "p-read")
        .unwrap();
    let registry = ConfigRegistry::new();
    let ledger = CapturingLedger::default();
    let actor = UserId::platform("operator");
    let event_ctx = actor_ctx(&actor);
    let eval_ctx = ordinary_eval_ctx();

    store
        .detach_policy_with_control_events(
            PrincipalRef::User(target.clone()),
            "p-read",
            &control(&event_ctx, &ledger, &registry, &actor, &eval_ctx),
        )
        .expect("policy detach succeeds");

    assert!(store.effective_policies(&target).is_empty());
    let event = last_event(&ledger);
    assert_event(
        &event,
        EventKind::PolicyDetach,
        Outcome::Allowed,
        "policy:detach",
        "p-read",
    );
}

#[test]
fn detach_policy_denied_emits_control_event() {
    let store = AuthStore::new(AuthConfig::default());
    store
        .put_policy(allow_policy("p-managed", "select", "table", "orders"))
        .unwrap();
    let registry = ConfigRegistry::new();
    register_managed_policy(&registry, &store, "p-managed");
    let ledger = CapturingLedger::default();
    let actor = UserId::platform("ordinary");
    let event_ctx = actor_ctx(&actor);
    let eval_ctx = ordinary_eval_ctx();

    store
        .detach_policy_with_control_events(
            PrincipalRef::User(UserId::platform("alice")),
            "p-managed",
            &control(&event_ctx, &ledger, &registry, &actor, &eval_ctx),
        )
        .expect_err("managed detach is denied");

    let event = last_event(&ledger);
    assert_event(
        &event,
        EventKind::PolicyDetach,
        Outcome::Denied,
        "policy:detach",
        "p-managed",
    );
}

#[test]
fn detach_policy_error_emits_control_event() {
    let store = AuthStore::new(AuthConfig::default());
    let registry = ConfigRegistry::new();
    let ledger = CapturingLedger::default();
    let actor = UserId::platform("operator");
    let event_ctx = actor_ctx(&actor);
    let eval_ctx = ordinary_eval_ctx();

    store
        .detach_policy_with_control_events(
            PrincipalRef::User(UserId::platform("alice")),
            "missing",
            &control(&event_ctx, &ledger, &registry, &actor, &eval_ctx),
        )
        .expect_err("missing policy detach is rejected");

    let event = last_event(&ledger);
    assert_event(
        &event,
        EventKind::PolicyDetach,
        Outcome::Error,
        "policy:detach",
        "missing",
    );
}

#[test]
fn compliance_mode_rolls_back_policy_mutations_when_ledger_fails() {
    let store = AuthStore::new(AuthConfig::default());
    let registry = ConfigRegistry::new();
    let ledger = FailingLedger;
    let actor = UserId::platform("operator");
    let event_ctx = actor_ctx(&actor);
    let eval_ctx = ordinary_eval_ctx();

    store
        .put_policy_with_control_events(
            allow_policy("p-create", "select", "table", "orders"),
            &compliance_control(&event_ctx, &ledger, &registry, &actor, &eval_ctx),
        )
        .expect_err("create rolls back when allowed event cannot persist");
    assert!(store.get_policy("p-create").is_none());

    store
        .put_policy(allow_policy("p-update", "select", "table", "orders"))
        .unwrap();
    store
        .put_policy_with_control_events(
            allow_policy("p-update", "insert", "table", "orders"),
            &compliance_control(&event_ctx, &ledger, &registry, &actor, &eval_ctx),
        )
        .expect_err("update rolls back when allowed event cannot persist");
    let restored = store.get_policy("p-update").unwrap();
    let action = restored.statements[0].actions[0].clone();
    assert_eq!(action, ActionPattern::Exact("select".to_string()));

    store
        .put_policy(allow_policy("p-delete", "select", "table", "orders"))
        .unwrap();
    store
        .delete_policy_with_control_events(
            "p-delete",
            &compliance_control(&event_ctx, &ledger, &registry, &actor, &eval_ctx),
        )
        .expect_err("delete rolls back when allowed event cannot persist");
    assert!(store.get_policy("p-delete").is_some());

    store
        .put_policy(allow_policy("p-attach", "select", "table", "orders"))
        .unwrap();
    let target = UserId::platform("alice");
    store
        .attach_policy_with_control_events(
            PrincipalRef::User(target.clone()),
            "p-attach",
            &compliance_control(&event_ctx, &ledger, &registry, &actor, &eval_ctx),
        )
        .expect_err("attach rolls back when allowed event cannot persist");
    assert!(store.effective_policies(&target).is_empty());

    store
        .attach_policy(PrincipalRef::User(target.clone()), "p-attach")
        .unwrap();
    store
        .detach_policy_with_control_events(
            PrincipalRef::User(target.clone()),
            "p-attach",
            &compliance_control(&event_ctx, &ledger, &registry, &actor, &eval_ctx),
        )
        .expect_err("detach rolls back when allowed event cannot persist");
    assert_eq!(store.effective_policies(&target).len(), 1);
}
