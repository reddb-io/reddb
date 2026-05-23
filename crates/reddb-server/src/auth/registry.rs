//! `red.registry` — governance metadata surface for RedDB-owned resources.
//!
//! Tracer slice for #648. The registry governs metadata about config
//! resources (type/schema, mutability, sensitivity, managed status,
//! required action/resource, evidence requirement). Values themselves
//! live in their native stores — `red.config`, `red.vault`, the policy
//! store; the registry only describes how those values are validated
//! and authorized.
//!
//! Invariants:
//!
//! * The active surface returns the current version for any registered
//!   resource.
//! * History records every superseded version with actor, time, and a
//!   change reason.
//! * Entries are mutated only through this module's governance API
//!   ([`ConfigRegistry::register`] / [`ConfigRegistry::supersede`]),
//!   which calls into [`AuthStore::check_policy_authz`]. There is no
//!   SQL surface — ordinary DML cannot reach these entries by
//!   construction.

use std::collections::HashMap;
use std::sync::RwLock;

use super::policies::{EvalContext, ResourceRef};
use super::store::AuthStore;
use super::UserId;

/// How a config resource may be changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mutability {
    /// Fixed at registration — supersede is rejected.
    Immutable,
    /// Mutable only via governance commands (registry API), never via DML.
    MutableViaGovernance,
}

/// Data classification of the underlying value the entry governs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sensitivity {
    Public,
    Internal,
    Confidential,
    Secret,
}

/// Evidence the Control Event Ledger must capture for mutations of the
/// underlying resource. Metadata-only is the default; `Full` includes
/// the previous and next normalized value fingerprints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceRequirement {
    None,
    Metadata,
    Full,
}

/// A single registry entry — the governance metadata for one config
/// resource (a config key, a vault path, a policy id, an audit surface).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRegistryEntry {
    /// Canonical resource id (e.g. `"red.config.audit.enabled"`).
    pub id: String,
    /// Monotonically increasing version. Starts at 1 on register; each
    /// `supersede` increments by one.
    pub version: u64,
    /// Logical type of the resource — e.g. `"config_key"`, `"vault_path"`,
    /// `"policy"`, `"audit_surface"`.
    pub resource_type: String,
    /// Schema / value-shape description (free-form for the tracer; a
    /// future slice can promote this to a structured schema id).
    pub schema: String,
    pub mutability: Mutability,
    pub sensitivity: Sensitivity,
    /// `true` for operator-owned guardrail entries (managed-policy /
    /// managed-config namespace style). `false` for ordinary entries.
    pub managed: bool,
    /// Policy action a caller must satisfy to mutate the underlying
    /// resource (not the registry entry itself).
    pub required_action: String,
    /// Policy resource the action applies to.
    pub required_resource: String,
    pub evidence_requirement: EvidenceRequirement,
    /// Display form of the principal who last wrote this entry.
    pub updated_by: String,
    /// Unix ms when this version became active.
    pub updated_at_ms: u128,
}

/// One row of registry history — a superseded version plus the
/// who/when/why metadata for the change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRegistryHistoryRecord {
    pub entry: ConfigRegistryEntry,
    /// Unix ms when the entry was superseded (i.e. when the *next*
    /// version became active).
    pub superseded_at_ms: u128,
    /// Display form of the principal that wrote the superseding entry.
    pub superseded_by: String,
    pub change_reason: String,
}

/// Errors surfaced by the registry's governance API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryError {
    /// Caller failed the policy-first authorization check.
    Unauthorized { action: String, resource: String },
    /// Lookup target does not exist.
    NotFound(String),
    /// Tried to supersede an `Immutable` entry.
    Immutable(String),
    /// Tried to register an id that already has an active entry.
    AlreadyRegistered(String),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized { action, resource } => write!(
                f,
                "registry mutation denied by policy: action={action} resource={resource}"
            ),
            Self::NotFound(id) => write!(f, "registry entry not found: {id}"),
            Self::Immutable(id) => write!(f, "registry entry is immutable: {id}"),
            Self::AlreadyRegistered(id) => write!(f, "registry entry already exists: {id}"),
        }
    }
}

impl std::error::Error for RegistryError {}

/// Draft used when calling [`ConfigRegistry::register`] or
/// [`ConfigRegistry::supersede`]. The registry stamps `version`,
/// `updated_by`, and `updated_at_ms` itself so callers can't forge them.
#[derive(Debug, Clone)]
pub struct ConfigRegistryDraft {
    pub id: String,
    pub resource_type: String,
    pub schema: String,
    pub mutability: Mutability,
    pub sensitivity: Sensitivity,
    pub managed: bool,
    pub required_action: String,
    pub required_resource: String,
    pub evidence_requirement: EvidenceRequirement,
}

