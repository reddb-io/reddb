use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use reddb_server::auth::store::AuthStore;
use reddb_server::auth::{AuthConfig, Role, UserId};
use reddb_server::runtime::control_events::{
    ActorRef, ControlEvent, ControlEventConfig, ControlEventCtx, ControlEventError,
    ControlEventLedger, EventId, EventKind, Outcome, Sensitivity,
};

#[derive(Debug)]
struct CapturedEvent {
    actor_kind: String,
    kind: EventKind,
    outcome: Outcome,
    fields: HashMap<String, Sensitivity>,
}

#[derive(Default)]
struct CapturingLedger {
    events: Mutex<Vec<CapturedEvent>>,
}

impl CapturingLedger {
    fn events(&self) -> Vec<CapturedEvent> {
        self.events.lock().expect("events lock").clone()
    }
}

impl Clone for CapturedEvent {
    fn clone(&self) -> Self {
        Self {
            actor_kind: self.actor_kind.clone(),
            kind: self.kind,
            outcome: self.outcome,
            fields: self.fields.clone(),
        }
    }
}

impl ControlEventLedger for CapturingLedger {
    fn emit(
        &self,
        ctx: &ControlEventCtx<'_>,
        event: ControlEvent,
    ) -> Result<EventId, ControlEventError> {
        self.events
            .lock()
            .expect("events lock")
            .push(CapturedEvent {
                actor_kind: ctx.actor.kind_str().to_string(),
                kind: event.kind,
                outcome: event.outcome,
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

fn actor_ctx<'a>(actor: &'a UserId) -> ControlEventCtx<'a> {
    ControlEventCtx {
        actor: ActorRef::User(actor),
        scope: None,
        request_id: None,
        trace_id: None,
    }
}

fn compliance_config() -> ControlEventConfig {
    ControlEventConfig {
        compliance_mode: true,
    }
}

fn store() -> AuthStore {
    AuthStore::new(AuthConfig::default())
}

#[test]
fn user_lifecycle_paths_emit_allowed_and_denied_events() {
    let store = store();
    let actor = UserId::platform("admin");
    let ctx = actor_ctx(&actor);
    let ledger = CapturingLedger::default();

    store
        .create_user_with_control_events(
            "alice",
            "secret",
            Role::Admin,
            &ctx,
            &ledger,
            ControlEventConfig::default(),
        )
        .expect("create user");
    store
        .create_user_with_control_events(
            "alice",
            "secret",
            Role::Read,
            &ctx,
            &ledger,
            ControlEventConfig::default(),
        )
        .expect_err("duplicate user is denied");
    store
        .change_role_in_tenant_with_control_events(
            None,
            "alice",
            Role::Read,
            &ctx,
            &ledger,
            ControlEventConfig::default(),
        )
        .expect("update user");
    store
        .disable_user("alice", &ctx, &ledger, ControlEventConfig::default())
        .expect("disable user");
    let api_key = store
        .create_api_key_with_control_events(
            "alice",
            "ci",
            Role::Read,
            &ctx,
            &ledger,
            ControlEventConfig::default(),
        )
        .expect("create api key");
    store
        .revoke_api_key_with_control_events(
            &api_key.key,
            &ctx,
            &ledger,
            ControlEventConfig::default(),
        )
        .expect("revoke api key");
    store
        .delete_user_with_control_events("missing", &ctx, &ledger, ControlEventConfig::default())
        .expect_err("missing user is denied");
    store
        .delete_user_with_control_events("alice", &ctx, &ledger, ControlEventConfig::default())
        .expect("delete user");

    let events = ledger.events();
    assert!(events
        .iter()
        .any(|e| e.kind == EventKind::UserCreate && e.outcome == Outcome::Allowed));
    assert!(events
        .iter()
        .any(|e| e.kind == EventKind::UserCreate && e.outcome == Outcome::Denied));
    assert!(events
        .iter()
        .any(|e| e.kind == EventKind::UserUpdate && e.outcome == Outcome::Allowed));
    assert!(events
        .iter()
        .any(|e| e.kind == EventKind::UserDisable && e.outcome == Outcome::Allowed));
    assert!(events
        .iter()
        .any(|e| e.kind == EventKind::ApiKeyCreate && e.outcome == Outcome::Allowed));
    assert!(events
        .iter()
        .any(|e| e.kind == EventKind::ApiKeyRevoke && e.outcome == Outcome::Allowed));
    assert!(events
        .iter()
        .any(|e| e.kind == EventKind::UserDelete && e.outcome == Outcome::Allowed));
    assert!(events
        .iter()
        .any(|e| e.kind == EventKind::UserDelete && e.outcome == Outcome::Denied));
}

#[test]
fn bootstrap_emits_single_system_user_create_event() {
    let store = store();
    let ledger = Arc::new(CapturingLedger::default());
    store.configure_control_events(ledger.clone(), ControlEventConfig::default());

    store.bootstrap("root", "secret").expect("bootstrap");

    let events = ledger.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].actor_kind, "system");
    assert_eq!(events[0].kind, EventKind::UserCreate);
    assert_eq!(events[0].outcome, Outcome::Allowed);
    assert!(matches!(
        events[0].fields.get("password"),
        Some(Sensitivity::Redacted)
    ));
}