/// In-process registry. Accessed only through governance methods; never
/// exposed as a SQL collection or wire surface.
#[derive(Default)]
pub struct ConfigRegistry {
    active: RwLock<HashMap<String, ConfigRegistryEntry>>,
    history: RwLock<HashMap<String, Vec<ConfigRegistryHistoryRecord>>>,
}

/// Policy action for creating a new registry entry.
pub const ACTION_REGISTER: &str = "red.registry:register";
/// Policy action for superseding (mutating) an existing registry entry.
pub const ACTION_SUPERSEDE: &str = "red.registry:supersede";
/// Resource kind used when building the [`ResourceRef`] for the
/// authorization check.
pub const RESOURCE_KIND: &str = "registry";

impl ConfigRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new entry. Returns `AlreadyRegistered` if an active
    /// version already exists; use [`Self::supersede`] in that case.
    ///
    /// Authorization: `auth.check_policy_authz(actor, "red.registry:register",
    /// registry:<id>, ctx)` must return `true`.
    pub fn register(
        &self,
        auth: &AuthStore,
        actor: &UserId,
        ctx: &EvalContext,
        draft: ConfigRegistryDraft,
        now_ms: u128,
    ) -> Result<ConfigRegistryEntry, RegistryError> {
        let resource = ResourceRef::new(RESOURCE_KIND, draft.id.clone());
        if !auth.check_policy_authz(actor, ACTION_REGISTER, &resource, ctx) {
            return Err(RegistryError::Unauthorized {
                action: ACTION_REGISTER.to_string(),
                resource: format!("{}:{}", RESOURCE_KIND, draft.id),
            });
        }

        let mut active = self.active.write().unwrap_or_else(|e| e.into_inner());
        if active.contains_key(&draft.id) {
            return Err(RegistryError::AlreadyRegistered(draft.id));
        }
        let entry = ConfigRegistryEntry {
            id: draft.id.clone(),
            version: 1,
            resource_type: draft.resource_type,
            schema: draft.schema,
            mutability: draft.mutability,
            sensitivity: draft.sensitivity,
            managed: draft.managed,
            required_action: draft.required_action,
            required_resource: draft.required_resource,
            evidence_requirement: draft.evidence_requirement,
            updated_by: actor.to_string(),
            updated_at_ms: now_ms,
        };
        active.insert(draft.id, entry.clone());
        Ok(entry)
    }

    /// Supersede the active entry for `id`. The previous version is
    /// pushed into history with `superseded_at_ms == now_ms` and the
    /// caller-supplied `change_reason`. Rejected if the active entry is
    /// `Immutable`.
    ///
    /// Authorization: `auth.check_policy_authz(actor, "red.registry:supersede",
    /// registry:<id>, ctx)` must return `true`.
    pub fn supersede(
        &self,
        auth: &AuthStore,
        actor: &UserId,
        ctx: &EvalContext,
        draft: ConfigRegistryDraft,
        change_reason: impl Into<String>,
        now_ms: u128,
    ) -> Result<ConfigRegistryEntry, RegistryError> {
        let resource = ResourceRef::new(RESOURCE_KIND, draft.id.clone());
        if !auth.check_policy_authz(actor, ACTION_SUPERSEDE, &resource, ctx) {
            return Err(RegistryError::Unauthorized {
                action: ACTION_SUPERSEDE.to_string(),
                resource: format!("{}:{}", RESOURCE_KIND, draft.id),
            });
        }

        let mut active = self.active.write().unwrap_or_else(|e| e.into_inner());
        let prev = active
            .get(&draft.id)
            .cloned()
            .ok_or_else(|| RegistryError::NotFound(draft.id.clone()))?;
        if prev.mutability == Mutability::Immutable {
            return Err(RegistryError::Immutable(draft.id));
        }

        let next = ConfigRegistryEntry {
            id: draft.id.clone(),
            version: prev.version + 1,
            resource_type: draft.resource_type,
            schema: draft.schema,
            mutability: draft.mutability,
            sensitivity: draft.sensitivity,
            managed: draft.managed,
            required_action: draft.required_action,
            required_resource: draft.required_resource,
            evidence_requirement: draft.evidence_requirement,
            updated_by: actor.to_string(),
            updated_at_ms: now_ms,
        };
        active.insert(draft.id.clone(), next.clone());

        let record = ConfigRegistryHistoryRecord {
            entry: prev,
            superseded_at_ms: now_ms,
            superseded_by: actor.to_string(),
            change_reason: change_reason.into(),
        };
        self.history
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(draft.id)
            .or_default()
            .push(record);
        Ok(next)
    }

    /// Active surface — current version for `id`, or `None`.
    pub fn get_active(&self, id: &str) -> Option<ConfigRegistryEntry> {
        self.active.read().ok().and_then(|m| m.get(id).cloned())
    }

    /// All currently-active entries (id-sorted for deterministic output).
    pub fn list_active(&self) -> Vec<ConfigRegistryEntry> {
        let map = match self.active.read() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        let mut out: Vec<ConfigRegistryEntry> = map.values().cloned().collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// History for `id`, oldest first. Empty when the id never had a
    /// supersede (or never existed).
    pub fn history(&self, id: &str) -> Vec<ConfigRegistryHistoryRecord> {
        self.history
            .read()
            .ok()
            .and_then(|m| m.get(id).cloned())
            .unwrap_or_default()
    }

    /// Restore an active entry that was already accepted through a trusted
    /// bootstrap path and persisted in internal config state. This is not a
    /// governance mutation; it only rehydrates the process-local registry
    /// after `system.bootstrap.completed` makes the manifest file optional.
    pub(crate) fn restore_bootstrap_entry(
        &self,
        entry: ConfigRegistryEntry,
    ) -> Result<(), RegistryError> {
        let mut active = self.active.write().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = active.get(&entry.id) {
            if existing == &entry {
                return Ok(());
            }
            return Err(RegistryError::AlreadyRegistered(entry.id));
        }
        active.insert(entry.id.clone(), entry);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::policies::Policy;
    use crate::auth::{AuthConfig, Role};

    fn store_with_admin() -> (std::sync::Arc<AuthStore>, UserId) {
        let store = std::sync::Arc::new(AuthStore::new(AuthConfig::default()));
        store.create_user("ops", "p", Role::Admin).unwrap();
        let uid = UserId::platform("ops");
        (store, uid)
    }

    fn ctx() -> EvalContext {
        EvalContext {
            principal_tenant: None,
            current_tenant: None,
            peer_ip: None,
            mfa_present: false,
            now_ms: 1_700_000_000_000,
            principal_is_admin_role: true,
            principal_is_system_owned: false,
            principal_is_platform_scoped: true,
        }
    }

    fn allow_all_registry(id: &str) -> Policy {
        Policy::from_json_str(&format!(
            r#"{{
                "id": "{id}",
                "version": 1,
                "statements": [{{
                    "effect": "allow",
                    "actions": ["red.registry:*"],
                    "resources": ["registry:*"]
                }}]
            }}"#
        ))
        .unwrap()
    }

    fn deny_all_registry(id: &str) -> Policy {
        Policy::from_json_str(&format!(
            r#"{{
                "id": "{id}",
                "version": 1,
                "statements": [{{
                    "effect": "deny",
                    "actions": ["red.registry:*"],
                    "resources": ["registry:*"]
                }}]
            }}"#
        ))
        .unwrap()
    }

    fn sample_draft(id: &str) -> ConfigRegistryDraft {
        ConfigRegistryDraft {
            id: id.to_string(),
            resource_type: "config_key".into(),
            schema: "string".into(),
            mutability: Mutability::MutableViaGovernance,
            sensitivity: Sensitivity::Internal,
            managed: true,
            required_action: "config:write".into(),
            required_resource: format!("config:{id}"),
            evidence_requirement: EvidenceRequirement::Metadata,
        }
    }

    #[test]
    fn register_then_get_active_returns_v1() {
        let (store, uid) = store_with_admin();
        store.put_policy(allow_all_registry("p-allow")).unwrap();
        store
            .attach_policy(
                super::super::store::PrincipalRef::User(uid.clone()),
                "p-allow",
            )
            .unwrap();
        let reg = ConfigRegistry::new();

        let entry = reg
            .register(
                &store,
                &uid,
                &ctx(),
                sample_draft("red.config.audit.enabled"),
                1_000,
            )
            .expect("register");
        assert_eq!(entry.version, 1);

        let got = reg.get_active("red.config.audit.enabled").unwrap();
        assert_eq!(got, entry);
        assert!(reg.history("red.config.audit.enabled").is_empty());
    }

    #[test]
    fn supersede_promotes_v2_and_records_history() {
        let (store, uid) = store_with_admin();
        store.put_policy(allow_all_registry("p-allow")).unwrap();
        store
            .attach_policy(
                super::super::store::PrincipalRef::User(uid.clone()),
                "p-allow",
            )
            .unwrap();
        let reg = ConfigRegistry::new();

        let v1 = reg
            .register(&store, &uid, &ctx(), sample_draft("k"), 1_000)
            .unwrap();
        let mut next = sample_draft("k");
        next.schema = "string-v2".into();
        let v2 = reg
            .supersede(&store, &uid, &ctx(), next, "tightened schema", 2_000)
            .unwrap();
        assert_eq!(v2.version, 2);
        assert_eq!(reg.get_active("k").unwrap(), v2);

        let hist = reg.history("k");
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].entry, v1);
        assert_eq!(hist[0].superseded_at_ms, 2_000);
        assert_eq!(hist[0].superseded_by, uid.to_string());
        assert_eq!(hist[0].change_reason, "tightened schema");
    }

    #[test]
    fn explicit_deny_blocks_mutation_even_for_admin() {
        let (store, uid) = store_with_admin();
        store.put_policy(allow_all_registry("p-allow")).unwrap();
        store.put_policy(deny_all_registry("p-deny")).unwrap();
        store
            .attach_policy(
                super::super::store::PrincipalRef::User(uid.clone()),
                "p-allow",
            )
            .unwrap();
        store
            .attach_policy(
                super::super::store::PrincipalRef::User(uid.clone()),
                "p-deny",
            )
            .unwrap();
        let reg = ConfigRegistry::new();

        let err = reg
            .register(&store, &uid, &ctx(), sample_draft("k"), 1_000)
            .unwrap_err();
        assert!(
            matches!(err, RegistryError::Unauthorized { .. }),
            "got {err:?}"
        );
        assert!(reg.get_active("k").is_none());
    }

    #[test]
    fn ordinary_user_without_registry_policy_is_denied() {
        // Non-admin principal, no policy granting `red.registry:*` →
        // policy-first DefaultDeny rejects the mutation.
        let store = std::sync::Arc::new(AuthStore::new(AuthConfig::default()));
        store.create_user("alice", "p", Role::Write).unwrap();
        let uid = UserId::platform("alice");
        // Insert any policy so IAM is the authoritative path.
        store
            .put_policy(
                Policy::from_json_str(
                    r#"{"id":"p-unrelated","version":1,"statements":[{"effect":"allow","actions":["select"],"resources":["table:public.x"]}]}"#,
                )
                .unwrap(),
            )
            .unwrap();
        let mut c = ctx();
        c.principal_is_admin_role = false;
        let reg = ConfigRegistry::new();
        let err = reg
            .register(&store, &uid, &c, sample_draft("k"), 1_000)
            .unwrap_err();
        assert!(
            matches!(err, RegistryError::Unauthorized { .. }),
            "got {err:?}"
        );
    }

    #[test]
    fn immutable_entries_reject_supersede() {
        let (store, uid) = store_with_admin();
        store.put_policy(allow_all_registry("p-allow")).unwrap();
        store
            .attach_policy(
                super::super::store::PrincipalRef::User(uid.clone()),
                "p-allow",
            )
            .unwrap();
        let reg = ConfigRegistry::new();

        let mut draft = sample_draft("k");
        draft.mutability = Mutability::Immutable;
        reg.register(&store, &uid, &ctx(), draft, 1_000).unwrap();

        let err = reg
            .supersede(
                &store,
                &uid,
                &ctx(),
                sample_draft("k"),
                "should fail",
                2_000,
            )
            .unwrap_err();
        assert!(matches!(err, RegistryError::Immutable(_)), "got {err:?}");
        assert_eq!(reg.get_active("k").unwrap().version, 1);
        assert!(reg.history("k").is_empty());
    }

    #[test]
    fn register_twice_is_already_registered() {
        let (store, uid) = store_with_admin();
        store.put_policy(allow_all_registry("p-allow")).unwrap();
        store
            .attach_policy(
                super::super::store::PrincipalRef::User(uid.clone()),
                "p-allow",
            )
            .unwrap();
        let reg = ConfigRegistry::new();
        reg.register(&store, &uid, &ctx(), sample_draft("k"), 1_000)
            .unwrap();
        let err = reg
            .register(&store, &uid, &ctx(), sample_draft("k"), 1_500)
            .unwrap_err();
        assert!(
            matches!(err, RegistryError::AlreadyRegistered(_)),
            "got {err:?}"
        );
    }

    #[test]
    fn registry_is_not_exposed_as_sql_collection() {
        // The registry lives outside the SQL surface — ordinary DML
        // cannot reach these entries because there is no collection /
        // table / virtual surface that mirrors them. This test pins
        // that contract: a ConfigRegistry stands alone; nothing on
        // AuthStore or the storage path exposes it as a row source.
        let reg = ConfigRegistry::new();
        // The public API surface is the governance methods only:
        let _ = reg.list_active();
        let _ = reg.history("k");
        // (No `as_collection()` / `as_table()` / SQL accessor exists by
        // construction — if a future change adds one, this test should
        // be reviewed alongside the new wire surface.)
    }
}