#[test]
fn emitted_user_events_never_store_raw_password_or_plaintext_api_key() {
    let store = store();
    let actor = UserId::platform("admin");
    let ctx = actor_ctx(&actor);
    let ledger = CapturingLedger::default();

    store
        .create_user_with_control_events(
            "alice",
            "super-secret-password",
            Role::Admin,
            &ctx,
            &ledger,
            ControlEventConfig::default(),
        )
        .expect("create user");
    store
        .change_password_with_control_events(
            "alice",
            "super-secret-password",
            "new-super-secret-password",
            &ctx,
            &ledger,
            ControlEventConfig::default(),
        )
        .expect("change password");
    let api_key = store
        .create_api_key_with_control_events(
            "alice",
            "ci",
            Role::Read,
            &ctx,
            &ledger,
            ControlEventConfig::default(),
        )
        .expect("create api key");

    for event in ledger.events() {
        if let Some(password) = event.fields.get("password") {
            assert!(
                !matches!(password, Sensitivity::Raw(_)),
                "password must not be raw"
            );
        }
        if let Some(api_key_field) = event.fields.get("api_key") {
            assert_eq!(api_key_field, &Sensitivity::Redacted);
        }
        for value in event.fields.values() {
            if let Sensitivity::Raw(raw) = value {
                assert_ne!(raw, "super-secret-password");
                assert_ne!(raw, "new-super-secret-password");
                assert_ne!(raw, &api_key.key);
            }
        }
    }

    let events = ledger.events();
    let api_event = events
        .iter()
        .find(|event| event.kind == EventKind::ApiKeyCreate)
        .expect("api key event");
    assert!(matches!(
        api_event.fields.get("api_key_id"),
        Some(Sensitivity::Raw(_))
    ));
}

#[test]
fn compliance_mode_rolls_back_user_lifecycle_mutations_when_allowed_event_fails() {
    let actor = UserId::platform("admin");
    let ctx = actor_ctx(&actor);
    let ledger = FailingLedger;

    let create_store = store();
    create_store
        .create_user_with_control_events(
            "alice",
            "secret",
            Role::Admin,
            &ctx,
            &ledger,
            compliance_config(),
        )
        .expect_err("failing ledger rejects create");
    assert!(create_store.get_user(None, "alice").is_none());

    let update_store = store();
    update_store
        .create_user("alice", "secret", Role::Admin)
        .expect("seed user");
    update_store
        .change_role_in_tenant_with_control_events(
            None,
            "alice",
            Role::Read,
            &ctx,
            &ledger,
            compliance_config(),
        )
        .expect_err("failing ledger rejects update");
    assert_eq!(
        update_store.get_user(None, "alice").expect("user").role,
        Role::Admin
    );

    let disable_store = store();
    disable_store
        .create_user("alice", "secret", Role::Admin)
        .expect("seed user");
    disable_store
        .disable_user("alice", &ctx, &ledger, compliance_config())
        .expect_err("failing ledger rejects disable");
    assert!(disable_store.get_user(None, "alice").expect("user").enabled);

    let delete_store = store();
    delete_store
        .create_user("alice", "secret", Role::Admin)
        .expect("seed user");
    delete_store
        .delete_user_with_control_events("alice", &ctx, &ledger, compliance_config())
        .expect_err("failing ledger rejects delete");
    assert!(delete_store.get_user(None, "alice").is_some());

    let api_create_store = store();
    api_create_store
        .create_user("alice", "secret", Role::Admin)
        .expect("seed user");
    api_create_store
        .create_api_key_with_control_events(
            "alice",
            "ci",
            Role::Read,
            &ctx,
            &ledger,
            compliance_config(),
        )
        .expect_err("failing ledger rejects api key create");
    assert!(api_create_store
        .get_user(None, "alice")
        .expect("user")
        .api_keys
        .is_empty());

    let api_revoke_store = store();
    api_revoke_store
        .create_user("alice", "secret", Role::Admin)
        .expect("seed user");
    let key = api_revoke_store
        .create_api_key("alice", "ci", Role::Read)
        .expect("seed key");
    api_revoke_store
        .revoke_api_key_with_control_events(&key.key, &ctx, &ledger, compliance_config())
        .expect_err("failing ledger rejects api key revoke");
    assert!(api_revoke_store.validate_token_full(&key.key).is_some());

    let bootstrap_store = store();
    bootstrap_store
        .bootstrap_with_control_events("root", "secret", &ctx, &ledger, compliance_config())
        .expect_err("failing ledger rejects bootstrap");
    assert!(bootstrap_store.needs_bootstrap());
    assert!(bootstrap_store.list_users().is_empty());
}
