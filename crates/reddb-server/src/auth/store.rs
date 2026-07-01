//! AuthStore -- manages users, sessions, and API keys in memory.
//!
//! Password hashing delegates to the existing Argon2id implementation in
//! `crate::storage::encryption::argon2id`.  Token generation uses the
//! OS CSPRNG (`crate::crypto::os_random`) plus SHA-256.
#![deny(clippy::disallowed_methods)]

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use crate::crypto::os_random;
use crate::crypto::sha256::sha256;
use crate::storage::encryption::argon2id::{derive_key, Argon2Params};
use crate::storage::engine::pager::Pager;

use super::column_policy_gate::{ColumnAccessRequest, ColumnPolicyGate, ColumnPolicyOutcome};
use super::enforcement_mode::{legacy_rbac_decision, PolicyEnforcementMode};
use super::managed_policy::{ManagedPolicyDecision, ManagedPolicyGate, PolicyOp};
use super::policies::{self as iam_policies, EvalContext, Policy, ResourceRef, SimulationOutcome};
use super::privileges::{
    check_grant, Action, AuthzContext, AuthzError, Grant, GrantPrincipal, GrantsView,
    PermissionCache, Resource, UserAttributes,
};
use super::registry::ConfigRegistry;
use super::vault::{KeyPair, Vault, VaultState};
use super::{now_ms, ApiKey, AuthConfig, AuthError, Role, Session, User, UserId};
use crate::runtime::control_events::{
    ControlEvent, ControlEventConfig, ControlEventCtx, ControlEventLedger, EventKind, Outcome,
    Sensitivity,
};

// ---------------------------------------------------------------------------
// PrincipalRef + SimCtx — IAM policy attachments
// ---------------------------------------------------------------------------

/// Principal targeted by `attach_policy` / `detach_policy`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PrincipalRef {
    User(UserId),
    Group(String),
}

/// Reserved IAM group that every principal belongs to. Used by the
/// GRANT-to-PUBLIC compatibility layer.
pub const PUBLIC_IAM_GROUP: &str = "__public__";

/// Optional context overrides for `simulate` — anything not set falls back
/// to a default value when the kernel evaluates the request.
#[derive(Debug, Clone, Default)]
pub struct SimCtx {
    pub current_tenant: Option<String>,
    pub peer_ip: Option<std::net::IpAddr>,
    pub mfa_present: bool,
    pub now_ms: Option<u128>,
}

/// Control-event dependencies for audited IAM policy mutations.
///
/// The existing `put_policy` / `delete_policy` / attach / detach
/// methods remain unaudited for bootstrap and synthetic GRANT internals.
/// Public mutation surfaces that need evidence pass this context to the
/// `*_with_control_events` siblings.
pub struct PolicyMutationControl<'a> {
    pub ctx: &'a ControlEventCtx<'a>,
    pub ledger: &'a dyn ControlEventLedger,
    pub config: ControlEventConfig,
    pub registry: Option<&'a ConfigRegistry>,
    pub actor: &'a UserId,
    pub eval_ctx: &'a EvalContext,
}

/// Control-event dependencies for audited user lifecycle mutations.
pub struct UserLifecycleControl<'a> {
    pub ctx: &'a ControlEventCtx<'a>,
    pub ledger: &'a dyn ControlEventLedger,
    pub config: ControlEventConfig,
}

#[derive(Clone)]
struct AuthStoreControlEvents {
    ledger: Arc<dyn ControlEventLedger>,
    config: ControlEventConfig,
}

// ---------------------------------------------------------------------------
// BootstrapResult
// ---------------------------------------------------------------------------

/// Result of a successful bootstrap operation.
///
/// The `certificate` is the hex-encoded string the admin must save --
/// it is the ONLY way to unseal the vault after a restart.
#[derive(Debug)]
pub struct BootstrapResult {
    pub user: User,
    pub api_key: ApiKey,
    /// Certificate hex string.  `None` when vault is not configured.
    pub certificate: Option<String>,
}

// ---------------------------------------------------------------------------
// AuthStore
// ---------------------------------------------------------------------------

/// Central in-process authority for auth state.
///
/// All mutations are guarded by `RwLock`s so the store is `Send + Sync`.
pub struct AuthStore {
    /// `(tenant_id, username) -> User`. Tenant scoping is built into the
    /// key so `alice@acme` and `alice@globex` are distinct identities.
    users: RwLock<HashMap<UserId, User>>,
    sessions: RwLock<HashMap<String, Session>>,
    /// key-string -> (owner UserId, role)
    api_key_index: RwLock<HashMap<String, (UserId, Role)>>,
    /// Once true, bootstrap() is permanently sealed.
    bootstrapped: AtomicBool,
    config: AuthConfig,
    /// Optional encrypted vault for persisting auth state to pager pages.
    vault: RwLock<Option<Vault>>,
    /// Reference to the pager for vault page I/O.
    pager: Option<Arc<Pager>>,
    /// Certificate-based keypair for token signing and vault seal.
    /// Populated after bootstrap or after restoring from a sealed vault.
    keypair: RwLock<Option<KeyPair>>,
    /// Encrypted key-value store for arbitrary secrets.
    /// Persisted to vault alongside users/api_keys.
    vault_kv: RwLock<HashMap<String, String>>,
    /// Plain (non-encrypted) user key-value store for `$kv.*` / `SET KV`
    /// (#1602). Deliberately isolated from `vault_kv` (encrypted secrets)
    /// and from the runtime config store — three independent flat-maps,
    /// no mixing. Backs the `kv:read` / `kv:write` IAM-gated resolver.
    plain_kv: RwLock<HashMap<String, String>>,
    /// Per-user GRANT rows. Persisted via `vault_kv` under the
    /// `red.acl.grants.<tenant>/<user>` key prefix so existing snapshot
    /// logic keeps working without modification. See `privileges` module
    /// for the resolution algorithm.
    grants: RwLock<HashMap<UserId, Vec<Grant>>>,
    /// PUBLIC grants — apply to every authenticated principal.
    public_grants: RwLock<Vec<Grant>>,
    /// PG-style account attributes (`VALID UNTIL`, `CONNECTION LIMIT`,
    /// `search_path`). Keyed by `UserId`. Persisted under
    /// `red.acl.attrs.<tenant>/<user>`.
    user_attributes: RwLock<HashMap<UserId, UserAttributes>>,
    /// Live session count per user — used by `CONNECTION LIMIT`
    /// enforcement on login. Bumped at authenticate, decremented at
    /// session revoke / expiry.
    session_count_by_user: RwLock<HashMap<UserId, u32>>,
    /// Pre-resolved (resource, action) cache built per-user so the
    /// hot path skips a linear scan of the user's grants on every
    /// statement. Invalidated on GRANT / REVOKE / ALTER USER.
    permission_cache: RwLock<HashMap<UserId, PermissionCache>>,
    /// IAM-style policies, keyed by id. Persisted under
    /// `red.iam.policies`. The kernel in `super::policies` owns the
    /// Policy type — this map just deduplicates and shares.
    policies: RwLock<HashMap<String, Arc<Policy>>>,
    /// Per-user policy attachments — ordered list of policy ids.
    /// Persisted under `red.iam.attachments.users`.
    user_attachments: RwLock<HashMap<UserId, Vec<String>>>,
    /// Per-group policy attachments. Users join groups through
    /// `UserAttributes::groups`; effective policies resolve group
    /// attachments before user-direct attachments.
    group_attachments: RwLock<HashMap<String, Vec<String>>>,
    /// Cached effective `Vec<Arc<Policy>>` per user. Invalidated on
    /// any policy mutation that affects the user's attachments.
    iam_effective_cache: RwLock<HashMap<UserId, Vec<Arc<Policy>>>>,
    /// Once any IAM policy is installed, authorization switches to the
    /// IAM evaluator and stays deny-by-default even if policies are
    /// later deleted. Persisted under `red.iam.enabled`.
    iam_authorization_enabled: AtomicBool,
    /// Selects the fallback behaviour when the IAM evaluator returns
    /// `DefaultDeny`. See [`PolicyEnforcementMode`] and #712 / S5A.
    /// Default is [`PolicyEnforcementMode::default_existing_install`]
    /// (`LegacyRbac`) so an existing install upgrading past #712
    /// keeps its current authorization posture until the operator
    /// flips it explicitly. The bootstrap path sets this to
    /// `PolicyOnly` for fresh installs; the boot-time config loader
    /// supersedes both when a `red.config.policy.enforcement_mode`
    /// value is present in the configured value store.
    enforcement_mode: RwLock<PolicyEnforcementMode>,
    /// Once-per-boot flag used by [`AuthStore::take_legacy_rbac_warn_once`]
    /// to deliver exactly one `warn`-level log line when the store is
    /// running under [`PolicyEnforcementMode::LegacyRbac`]. The boot
    /// path checks the flag and, if it can claim it, emits the line.
    legacy_rbac_boot_warn_emitted: AtomicBool,
    /// `(tenant, role) → HashSet<CollectionId>` cache used by the AI
    /// pipeline (issue #119). Distinct from `permission_cache`, which
    /// is keyed by `UserId` and answers `(resource, action)` lookups —
    /// this cache answers the inverse "what collections is this scope
    /// allowed to read?" query that `AuthorizedSearch` uses to
    /// pre-filter SEARCH SIMILAR / SEARCH CONTEXT candidates before any
    /// similarity score is computed. Entries TTL out at 60s and are
    /// invalidated explicitly on GRANT/REVOKE/CREATE POLICY/DROP
    /// POLICY/DROP COLLECTION.
    visible_collections_cache: super::scope_cache::AuthCache,
    control_events: RwLock<Option<AuthStoreControlEvents>>,
}

// Use fast-but-safe Argon2id params for auth hashing (smaller than the
// default 64 MB so that user-management RPCs respond quickly).
fn auth_argon2_params() -> Argon2Params {
    Argon2Params {
        m_cost: 4 * 1024, // 4 MB
        t_cost: 3,
        p: 1,
        tag_len: 32,
    }
}

fn policy_control_fields(
    policy_id: &str,
    policy: Option<&Policy>,
    principal: Option<&PrincipalRef>,
) -> HashMap<String, Sensitivity> {
    let mut fields = HashMap::new();
    fields.insert("policy_id".to_string(), Sensitivity::raw(policy_id));
    if let Some(policy) = policy {
        let effects = policy
            .statements
            .iter()
            .map(|s| match s.effect {
                iam_policies::Effect::Allow => "allow",
                iam_policies::Effect::Deny => "deny",
            })
            .collect::<Vec<_>>()
            .join(",");
        let actions = policy
            .statements
            .iter()
            .flat_map(|s| s.actions.iter())
            .map(policy_action_pattern)
            .collect::<Vec<_>>()
            .join(",");
        let resources = policy
            .statements
            .iter()
            .flat_map(|s| s.resources.iter())
            .map(policy_resource_pattern)
            .collect::<Vec<_>>()
            .join(",");
        fields.insert("effect".to_string(), Sensitivity::raw(effects));
        fields.insert("action".to_string(), Sensitivity::raw(actions));
        fields.insert("resource".to_string(), Sensitivity::raw(resources));
        if let Some(attrs) = policy_principal_attrs(policy) {
            fields.insert("principal_attrs".to_string(), Sensitivity::raw(attrs));
        }
    }
    if let Some(principal) = principal {
        fields.insert(
            "principal".to_string(),
            Sensitivity::raw(policy_principal_label(principal)),
        );
    }
    fields
}

fn policy_action_pattern(pattern: &iam_policies::ActionPattern) -> String {
    match pattern {
        iam_policies::ActionPattern::Exact(s) => s.clone(),
        iam_policies::ActionPattern::Wildcard => "*".to_string(),
        iam_policies::ActionPattern::Prefix(s) => format!("{s}:*"),
    }
}

fn policy_resource_pattern(pattern: &iam_policies::ResourcePattern) -> String {
    match pattern {
        iam_policies::ResourcePattern::Exact { kind, name } => format!("{kind}:{name}"),
        iam_policies::ResourcePattern::Glob(s) => s.clone(),
        iam_policies::ResourcePattern::Wildcard => "*".to_string(),
    }
}

fn policy_principal_attrs(policy: &Policy) -> Option<String> {
    let mut attrs = Vec::new();
    for statement in &policy.statements {
        let Some(condition) = &statement.condition else {
            continue;
        };
        if let Some(value) = condition.platform_scoped {
            attrs.push(format!("platform_scoped={value}"));
        }
        if let Some(value) = condition.mfa {
            attrs.push(format!("mfa={value}"));
        }
        if let Some(value) = condition.tenant_match {
            attrs.push(format!("tenant_match={value}"));
        }
    }
    if attrs.is_empty() {
        None
    } else {
        Some(attrs.join(","))
    }
}

fn policy_principal_label(principal: &PrincipalRef) -> String {
    match principal {
        PrincipalRef::User(uid) => format!("user:{uid}"),
        PrincipalRef::Group(group) => format!("group:{group}"),
    }
}

fn default_user_lifecycle_ctx<'a>() -> ControlEventCtx<'a> {
    ControlEventCtx {
        actor: crate::runtime::control_events::ActorRef::Anonymous,
        scope: None,
        request_id: None,
        trace_id: None,
    }
}

fn bootstrap_user_lifecycle_ctx<'a>() -> ControlEventCtx<'a> {
    ControlEventCtx {
        actor: crate::runtime::control_events::ActorRef::System("bootstrap"),
        scope: None,
        request_id: None,
        trace_id: None,
    }
}

fn user_resource(id: &UserId) -> String {
    format!("user:{id}")
}

fn api_key_resource(api_key_id: &str) -> String {
    format!("apikey:{api_key_id}")
}

fn api_key_id(key: &str) -> String {
    hex::encode(sha256(key.as_bytes()))
}

fn password_evidence() -> Sensitivity {
    Sensitivity::redacted()
}

fn user_control_fields(
    id: &UserId,
    role: Option<Role>,
    enabled: Option<bool>,
    include_password: bool,
) -> HashMap<String, Sensitivity> {
    let mut fields = HashMap::new();
    fields.insert(
        "username".to_string(),
        Sensitivity::raw(id.username.clone()),
    );
    fields.insert(
        "tenant_id".to_string(),
        Sensitivity::raw(id.tenant.clone().unwrap_or_default()),
    );
    if let Some(role) = role {
        fields.insert("role".to_string(), Sensitivity::raw(role.as_str()));
    }
    if let Some(enabled) = enabled {
        fields.insert("enabled".to_string(), Sensitivity::raw(enabled.to_string()));
    }
    if include_password {
        fields.insert("password".to_string(), password_evidence());
    }
    fields
}

fn api_key_control_fields(
    id: &UserId,
    role: Role,
    api_key_id: &str,
) -> HashMap<String, Sensitivity> {
    let mut fields = user_control_fields(id, Some(role), None, false);
    fields.insert("api_key_id".to_string(), Sensitivity::raw(api_key_id));
    fields.insert("api_key".to_string(), Sensitivity::redacted());
    fields
}

fn user_error_is_denied(err: &AuthError) -> bool {
    !matches!(err, AuthError::Internal(_))
}

impl AuthStore {
    // -----------------------------------------------------------------
    // Construction
    // -----------------------------------------------------------------

    pub fn new(config: AuthConfig) -> Self {
        Self {
            users: RwLock::new(HashMap::new()),
            sessions: RwLock::new(HashMap::new()),
            api_key_index: RwLock::new(HashMap::new()),
            bootstrapped: AtomicBool::new(false),
            config,
            vault: RwLock::new(None),
            pager: None,
            keypair: RwLock::new(None),
            vault_kv: RwLock::new(HashMap::new()),
            plain_kv: RwLock::new(HashMap::new()),
            grants: RwLock::new(HashMap::new()),
            public_grants: RwLock::new(Vec::new()),
            user_attributes: RwLock::new(HashMap::new()),
            session_count_by_user: RwLock::new(HashMap::new()),
            permission_cache: RwLock::new(HashMap::new()),
            policies: RwLock::new(HashMap::new()),
            user_attachments: RwLock::new(HashMap::new()),
            group_attachments: RwLock::new(HashMap::new()),
            iam_effective_cache: RwLock::new(HashMap::new()),
            iam_authorization_enabled: AtomicBool::new(false),
            enforcement_mode: RwLock::new(PolicyEnforcementMode::default_existing_install()),
            legacy_rbac_boot_warn_emitted: AtomicBool::new(false),
            visible_collections_cache: super::scope_cache::AuthCache::new(
                super::scope_cache::DEFAULT_TTL,
            ),
            control_events: RwLock::new(None),
        }
    }

    pub fn configure_control_events(
        &self,
        ledger: Arc<dyn ControlEventLedger>,
        config: ControlEventConfig,
    ) {
        *self
            .control_events
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(AuthStoreControlEvents { ledger, config });
    }

    fn configured_control_events(&self) -> Option<AuthStoreControlEvents> {
        self.control_events
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Create an `AuthStore` backed by encrypted vault pages inside the
    /// main `.rdb` database file.
    ///
    /// If vault pages already exist, their contents are loaded and
    /// restored into the in-memory store.  All subsequent mutations are
    /// automatically persisted to the vault pages via the pager.
    pub fn with_vault(config: AuthConfig, pager: Arc<Pager>) -> Result<Self, AuthError> {
        let certificate = crate::utils::env_with_file_fallback("REDDB_CERTIFICATE");
        Self::with_vault_optional_certificate(config, pager, certificate.as_deref())
    }

    pub fn with_vault_certificate(
        config: AuthConfig,
        pager: Arc<Pager>,
        certificate_hex: &str,
    ) -> Result<Self, AuthError> {
        Self::with_vault_optional_certificate(config, pager, Some(certificate_hex))
    }

    fn with_vault_optional_certificate(
        config: AuthConfig,
        pager: Arc<Pager>,
        certificate_hex: Option<&str>,
    ) -> Result<Self, AuthError> {
        let mut store = Self::new(config);

        // Restore persisted state if vault pages exist. A fresh database does
        // not need a temporary seal: bootstrap generates the certificate and
        // installs the first real vault atomically.
        if Vault::has_saved_state(&pager) {
            let vault = match certificate_hex {
                Some(certificate) => Vault::with_certificate(&pager, certificate)?,
                None => Vault::open(&pager)?,
            };
            if let Some(state) = vault.load(&pager)? {
                store.restore_from_vault(state);
            }
            *store.vault.write().unwrap_or_else(|e| e.into_inner()) = Some(vault);
        } else if let Some(certificate) = certificate_hex {
            let vault = Vault::with_certificate(&pager, certificate)?;
            *store.vault.write().unwrap_or_else(|e| e.into_inner()) = Some(vault);
        }

        store.pager = Some(pager);
        Ok(store)
    }

    pub fn config(&self) -> &AuthConfig {
        &self.config
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    /// Returns true when no users exist yet and bootstrap hasn't been sealed.
    pub fn needs_bootstrap(&self) -> bool {
        !self.bootstrapped.load(Ordering::Acquire)
            && self.users.read().map(|u| u.is_empty()).unwrap_or(true)
    }

    /// Internal: read-locked lookup by `UserId`.
    fn get_user_cloned(&self, id: &UserId) -> Option<User> {
        self.users.read().ok().and_then(|m| m.get(id).cloned())
    }

    /// Whether bootstrap has already been performed (sealed).
    pub fn is_bootstrapped(&self) -> bool {
        self.bootstrapped.load(Ordering::Acquire)
    }

    /// Bootstrap the first admin user. One-shot, irreversible.
    ///
    /// Uses an atomic compare-exchange to guarantee that even under
    /// concurrent calls, only the first one succeeds. Once sealed,
    /// all subsequent calls fail immediately -- there is no undo.
    ///
    /// When a vault/pager is configured, a certificate-based keypair is
    /// generated and the vault is re-encrypted with the certificate-derived
    /// key.  The certificate hex string is returned in `BootstrapResult`
    /// so the admin can save it.
    pub fn bootstrap(&self, username: &str, password: &str) -> Result<BootstrapResult, AuthError> {
        if let Some(configured) = self.configured_control_events() {
            let ctx = bootstrap_user_lifecycle_ctx();
            let control = UserLifecycleControl {
                ctx: &ctx,
                ledger: configured.ledger.as_ref(),
                config: configured.config,
            };
            self.bootstrap_with_control_events_inner(username, password, &control)
        } else {
            self.bootstrap_unaudited(username, password)
        }
    }

    pub fn bootstrap_with_control_events(
        &self,
        username: &str,
        password: &str,
        ctx: &ControlEventCtx<'_>,
        ledger: &dyn ControlEventLedger,
        config: ControlEventConfig,
    ) -> Result<BootstrapResult, AuthError> {
        let system_ctx = ControlEventCtx {
            actor: crate::runtime::control_events::ActorRef::System("bootstrap"),
            scope: ctx.scope.clone(),
            request_id: ctx.request_id.clone(),
            trace_id: ctx.trace_id.clone(),
        };
        let control = UserLifecycleControl {
            ctx: &system_ctx,
            ledger,
            config,
        };
        self.bootstrap_with_control_events_inner(username, password, &control)
    }

    /// Backwards-compatible bootstrap entry point for older callers
    /// that used to ask for an operator-owned first admin. Authorization
    /// is now policy-first, so this creates the same ordinary platform
    /// admin as [`Self::bootstrap`].
    pub fn bootstrap_system_admin(
        &self,
        username: &str,
        password: &str,
    ) -> Result<BootstrapResult, AuthError> {
        self.bootstrap(username, password)
    }

    fn bootstrap_unaudited(
        &self,
        username: &str,
        password: &str,
    ) -> Result<BootstrapResult, AuthError> {
        // Atomic seal: only the first caller wins.
        if self
            .bootstrapped
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(AuthError::Forbidden(
                "bootstrap already completed — sealed permanently".to_string(),
            ));
        }

        // Double-check users are actually empty (belt and suspenders).
        {
            let users = self.users.read().map_err(lock_err)?;
            if !users.is_empty() {
                return Err(AuthError::Forbidden(
                    "bootstrap already completed — users exist".to_string(),
                ));
            }
        }

        let user = self.create_user_in_tenant_unaudited(None, username, password, Role::Admin)?;
        let key =
            self.create_api_key_in_tenant_unaudited(None, username, "bootstrap", Role::Admin)?;

        // Generate a certificate-based keypair and re-seal the vault.
        let certificate = if let Some(ref pager) = self.pager {
            let kp = KeyPair::generate();
            let cert_hex = kp.certificate_hex();

            // Re-create the vault using the certificate-derived key.
            let new_vault = Vault::with_certificate_bytes(pager, &kp.certificate)
                .map_err(|e| AuthError::Internal(format!("vault re-seal failed: {e}")))?;

            // Store the keypair so token signing works immediately.
            if let Ok(mut kp_guard) = self.keypair.write() {
                *kp_guard = Some(kp);
            }

            // Replace the vault and persist with the master secret included.
            if let Ok(mut vault_guard) = self.vault.write() {
                *vault_guard = Some(new_vault);
            }
            // Generate the AES-256 secret key for Value::Secret encryption.
            self.ensure_vault_secret_key();
            self.persist_to_vault();

            Some(cert_hex)
        } else {
            None
        };

        // #712 / S5A: fresh bootstraps land in the strict posture.
        // Persist explicitly to vault_kv so subsequent boots
        // (rehydrate_iam) pick up `policy_only` instead of the
        // existing-install default. Existing installs upgrading past
        // this commit never traverse this branch — `bootstrap()` is
        // sealed once and never re-runs.
        self.set_enforcement_mode(PolicyEnforcementMode::default_fresh_bootstrap());

        Ok(BootstrapResult {
            user,
            api_key: key,
            certificate,
        })
    }

    fn bootstrap_with_control_events_inner(
        &self,
        username: &str,
        password: &str,
        control: &UserLifecycleControl<'_>,
    ) -> Result<BootstrapResult, AuthError> {
        let id = UserId::from_parts(None, username);
        match self.bootstrap_unaudited(username, password) {
            Ok(result) => {
                let event_result = self.emit_user_lifecycle_allowed(
                    control,
                    EventKind::UserCreate,
                    "user.create",
                    &id,
                    user_control_fields(&id, Some(Role::Admin), Some(true), true),
                );
                if let Err(err) = event_result {
                    self.rollback_bootstrap(&id);
                    return Err(err);
                }
                Ok(result)
            }
            Err(err) => {
                self.emit_user_lifecycle_outcome(
                    control,
                    if user_error_is_denied(&err) {
                        Outcome::Denied
                    } else {
                        Outcome::Error
                    },
                    EventKind::UserCreate,
                    "user.create",
                    &id,
                    Some(err.to_string()),
                    user_control_fields(&id, Some(Role::Admin), Some(true), true),
                );
                Err(err)
            }
        }
    }

    /// Auto-bootstrap from environment variables if no users exist.
    ///
    /// Checks `REDDB_USERNAME` and `REDDB_PASSWORD`. If both are set and
    /// the user store is empty, creates the first admin user automatically.
    /// This mirrors the Docker pattern (`MYSQL_ROOT_PASSWORD`, etc.).
    ///
    /// Returns `Some(BootstrapResult)` if bootstrapped, `None` if skipped.
    pub fn bootstrap_from_env(&self) -> Option<BootstrapResult> {
        if !self.needs_bootstrap() {
            return None;
        }

        let username = crate::utils::env_with_file_fallback("REDDB_USERNAME")?;
        let password = crate::utils::env_with_file_fallback("REDDB_PASSWORD")?;

        if username.is_empty() || password.is_empty() {
            return None;
        }

        match self.bootstrap(&username, &password) {
            Ok(result) => {
                // F-04: `username` is REDDB_USERNAME — operator-supplied
                // (env), but still routed through the LogField escaper
                // because env strings cross trust boundaries in some
                // deployment models (k8s downward API, Vault sidecar,
                // external secret operator). See ADR 0010.
                tracing::info!(
                    username = %reddb_wire::audit_safe_log_field(&username),
                    "bootstrapped admin user from REDDB_USERNAME/REDDB_PASSWORD"
                );
                if let Some(ref cert) = result.certificate {
                    // Certificate must be readable by the operator — keep it
                    // in the log stream but print raw to stderr too so it
                    // survives even if the log file gets rotated.
                    eprintln!("[reddb] CERTIFICATE: {}", cert);
                    tracing::warn!(
                        "vault certificate issued — save it: ONLY way to unseal after restart"
                    );
                }
                Some(result)
            }
            Err(e) => {
                tracing::error!(err = %e, "env bootstrap failed");
                None
            }
        }
    }

    // -----------------------------------------------------------------
    // Vault persistence
    // -----------------------------------------------------------------

    /// Persist the current auth state to the vault pages (if configured).
    fn persist_to_vault_result(&self) -> Result<(), AuthError> {
        let vault_guard = self.vault.read().unwrap_or_else(|e| e.into_inner());
        if let (Some(ref vault), Some(ref pager)) = (&*vault_guard, &self.pager) {
            let state = self.snapshot();
            vault.save(pager, &state)?;
        }
        Ok(())
    }

    /// Persist the current auth state to the vault pages (if configured).
    ///
    /// Legacy auth mutations still treat in-memory state as authoritative.
    /// New secret-management paths use the `try_` methods below so callers
    /// get a hard error if the vault write fails.
    fn persist_to_vault(&self) {
        if let Err(e) = self.persist_to_vault_result() {
            tracing::error!(err = %e, "vault persist failed");
            // Issue #205 — vault persist is the secret-rotation
            // serialization point: when this fails, freshly rotated
            // credentials live only in memory and a process restart
            // loses them. Operator-grade event so the operator can
            // intervene before the next restart.
            crate::telemetry::operator_event::OperatorEvent::SecretRotationFailed {
                secret_ref: "auth_vault".to_string(),
                error: e.to_string(),
            }
            .emit_global();
        }
    }

    /// True when this store has an encrypted vault and pager wired in.
    pub fn is_vault_backed(&self) -> bool {
        self.pager.is_some()
            && self
                .vault
                .read()
                .map(|guard| guard.is_some())
                .unwrap_or(false)
    }

    // -----------------------------------------------------------------
    // Vault KV — encrypted key-value store for arbitrary secrets
    // -----------------------------------------------------------------

    /// Read a value from the vault KV store. Returns `None` if not set.
    pub fn vault_kv_get(&self, key: &str) -> Option<String> {
        self.vault_kv
            .read()
            .ok()
            .and_then(|kv| kv.get(key).cloned())
    }

    /// Snapshot vault KV values for statement-local secret resolution.
    pub fn vault_kv_snapshot(&self) -> HashMap<String, String> {
        self.vault_kv
            .read()
            .map(|kv| kv.clone())
            .unwrap_or_default()
    }

    /// Export vault KV as an encrypted logical blob for JSONL dump/restore.
    /// Returns `None` when the vault has no KV entries.
    pub fn vault_kv_export_encrypted(&self) -> Result<Option<String>, AuthError> {
        if !self.is_vault_backed() {
            return Err(AuthError::Forbidden(
                "vault KV export requires an enabled, unsealed vault".to_string(),
            ));
        }
        let kv = self.vault_kv_snapshot();
        if kv.is_empty() {
            return Ok(None);
        }

        let vault_guard = self.vault.read().map_err(lock_err)?;
        let vault = vault_guard.as_ref().ok_or_else(|| {
            AuthError::Forbidden("vault KV export requires an enabled, unsealed vault".to_string())
        })?;
        let state = VaultState {
            users: Vec::new(),
            api_keys: Vec::new(),
            bootstrapped: false,
            master_secret: None,
            kv,
        };
        Ok(Some(vault.seal_logical_export(&state)?))
    }

    /// Merge imported vault KV entries and fail if the encrypted vault
    /// write cannot be made durable.
    pub fn vault_kv_try_import(
        &self,
        entries: HashMap<String, String>,
    ) -> Result<usize, AuthError> {
        if !self.is_vault_backed() {
            return Err(AuthError::Forbidden(
                "vault KV import requires an enabled, unsealed vault".to_string(),
            ));
        }
        if entries.is_empty() {
            return Ok(0);
        }

        let mut previous = HashMap::new();
        {
            let mut kv = self.vault_kv.write().map_err(lock_err)?;
            for (key, value) in &entries {
                previous.insert(key.clone(), kv.insert(key.clone(), value.clone()));
            }
        }

        if let Err(err) = self.persist_to_vault_result() {
            if let Ok(mut kv) = self.vault_kv.write() {
                for (key, old) in previous {
                    match old {
                        Some(value) => {
                            kv.insert(key, value);
                        }
                        None => {
                            kv.remove(&key);
                        }
                    }
                }
            }
            return Err(err);
        }

        Ok(entries.len())
    }

    /// Import false placeholders for secrets whose encrypted payload
    /// could not be decrypted during logical restore.
    pub fn vault_kv_try_import_placeholders(&self, keys: &[String]) -> Result<usize, AuthError> {
        let entries = keys
            .iter()
            .map(|key| (key.clone(), "false".to_string()))
            .collect();
        self.vault_kv_try_import(entries)
    }

    /// Write a value to the vault KV store, persisting to disk.
    pub fn vault_kv_set(&self, key: String, value: String) {
        if let Ok(mut kv) = self.vault_kv.write() {
            kv.insert(key, value);
        }
        self.persist_to_vault();
    }

    /// Write a value to the vault KV store and fail if the vault write
    /// cannot be made durable.
    pub fn vault_kv_try_set(&self, key: String, value: String) -> Result<(), AuthError> {
        if !self.is_vault_backed() {
            return Err(AuthError::Forbidden(
                "SET SECRET requires an enabled, unsealed vault".to_string(),
            ));
        }

        let previous = {
            let mut kv = self.vault_kv.write().map_err(lock_err)?;
            kv.insert(key.clone(), value)
        };

        if let Err(err) = self.persist_to_vault_result() {
            if let Ok(mut kv) = self.vault_kv.write() {
                match previous {
                    Some(value) => {
                        kv.insert(key, value);
                    }
                    None => {
                        kv.remove(&key);
                    }
                }
            }
            return Err(err);
        }

        Ok(())
    }

    /// Delete a value from the vault KV store. Returns true if it existed.
    pub fn vault_kv_delete(&self, key: &str) -> bool {
        let existed = self
            .vault_kv
            .write()
            .map(|mut kv| kv.remove(key).is_some())
            .unwrap_or(false);
        if existed {
            self.persist_to_vault();
        }
        existed
    }

    /// Delete a value from the vault KV store and fail if the vault write
    /// cannot be made durable.
    pub fn vault_kv_try_delete(&self, key: &str) -> Result<bool, AuthError> {
        if !self.is_vault_backed() {
            return Err(AuthError::Forbidden(
                "DELETE SECRET requires an enabled, unsealed vault".to_string(),
            ));
        }

        let removed = {
            let mut kv = self.vault_kv.write().map_err(lock_err)?;
            kv.remove(key)
        };

        if removed.is_none() {
            return Ok(false);
        }

        if let Err(err) = self.persist_to_vault_result() {
            if let Ok(mut kv) = self.vault_kv.write() {
                if let Some(value) = removed {
                    kv.insert(key.to_string(), value);
                }
            }
            return Err(err);
        }

        Ok(true)
    }

    /// List all keys in the vault KV store.
    pub fn vault_kv_keys(&self) -> Vec<String> {
        self.vault_kv
            .read()
            .map(|kv| kv.keys().cloned().collect())
            .unwrap_or_default()
    }

    // -----------------------------------------------------------------
    // Plain KV — non-encrypted user key-value store (#1602)
    //
    // A sibling to the vault KV above, but stored in its own flat-map
    // with no encryption. Backs `$kv.*` / `SET KV` / `DELETE KV` and is
    // gated by `kv:read` / `kv:write` at the runtime layer. Kept fully
    // separate from `vault_kv` (secrets) and the config store.
    // -----------------------------------------------------------------

    /// Read a value from the plain KV store. Returns `None` if not set.
    pub fn plain_kv_get(&self, key: &str) -> Option<String> {
        self.plain_kv
            .read()
            .ok()
            .and_then(|kv| kv.get(key).cloned())
    }

    /// Snapshot plain KV values for statement-local resolution.
    pub fn plain_kv_snapshot(&self) -> HashMap<String, String> {
        self.plain_kv
            .read()
            .map(|kv| kv.clone())
            .unwrap_or_default()
    }

    /// Write a value to the plain KV store.
    pub fn plain_kv_set(&self, key: String, value: String) {
        if let Ok(mut kv) = self.plain_kv.write() {
            kv.insert(key, value);
        }
    }

    /// Delete a value from the plain KV store. Returns true if it existed.
    pub fn plain_kv_delete(&self, key: &str) -> bool {
        self.plain_kv
            .write()
            .map(|mut kv| kv.remove(key).is_some())
            .unwrap_or(false)
    }

    /// List all keys in the plain KV store.
    pub fn plain_kv_keys(&self) -> Vec<String> {
        self.plain_kv
            .read()
            .map(|kv| kv.keys().cloned().collect())
            .unwrap_or_default()
    }

    /// Convenience: get the 32-byte secret key for Value::Secret encryption.
    /// Generated on first boot and stored at `red.secret.aes_key`.
    pub fn vault_secret_key(&self) -> Option<[u8; 32]> {
        let hex_str = self.vault_kv_get("red.secret.aes_key")?;
        let bytes = hex::decode(hex_str).ok()?;
        if bytes.len() == 32 {
            let mut key = [0u8; 32];
            key.copy_from_slice(&bytes);
            Some(key)
        } else {
            None
        }
    }

    /// Generate and store the AES-256 secret key on first boot if not present.
    pub fn ensure_vault_secret_key(&self) {
        if self.vault_kv_get("red.secret.aes_key").is_none() {
            let key = random_bytes(32);
            self.vault_kv_set("red.secret.aes_key".to_string(), hex::encode(key));
        }
    }

    /// Take a snapshot of the current auth state for vault serialization.
    fn snapshot(&self) -> VaultState {
        let users_guard = self.users.read().unwrap_or_else(|e| e.into_inner());
        let users: Vec<User> = users_guard.values().cloned().collect();

        // Collect (owner UserId, api_key) pairs from all users so a
        // tenant-scoped owner can be reattached on restore.
        let mut api_keys = Vec::new();
        for user in &users {
            let owner = UserId::from_parts(user.tenant_id.as_deref(), &user.username);
            for key in &user.api_keys {
                api_keys.push((owner.clone(), key.clone()));
            }
        }

        // Include the master secret if a keypair is loaded.
        let master_secret = self
            .keypair
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().map(|kp| kp.master_secret.clone()));

        let kv = self.vault_kv.read().map(|m| m.clone()).unwrap_or_default();

        VaultState {
            users,
            api_keys,
            bootstrapped: self.bootstrapped.load(Ordering::Acquire),
            master_secret,
            kv,
        }
    }

    /// Restore the in-memory auth state from a vault snapshot.
    fn restore_from_vault(&mut self, state: VaultState) {
        // Restore bootstrap seal.
        if state.bootstrapped {
            self.bootstrapped.store(true, Ordering::Release);
        }

        // Restore keypair from master secret (if present).
        if let Some(secret) = state.master_secret {
            let kp = KeyPair::from_master_secret(secret);
            if let Ok(mut guard) = self.keypair.write() {
                *guard = Some(kp);
            }
        }

        // Restore KV store.
        if let Ok(mut kv) = self.vault_kv.write() {
            *kv = state.kv;
        }

        // Restore users.
        let mut users = self.users.write().unwrap_or_else(|e| e.into_inner());
        let mut idx = self
            .api_key_index
            .write()
            .unwrap_or_else(|e| e.into_inner());

        for user in state.users {
            let id = UserId::from_parts(user.tenant_id.as_deref(), &user.username);
            // Register API keys in the index.
            for key in &user.api_keys {
                idx.insert(key.key.clone(), (id.clone(), key.role));
            }
            users.insert(id, user);
        }
        drop(idx);
        drop(users);

        self.rehydrate_acl();
        self.rehydrate_iam();
    }

    // -----------------------------------------------------------------
    // User management
    // -----------------------------------------------------------------

    /// Create a new platform-scoped user (`tenant_id = None`).
    ///
    /// For tenant-scoped creation, use [`Self::create_user_in_tenant`].
    pub fn create_user(
        &self,
        username: &str,
        password: &str,
        role: Role,
    ) -> Result<User, AuthError> {
        self.create_user_in_tenant(None, username, password, role)
    }

    pub fn create_user_with_control_events(
        &self,
        username: &str,
        password: &str,
        role: Role,
        ctx: &ControlEventCtx<'_>,
        ledger: &dyn ControlEventLedger,
        config: ControlEventConfig,
    ) -> Result<User, AuthError> {
        self.create_user_in_tenant_with_control_events(
            None, username, password, role, ctx, ledger, config,
        )
    }

    /// Create a user under the given tenant scope. `tenant_id == None`
    /// produces a platform-wide user. `(tenant, username)` is the
    /// uniqueness key — the same `username` may exist independently
    /// under multiple tenants.
    pub fn create_user_in_tenant(
        &self,
        tenant_id: Option<&str>,
        username: &str,
        password: &str,
        role: Role,
    ) -> Result<User, AuthError> {
        if let Some(configured) = self.configured_control_events() {
            let ctx = default_user_lifecycle_ctx();
            let control = UserLifecycleControl {
                ctx: &ctx,
                ledger: configured.ledger.as_ref(),
                config: configured.config,
            };
            self.create_user_in_tenant_controlled(tenant_id, username, password, role, &control)
        } else {
            self.create_user_in_tenant_unaudited(tenant_id, username, password, role)
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_user_in_tenant_with_control_events(
        &self,
        tenant_id: Option<&str>,
        username: &str,
        password: &str,
        role: Role,
        ctx: &ControlEventCtx<'_>,
        ledger: &dyn ControlEventLedger,
        config: ControlEventConfig,
    ) -> Result<User, AuthError> {
        let control = UserLifecycleControl {
            ctx,
            ledger,
            config,
        };
        self.create_user_in_tenant_controlled(tenant_id, username, password, role, &control)
    }

    pub fn create_admin_user(
        &self,
        username: &str,
        password: &str,
        role: Role,
        tenant_id: Option<&str>,
    ) -> Result<User, AuthError> {
        if let Some(configured) = self.configured_control_events() {
            let ctx = default_user_lifecycle_ctx();
            let control = UserLifecycleControl {
                ctx: &ctx,
                ledger: configured.ledger.as_ref(),
                config: configured.config,
            };
            self.create_user_in_tenant_controlled(tenant_id, username, password, role, &control)
        } else {
            self.create_user_in_tenant_unaudited(tenant_id, username, password, role)
        }
    }

    fn create_user_in_tenant_unaudited(
        &self,
        tenant_id: Option<&str>,
        username: &str,
        password: &str,
        role: Role,
    ) -> Result<User, AuthError> {
        let id = UserId::from_parts(tenant_id, username);
        let mut users = self.users.write().map_err(lock_err)?;
        if users.contains_key(&id) {
            return Err(AuthError::UserExists(id.to_string()));
        }

        let now = now_ms();
        let user = User {
            username: username.to_string(),
            tenant_id: tenant_id.map(|s| s.to_string()),
            password_hash: hash_password(password),
            scram_verifier: Some(make_scram_verifier(password)),
            role,
            api_keys: Vec::new(),
            created_at: now,
            updated_at: now,
            enabled: true,
        };
        users.insert(id, user.clone());
        drop(users); // release lock before vault I/O
        self.persist_to_vault();
        Ok(user)
    }

    fn create_user_in_tenant_controlled(
        &self,
        tenant_id: Option<&str>,
        username: &str,
        password: &str,
        role: Role,
        control: &UserLifecycleControl<'_>,
    ) -> Result<User, AuthError> {
        let id = UserId::from_parts(tenant_id, username);
        match self.create_user_in_tenant_unaudited(tenant_id, username, password, role) {
            Ok(user) => {
                if let Err(err) = self.emit_user_lifecycle_allowed(
                    control,
                    EventKind::UserCreate,
                    "user.create",
                    &id,
                    user_control_fields(&id, Some(role), Some(true), true),
                ) {
                    self.rollback_create_user(&id);
                    return Err(err);
                }
                Ok(user)
            }
            Err(err) => {
                self.emit_user_lifecycle_outcome(
                    control,
                    if user_error_is_denied(&err) {
                        Outcome::Denied
                    } else {
                        Outcome::Error
                    },
                    EventKind::UserCreate,
                    "user.create",
                    &id,
                    Some(err.to_string()),
                    user_control_fields(&id, Some(role), Some(true), true),
                );
                Err(err)
            }
        }
    }

    /// Look up a user's SCRAM verifier by full `UserId`.
    ///
    /// The wire handshake passes the tenant resolved from the session
    /// (or `None` for the bootstrap admin) so cross-tenant collisions
    /// never authenticate the wrong identity.
    pub fn lookup_scram_verifier(&self, id: &UserId) -> Option<crate::auth::scram::ScramVerifier> {
        // Synthetic platform-owner principal must never authenticate.
        if crate::auth::self_lock_guard::is_synthetic_principal(&id.username) {
            return None;
        }
        let users = self.users.read().ok()?;
        users.get(id).and_then(|u| u.scram_verifier.clone())
    }

    /// Backwards-compatible shim for the v2 wire bootstrap path: looks
    /// up a user by username assuming the platform (`tenant=None`)
    /// scope. Use this only where the handshake hasn't yet learned the
    /// caller's tenant.
    pub fn lookup_scram_verifier_global(
        &self,
        username: &str,
    ) -> Option<crate::auth::scram::ScramVerifier> {
        self.lookup_scram_verifier(&UserId::platform(username))
    }

    /// Return all users (password hashes redacted).
    ///
    /// The synthetic [`crate::auth::self_lock_guard::PLATFORM_OWNER_USERNAME`]
    /// principal is filtered out — it is a policy-graph anchor, not an
    /// operator-visible account.
    pub fn list_users(&self) -> Vec<User> {
        let users = match self.users.read() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        users
            .values()
            .filter(|u| !crate::auth::self_lock_guard::is_synthetic_principal(&u.username))
            .map(|u| User {
                password_hash: String::new(), // redacted
                ..u.clone()
            })
            .collect()
    }

    /// Return users restricted to a tenant scope.
    ///
    /// `tenant_filter`:
    ///   - `None` listing in `Some(None)` — only platform users
    ///   - `Some(Some("acme"))` — only users in tenant `acme`
    ///   - `None` argument — all users (admin-only callers)
    pub fn list_users_scoped(&self, tenant_filter: Option<Option<&str>>) -> Vec<User> {
        let users = match self.users.read() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        users
            .values()
            .filter(|u| match tenant_filter {
                None => true,
                Some(t) => u.tenant_id.as_deref() == t,
            })
            .filter(|u| !crate::auth::self_lock_guard::is_synthetic_principal(&u.username))
            .map(|u| User {
                password_hash: String::new(), // redacted
                ..u.clone()
            })
            .collect()
    }

    /// Resource shape used by IAM policies that govern user lifecycle
    /// mutations. Platform users are addressed as `user:<username>`;
    /// tenant users are addressed as `user:tenant/<tenant>/<username>`
    /// by the existing policy resource qualifier.
    pub fn user_lifecycle_resource(uid: &UserId) -> ResourceRef {
        let resource = ResourceRef::new("user", uid.username.clone());
        match &uid.tenant {
            Some(tenant) => resource.with_tenant(tenant.clone()),
            None => resource,
        }
    }

    pub fn eval_context_for_principal(
        &self,
        principal: &UserId,
        role: Role,
        current_tenant: Option<String>,
    ) -> EvalContext {
        EvalContext {
            principal_tenant: principal.tenant.clone(),
            current_tenant,
            peer_ip: None,
            mfa_present: false,
            now_ms: now_ms(),
            principal_is_admin_role: role == Role::Admin,
            principal_is_platform_scoped: principal.tenant.is_none(),
        }
    }

    /// Policy gate for user lifecycle mutations.
    ///
    /// This is intentionally separate from the store mutation methods:
    /// bootstrap and manifest loading can still seed users directly,
    /// while public surfaces must authorize `user:*` before calling the
    /// mutator. Explicit Deny wins even in `LegacyRbac` mode.
    pub fn check_user_lifecycle_authz(
        &self,
        actor: &UserId,
        actor_role: Role,
        action: &str,
        target: &UserId,
    ) -> bool {
        let resource = Self::user_lifecycle_resource(target);
        let ctx = self.eval_context_for_principal(actor, actor_role, target.tenant.clone());
        self.check_policy_authz_with_role(actor, action, &resource, &ctx, actor_role)
    }

    /// Returns `true` only when an IAM policy explicitly denies the
    /// action on the target. `DefaultDeny` (no matching policy) is
    /// treated as permitted, so this is safe to call after the caller
    /// has already verified the actor's role — it only blocks protected
    /// principals (e.g. the cloud-preset `red_admin` guardrail).
    pub fn has_explicit_user_lifecycle_deny(
        &self,
        actor: &UserId,
        actor_role: Role,
        action: &str,
        target: &UserId,
    ) -> bool {
        let resource = Self::user_lifecycle_resource(target);
        let ctx = self.eval_context_for_principal(actor, actor_role, target.tenant.clone());
        let pols = self.effective_policies(actor);
        let refs: Vec<&Policy> = pols.iter().map(|p| p.as_ref()).collect();
        matches!(
            iam_policies::evaluate(&refs, action, &resource, &ctx),
            iam_policies::Decision::Deny { .. }
        )
    }

    pub fn get_user(&self, tenant_id: Option<&str>, username: &str) -> Option<User> {
        let id = UserId::from_parts(tenant_id, username);
        self.get_user_cloned(&id).map(|u| User {
            password_hash: String::new(),
            ..u
        })
    }

    /// Delete a platform-scoped user (`tenant_id = None`) and revoke
    /// all of their API keys + sessions.
    ///
    /// For tenant-scoped deletes, use [`Self::delete_user_in_tenant`].
    pub fn delete_user(&self, username: &str) -> Result<(), AuthError> {
        self.delete_user_in_tenant(None, username)
    }

    pub fn delete_user_with_control_events(
        &self,
        username: &str,
        ctx: &ControlEventCtx<'_>,
        ledger: &dyn ControlEventLedger,
        config: ControlEventConfig,
    ) -> Result<(), AuthError> {
        self.delete_user_in_tenant_with_control_events(None, username, ctx, ledger, config)
    }

    /// Delete a user identified by `(tenant_id, username)` and revoke
    /// all of their API keys + sessions.
    pub fn delete_user_in_tenant(
        &self,
        tenant_id: Option<&str>,
        username: &str,
    ) -> Result<(), AuthError> {
        if let Some(configured) = self.configured_control_events() {
            let ctx = default_user_lifecycle_ctx();
            let control = UserLifecycleControl {
                ctx: &ctx,
                ledger: configured.ledger.as_ref(),
                config: configured.config,
            };
            self.delete_user_in_tenant_controlled(tenant_id, username, &control)
        } else {
            self.delete_user_in_tenant_unaudited(tenant_id, username)
        }
    }

    pub fn delete_user_in_tenant_with_control_events(
        &self,
        tenant_id: Option<&str>,
        username: &str,
        ctx: &ControlEventCtx<'_>,
        ledger: &dyn ControlEventLedger,
        config: ControlEventConfig,
    ) -> Result<(), AuthError> {
        let control = UserLifecycleControl {
            ctx,
            ledger,
            config,
        };
        self.delete_user_in_tenant_controlled(tenant_id, username, &control)
    }

    fn delete_user_in_tenant_unaudited(
        &self,
        tenant_id: Option<&str>,
        username: &str,
    ) -> Result<(), AuthError> {
        let id = UserId::from_parts(tenant_id, username);
        let mut users = self.users.write().map_err(lock_err)?;
        let user = users
            .remove(&id)
            .ok_or_else(|| AuthError::UserNotFound(id.to_string()))?;

        // Remove API key index entries.
        if let Ok(mut idx) = self.api_key_index.write() {
            for api_key in &user.api_keys {
                idx.remove(&api_key.key);
            }
        }

        // Remove sessions belonging to this user (match on tenant+username
        // so we don't tear down a same-named user in another tenant).
        if let Ok(mut sessions) = self.sessions.write() {
            sessions
                .retain(|_, s| !(s.username == username && s.tenant_id.as_deref() == tenant_id));
        }

        self.persist_to_vault();
        Ok(())
    }

    fn delete_user_in_tenant_controlled(
        &self,
        tenant_id: Option<&str>,
        username: &str,
        control: &UserLifecycleControl<'_>,
    ) -> Result<(), AuthError> {
        let id = UserId::from_parts(tenant_id, username);
        let rollback = self.user_delete_rollback_snapshot(&id, tenant_id, username);
        match self.delete_user_in_tenant_unaudited(tenant_id, username) {
            Ok(()) => {
                if let Err(err) = self.emit_user_lifecycle_allowed(
                    control,
                    EventKind::UserDelete,
                    "user.delete",
                    &id,
                    user_control_fields(&id, rollback.as_ref().map(|r| r.0.role), None, false),
                ) {
                    if let Some((user, sessions)) = rollback {
                        self.restore_deleted_user(&id, user, sessions);
                    }
                    return Err(err);
                }
                Ok(())
            }
            Err(err) => {
                self.emit_user_lifecycle_outcome(
                    control,
                    if user_error_is_denied(&err) {
                        Outcome::Denied
                    } else {
                        Outcome::Error
                    },
                    EventKind::UserDelete,
                    "user.delete",
                    &id,
                    Some(err.to_string()),
                    user_control_fields(&id, rollback.as_ref().map(|r| r.0.role), None, false),
                );
                Err(err)
            }
        }
    }

    /// Change password (requires the old password). Defaults to
    /// platform tenant; use [`Self::change_password_in_tenant`] for
    /// scoped users.
    pub fn change_password(
        &self,
        username: &str,
        old_password: &str,
        new_password: &str,
    ) -> Result<(), AuthError> {
        self.change_password_in_tenant(None, username, old_password, new_password)
    }

    pub fn change_password_with_control_events(
        &self,
        username: &str,
        old_password: &str,
        new_password: &str,
        ctx: &ControlEventCtx<'_>,
        ledger: &dyn ControlEventLedger,
        config: ControlEventConfig,
    ) -> Result<(), AuthError> {
        self.change_password_in_tenant_with_control_events(
            None,
            username,
            old_password,
            new_password,
            ctx,
            ledger,
            config,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn change_password_in_tenant(
        &self,
        tenant_id: Option<&str>,
        username: &str,
        old_password: &str,
        new_password: &str,
    ) -> Result<(), AuthError> {
        if let Some(configured) = self.configured_control_events() {
            let ctx = default_user_lifecycle_ctx();
            let control = UserLifecycleControl {
                ctx: &ctx,
                ledger: configured.ledger.as_ref(),
                config: configured.config,
            };
            self.change_password_in_tenant_controlled(
                tenant_id,
                username,
                old_password,
                new_password,
                &control,
            )
        } else {
            self.change_password_in_tenant_unaudited(
                tenant_id,
                username,
                old_password,
                new_password,
            )
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn change_password_in_tenant_with_control_events(
        &self,
        tenant_id: Option<&str>,
        username: &str,
        old_password: &str,
        new_password: &str,
        ctx: &ControlEventCtx<'_>,
        ledger: &dyn ControlEventLedger,
        config: ControlEventConfig,
    ) -> Result<(), AuthError> {
        let control = UserLifecycleControl {
            ctx,
            ledger,
            config,
        };
        self.change_password_in_tenant_controlled(
            tenant_id,
            username,
            old_password,
            new_password,
            &control,
        )
    }

    fn change_password_in_tenant_unaudited(
        &self,
        tenant_id: Option<&str>,
        username: &str,
        old_password: &str,
        new_password: &str,
    ) -> Result<(), AuthError> {
        let id = UserId::from_parts(tenant_id, username);
        let mut users = self.users.write().map_err(lock_err)?;
        let user = users
            .get_mut(&id)
            .ok_or_else(|| AuthError::UserNotFound(id.to_string()))?;

        if !verify_password(old_password, &user.password_hash) {
            return Err(AuthError::InvalidCredentials);
        }

        user.password_hash = hash_password(new_password);
        user.scram_verifier = Some(make_scram_verifier(new_password));
        user.updated_at = now_ms();
        drop(users); // release lock before vault I/O
        self.persist_to_vault();
        Ok(())
    }

    fn change_password_in_tenant_controlled(
        &self,
        tenant_id: Option<&str>,
        username: &str,
        old_password: &str,
        new_password: &str,
        control: &UserLifecycleControl<'_>,
    ) -> Result<(), AuthError> {
        let id = UserId::from_parts(tenant_id, username);
        let previous = self.get_user_cloned(&id);
        match self.change_password_in_tenant_unaudited(
            tenant_id,
            username,
            old_password,
            new_password,
        ) {
            Ok(()) => {
                if let Err(err) = self.emit_user_lifecycle_allowed(
                    control,
                    EventKind::UserUpdate,
                    "user.update",
                    &id,
                    user_control_fields(
                        &id,
                        previous.as_ref().map(|u| u.role),
                        previous.as_ref().map(|u| u.enabled),
                        true,
                    ),
                ) {
                    self.restore_user_snapshot(&id, previous);
                    return Err(err);
                }
                Ok(())
            }
            Err(err) => {
                self.emit_user_lifecycle_outcome(
                    control,
                    if user_error_is_denied(&err) {
                        Outcome::Denied
                    } else {
                        Outcome::Error
                    },
                    EventKind::UserUpdate,
                    "user.update",
                    &id,
                    Some(err.to_string()),
                    user_control_fields(
                        &id,
                        previous.as_ref().map(|u| u.role),
                        previous.as_ref().map(|u| u.enabled),
                        true,
                    ),
                );
                Err(err)
            }
        }
    }

    /// Change a user's role (admin-only operation). Defaults to platform
    /// tenant; use [`Self::change_role_in_tenant`] for scoped users.
    pub fn change_role(&self, username: &str, new_role: Role) -> Result<(), AuthError> {
        self.change_role_in_tenant(None, username, new_role)
    }

    pub fn change_role_in_tenant(
        &self,
        tenant_id: Option<&str>,
        username: &str,
        new_role: Role,
    ) -> Result<(), AuthError> {
        if let Some(configured) = self.configured_control_events() {
            let ctx = default_user_lifecycle_ctx();
            let control = UserLifecycleControl {
                ctx: &ctx,
                ledger: configured.ledger.as_ref(),
                config: configured.config,
            };
            self.change_role_in_tenant_controlled(tenant_id, username, new_role, &control)
        } else {
            self.change_role_in_tenant_unaudited(tenant_id, username, new_role)
        }
    }

    pub fn change_role_in_tenant_with_control_events(
        &self,
        tenant_id: Option<&str>,
        username: &str,
        new_role: Role,
        ctx: &ControlEventCtx<'_>,
        ledger: &dyn ControlEventLedger,
        config: ControlEventConfig,
    ) -> Result<(), AuthError> {
        let control = UserLifecycleControl {
            ctx,
            ledger,
            config,
        };
        self.change_role_in_tenant_controlled(tenant_id, username, new_role, &control)
    }

    fn change_role_in_tenant_unaudited(
        &self,
        tenant_id: Option<&str>,
        username: &str,
        new_role: Role,
    ) -> Result<(), AuthError> {
        let id = UserId::from_parts(tenant_id, username);
        let mut users = self.users.write().map_err(lock_err)?;
        let user = users
            .get_mut(&id)
            .ok_or_else(|| AuthError::UserNotFound(id.to_string()))?;

        let prior_role = user.role;
        user.role = new_role;
        user.updated_at = now_ms();

        // Issue #205 — promotion to Admin is an operator-grade event:
        // the new role grants destructive capabilities (DROP, ALTER,
        // GRANT) that an operator must observe out-of-band even when
        // the auth path itself is healthy.
        if new_role == Role::Admin && prior_role != Role::Admin {
            crate::telemetry::operator_event::OperatorEvent::AdminCapabilityGranted {
                granted_to: id.to_string(),
                capability: "Role::Admin".to_string(),
                granted_by: "auth_store::change_role".to_string(),
            }
            .emit_global();
        }

        // Downgrade any API keys that now exceed the user's role.
        for key in &mut user.api_keys {
            if key.role > new_role {
                key.role = new_role;
            }
        }

        // Update the api_key_index as well.
        if let Ok(mut idx) = self.api_key_index.write() {
            for key in &user.api_keys {
                if let Some(entry) = idx.get_mut(&key.key) {
                    entry.1 = key.role;
                }
            }
        }

        self.persist_to_vault();
        Ok(())
    }

    fn change_role_in_tenant_controlled(
        &self,
        tenant_id: Option<&str>,
        username: &str,
        new_role: Role,
        control: &UserLifecycleControl<'_>,
    ) -> Result<(), AuthError> {
        let id = UserId::from_parts(tenant_id, username);
        let previous = self.get_user_cloned(&id);
        match self.change_role_in_tenant_unaudited(tenant_id, username, new_role) {
            Ok(()) => {
                if let Err(err) = self.emit_user_lifecycle_allowed(
                    control,
                    EventKind::UserUpdate,
                    "user.update",
                    &id,
                    user_control_fields(
                        &id,
                        Some(new_role),
                        previous.as_ref().map(|u| u.enabled),
                        false,
                    ),
                ) {
                    self.restore_user_snapshot(&id, previous);
                    return Err(err);
                }
                Ok(())
            }
            Err(err) => {
                self.emit_user_lifecycle_outcome(
                    control,
                    if user_error_is_denied(&err) {
                        Outcome::Denied
                    } else {
                        Outcome::Error
                    },
                    EventKind::UserUpdate,
                    "user.update",
                    &id,
                    Some(err.to_string()),
                    user_control_fields(
                        &id,
                        Some(new_role),
                        previous.as_ref().map(|u| u.enabled),
                        false,
                    ),
                );
                Err(err)
            }
        }
    }

    // -----------------------------------------------------------------
    // Authentication (login)
    // -----------------------------------------------------------------

    /// Verify credentials for a platform-tenant user (`tenant_id = None`)
    /// and create a session. For tenant-scoped login use
    /// [`Self::authenticate_in_tenant`].
    ///
    /// When a keypair is available (certificate-based seal), session tokens
    /// are signed with the master secret so the server can verify they were
    /// genuinely issued by this vault instance.
    pub fn authenticate(&self, username: &str, password: &str) -> Result<Session, AuthError> {
        self.authenticate_in_tenant(None, username, password)
    }

    /// Verify credentials for `(tenant_id, username, password)` and
    /// create a session. Tenant-aware: `alice@acme` and `alice@globex`
    /// authenticate independently.
    pub fn authenticate_in_tenant(
        &self,
        tenant_id: Option<&str>,
        username: &str,
        password: &str,
    ) -> Result<Session, AuthError> {
        // The synthetic platform-owner principal exists only as a
        // policy-graph anchor (see `crate::auth::self_lock_guard`); it
        // must never authenticate via any transport.
        if crate::auth::self_lock_guard::is_synthetic_principal(username) {
            return Err(AuthError::InvalidCredentials);
        }
        let id = UserId::from_parts(tenant_id, username);
        let users = self.users.read().map_err(lock_err)?;
        let user = users.get(&id).ok_or(AuthError::InvalidCredentials)?;

        if !user.enabled {
            return Err(AuthError::InvalidCredentials);
        }

        if !verify_password(password, &user.password_hash) {
            return Err(AuthError::InvalidCredentials);
        }

        // Generate token: signed if keypair is available, random otherwise.
        let token = match self.keypair.read().ok().and_then(|g| {
            g.as_ref().map(|kp| {
                let token_id = random_hex(16);
                let sig = kp.sign(format!("session:{}", token_id).as_bytes());
                // Take first 16 bytes of signature for compact token.
                format!("rs_{}{}", token_id, hex::encode(&sig[..16]))
            })
        }) {
            Some(signed_token) => signed_token,
            None => generate_session_token(),
        };

        let now = now_ms();
        let session = Session {
            token,
            username: username.to_string(),
            tenant_id: user.tenant_id.clone(),
            role: user.role,
            created_at: now,
            expires_at: now + (self.config.session_ttl_secs as u128 * 1000),
        };

        drop(users); // release read lock before acquiring write

        let mut sessions = self.sessions.write().map_err(lock_err)?;
        sessions.insert(session.token.clone(), session.clone());
        Ok(session)
    }

    // -----------------------------------------------------------------
    // Token validation
    // -----------------------------------------------------------------

    /// Validate a token (session or API key).
    ///
    /// Returns `(username, role)` if valid, `None` otherwise. Tenant
    /// scope is dropped here for compatibility with the bulk of the
    /// existing caller surface (routing, gRPC control, redwire). Use
    /// [`Self::validate_token_full`] when the caller needs the
    /// resolved `UserId` (e.g. to pin `CURRENT_TENANT()`).
    pub fn validate_token(&self, token: &str) -> Option<(String, Role)> {
        self.validate_token_full(token)
            .map(|(id, role)| (id.username, role))
    }

    /// Tenant-aware token validation. Returns the resolved `UserId`
    /// (which carries the tenant) and the granted `Role`.
    pub fn validate_token_full(&self, token: &str) -> Option<(UserId, Role)> {
        // Try session tokens first.
        if token.starts_with("rs_") {
            if let Ok(sessions) = self.sessions.read() {
                if let Some(session) = sessions.get(token) {
                    let now = now_ms();
                    if now < session.expires_at {
                        return Some((
                            UserId::from_parts(session.tenant_id.as_deref(), &session.username),
                            session.role,
                        ));
                    }
                }
            }
            return None;
        }

        // Try API keys.
        if token.starts_with("rk_") {
            if let Ok(idx) = self.api_key_index.read() {
                return idx.get(token).cloned();
            }
            return None;
        }

        None
    }

    // -----------------------------------------------------------------
    // API Key management
    // -----------------------------------------------------------------

    /// Create a persistent API key for a platform-tenant user.
    ///
    /// For tenant-scoped users use [`Self::create_api_key_in_tenant`].
    pub fn create_api_key(
        &self,
        username: &str,
        name: &str,
        role: Role,
    ) -> Result<ApiKey, AuthError> {
        self.create_api_key_in_tenant(None, username, name, role)
    }

    pub fn create_api_key_with_control_events(
        &self,
        username: &str,
        name: &str,
        role: Role,
        ctx: &ControlEventCtx<'_>,
        ledger: &dyn ControlEventLedger,
        config: ControlEventConfig,
    ) -> Result<ApiKey, AuthError> {
        self.create_api_key_in_tenant_with_control_events(
            None, username, name, role, ctx, ledger, config,
        )
    }

    pub fn create_api_key_in_tenant(
        &self,
        tenant_id: Option<&str>,
        username: &str,
        name: &str,
        role: Role,
    ) -> Result<ApiKey, AuthError> {
        if let Some(configured) = self.configured_control_events() {
            let ctx = default_user_lifecycle_ctx();
            let control = UserLifecycleControl {
                ctx: &ctx,
                ledger: configured.ledger.as_ref(),
                config: configured.config,
            };
            self.create_api_key_in_tenant_controlled(tenant_id, username, name, role, &control)
        } else {
            self.create_api_key_in_tenant_unaudited(tenant_id, username, name, role)
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn create_api_key_in_tenant_with_control_events(
        &self,
        tenant_id: Option<&str>,
        username: &str,
        name: &str,
        role: Role,
        ctx: &ControlEventCtx<'_>,
        ledger: &dyn ControlEventLedger,
        config: ControlEventConfig,
    ) -> Result<ApiKey, AuthError> {
        let control = UserLifecycleControl {
            ctx,
            ledger,
            config,
        };
        self.create_api_key_in_tenant_controlled(tenant_id, username, name, role, &control)
    }

    fn create_api_key_in_tenant_unaudited(
        &self,
        tenant_id: Option<&str>,
        username: &str,
        name: &str,
        role: Role,
    ) -> Result<ApiKey, AuthError> {
        let id = UserId::from_parts(tenant_id, username);
        let mut users = self.users.write().map_err(lock_err)?;
        let user = users
            .get_mut(&id)
            .ok_or_else(|| AuthError::UserNotFound(id.to_string()))?;

        // The key's role cannot exceed the user's role.
        if role > user.role {
            return Err(AuthError::RoleExceeded {
                requested: role,
                ceiling: user.role,
            });
        }

        let api_key = ApiKey {
            key: generate_api_key(),
            name: name.to_string(),
            role,
            created_at: now_ms(),
        };

        user.api_keys.push(api_key.clone());
        user.updated_at = now_ms();

        // Update the index.
        if let Ok(mut idx) = self.api_key_index.write() {
            idx.insert(api_key.key.clone(), (id.clone(), api_key.role));
        }

        drop(users); // release lock before vault I/O
        self.persist_to_vault();
        Ok(api_key)
    }

    fn create_api_key_in_tenant_controlled(
        &self,
        tenant_id: Option<&str>,
        username: &str,
        name: &str,
        role: Role,
        control: &UserLifecycleControl<'_>,
    ) -> Result<ApiKey, AuthError> {
        let id = UserId::from_parts(tenant_id, username);
        match self.create_api_key_in_tenant_unaudited(tenant_id, username, name, role) {
            Ok(api_key) => {
                let key_id = api_key_id(&api_key.key);
                if let Err(err) = self.emit_api_key_allowed(
                    control,
                    EventKind::ApiKeyCreate,
                    "apikey.create",
                    &key_id,
                    api_key_control_fields(&id, role, &key_id),
                ) {
                    self.rollback_create_api_key(&id, &api_key.key);
                    return Err(err);
                }
                Ok(api_key)
            }
            Err(err) => {
                self.emit_user_lifecycle_outcome(
                    control,
                    if user_error_is_denied(&err) {
                        Outcome::Denied
                    } else {
                        Outcome::Error
                    },
                    EventKind::ApiKeyCreate,
                    "apikey.create",
                    &id,
                    Some(err.to_string()),
                    user_control_fields(&id, Some(role), None, false),
                );
                Err(err)
            }
        }
    }

    /// Revoke (delete) an API key.
    pub fn revoke_api_key(&self, key: &str) -> Result<(), AuthError> {
        if let Some(configured) = self.configured_control_events() {
            let ctx = default_user_lifecycle_ctx();
            let control = UserLifecycleControl {
                ctx: &ctx,
                ledger: configured.ledger.as_ref(),
                config: configured.config,
            };
            self.revoke_api_key_controlled(key, &control)
        } else {
            self.revoke_api_key_unaudited(key)
        }
    }

    pub fn revoke_api_key_with_control_events(
        &self,
        key: &str,
        ctx: &ControlEventCtx<'_>,
        ledger: &dyn ControlEventLedger,
        config: ControlEventConfig,
    ) -> Result<(), AuthError> {
        let control = UserLifecycleControl {
            ctx,
            ledger,
            config,
        };
        self.revoke_api_key_controlled(key, &control)
    }

    fn revoke_api_key_unaudited(&self, key: &str) -> Result<(), AuthError> {
        let mut users = self.users.write().map_err(lock_err)?;

        // Find which user owns this key (look up by the api_key_index
        // first; fall back to a scan for legacy state restored before
        // the index was reseeded).
        let owner_id: UserId = {
            if let Ok(idx) = self.api_key_index.read() {
                if let Some((id, _)) = idx.get(key) {
                    id.clone()
                } else {
                    return Err(AuthError::KeyNotFound(key.to_string()));
                }
            } else {
                let owner = users
                    .iter()
                    .find(|(_, u)| u.api_keys.iter().any(|k| k.key == key));
                match owner {
                    Some((id, _)) => id.clone(),
                    None => return Err(AuthError::KeyNotFound(key.to_string())),
                }
            }
        };

        let user = users
            .get_mut(&owner_id)
            .ok_or_else(|| AuthError::KeyNotFound(key.to_string()))?;
        user.api_keys.retain(|k| k.key != key);
        user.updated_at = now_ms();

        // Remove from index.
        if let Ok(mut idx) = self.api_key_index.write() {
            idx.remove(key);
        }

        self.persist_to_vault();
        Ok(())
    }

    fn revoke_api_key_controlled(
        &self,
        key: &str,
        control: &UserLifecycleControl<'_>,
    ) -> Result<(), AuthError> {
        let key_id = api_key_id(key);
        let rollback = self.api_key_rollback_snapshot(key);
        let id = rollback
            .as_ref()
            .map(|r| r.0.clone())
            .unwrap_or_else(|| UserId::from_parts(None, ""));
        let role = rollback.as_ref().map(|r| r.1.role).unwrap_or(Role::Read);
        match self.revoke_api_key_unaudited(key) {
            Ok(()) => {
                if let Err(err) = self.emit_api_key_allowed(
                    control,
                    EventKind::ApiKeyRevoke,
                    "apikey.revoke",
                    &key_id,
                    api_key_control_fields(&id, role, &key_id),
                ) {
                    if let Some((owner, api_key)) = rollback {
                        self.restore_api_key(&owner, api_key);
                    }
                    return Err(err);
                }
                Ok(())
            }
            Err(err) => {
                self.emit_user_lifecycle_outcome(
                    control,
                    if user_error_is_denied(&err) {
                        Outcome::Denied
                    } else {
                        Outcome::Error
                    },
                    EventKind::ApiKeyRevoke,
                    "apikey.revoke",
                    &id,
                    Some(err.to_string()),
                    api_key_control_fields(&id, role, &key_id),
                );
                Err(err)
            }
        }
    }

    // -----------------------------------------------------------------
    // Session management
    // -----------------------------------------------------------------

    /// Revoke a session token.
    pub fn revoke_session(&self, token: &str) {
        if let Ok(mut sessions) = self.sessions.write() {
            sessions.remove(token);
        }
    }

    /// Purge expired sessions (housekeeping).
    pub fn purge_expired_sessions(&self) -> usize {
        let now = now_ms();
        if let Ok(mut sessions) = self.sessions.write() {
            let before = sessions.len();
            sessions.retain(|_, s| s.expires_at > now);
            return before - sessions.len();
        }
        0
    }

    // -----------------------------------------------------------------
    // Granular RBAC — GRANT / REVOKE
    //
    // The privilege engine lives in `super::privileges`. These helpers
    // are the AuthStore facade: they keep an in-memory map of grants per
    // user (plus a `public_grants` list), persist additions/removals to
    // the existing `vault_kv` store, and rebuild the per-user
    // `PermissionCache` so the hot path stays O(1).
    //
    // Persistence design: rather than extend the snapshot/restore
    // pipeline (Agent #2's territory) we serialise grants and account
    // attributes to the vault KV store. That gives us atomic write +
    // encrypted-at-rest semantics for free without touching the
    // existing USER/KEY/KV serializer paths. On restart `rehydrate_acl`
    // reads these KV entries back into the in-memory maps.
    // -----------------------------------------------------------------

    /// Persist a grant. Returns `Forbidden` when the granting user is
    /// not Admin or attempts a cross-tenant grant.
    pub fn grant(
        &self,
        granter: &UserId,
        granter_role: Role,
        principal: GrantPrincipal,
        resource: Resource,
        actions: Vec<Action>,
        with_grant_option: bool,
        tenant: Option<String>,
    ) -> Result<(), AuthError> {
        if granter_role != Role::Admin {
            return Err(AuthError::Forbidden(format!(
                "GRANT requires Admin role; granter `{}` has `{:?}`",
                granter, granter_role
            )));
        }

        // Cross-tenant guard: a tenant-scoped admin cannot mint grants
        // outside their tenant. Platform admin (tenant=None) may grant
        // anywhere.
        if granter.tenant.is_some() && granter.tenant != tenant {
            return Err(AuthError::Forbidden(format!(
                "cross-tenant GRANT denied: granter tenant `{:?}` != grant tenant `{:?}`",
                granter.tenant, tenant
            )));
        }

        let mut actions_set = std::collections::BTreeSet::new();
        for a in actions {
            actions_set.insert(a);
        }
        let g = Grant {
            principal: principal.clone(),
            resource,
            actions: actions_set,
            with_grant_option,
            granted_by: granter.to_string(),
            granted_at: now_ms(),
            tenant,
            columns: None,
        };

        match &principal {
            GrantPrincipal::User(uid) => {
                self.grants
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .entry(uid.clone())
                    .or_default()
                    .push(g.clone());
                self.invalidate_permission_cache(Some(uid));
            }
            GrantPrincipal::Public => {
                self.public_grants
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(g.clone());
                self.invalidate_permission_cache(None);
            }
            GrantPrincipal::Group(_) => {
                return Err(AuthError::Forbidden(
                    "GROUP principals are not yet supported; use a USER or PUBLIC".to_string(),
                ));
            }
        }

        // Issue #119: a fresh grant changes the visible-collections set
        // for `(tenant, role)` callers under the same tenant. Drop those
        // cache entries so the next AI command sees the new SELECT
        // privilege immediately.
        self.invalidate_visible_collections_for_tenant(g.tenant.as_deref());

        self.persist_acl_to_kv();
        Ok(())
    }

    /// Drop matching grants from a principal. Returns the number of
    /// grants removed.
    pub fn revoke(
        &self,
        granter_role: Role,
        principal: &GrantPrincipal,
        resource: &Resource,
        actions: &[Action],
    ) -> Result<usize, AuthError> {
        if granter_role != Role::Admin {
            return Err(AuthError::Forbidden(format!(
                "REVOKE requires Admin role; granter has `{:?}`",
                granter_role
            )));
        }

        let removed = match principal {
            GrantPrincipal::User(uid) => {
                let mut g = self.grants.write().unwrap_or_else(|e| e.into_inner());
                let before = g.get(uid).map(|v| v.len()).unwrap_or(0);
                if let Some(list) = g.get_mut(uid) {
                    list.retain(|gr| {
                        !(gr.resource == *resource
                            && (actions.iter().any(|a| gr.actions.contains(a))
                                || (gr.actions.contains(&Action::All) && !actions.is_empty())))
                    });
                }
                let after = g.get(uid).map(|v| v.len()).unwrap_or(0);
                drop(g);
                self.invalidate_permission_cache(Some(uid));
                before - after
            }
            GrantPrincipal::Public => {
                let mut p = self
                    .public_grants
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                let before = p.len();
                p.retain(|gr| {
                    !(gr.resource == *resource
                        && (actions.iter().any(|a| gr.actions.contains(a))
                            || (gr.actions.contains(&Action::All) && !actions.is_empty())))
                });
                let after = p.len();
                drop(p);
                self.invalidate_permission_cache(None);
                before - after
            }
            GrantPrincipal::Group(_) => 0,
        };

        if removed > 0 {
            // Issue #119: REVOKE may shrink the visible-collections set
            // for any `(tenant, role)` slot. We don't know the exact
            // tenant when the principal is `Public`, so a `Public`
            // revoke clears the whole cache; user revokes scope to the
            // user's tenant.
            match principal {
                GrantPrincipal::User(uid) => {
                    self.invalidate_visible_collections_for_tenant(uid.tenant.as_deref());
                }
                GrantPrincipal::Public | GrantPrincipal::Group(_) => {
                    self.invalidate_visible_collections_cache();
                }
            }
            self.persist_acl_to_kv();
        }
        Ok(removed)
    }

    /// Compute the set of collection ids a given `(tenant, role)`
    /// scope can read, consulting the explicit grant table. The result
    /// is cached for `super::scope_cache::DEFAULT_TTL` (60s) and
    /// invalidated on every GRANT/REVOKE/policy/collection mutation
    /// that could change the answer.
    ///
    /// `all_collections` is the full list of collection ids known to
    /// the storage layer. The runtime hands it in so this module stays
    /// decoupled from the storage crate. Each collection passes through
    /// `check_grant(SELECT)` under a synthetic `(principal, role,
    /// tenant)` view. The cache key includes principal because direct
    /// grants can differ between users that share the same tenant and
    /// role.
    pub fn visible_collections_for_scope(
        &self,
        tenant: Option<&str>,
        role: Role,
        principal: &str,
        all_collections: &[String],
    ) -> std::collections::HashSet<String> {
        let key = super::scope_cache::ScopeKey::new(tenant, principal, role);
        if let Some(hit) = self.visible_collections_cache.get(&key) {
            return hit;
        }
        // Slow path: walk every collection through `check_grant`. We
        // build the AuthzContext once, then reuse it per resource.
        let ctx = AuthzContext {
            principal,
            effective_role: role,
            tenant,
        };
        let mut visible = std::collections::HashSet::new();
        for collection in all_collections {
            let resource = Resource::table_from_name(collection);
            if self.check_grant(&ctx, Action::Select, &resource).is_ok() {
                visible.insert(collection.clone());
            }
        }
        self.visible_collections_cache.insert(key, visible.clone());
        visible
    }

    /// Stats probe required by issue #119 — exposes hit/miss counts and
    /// invalidations for the visible-collections cache so metrics
    /// pipelines can compute a hit rate.
    pub fn auth_cache_stats(&self) -> super::scope_cache::AuthCacheStats {
        self.visible_collections_cache.stats()
    }

    /// Drop every cached `(tenant, role)` entry. Called from CREATE
    /// POLICY / DROP POLICY / DROP COLLECTION paths where the affected
    /// tenant set is unknown.
    pub fn invalidate_visible_collections_cache(&self) {
        self.visible_collections_cache.invalidate_all();
    }

    /// Drop cached entries for one tenant. Called from GRANT / REVOKE
    /// where the principal's tenant is known.
    pub fn invalidate_visible_collections_for_tenant(&self, tenant: Option<&str>) {
        self.visible_collections_cache.invalidate_tenant(tenant);
    }

    /// Snapshot of every grant the principal effectively has, including
    /// `Public` grants. Audit / introspection helper.
    pub fn effective_grants(&self, uid: &UserId) -> Vec<Grant> {
        let mut out = Vec::new();
        if let Ok(g) = self.grants.read() {
            if let Some(list) = g.get(uid) {
                out.extend(list.iter().cloned());
            }
        }
        if let Ok(p) = self.public_grants.read() {
            out.extend(p.iter().cloned());
        }
        out
    }

    /// Run a privilege check using the in-memory grant tables. Returns
    /// `Ok(())` on allow, `Err(AuthzError)` on deny.
    pub fn check_grant(
        &self,
        ctx: &AuthzContext<'_>,
        action: Action,
        resource: &Resource,
    ) -> Result<(), AuthzError> {
        if ctx.effective_role == Role::Admin {
            return Ok(());
        }

        let uid = UserId::from_parts(ctx.tenant, ctx.principal);

        // Fast path: per-user pre-resolved cache.
        if let Ok(cache) = self.permission_cache.read() {
            if let Some(pc) = cache.get(&uid) {
                if pc.allows(resource, action) {
                    return Ok(());
                }
            }
        }

        // Slow path: linear scan + rebuild cache as a side-effect.
        let user_grants = self
            .grants
            .read()
            .ok()
            .and_then(|g| g.get(&uid).cloned())
            .unwrap_or_default();
        let any_user_grants = self
            .grants
            .read()
            .ok()
            .map(|g| g.values().any(|list| !list.is_empty()))
            .unwrap_or(false);
        let public_grants = self
            .public_grants
            .read()
            .ok()
            .map(|p| p.clone())
            .unwrap_or_default();
        if user_grants.is_empty() && public_grants.is_empty() && any_user_grants {
            return Err(AuthzError::PermissionDenied {
                action,
                resource: resource.clone(),
                principal: ctx.principal.to_string(),
            });
        }
        let view = GrantsView {
            user_grants: &user_grants,
            public_grants: &public_grants,
        };
        let result = check_grant(ctx, action, resource, &view);

        if result.is_ok() {
            let pc = PermissionCache::build(&user_grants, &public_grants);
            if let Ok(mut cache) = self.permission_cache.write() {
                cache.insert(uid, pc);
            }
        }
        result
    }

    // -----------------------------------------------------------------
    // ALTER USER attributes (VALID UNTIL, CONNECTION LIMIT, etc.)
    // -----------------------------------------------------------------

    /// Replace the attribute record for `uid`.
    pub fn set_user_attributes(
        &self,
        uid: &UserId,
        attrs: UserAttributes,
    ) -> Result<(), AuthError> {
        let users = self.users.read().map_err(lock_err)?;
        if !users.contains_key(uid) {
            return Err(AuthError::UserNotFound(uid.to_string()));
        }
        drop(users);

        self.user_attributes
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(uid.clone(), attrs);
        self.invalidate_iam_cache(Some(uid));
        self.persist_acl_to_kv();
        Ok(())
    }

    /// Read the attributes for `uid`. Returns `Default::default()` for
    /// users that have never been altered.
    pub fn user_attributes(&self, uid: &UserId) -> UserAttributes {
        self.user_attributes
            .read()
            .ok()
            .and_then(|m| m.get(uid).cloned())
            .unwrap_or_default()
    }

    pub fn add_user_to_group(&self, uid: &UserId, group: &str) -> Result<(), AuthError> {
        if group.trim().is_empty() {
            return Err(AuthError::Forbidden("group name cannot be empty".into()));
        }
        let mut attrs = self.user_attributes(uid);
        if !attrs.groups.iter().any(|g| g == group) {
            attrs.groups.push(group.to_string());
            attrs.groups.sort();
        }
        self.set_user_attributes(uid, attrs)
    }

    pub fn remove_user_from_group(&self, uid: &UserId, group: &str) -> Result<(), AuthError> {
        let mut attrs = self.user_attributes(uid);
        attrs.groups.retain(|g| g != group);
        self.set_user_attributes(uid, attrs)
    }

    /// Toggle `User.enabled` without rotating credentials.
    pub fn set_user_enabled(&self, uid: &UserId, enabled: bool) -> Result<(), AuthError> {
        if let Some(configured) = self.configured_control_events() {
            let ctx = default_user_lifecycle_ctx();
            let control = UserLifecycleControl {
                ctx: &ctx,
                ledger: configured.ledger.as_ref(),
                config: configured.config,
            };
            self.set_user_enabled_controlled(uid, enabled, &control)
        } else {
            self.set_user_enabled_unaudited(uid, enabled)
        }
    }

    pub fn disable_user(
        &self,
        username: &str,
        ctx: &ControlEventCtx<'_>,
        ledger: &dyn ControlEventLedger,
        config: ControlEventConfig,
    ) -> Result<(), AuthError> {
        self.disable_user_in_tenant(None, username, ctx, ledger, config)
    }

    pub fn disable_user_in_tenant(
        &self,
        tenant_id: Option<&str>,
        username: &str,
        ctx: &ControlEventCtx<'_>,
        ledger: &dyn ControlEventLedger,
        config: ControlEventConfig,
    ) -> Result<(), AuthError> {
        let uid = UserId::from_parts(tenant_id, username);
        let control = UserLifecycleControl {
            ctx,
            ledger,
            config,
        };
        self.set_user_enabled_controlled(&uid, false, &control)
    }

    fn set_user_enabled_unaudited(&self, uid: &UserId, enabled: bool) -> Result<(), AuthError> {
        let mut users = self.users.write().map_err(lock_err)?;
        let user = users
            .get_mut(uid)
            .ok_or_else(|| AuthError::UserNotFound(uid.to_string()))?;
        user.enabled = enabled;
        user.updated_at = now_ms();
        drop(users);
        self.persist_to_vault();
        Ok(())
    }

    fn set_user_enabled_controlled(
        &self,
        uid: &UserId,
        enabled: bool,
        control: &UserLifecycleControl<'_>,
    ) -> Result<(), AuthError> {
        let previous = self.get_user_cloned(uid);
        let kind = if enabled {
            EventKind::UserUpdate
        } else {
            EventKind::UserDisable
        };
        let action = if enabled {
            "user.update"
        } else {
            "user.disable"
        };
        match self.set_user_enabled_unaudited(uid, enabled) {
            Ok(()) => {
                if let Err(err) = self.emit_user_lifecycle_allowed(
                    control,
                    kind,
                    action,
                    uid,
                    user_control_fields(
                        uid,
                        previous.as_ref().map(|u| u.role),
                        Some(enabled),
                        false,
                    ),
                ) {
                    self.restore_user_snapshot(uid, previous);
                    return Err(err);
                }
                Ok(())
            }
            Err(err) => {
                self.emit_user_lifecycle_outcome(
                    control,
                    if user_error_is_denied(&err) {
                        Outcome::Denied
                    } else {
                        Outcome::Error
                    },
                    kind,
                    action,
                    uid,
                    Some(err.to_string()),
                    user_control_fields(
                        uid,
                        previous.as_ref().map(|u| u.role),
                        Some(enabled),
                        false,
                    ),
                );
                Err(err)
            }
        }
    }

    fn emit_user_lifecycle_allowed(
        &self,
        control: &UserLifecycleControl<'_>,
        kind: EventKind,
        action: &'static str,
        id: &UserId,
        fields: HashMap<String, Sensitivity>,
    ) -> Result<(), AuthError> {
        self.emit_user_lifecycle_event(control, Outcome::Allowed, kind, action, id, None, fields)
    }

    fn emit_api_key_allowed(
        &self,
        control: &UserLifecycleControl<'_>,
        kind: EventKind,
        action: &'static str,
        api_key_id: &str,
        fields: HashMap<String, Sensitivity>,
    ) -> Result<(), AuthError> {
        self.emit_control_event(
            control,
            Outcome::Allowed,
            kind,
            action,
            Some(api_key_resource(api_key_id)),
            None,
            fields,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_user_lifecycle_outcome(
        &self,
        control: &UserLifecycleControl<'_>,
        outcome: Outcome,
        kind: EventKind,
        action: &'static str,
        id: &UserId,
        reason: Option<String>,
        fields: HashMap<String, Sensitivity>,
    ) {
        let _ = self.emit_user_lifecycle_event(control, outcome, kind, action, id, reason, fields);
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_user_lifecycle_event(
        &self,
        control: &UserLifecycleControl<'_>,
        outcome: Outcome,
        kind: EventKind,
        action: &'static str,
        id: &UserId,
        reason: Option<String>,
        fields: HashMap<String, Sensitivity>,
    ) -> Result<(), AuthError> {
        self.emit_control_event(
            control,
            outcome,
            kind,
            action,
            Some(user_resource(id)),
            reason,
            fields,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_control_event(
        &self,
        control: &UserLifecycleControl<'_>,
        outcome: Outcome,
        kind: EventKind,
        action: &'static str,
        resource: Option<String>,
        reason: Option<String>,
        fields: HashMap<String, Sensitivity>,
    ) -> Result<(), AuthError> {
        let event = ControlEvent {
            kind,
            outcome,
            action: Cow::Borrowed(action),
            resource,
            reason,
            matched_policy_id: None,
            fields,
        };
        match control.ledger.emit(control.ctx, event) {
            Ok(_) => Ok(()),
            Err(err) if control.config.require_persistence() => {
                Err(AuthError::Internal(err.to_string()))
            }
            Err(_) => Ok(()),
        }
    }

    fn rollback_create_user(&self, id: &UserId) {
        let removed = self
            .users
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        if let Some(user) = removed {
            self.remove_api_key_index_entries(&user);
        }
        self.persist_to_vault();
    }

    fn rollback_bootstrap(&self, id: &UserId) {
        self.bootstrapped.store(false, Ordering::Release);
        self.rollback_create_user(id);
    }

    fn restore_user_snapshot(&self, id: &UserId, previous: Option<User>) {
        match previous {
            Some(user) => {
                self.users
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert(id.clone(), user.clone());
                self.index_user_api_keys(id, &user);
            }
            None => {
                self.rollback_create_user(id);
                return;
            }
        }
        self.persist_to_vault();
    }

    fn user_delete_rollback_snapshot(
        &self,
        id: &UserId,
        tenant_id: Option<&str>,
        username: &str,
    ) -> Option<(User, Vec<(String, Session)>)> {
        let user = self.get_user_cloned(id)?;
        let sessions = self
            .sessions
            .read()
            .map(|sessions| {
                sessions
                    .iter()
                    .filter(|(_, session)| {
                        session.username == username && session.tenant_id.as_deref() == tenant_id
                    })
                    .map(|(token, session)| (token.clone(), session.clone()))
                    .collect()
            })
            .unwrap_or_default();
        Some((user, sessions))
    }

    fn restore_deleted_user(&self, id: &UserId, user: User, sessions: Vec<(String, Session)>) {
        self.users
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.clone(), user.clone());
        self.index_user_api_keys(id, &user);
        if !sessions.is_empty() {
            let mut guard = self.sessions.write().unwrap_or_else(|e| e.into_inner());
            for (token, session) in sessions {
                guard.insert(token, session);
            }
        }
        self.persist_to_vault();
    }

    fn rollback_create_api_key(&self, id: &UserId, key: &str) {
        if let Ok(mut users) = self.users.write() {
            if let Some(user) = users.get_mut(id) {
                user.api_keys.retain(|api_key| api_key.key != key);
                user.updated_at = now_ms();
            }
        }
        if let Ok(mut idx) = self.api_key_index.write() {
            idx.remove(key);
        }
        self.persist_to_vault();
    }

    fn api_key_rollback_snapshot(&self, key: &str) -> Option<(UserId, ApiKey)> {
        let users = self.users.read().ok()?;
        users.iter().find_map(|(id, user)| {
            user.api_keys
                .iter()
                .find(|api_key| api_key.key == key)
                .cloned()
                .map(|api_key| (id.clone(), api_key))
        })
    }

    fn restore_api_key(&self, id: &UserId, api_key: ApiKey) {
        if let Ok(mut users) = self.users.write() {
            if let Some(user) = users.get_mut(id) {
                if !user
                    .api_keys
                    .iter()
                    .any(|candidate| candidate.key == api_key.key)
                {
                    user.api_keys.push(api_key.clone());
                    user.updated_at = now_ms();
                }
            }
        }
        if let Ok(mut idx) = self.api_key_index.write() {
            idx.insert(api_key.key.clone(), (id.clone(), api_key.role));
        }
        self.persist_to_vault();
    }

    fn remove_api_key_index_entries(&self, user: &User) {
        if let Ok(mut idx) = self.api_key_index.write() {
            for api_key in &user.api_keys {
                idx.remove(&api_key.key);
            }
        }
    }

    fn index_user_api_keys(&self, id: &UserId, user: &User) {
        if let Ok(mut idx) = self.api_key_index.write() {
            for api_key in &user.api_keys {
                idx.insert(api_key.key.clone(), (id.clone(), api_key.role));
            }
        }
    }

    // -----------------------------------------------------------------
    // Login-side enforcement (HTTP path)
    // -----------------------------------------------------------------

    /// Authenticate with VALID UNTIL / CONNECTION LIMIT enforcement.
    /// Wraps `authenticate_in_tenant` and additionally:
    ///   * rejects logins after `valid_until`,
    ///   * rejects logins when the live session count would exceed the
    ///     `connection_limit` attribute.
    pub fn authenticate_with_attrs(
        &self,
        tenant_id: Option<&str>,
        username: &str,
        password: &str,
    ) -> Result<Session, AuthError> {
        let uid = UserId::from_parts(tenant_id, username);
        let attrs = self.user_attributes(&uid);

        if let Some(deadline) = attrs.valid_until {
            if now_ms() >= deadline {
                return Err(AuthError::Forbidden(format!(
                    "account `{}` expired (VALID UNTIL exceeded)",
                    uid
                )));
            }
        }

        if let Some(limit) = attrs.connection_limit {
            let current = self
                .session_count_by_user
                .read()
                .ok()
                .and_then(|m| m.get(&uid).copied())
                .unwrap_or(0);
            if current >= limit {
                return Err(AuthError::Forbidden(format!(
                    "account `{}` exceeded CONNECTION LIMIT ({})",
                    uid, limit
                )));
            }
        }

        let session = self.authenticate_in_tenant(tenant_id, username, password)?;

        if let Ok(mut counts) = self.session_count_by_user.write() {
            *counts.entry(uid).or_insert(0) += 1;
        }
        Ok(session)
    }

    /// Decrement the live-session count for `uid`. Call from session
    /// revoke / expiry paths so CONNECTION LIMIT stays accurate.
    pub fn decrement_session_count(&self, uid: &UserId) {
        if let Ok(mut counts) = self.session_count_by_user.write() {
            if let Some(c) = counts.get_mut(uid) {
                *c = c.saturating_sub(1);
            }
        }
    }

    // -----------------------------------------------------------------
    // ACL persistence — vault_kv backed
    // -----------------------------------------------------------------

    /// Re-read the ACL state from `vault_kv`. Call after vault load /
    /// restore so the in-memory maps reflect the persisted data.
    pub fn rehydrate_acl(&self) {
        let kv_snapshot: Vec<(String, String)> = self
            .vault_kv
            .read()
            .map(|kv| {
                kv.iter()
                    .filter(|(k, _)| {
                        k.starts_with("red.acl.grants.")
                            || k.starts_with("red.acl.attrs.")
                            || k == &"red.acl.public_grants"
                    })
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default();

        for (k, v) in kv_snapshot {
            if k == "red.acl.public_grants" {
                if let Some(parsed) = decode_grants_blob(&v) {
                    *self
                        .public_grants
                        .write()
                        .unwrap_or_else(|e| e.into_inner()) = parsed;
                }
            } else if let Some(suffix) = k.strip_prefix("red.acl.grants.") {
                if let Some(uid) = decode_uid(suffix) {
                    if let Some(mut parsed) = decode_grants_blob(&v) {
                        // Restore the principal field — the on-disk
                        // line stores only resource+action shape.
                        for g in parsed.iter_mut() {
                            g.principal = GrantPrincipal::User(uid.clone());
                        }
                        self.grants
                            .write()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(uid, parsed);
                    }
                }
            } else if let Some(suffix) = k.strip_prefix("red.acl.attrs.") {
                if let Some(uid) = decode_uid(suffix) {
                    if let Some(parsed) = decode_attrs_blob(&v) {
                        self.user_attributes
                            .write()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(uid, parsed);
                    }
                }
            }
        }

        self.permission_cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    /// Snapshot every ACL change back into the vault KV store.
    fn persist_acl_to_kv(&self) {
        let public = self
            .public_grants
            .read()
            .ok()
            .map(|p| encode_grants_blob(&p))
            .unwrap_or_default();
        self.vault_kv_set("red.acl.public_grants".to_string(), public);

        let snapshot: Vec<(UserId, Vec<Grant>)> = self
            .grants
            .read()
            .ok()
            .map(|g| g.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        for (uid, list) in snapshot {
            let key = format!("red.acl.grants.{}", encode_uid(&uid));
            let val = encode_grants_blob(&list);
            self.vault_kv_set(key, val);
        }

        let attrs_snapshot: Vec<(UserId, UserAttributes)> = self
            .user_attributes
            .read()
            .ok()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        for (uid, attrs) in attrs_snapshot {
            let key = format!("red.acl.attrs.{}", encode_uid(&uid));
            let val = encode_attrs_blob(&attrs);
            self.vault_kv_set(key, val);
        }
    }

    fn invalidate_permission_cache(&self, uid: Option<&UserId>) {
        if let Ok(mut cache) = self.permission_cache.write() {
            match uid {
                Some(u) => {
                    cache.remove(u);
                }
                None => cache.clear(),
            }
        }
    }

    // -----------------------------------------------------------------
    // IAM policies — put / delete / attach / detach / simulate
    //
    // The kernel in `super::policies` owns the Policy type and the
    // evaluator. AuthStore handles persistence + per-user cache + the
    // GRANT translation layer (synthetic `_grant_*` policies).
    // -----------------------------------------------------------------

    pub fn put_policy_with_control_events(
        &self,
        p: Policy,
        control: &PolicyMutationControl<'_>,
    ) -> Result<(), AuthError> {
        let policy_id = p.id.clone();
        let kind = if self.get_policy(&policy_id).is_some() {
            EventKind::PolicyUpdate
        } else {
            EventKind::PolicyCreate
        };

        if p.id.starts_with("_grant_") || p.id.starts_with("_default_") {
            let err = AuthError::Forbidden(format!("policy id `{}` is reserved", p.id));
            self.emit_policy_error(
                control,
                kind,
                "policy:put",
                &policy_id,
                Some(&p),
                None,
                &err,
            );
            return Err(err);
        }

        if let Some(ManagedPolicyDecision::Deny {
            entry_id,
            matched_action,
            matched_resource,
            reason,
            ..
        }) = self.managed_policy_decision(&policy_id, PolicyOp::Put, control)
        {
            self.emit_policy_denied(
                control,
                kind,
                "policy:put",
                &policy_id,
                Some(&p),
                None,
                &reason.to_string(),
                Some(entry_id),
                Some((matched_action, matched_resource)),
            );
            return Err(Self::managed_policy_error(&policy_id, &reason.to_string()));
        }

        let previous = self.get_policy(&policy_id);
        let was_enabled = self.iam_authorization_enabled();
        match self.put_policy_internal(p.clone()) {
            Ok(()) => match self.emit_policy_allowed(
                control,
                kind,
                "policy:put",
                &policy_id,
                Some(&p),
                None,
                None,
            ) {
                Ok(()) => Ok(()),
                Err(err) => {
                    self.restore_policy_put(&policy_id, previous, was_enabled);
                    Err(err)
                }
            },
            Err(err) => {
                self.emit_policy_error(
                    control,
                    kind,
                    "policy:put",
                    &policy_id,
                    Some(&p),
                    None,
                    &err,
                );
                Err(err)
            }
        }
    }

    pub fn delete_policy_with_control_events(
        &self,
        id: &str,
        control: &PolicyMutationControl<'_>,
    ) -> Result<(), AuthError> {
        let existing = self.get_policy(id);
        if let Some(ManagedPolicyDecision::Deny {
            entry_id,
            matched_action,
            matched_resource,
            reason,
            ..
        }) = self.managed_policy_decision(id, PolicyOp::Drop, control)
        {
            self.emit_policy_denied(
                control,
                EventKind::PolicyDelete,
                "policy:drop",
                id,
                existing.as_deref(),
                None,
                &reason.to_string(),
                Some(entry_id),
                Some((matched_action, matched_resource)),
            );
            return Err(Self::managed_policy_error(id, &reason.to_string()));
        }

        let user_attachments = self
            .user_attachments
            .read()
            .map(|m| m.clone())
            .unwrap_or_default();
        let group_attachments = self
            .group_attachments
            .read()
            .map(|m| m.clone())
            .unwrap_or_default();
        let was_enabled = self.iam_authorization_enabled();
        match self.delete_policy(id) {
            Ok(()) => match self.emit_policy_allowed(
                control,
                EventKind::PolicyDelete,
                "policy:drop",
                id,
                existing.as_deref(),
                None,
                None,
            ) {
                Ok(()) => Ok(()),
                Err(err) => {
                    if let Some(policy) = existing {
                        self.restore_policy_delete(
                            id,
                            policy,
                            user_attachments,
                            group_attachments,
                            was_enabled,
                        );
                    }
                    Err(err)
                }
            },
            Err(err) => {
                self.emit_policy_error(
                    control,
                    EventKind::PolicyDelete,
                    "policy:drop",
                    id,
                    existing.as_deref(),
                    None,
                    &err,
                );
                Err(err)
            }
        }
    }

    pub fn attach_policy_with_control_events(
        &self,
        principal: PrincipalRef,
        policy_id: &str,
        control: &PolicyMutationControl<'_>,
    ) -> Result<(), AuthError> {
        let existing = self.get_policy(policy_id);
        if let Some(ManagedPolicyDecision::Deny {
            entry_id,
            matched_action,
            matched_resource,
            reason,
            ..
        }) = self.managed_policy_decision(policy_id, PolicyOp::Attach, control)
        {
            self.emit_policy_denied(
                control,
                EventKind::PolicyAttach,
                "policy:attach",
                policy_id,
                existing.as_deref(),
                Some(&principal),
                &reason.to_string(),
                Some(entry_id),
                Some((matched_action, matched_resource)),
            );
            return Err(Self::managed_policy_error(policy_id, &reason.to_string()));
        }

        if let Some(err_msg) = self.check_self_lock_invariant_for_attach(policy_id) {
            self.emit_policy_denied(
                control,
                EventKind::PolicyAttach,
                "policy:attach",
                policy_id,
                existing.as_deref(),
                Some(&principal),
                &err_msg,
                None,
                None,
            );
            return Err(AuthError::Forbidden(err_msg));
        }

        let user_attachments = self
            .user_attachments
            .read()
            .map(|m| m.clone())
            .unwrap_or_default();
        let group_attachments = self
            .group_attachments
            .read()
            .map(|m| m.clone())
            .unwrap_or_default();
        match self.attach_policy(principal.clone(), policy_id) {
            Ok(()) => match self.emit_policy_allowed(
                control,
                EventKind::PolicyAttach,
                "policy:attach",
                policy_id,
                existing.as_deref(),
                Some(&principal),
                None,
            ) {
                Ok(()) => Ok(()),
                Err(err) => {
                    self.restore_policy_attachments(user_attachments, group_attachments);
                    Err(err)
                }
            },
            Err(err) => {
                self.emit_policy_error(
                    control,
                    EventKind::PolicyAttach,
                    "policy:attach",
                    policy_id,
                    existing.as_deref(),
                    Some(&principal),
                    &err,
                );
                Err(err)
            }
        }
    }

    pub fn detach_policy_with_control_events(
        &self,
        principal: PrincipalRef,
        policy_id: &str,
        control: &PolicyMutationControl<'_>,
    ) -> Result<(), AuthError> {
        let existing = self.get_policy(policy_id);
        if let Some(ManagedPolicyDecision::Deny {
            entry_id,
            matched_action,
            matched_resource,
            reason,
            ..
        }) = self.managed_policy_decision(policy_id, PolicyOp::Detach, control)
        {
            self.emit_policy_denied(
                control,
                EventKind::PolicyDetach,
                "policy:detach",
                policy_id,
                existing.as_deref(),
                Some(&principal),
                &reason.to_string(),
                Some(entry_id),
                Some((matched_action, matched_resource)),
            );
            return Err(Self::managed_policy_error(policy_id, &reason.to_string()));
        }
        let Some(existing_policy) = existing else {
            let err = AuthError::Forbidden(format!("policy `{policy_id}` not found"));
            self.emit_policy_error(
                control,
                EventKind::PolicyDetach,
                "policy:detach",
                policy_id,
                None,
                Some(&principal),
                &err,
            );
            return Err(err);
        };

        let user_attachments = self
            .user_attachments
            .read()
            .map(|m| m.clone())
            .unwrap_or_default();
        let group_attachments = self
            .group_attachments
            .read()
            .map(|m| m.clone())
            .unwrap_or_default();
        match self.detach_policy(principal.clone(), policy_id) {
            Ok(()) => match self.emit_policy_allowed(
                control,
                EventKind::PolicyDetach,
                "policy:detach",
                policy_id,
                Some(existing_policy.as_ref()),
                Some(&principal),
                None,
            ) {
                Ok(()) => Ok(()),
                Err(err) => {
                    self.restore_policy_attachments(user_attachments, group_attachments);
                    Err(err)
                }
            },
            Err(err) => {
                self.emit_policy_error(
                    control,
                    EventKind::PolicyDetach,
                    "policy:detach",
                    policy_id,
                    Some(existing_policy.as_ref()),
                    Some(&principal),
                    &err,
                );
                Err(err)
            }
        }
    }

    fn managed_policy_decision(
        &self,
        policy_id: &str,
        op: PolicyOp,
        control: &PolicyMutationControl<'_>,
    ) -> Option<ManagedPolicyDecision> {
        control.registry.map(|registry| {
            ManagedPolicyGate::new(registry).check_mutation(
                self,
                control.actor,
                control.eval_ctx,
                policy_id,
                op,
            )
        })
    }

    /// Run the [`crate::auth::self_lock_guard`] invariant against the
    /// current policy set. Returns `Some(error_message)` when an attach
    /// must be refused, `None` when the system would remain unlockable.
    ///
    /// `_policy_id` is informational — the invariant is on the full
    /// graph and doesn't currently special-case the new entry.
    fn check_self_lock_invariant_for_attach(&self, _policy_id: &str) -> Option<String> {
        use crate::auth::self_lock_guard::{
            check_self_lock_invariant, format_block_error, InvariantOutcome,
        };
        let policies: Vec<Arc<Policy>> = match self.policies.read() {
            Ok(map) => map.values().cloned().collect(),
            Err(_) => return None,
        };
        let outcome = check_self_lock_invariant(&policies);
        match &outcome {
            InvariantOutcome::Ok => None,
            InvariantOutcome::Blocked { .. } => format_block_error(&outcome),
        }
    }

    fn managed_policy_error(policy_id: &str, reason: &str) -> AuthError {
        AuthError::Forbidden(format!(
            "managed policy mutation blocked for `{policy_id}`: {reason}"
        ))
    }

    fn emit_policy_allowed(
        &self,
        control: &PolicyMutationControl<'_>,
        kind: EventKind,
        action: &'static str,
        policy_id: &str,
        policy: Option<&Policy>,
        principal: Option<&PrincipalRef>,
        matched_policy_id: Option<String>,
    ) -> Result<(), AuthError> {
        let event = ControlEvent {
            kind,
            outcome: Outcome::Allowed,
            action: std::borrow::Cow::Borrowed(action),
            resource: Some(format!("policy:{policy_id}")),
            reason: None,
            matched_policy_id,
            fields: policy_control_fields(policy_id, policy, principal),
        };
        match control.ledger.emit(control.ctx, event) {
            Ok(_) => Ok(()),
            Err(err) if control.config.require_persistence() => {
                Err(AuthError::Internal(err.to_string()))
            }
            Err(_) => Ok(()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_policy_denied(
        &self,
        control: &PolicyMutationControl<'_>,
        kind: EventKind,
        action: &'static str,
        policy_id: &str,
        policy: Option<&Policy>,
        principal: Option<&PrincipalRef>,
        reason: &str,
        matched_policy_id: Option<String>,
        matched: Option<(String, String)>,
    ) {
        let mut fields = policy_control_fields(policy_id, policy, principal);
        if let Some((matched_action, matched_resource)) = matched {
            fields.insert(
                "matched_action".to_string(),
                Sensitivity::raw(matched_action),
            );
            fields.insert(
                "matched_resource".to_string(),
                Sensitivity::raw(matched_resource),
            );
        }
        let event = ControlEvent {
            kind,
            outcome: Outcome::Denied,
            action: std::borrow::Cow::Borrowed(action),
            resource: Some(format!("policy:{policy_id}")),
            reason: Some(reason.to_string()),
            matched_policy_id,
            fields,
        };
        let _ = control.ledger.emit(control.ctx, event);
    }

    fn emit_policy_error(
        &self,
        control: &PolicyMutationControl<'_>,
        kind: EventKind,
        action: &'static str,
        policy_id: &str,
        policy: Option<&Policy>,
        principal: Option<&PrincipalRef>,
        err: &AuthError,
    ) {
        let event = ControlEvent {
            kind,
            outcome: Outcome::Error,
            action: std::borrow::Cow::Borrowed(action),
            resource: Some(format!("policy:{policy_id}")),
            reason: Some(err.to_string()),
            matched_policy_id: None,
            fields: policy_control_fields(policy_id, policy, principal),
        };
        let _ = control.ledger.emit(control.ctx, event);
    }

    fn restore_policy_put(
        &self,
        policy_id: &str,
        previous: Option<Arc<Policy>>,
        was_enabled: bool,
    ) {
        let mut policies = self.policies.write().unwrap_or_else(|e| e.into_inner());
        match previous {
            Some(policy) => {
                policies.insert(policy_id.to_string(), policy);
            }
            None => {
                policies.remove(policy_id);
            }
        }
        drop(policies);
        self.iam_authorization_enabled
            .store(was_enabled, Ordering::Release);
        self.iam_effective_cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.invalidate_visible_collections_cache();
        self.persist_iam_to_kv();
    }

    fn restore_policy_delete(
        &self,
        policy_id: &str,
        policy: Arc<Policy>,
        user_attachments: HashMap<UserId, Vec<String>>,
        group_attachments: HashMap<String, Vec<String>>,
        was_enabled: bool,
    ) {
        self.policies
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(policy_id.to_string(), policy);
        self.restore_policy_attachments(user_attachments, group_attachments);
        self.iam_authorization_enabled
            .store(was_enabled, Ordering::Release);
        self.persist_iam_to_kv();
    }

    fn restore_policy_attachments(
        &self,
        user_attachments: HashMap<UserId, Vec<String>>,
        group_attachments: HashMap<String, Vec<String>>,
    ) {
        *self
            .user_attachments
            .write()
            .unwrap_or_else(|e| e.into_inner()) = user_attachments;
        *self
            .group_attachments
            .write()
            .unwrap_or_else(|e| e.into_inner()) = group_attachments;
        self.iam_effective_cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.invalidate_visible_collections_cache();
        self.persist_iam_to_kv();
    }

    /// Insert or replace a policy by id. Rejects synthetic ids
    /// (`_grant_*` / `_default_*`) so callers can't hand-write them
    /// from the public API. Use `put_policy_internal` for synthetic
    /// inserts.
    pub fn put_policy(&self, p: Policy) -> Result<(), AuthError> {
        if p.id.starts_with("_grant_") || p.id.starts_with("_default_") {
            return Err(AuthError::Forbidden(format!(
                "policy id `{}` is reserved",
                p.id
            )));
        }
        self.put_policy_internal(p)
    }

    /// Internal put bypassing the synthetic-namespace guard. Used by
    /// the GRANT translation layer; exposed publicly so integration
    /// tests can register synthetic `_grant_*` policies without going
    /// through the SQL frontend.
    pub fn put_policy_internal(&self, p: Policy) -> Result<(), AuthError> {
        p.validate()
            .map_err(|e| AuthError::Forbidden(format!("invalid policy `{}`: {e}", p.id)))?;
        let id = p.id.clone();
        self.policies
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, Arc::new(p));
        self.iam_authorization_enabled
            .store(true, Ordering::Release);
        self.iam_effective_cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // Issue #119: a policy mutation can change the visible-
        // collections answer for any (tenant, role); we don't know
        // which up-front, so blow the whole cache.
        self.invalidate_visible_collections_cache();
        self.persist_iam_to_kv();
        Ok(())
    }

    /// Whether the IAM evaluator should be authoritative for runtime
    /// authorization. This flips on the first policy write and remains
    /// on after deletes so dropping all policies leaves the instance in
    /// default-deny rather than silently returning to role fallback.
    pub fn iam_authorization_enabled(&self) -> bool {
        self.iam_authorization_enabled.load(Ordering::Acquire)
    }

    /// Remove a policy and any attachments referencing it.
    pub fn delete_policy(&self, id: &str) -> Result<(), AuthError> {
        let removed = self
            .policies
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id)
            .is_some();
        if !removed {
            return Err(AuthError::Forbidden(format!("policy `{id}` not found")));
        }
        // Detach from every user / group.
        if let Ok(mut ua) = self.user_attachments.write() {
            for ids in ua.values_mut() {
                ids.retain(|p| p != id);
            }
            ua.retain(|_, v| !v.is_empty());
        }
        if let Ok(mut ga) = self.group_attachments.write() {
            for ids in ga.values_mut() {
                ids.retain(|p| p != id);
            }
            ga.retain(|_, v| !v.is_empty());
        }
        self.iam_effective_cache
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // Issue #119: dropping a policy can shrink any caller's visible
        // set; clear the (tenant, role) cache so AI commands re-resolve.
        self.invalidate_visible_collections_cache();
        self.persist_iam_to_kv();
        Ok(())
    }

    /// List all policies (id-sorted for deterministic output).
    pub fn list_policies(&self) -> Vec<Arc<Policy>> {
        let map = match self.policies.read() {
            Ok(g) => g,
            Err(_) => return Vec::new(),
        };
        let mut out: Vec<Arc<Policy>> = map.values().cloned().collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Fetch a single policy by id.
    pub fn get_policy(&self, id: &str) -> Option<Arc<Policy>> {
        self.policies.read().ok().and_then(|m| m.get(id).cloned())
    }

    /// List policies directly attached to a group.
    pub fn group_policies(&self, group: &str) -> Vec<Arc<Policy>> {
        let policies = self.policies.read();
        let attachments = self.group_attachments.read();
        let mut out = Vec::new();
        if let (Ok(p_map), Ok(ga_map)) = (policies, attachments) {
            if let Some(ids) = ga_map.get(group) {
                for id in ids {
                    if let Some(p) = p_map.get(id) {
                        out.push(p.clone());
                    }
                }
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Delete synthetic policies produced by SQL GRANT translation.
    /// REVOKE uses this to keep the IAM lane and the legacy grant table
    /// in lock-step.
    pub fn delete_synthetic_grant_policies(
        &self,
        principal: &GrantPrincipal,
        resource: &Resource,
        actions: &[Action],
    ) -> usize {
        let attached = match principal {
            GrantPrincipal::User(uid) => self
                .user_attachments
                .read()
                .ok()
                .and_then(|m| m.get(uid).cloned())
                .unwrap_or_default(),
            GrantPrincipal::Group(group) => self
                .group_attachments
                .read()
                .ok()
                .and_then(|m| m.get(group).cloned())
                .unwrap_or_default(),
            GrantPrincipal::Public => self
                .group_attachments
                .read()
                .ok()
                .and_then(|m| m.get(PUBLIC_IAM_GROUP).cloned())
                .unwrap_or_default(),
        };
        if attached.is_empty() {
            return 0;
        }

        let mut delete_ids = Vec::new();
        if let Ok(policies) = self.policies.read() {
            for id in attached {
                let Some(policy) = policies.get(&id) else {
                    continue;
                };
                if !policy.id.starts_with("_grant_") {
                    continue;
                }
                if synthetic_grant_matches(policy, resource, actions) {
                    delete_ids.push(policy.id.clone());
                }
            }
        }

        let mut deleted = 0usize;
        for id in delete_ids {
            if self.delete_policy(&id).is_ok() {
                deleted += 1;
            }
        }
        deleted
    }

    /// Apply the `REDDB_POLICY_BREAK_GLASS` recovery path: install or
    /// refresh the unlock policy, ensure the synthetic platform-owner
    /// user record exists, rebind the unlock policy to it, and emit a
    /// `policy.break_glass` audit event. Idempotent across reboots.
    ///
    /// `boot_ts_ms` is recorded in the audit event so operators can
    /// correlate the recovery with the boot transcript.
    pub fn apply_policy_break_glass(&self, boot_ts_ms: u128) -> Result<(), AuthError> {
        use crate::auth::self_lock_guard::{
            break_glass_audit_fields, unlock_policy, PLATFORM_OWNER_UNLOCK_POLICY_ID,
            PLATFORM_OWNER_USERNAME,
        };

        // (1) Install or refresh the unlock policy. `put_policy_internal`
        // bypasses the synthetic-namespace guard, lets us replace any
        // tampered persisted copy, and persists to vault on success.
        let policy = unlock_policy();
        self.put_policy_internal(policy)?;

        // (2) Ensure the synthetic platform-owner user record exists.
        // We bypass the public user-creation API to keep this user
        // immutable, disabled, and absent from operator listings even
        // if our filter ever regresses.
        let owner_id = UserId::platform(PLATFORM_OWNER_USERNAME);
        {
            let mut users = self.users.write().unwrap_or_else(|e| e.into_inner());
            users.entry(owner_id.clone()).or_insert_with(|| User {
                username: PLATFORM_OWNER_USERNAME.to_string(),
                tenant_id: None,
                // No usable password; even if `authenticate` were
                // wired to accept the principal it would fail
                // verification. Defence in depth: `enabled = false`
                // and the username filter both block it.
                password_hash: String::new(),
                scram_verifier: None,
                role: Role::Admin,
                api_keys: Vec::new(),
                created_at: boot_ts_ms,
                updated_at: boot_ts_ms,
                enabled: false,
            });
        }

        // (3) Rebind the unlock policy to the synthetic principal.
        // `attach_policy` is idempotent (the helper checks for an
        // existing binding before appending).
        self.attach_policy(
            PrincipalRef::User(owner_id.clone()),
            PLATFORM_OWNER_UNLOCK_POLICY_ID,
        )?;

        // (4) Emit a loud audit event. If the control-event ledger
        // isn't configured (e.g. unit tests without runtime wiring),
        // log a warn and proceed — recovery must not depend on audit
        // wiring being live.
        let fields_raw = break_glass_audit_fields(boot_ts_ms);
        let mut fields: HashMap<String, Sensitivity> = HashMap::new();
        for (k, v) in fields_raw {
            fields.insert(k, Sensitivity::raw(v));
        }
        if let Some(configured) = self.configured_control_events() {
            let ctx = ControlEventCtx {
                actor: crate::runtime::control_events::ActorRef::System("break_glass"),
                scope: None,
                request_id: None,
                trace_id: None,
            };
            let event = ControlEvent {
                kind: EventKind::PolicyBreakGlass,
                outcome: Outcome::Allowed,
                action: std::borrow::Cow::Borrowed("policy:break_glass"),
                resource: Some(format!("policy:{PLATFORM_OWNER_UNLOCK_POLICY_ID}")),
                reason: Some("REDDB_POLICY_BREAK_GLASS=1 at boot".into()),
                matched_policy_id: None,
                fields,
            };
            let _ = configured.ledger.emit(&ctx, event);
        }
        tracing::warn!(
            policy_id = PLATFORM_OWNER_UNLOCK_POLICY_ID,
            principal = PLATFORM_OWNER_USERNAME,
            boot_ts_ms = %boot_ts_ms,
            "REDDB_POLICY_BREAK_GLASS=1 — installed/refreshed platform-owner unlock policy and rebound to synthetic principal"
        );
        Ok(())
    }

    /// Attach a policy to a user or group. Returns an error if the
    /// policy id doesn't exist.
    pub fn attach_policy(&self, principal: PrincipalRef, policy_id: &str) -> Result<(), AuthError> {
        if !self
            .policies
            .read()
            .map(|m| m.contains_key(policy_id))
            .unwrap_or(false)
        {
            return Err(AuthError::Forbidden(format!(
                "policy `{policy_id}` not found"
            )));
        }
        match &principal {
            PrincipalRef::User(uid) => {
                let mut ua = self
                    .user_attachments
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                let list = ua.entry(uid.clone()).or_default();
                if !list.iter().any(|p| p == policy_id) {
                    list.push(policy_id.to_string());
                }
                drop(ua);
                self.invalidate_iam_cache(Some(uid));
            }
            PrincipalRef::Group(g) => {
                let mut ga = self
                    .group_attachments
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                let list = ga.entry(g.clone()).or_default();
                if !list.iter().any(|p| p == policy_id) {
                    list.push(policy_id.to_string());
                }
                drop(ga);
                self.invalidate_iam_cache(None);
            }
        }
        self.persist_iam_to_kv();
        Ok(())
    }

    /// Remove a policy attachment from a user or group.
    pub fn detach_policy(&self, principal: PrincipalRef, policy_id: &str) -> Result<(), AuthError> {
        match &principal {
            PrincipalRef::User(uid) => {
                if let Ok(mut ua) = self.user_attachments.write() {
                    if let Some(list) = ua.get_mut(uid) {
                        list.retain(|p| p != policy_id);
                        if list.is_empty() {
                            ua.remove(uid);
                        }
                    }
                }
                self.invalidate_iam_cache(Some(uid));
            }
            PrincipalRef::Group(g) => {
                if let Ok(mut ga) = self.group_attachments.write() {
                    if let Some(list) = ga.get_mut(g) {
                        list.retain(|p| p != policy_id);
                        if list.is_empty() {
                            ga.remove(g);
                        }
                    }
                }
                self.invalidate_iam_cache(None);
            }
        }
        self.persist_iam_to_kv();
        Ok(())
    }

    /// Resolve the ordered list of effective policies for a user:
    /// group attachments first (least specific), then user
    /// attachments (most specific). Cached per user.
    pub fn effective_policies(&self, user: &UserId) -> Vec<Arc<Policy>> {
        if let Ok(cache) = self.iam_effective_cache.read() {
            if let Some(hit) = cache.get(user) {
                return hit.clone();
            }
        }
        let policies = self.policies.read();
        let user_attachments = self.user_attachments.read();
        let group_attachments = self.group_attachments.read();
        let mut groups = self
            .user_attributes
            .read()
            .ok()
            .and_then(|m| m.get(user).map(|attrs| attrs.groups.clone()))
            .unwrap_or_default();
        groups.insert(0, PUBLIC_IAM_GROUP.to_string());
        let mut out: Vec<Arc<Policy>> = Vec::new();
        if let (Ok(p_map), Ok(ua_map), Ok(ga_map)) = (policies, user_attachments, group_attachments)
        {
            for group in groups {
                if let Some(ids) = ga_map.get(&group) {
                    for id in ids {
                        if let Some(p) = p_map.get(id) {
                            out.push(p.clone());
                        }
                    }
                }
            }
            if let Some(ids) = ua_map.get(user) {
                for id in ids {
                    if let Some(p) = p_map.get(id) {
                        out.push(p.clone());
                    }
                }
            }
        }
        if let Ok(mut cache) = self.iam_effective_cache.write() {
            cache.insert(user.clone(), out.clone());
        }
        out
    }

    /// Run the policy simulator for a principal. Synthesises an
    /// `EvalContext` from the user record + caller-supplied extras.
    pub fn simulate(
        &self,
        principal: &UserId,
        action: &str,
        resource: &ResourceRef,
        ctx_extras: SimCtx,
    ) -> SimulationOutcome {
        let user_role = self
            .users
            .read()
            .ok()
            .and_then(|u| u.get(principal).map(|u| u.role));
        let principal_is_admin_role = user_role == Some(Role::Admin);
        let now = ctx_extras.now_ms.unwrap_or_else(now_ms);
        let ctx = EvalContext {
            principal_tenant: principal.tenant.clone(),
            current_tenant: ctx_extras.current_tenant,
            peer_ip: ctx_extras.peer_ip,
            mfa_present: ctx_extras.mfa_present,
            now_ms: now,
            principal_is_admin_role,
            principal_is_platform_scoped: principal.tenant.is_none(),
        };
        let pols = self.effective_policies(principal);
        let refs: Vec<&Policy> = pols.iter().map(|p| p.as_ref()).collect();
        iam_policies::simulate(&refs, action, resource, &ctx)
    }

    /// Production hot-path policy evaluation. Returns `true` on Allow
    /// / AdminBypass, `false` on Deny / DefaultDeny.
    ///
    /// This entry point is **strict**: it never consults the
    /// [`PolicyEnforcementMode`] fallback. Governance APIs that gate
    /// admin-tier mutations (managed-config writes, registry
    /// supersedes, managed-policy lifecycle) call this so they cannot
    /// accidentally pick up the lenient `LegacyRbac` posture. Runtime
    /// hot-path callers that should respect the mode call
    /// [`AuthStore::check_policy_authz_with_role`] instead.
    pub fn check_policy_authz(
        &self,
        principal: &UserId,
        action: &str,
        resource: &ResourceRef,
        ctx: &EvalContext,
    ) -> bool {
        let pols = self.effective_policies(principal);
        let refs: Vec<&Policy> = pols.iter().map(|p| p.as_ref()).collect();
        let decision = iam_policies::evaluate(&refs, action, resource, ctx);
        matches!(
            decision,
            iam_policies::Decision::Allow { .. } | iam_policies::Decision::AdminBypass
        )
    }

    /// Mode-aware policy evaluation for runtime SQL/HTTP/wire surfaces.
    ///
    /// Returns `true` on `Allow`/`AdminBypass`, `false` on an explicit
    /// `Deny`. On `DefaultDeny` (no statement matched) the result
    /// depends on the active [`PolicyEnforcementMode`]:
    ///
    /// * `LegacyRbac` — defer to
    ///   [`legacy_rbac_decision`][super::enforcement_mode::legacy_rbac_decision]
    ///   using the caller's `role`. This preserves the pre-#712
    ///   behaviour where a principal with no attached policy is gated
    ///   only by their role.
    /// * `PolicyOnly` — return `false`. A principal needs an explicit
    ///   matching `Allow` to be authorized.
    ///
    /// An explicit `Deny` always wins, irrespective of mode and role.
    pub fn check_policy_authz_with_role(
        &self,
        principal: &UserId,
        action: &str,
        resource: &ResourceRef,
        ctx: &EvalContext,
        role: Role,
    ) -> bool {
        let pols = self.effective_policies(principal);
        let refs: Vec<&Policy> = pols.iter().map(|p| p.as_ref()).collect();
        let decision = iam_policies::evaluate(&refs, action, resource, ctx);
        match decision {
            iam_policies::Decision::Allow { .. } | iam_policies::Decision::AdminBypass => true,
            iam_policies::Decision::Deny { .. } => false,
            iam_policies::Decision::DefaultDeny => match self.enforcement_mode() {
                PolicyEnforcementMode::LegacyRbac => legacy_rbac_decision(role, action),
                PolicyEnforcementMode::PolicyOnly => false,
            },
        }
    }

    // -----------------------------------------------------------------
    // PolicyEnforcementMode (#712)
    // -----------------------------------------------------------------

    /// Read the active enforcement mode. Cheap (a single `RwLock` read);
    /// safe to call on the hot path.
    pub fn enforcement_mode(&self) -> PolicyEnforcementMode {
        *self
            .enforcement_mode
            .read()
            .unwrap_or_else(|e| e.into_inner())
    }

    /// Overwrite the active enforcement mode and persist it to the
    /// vault KV so the new value survives a restart. Returns the
    /// previous value so callers logging a transition (e.g. the
    /// boot-time loader, the S5B migration command) can record the
    /// before/after.
    pub fn set_enforcement_mode(&self, mode: PolicyEnforcementMode) -> PolicyEnforcementMode {
        let prev = {
            let mut guard = self
                .enforcement_mode
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let prev = *guard;
            *guard = mode;
            prev
        };
        self.vault_kv_set(
            "red.config.policy.enforcement_mode".to_string(),
            mode.as_str().to_string(),
        );
        prev
    }

    /// Claim the "emit the one-time legacy-RBAC boot warning" token.
    /// Returns `true` on the first call after construction (or after a
    /// process restart) **iff** the active mode is `LegacyRbac`;
    /// returns `false` on every subsequent call so the boot path can
    /// guarantee the warning is logged at most once per boot.
    pub fn take_legacy_rbac_warn_once(&self) -> bool {
        if self.enforcement_mode() != PolicyEnforcementMode::LegacyRbac {
            return false;
        }
        !self
            .legacy_rbac_boot_warn_emitted
            .swap(true, Ordering::AcqRel)
    }

    /// Evaluate a resolved table projection through the column policy
    /// gate. Query paths should pass already-resolved column names; this
    /// helper intentionally does not parse SQL projection syntax.
    pub fn check_column_projection_authz(
        &self,
        principal: &UserId,
        request: &ColumnAccessRequest,
        ctx: &EvalContext,
    ) -> ColumnPolicyOutcome {
        let pols = self.effective_policies(principal);
        let refs: Vec<&Policy> = pols.iter().map(|p| p.as_ref()).collect();
        ColumnPolicyGate::new(&refs).evaluate(request, ctx)
    }

    fn invalidate_iam_cache(&self, uid: Option<&UserId>) {
        if let Ok(mut cache) = self.iam_effective_cache.write() {
            match uid {
                Some(u) => {
                    cache.remove(u);
                }
                None => cache.clear(),
            }
        }
    }

    /// Drop every effective-policy cache entry. Called from execution
    /// paths that mutate policies/attachments without knowing which
    /// users will be affected.
    pub fn invalidate_all_iam_cache(&self) {
        self.invalidate_iam_cache(None);
    }

    // -----------------------------------------------------------------
    // IAM persistence — vault_kv backed under `red.iam.*` keys
    // -----------------------------------------------------------------

    /// Reload IAM state (policies + attachments) from the vault KV.
    /// Replaces the legacy `rehydrate_acl` reader — pre-1.0 we drop
    /// the old `red.acl.*` blob format entirely.
    pub fn rehydrate_iam(&self) {
        let mut enabled = self
            .vault_kv_get("red.iam.enabled")
            .map(|v| v == "true")
            .unwrap_or(false);
        // Policies — single JSON object keyed by id.
        if let Some(blob) = self.vault_kv_get("red.iam.policies") {
            if let Ok(val) = crate::serde_json::from_str::<crate::serde_json::Value>(&blob) {
                if let Some(obj) = val.as_object() {
                    let mut map = HashMap::new();
                    for (id, body) in obj.iter() {
                        let s = body.to_string_compact();
                        if let Ok(p) = Policy::from_json_str(&s) {
                            map.insert(id.clone(), Arc::new(p));
                        }
                    }
                    if !map.is_empty() {
                        enabled = true;
                    }
                    *self.policies.write().unwrap_or_else(|e| e.into_inner()) = map;
                }
            }
        }
        // User attachments.
        if let Some(blob) = self.vault_kv_get("red.iam.attachments.users") {
            if let Ok(val) = crate::serde_json::from_str::<crate::serde_json::Value>(&blob) {
                if let Some(obj) = val.as_object() {
                    let mut map: HashMap<UserId, Vec<String>> = HashMap::new();
                    for (encoded_uid, ids_v) in obj.iter() {
                        let Some(uid) = decode_uid(encoded_uid) else {
                            continue;
                        };
                        if let Some(arr) = ids_v.as_array() {
                            let ids: Vec<String> = arr
                                .iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect();
                            map.insert(uid, ids);
                        }
                    }
                    *self
                        .user_attachments
                        .write()
                        .unwrap_or_else(|e| e.into_inner()) = map;
                }
            }
        }
        // Group attachments.
        if let Some(blob) = self.vault_kv_get("red.iam.attachments.groups") {
            if let Ok(val) = crate::serde_json::from_str::<crate::serde_json::Value>(&blob) {
                if let Some(obj) = val.as_object() {
                    let mut map: HashMap<String, Vec<String>> = HashMap::new();
                    for (g, ids_v) in obj.iter() {
                        if let Some(arr) = ids_v.as_array() {
                            let ids: Vec<String> = arr
                                .iter()
                                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                                .collect();
                            map.insert(g.clone(), ids);
                        }
                    }
                    *self
                        .group_attachments
                        .write()
                        .unwrap_or_else(|e| e.into_inner()) = map;
                }
            }
        }
        self.iam_authorization_enabled
            .store(enabled, Ordering::Release);
        self.invalidate_iam_cache(None);

        // #712 / S5A: load persisted enforcement mode, if any. Absence
        // of the key means an existing install upgrading past #712 —
        // stay on the lenient default written at construction time.
        // Fresh installs receive `PolicyOnly` via the bootstrap path
        // (`mark_bootstrap_complete`) before the first restart, so this
        // hydrate path picks it up on every subsequent boot.
        if let Some(stored) = self.vault_kv_get("red.config.policy.enforcement_mode") {
            if let Some(mode) = PolicyEnforcementMode::parse(&stored) {
                *self
                    .enforcement_mode
                    .write()
                    .unwrap_or_else(|e| e.into_inner()) = mode;
            }
        }

        // #712 / S5A: legacy_rbac is a transitional posture; emit one
        // boot-time warning so operators see the migration nudge in
        // their logs. The flag inside take_legacy_rbac_warn_once
        // guarantees we don't spam reload cycles.
        if self.take_legacy_rbac_warn_once() {
            tracing::warn!(
                target: "reddb::auth::enforcement_mode",
                mode = "legacy_rbac",
                hard_version = crate::auth::enforcement_mode::POLICY_ONLY_HARD_VERSION,
                "policy enforcement_mode=legacy_rbac (transitional). Run \
                 `MIGRATE POLICY MODE TO 'policy_only'` (next slice S5B) \
                 before {} to flip this install over to the strict posture.",
                crate::auth::enforcement_mode::POLICY_ONLY_HARD_VERSION,
            );
        }
    }

    /// Snapshot policies + attachments into the vault KV. Called
    /// after every mutation.
    fn persist_iam_to_kv(&self) {
        let enabled = if self.iam_authorization_enabled() {
            "true"
        } else {
            "false"
        };
        self.vault_kv_set("red.iam.enabled".to_string(), enabled.to_string());

        // Policies: `{ "<id>": <policy_json>, ... }`
        let policies_obj = {
            let map = self.policies.read().unwrap_or_else(|e| e.into_inner());
            let mut obj = crate::serde_json::Map::new();
            for (id, p) in map.iter() {
                let s = p.to_json_string();
                if let Ok(v) = crate::serde_json::from_str::<crate::serde_json::Value>(&s) {
                    obj.insert(id.clone(), v);
                }
            }
            crate::serde_json::Value::Object(obj).to_string_compact()
        };
        self.vault_kv_set("red.iam.policies".to_string(), policies_obj);

        // User attachments: `{ "<encoded_uid>": [ "<policy_id>", ... ], ... }`
        let users_obj = {
            let map = self
                .user_attachments
                .read()
                .unwrap_or_else(|e| e.into_inner());
            let mut obj = crate::serde_json::Map::new();
            for (uid, ids) in map.iter() {
                let arr = crate::serde_json::Value::Array(
                    ids.iter()
                        .map(|s| crate::serde_json::Value::String(s.clone()))
                        .collect(),
                );
                obj.insert(encode_uid(uid), arr);
            }
            crate::serde_json::Value::Object(obj).to_string_compact()
        };
        self.vault_kv_set("red.iam.attachments.users".to_string(), users_obj);

        // Group attachments.
        let groups_obj = {
            let map = self
                .group_attachments
                .read()
                .unwrap_or_else(|e| e.into_inner());
            let mut obj = crate::serde_json::Map::new();
            for (g, ids) in map.iter() {
                let arr = crate::serde_json::Value::Array(
                    ids.iter()
                        .map(|s| crate::serde_json::Value::String(s.clone()))
                        .collect(),
                );
                obj.insert(g.clone(), arr);
            }
            crate::serde_json::Value::Object(obj).to_string_compact()
        };
        self.vault_kv_set("red.iam.attachments.groups".to_string(), groups_obj);
    }
}

fn synthetic_grant_matches(policy: &Policy, resource: &Resource, actions: &[Action]) -> bool {
    policy.statements.iter().any(|st| {
        st.effect == crate::auth::policies::Effect::Allow
            && st.condition.is_none()
            && grant_actions_overlap(&st.actions, actions)
            && grant_resource_matches(&st.resources, resource)
    })
}

fn grant_actions_overlap(
    patterns: &[crate::auth::policies::ActionPattern],
    actions: &[Action],
) -> bool {
    if actions.contains(&Action::All) {
        return true;
    }
    patterns.iter().any(|pat| match pat {
        crate::auth::policies::ActionPattern::Wildcard => true,
        crate::auth::policies::ActionPattern::Exact(s) => {
            actions.iter().any(|a| s.eq_ignore_ascii_case(a.as_str()))
        }
        crate::auth::policies::ActionPattern::Prefix(_) => false,
    })
}

fn grant_resource_matches(
    patterns: &[crate::auth::policies::ResourcePattern],
    resource: &Resource,
) -> bool {
    let expected = grant_resource_pattern(resource);
    patterns.iter().any(|pat| pat == &expected)
}

fn grant_resource_pattern(resource: &Resource) -> crate::auth::policies::ResourcePattern {
    use crate::auth::policies::ResourcePattern;

    match resource {
        Resource::Database => ResourcePattern::Glob("table:*".to_string()),
        Resource::Schema(s) => ResourcePattern::Glob(format!("table:{s}.*")),
        Resource::Table { schema, table } => ResourcePattern::Exact {
            kind: "table".to_string(),
            name: match schema {
                Some(s) => format!("{s}.{table}"),
                None => table.clone(),
            },
        },
        Resource::Function { schema, name } => ResourcePattern::Exact {
            kind: "function".to_string(),
            name: match schema {
                Some(s) => format!("{s}.{name}"),
                None => name.clone(),
            },
        },
    }
}

// ===========================================================================
// ACL serialization helpers — line-oriented, human-readable so an
// operator inspecting the vault dump can spot misconfigurations.
//
// Format (one record per line):
//   GRANT|<resource>|<actions_csv>|<with_grant_option>|<tenant_or_*>|<granted_by>|<granted_at>
//   ATTR|<valid_until>|<connection_limit>|<search_path>
//
// Resources are encoded as:
//   db                          → Database
//   schema:<name>               → Schema(name)
//   table:<schema_or_*>:<name>  → Table { schema, table }
//   func:<schema_or_*>:<name>   → Function { schema, name }
// ===========================================================================

fn encode_uid(uid: &UserId) -> String {
    match &uid.tenant {
        Some(t) => format!("{}/{}", t, uid.username),
        None => format!("*/{}", uid.username),
    }
}

fn decode_uid(s: &str) -> Option<UserId> {
    let (tenant, username) = s.split_once('/')?;
    Some(if tenant == "*" {
        UserId::platform(username)
    } else {
        UserId::scoped(tenant, username)
    })
}

fn encode_resource(r: &Resource) -> String {
    match r {
        Resource::Database => "db".into(),
        Resource::Schema(s) => format!("schema:{}", s),
        Resource::Table { schema, table } => {
            format!("table:{}:{}", schema.as_deref().unwrap_or("*"), table)
        }
        Resource::Function { schema, name } => {
            format!("func:{}:{}", schema.as_deref().unwrap_or("*"), name)
        }
    }
}

fn decode_resource(s: &str) -> Option<Resource> {
    if s == "db" {
        return Some(Resource::Database);
    }
    if let Some(rest) = s.strip_prefix("schema:") {
        return Some(Resource::Schema(rest.to_string()));
    }
    if let Some(rest) = s.strip_prefix("table:") {
        let (schema, table) = rest.split_once(':')?;
        return Some(Resource::Table {
            schema: if schema == "*" {
                None
            } else {
                Some(schema.to_string())
            },
            table: table.to_string(),
        });
    }
    if let Some(rest) = s.strip_prefix("func:") {
        let (schema, name) = rest.split_once(':')?;
        return Some(Resource::Function {
            schema: if schema == "*" {
                None
            } else {
                Some(schema.to_string())
            },
            name: name.to_string(),
        });
    }
    None
}

fn encode_grants_blob(grants: &[Grant]) -> String {
    let mut out = String::new();
    for g in grants {
        let actions: Vec<&str> = g.actions.iter().map(|a| a.as_str()).collect();
        out.push_str(&format!(
            "GRANT|{}|{}|{}|{}|{}|{}\n",
            encode_resource(&g.resource),
            actions.join(","),
            g.with_grant_option,
            g.tenant.as_deref().unwrap_or("*"),
            g.granted_by,
            g.granted_at,
        ));
    }
    out
}

fn decode_grants_blob(s: &str) -> Option<Vec<Grant>> {
    let mut out = Vec::new();
    for line in s.lines() {
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() != 7 || parts[0] != "GRANT" {
            return None;
        }
        let resource = decode_resource(parts[1])?;
        let mut actions = std::collections::BTreeSet::new();
        for token in parts[2].split(',') {
            if let Some(a) = Action::from_keyword(token) {
                actions.insert(a);
            }
        }
        let with_grant_option = parts[3] == "true";
        let tenant = if parts[4] == "*" {
            None
        } else {
            Some(parts[4].to_string())
        };
        let granted_by = parts[5].to_string();
        let granted_at: u128 = parts[6].parse().unwrap_or(0);
        out.push(Grant {
            // Principal field is reconstructed by the loader from the
            // storage-key prefix; default to `Public` here.
            principal: GrantPrincipal::Public,
            resource,
            actions,
            with_grant_option,
            granted_by,
            granted_at,
            tenant,
            columns: None,
        });
    }
    Some(out)
}

fn encode_attrs_blob(a: &UserAttributes) -> String {
    let valid = a
        .valid_until
        .map(|t| t.to_string())
        .unwrap_or_else(|| "*".into());
    let limit = a
        .connection_limit
        .map(|l| l.to_string())
        .unwrap_or_else(|| "*".into());
    let path = a.search_path.clone().unwrap_or_else(|| "*".into());
    let groups = if a.groups.is_empty() {
        "*".to_string()
    } else {
        a.groups.join(",")
    };
    format!("ATTR|{}|{}|{}|{}\n", valid, limit, path, groups)
}

fn decode_attrs_blob(s: &str) -> Option<UserAttributes> {
    let line = s.lines().next()?;
    let parts: Vec<&str> = line.split('|').collect();
    if !(parts.len() == 4 || parts.len() == 5) || parts[0] != "ATTR" {
        return None;
    }
    let groups = if parts.get(4).copied().unwrap_or("*") == "*" {
        Vec::new()
    } else {
        parts[4]
            .split(',')
            .filter(|g| !g.is_empty())
            .map(|g| g.to_string())
            .collect()
    };
    Some(UserAttributes {
        valid_until: if parts[1] == "*" {
            None
        } else {
            parts[1].parse().ok()
        },
        connection_limit: if parts[2] == "*" {
            None
        } else {
            parts[2].parse().ok()
        },
        search_path: if parts[3] == "*" {
            None
        } else {
            Some(parts[3].to_string())
        },
        groups,
    })
}

// ===========================================================================
// Password hashing
// ===========================================================================

/// Derive a SCRAM-SHA-256 verifier for a fresh user / password
/// rotation. Salt is 16 random bytes; iter is the engine default
/// (`scram::DEFAULT_ITER`). Stored alongside the Argon2 password
/// hash so HTTP login + v2 SCRAM can both authenticate the same
/// user.
fn make_scram_verifier(password: &str) -> crate::auth::scram::ScramVerifier {
    let salt = random_bytes(16);
    crate::auth::scram::ScramVerifier::from_password(
        password,
        salt,
        crate::auth::scram::DEFAULT_ITER,
    )
}

/// Hash a password using Argon2id.
///
/// Format: `argon2id$<salt_hex>$<hash_hex>`
pub(crate) fn hash_password(password: &str) -> String {
    let salt = random_bytes(16);
    let params = auth_argon2_params();
    let hash = derive_key(password.as_bytes(), &salt, &params);
    format!("argon2id${}${}", hex::encode(&salt), hex::encode(&hash))
}

/// Verify a password against a stored `argon2id$<salt>$<hash>` string.
pub(crate) fn verify_password(password: &str, stored_hash: &str) -> bool {
    let parts: Vec<&str> = stored_hash.splitn(3, '$').collect();
    if parts.len() != 3 || parts[0] != "argon2id" {
        return false;
    }

    let salt = match hex::decode(parts[1]) {
        Ok(s) => s,
        Err(_) => return false,
    };

    let expected_hash = match hex::decode(parts[2]) {
        Ok(h) => h,
        Err(_) => return false,
    };

    let params = auth_argon2_params();
    let computed = derive_key(password.as_bytes(), &salt, &params);
    constant_time_eq(&computed, &expected_hash)
}

/// Constant-time byte comparison to avoid timing side-channels.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

// ===========================================================================
// Token generation
// ===========================================================================

fn generate_session_token() -> String {
    format!("rs_{}", hex::encode(random_bytes(32)))
}

fn generate_api_key() -> String {
    format!("rk_{}", hex::encode(random_bytes(32)))
}

/// Generate `n` random bytes and return as a hex string.
fn random_hex(n: usize) -> String {
    hex::encode(random_bytes(n))
}

/// Generate `n` cryptographically random bytes using the OS CSPRNG,
/// then mix with SHA-256 for domain separation.
pub(crate) fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n.max(32)];
    if os_random::fill_bytes(&mut buf).is_err() {
        // Fallback: use system time and pointers as entropy (best-effort).
        let seed = now_ms().to_le_bytes();
        for (i, byte) in buf.iter_mut().enumerate() {
            *byte = seed[i % seed.len()] ^ (i as u8);
        }
    }
    // SHA-256 mix to ensure uniform distribution.
    let digest = sha256(&buf);
    if n <= 32 {
        digest[..n].to_vec()
    } else {
        // Chain SHA-256 for longer outputs (unusual but supported).
        let mut out = Vec::with_capacity(n);
        let mut prev = digest;
        while out.len() < n {
            out.extend_from_slice(&prev[..std::cmp::min(32, n - out.len())]);
            prev = sha256(&prev);
        }
        out
    }
}

// ===========================================================================
// Helpers
// ===========================================================================

fn lock_err<T>(_: T) -> AuthError {
    AuthError::Internal("lock poisoned".to_string())
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> AuthConfig {
        AuthConfig {
            enabled: true,
            session_ttl_secs: 60,
            require_auth: true,
            auto_encrypt_storage: false,
            vault_enabled: false,
            cert: Default::default(),
            oauth: Default::default(),
        }
    }

    #[test]
    fn test_create_and_list_users() {
        let store = AuthStore::new(test_config());
        store.create_user("alice", "pass1", Role::Admin).unwrap();
        store.create_user("bob", "pass2", Role::Read).unwrap();

        let users = store.list_users();
        assert_eq!(users.len(), 2);
        // Password hashes should be redacted.
        for u in &users {
            assert!(u.password_hash.is_empty());
        }
    }

    #[test]
    fn test_create_duplicate_user() {
        let store = AuthStore::new(test_config());
        store.create_user("alice", "pass", Role::Admin).unwrap();
        let err = store.create_user("alice", "pass2", Role::Read).unwrap_err();
        assert!(matches!(err, AuthError::UserExists(_)));
    }

    #[test]
    fn test_authenticate_and_validate() {
        let store = AuthStore::new(test_config());
        store.create_user("alice", "secret", Role::Write).unwrap();

        let session = store.authenticate("alice", "secret").unwrap();
        assert!(session.token.starts_with("rs_"));

        let (username, role) = store.validate_token(&session.token).unwrap();
        assert_eq!(username, "alice");
        assert_eq!(role, Role::Write);
    }

    #[test]
    fn test_authenticate_wrong_password() {
        let store = AuthStore::new(test_config());
        store.create_user("alice", "secret", Role::Read).unwrap();

        let err = store.authenticate("alice", "wrong").unwrap_err();
        assert!(matches!(err, AuthError::InvalidCredentials));
    }

    #[test]
    fn test_api_key_lifecycle() {
        let store = AuthStore::new(test_config());
        store.create_user("alice", "pass", Role::Admin).unwrap();

        let key = store
            .create_api_key("alice", "ci-token", Role::Write)
            .unwrap();
        assert!(key.key.starts_with("rk_"));

        let (username, role) = store.validate_token(&key.key).unwrap();
        assert_eq!(username, "alice");
        assert_eq!(role, Role::Write);

        store.revoke_api_key(&key.key).unwrap();
        assert!(store.validate_token(&key.key).is_none());
    }

    #[test]
    fn test_api_key_role_exceeded() {
        let store = AuthStore::new(test_config());
        store.create_user("bob", "pass", Role::Read).unwrap();

        let err = store
            .create_api_key("bob", "escalate", Role::Admin)
            .unwrap_err();
        assert!(matches!(err, AuthError::RoleExceeded { .. }));
    }

    #[test]
    fn test_change_password() {
        let store = AuthStore::new(test_config());
        store.create_user("alice", "old", Role::Write).unwrap();

        store.change_password("alice", "old", "new").unwrap();

        // Old password should fail.
        assert!(store.authenticate("alice", "old").is_err());
        // New password should succeed.
        assert!(store.authenticate("alice", "new").is_ok());
    }

    #[test]
    fn test_change_role() {
        let store = AuthStore::new(test_config());
        store.create_user("alice", "pass", Role::Admin).unwrap();
        store.create_api_key("alice", "key1", Role::Admin).unwrap();

        store.change_role("alice", Role::Read).unwrap();

        // User's role should be Read now.
        let users = store.list_users();
        let alice = users.iter().find(|u| u.username == "alice").unwrap();
        assert_eq!(alice.role, Role::Read);

        // API keys should have been downgraded.
        assert_eq!(alice.api_keys[0].role, Role::Read);
    }

    #[test]
    fn test_admin_user_mutations_are_policy_controlled_not_flag_guarded() {
        let store = AuthStore::new(test_config());
        store
            .create_admin_user("system", "pass", Role::Admin, None)
            .unwrap();

        let uid = UserId::platform("system");
        store.change_password("system", "pass", "new").unwrap();
        store.change_role("system", Role::Read).unwrap();
        store.set_user_enabled(&uid, false).unwrap();

        let key = store
            .create_api_key("system", "rotation", Role::Read)
            .unwrap();
        assert!(store.validate_token(&key.key).is_some());
        store.revoke_api_key(&key.key).unwrap();
        assert!(store.validate_token(&key.key).is_none());
        store.delete_user("system").unwrap();
    }

    #[test]
    fn test_user_lifecycle_deny_policy_blocks_cloud_admin_deletion() {
        use crate::auth::policies::Policy;

        let store = AuthStore::new(test_config());
        store
            .create_user("cloud-admin", "pass", Role::Admin)
            .unwrap();
        store
            .create_user("customer-admin", "pass", Role::Admin)
            .unwrap();

        store
            .put_policy(
                Policy::from_json_str(
                    r#"{
                        "id": "customer-admin-allow-all",
                        "version": 1,
                        "statements": [{
                            "effect": "allow",
                            "actions": ["*"],
                            "resources": ["*"]
                        }]
                    }"#,
                )
                .unwrap(),
            )
            .unwrap();
        store
            .put_policy(
                Policy::from_json_str(
                    r#"{
                        "id": "cloud-admin-protection",
                        "version": 1,
                        "statements": [{
                            "effect": "deny",
                            "actions": [
                                "user:delete",
                                "user:disable",
                                "user:password:change",
                                "user:role:update"
                            ],
                            "resources": ["user:cloud-admin"]
                        }]
                    }"#,
                )
                .unwrap(),
            )
            .unwrap();
        let customer = UserId::platform("customer-admin");
        store
            .attach_policy(
                PrincipalRef::User(customer.clone()),
                "customer-admin-allow-all",
            )
            .unwrap();
        store
            .attach_policy(
                PrincipalRef::User(customer.clone()),
                "cloud-admin-protection",
            )
            .unwrap();

        let cloud_admin = UserId::platform("cloud-admin");
        assert!(!store.check_user_lifecycle_authz(
            &customer,
            Role::Admin,
            "user:delete",
            &cloud_admin,
        ));
        assert!(!store.check_user_lifecycle_authz(
            &customer,
            Role::Admin,
            "user:disable",
            &cloud_admin,
        ));
        assert!(!store.check_user_lifecycle_authz(
            &customer,
            Role::Admin,
            "user:password:change",
            &cloud_admin,
        ));
        assert!(!store.check_user_lifecycle_authz(
            &customer,
            Role::Admin,
            "user:role:update",
            &cloud_admin,
        ));

        let another_user = UserId::platform("someone-else");
        assert!(store.check_user_lifecycle_authz(
            &customer,
            Role::Admin,
            "user:delete",
            &another_user,
        ));
    }

    #[test]
    fn test_regular_user_mutations_still_work() {
        let store = AuthStore::new(test_config());
        store.create_user("alice", "old", Role::Admin).unwrap();

        let uid = UserId::platform("alice");
        store.set_user_enabled(&uid, false).unwrap();
        assert!(matches!(
            store.authenticate("alice", "old"),
            Err(AuthError::InvalidCredentials)
        ));

        store.set_user_enabled(&uid, true).unwrap();
        store.change_password("alice", "old", "new").unwrap();
        store.change_role("alice", Role::Read).unwrap();
        store.delete_user("alice").unwrap();
        assert!(matches!(
            store.authenticate("alice", "new"),
            Err(AuthError::InvalidCredentials)
        ));
    }

    #[test]
    fn test_delete_user() {
        let store = AuthStore::new(test_config());
        store.create_user("alice", "pass", Role::Admin).unwrap();
        let key = store.create_api_key("alice", "key1", Role::Read).unwrap();
        let session = store.authenticate("alice", "pass").unwrap();

        store.delete_user("alice").unwrap();

        assert!(store.validate_token(&key.key).is_none());
        assert!(store.validate_token(&session.token).is_none());
        assert!(store.list_users().is_empty());
    }

    #[test]
    fn test_revoke_session() {
        let store = AuthStore::new(test_config());
        store.create_user("alice", "pass", Role::Read).unwrap();
        let session = store.authenticate("alice", "pass").unwrap();

        store.revoke_session(&session.token);
        assert!(store.validate_token(&session.token).is_none());
    }

    #[test]
    fn test_password_hash_format() {
        let hash = hash_password("test");
        assert!(hash.starts_with("argon2id$"));
        let parts: Vec<&str> = hash.splitn(3, '$').collect();
        assert_eq!(parts.len(), 3);
        // Salt is 16 bytes = 32 hex chars.
        assert_eq!(parts[1].len(), 32);
        // Hash is 32 bytes = 64 hex chars.
        assert_eq!(parts[2].len(), 64);
    }

    #[test]
    fn test_constant_time_eq() {
        assert!(constant_time_eq(b"hello", b"hello"));
        assert!(!constant_time_eq(b"hello", b"world"));
        assert!(!constant_time_eq(b"short", b"longer"));
    }

    #[test]
    fn test_bootstrap_seals_permanently() {
        let store = AuthStore::new(test_config());

        assert!(store.needs_bootstrap());
        assert!(!store.is_bootstrapped());

        // First bootstrap succeeds
        let result = store.bootstrap("admin", "secret");
        assert!(result.is_ok());
        let br = result.unwrap();
        assert_eq!(br.user.username, "admin");
        assert_eq!(br.user.role, Role::Admin);
        assert!(br.api_key.key.starts_with("rk_"));
        // No vault configured, so no certificate.
        assert!(br.certificate.is_none());

        // Sealed now
        assert!(!store.needs_bootstrap());
        assert!(store.is_bootstrapped());

        // Second bootstrap fails -- sealed permanently
        let result = store.bootstrap("admin2", "secret2");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("sealed permanently"));

        // Only 1 user exists (the first one)
        assert_eq!(store.list_users().len(), 1);
        assert_eq!(store.list_users()[0].username, "admin");
    }

    #[test]
    fn test_bootstrap_after_manual_user_creation() {
        let store = AuthStore::new(test_config());

        // Create a user manually first
        store.create_user("existing", "pass", Role::Read).unwrap();

        // Bootstrap sees the seal hasn't been set but users exist
        // The atomic seal fires first, then the users check catches it
        assert!(!store.needs_bootstrap()); // users exist → false
    }

    // ---------------------------------------------------------------
    // Tenant scoping
    // ---------------------------------------------------------------

    #[test]
    fn test_same_username_two_tenants_distinct() {
        let store = AuthStore::new(test_config());
        store
            .create_user_in_tenant(Some("acme"), "alice", "pw-acme", Role::Write)
            .unwrap();
        store
            .create_user_in_tenant(Some("globex"), "alice", "pw-globex", Role::Read)
            .unwrap();

        // Two distinct users.
        let users = store.list_users();
        assert_eq!(users.len(), 2);

        // Each verifies its own password under its own tenant.
        assert!(store
            .authenticate_in_tenant(Some("acme"), "alice", "pw-acme")
            .is_ok());
        assert!(store
            .authenticate_in_tenant(Some("globex"), "alice", "pw-globex")
            .is_ok());

        // Cross-tenant credentials are rejected.
        assert!(store
            .authenticate_in_tenant(Some("acme"), "alice", "pw-globex")
            .is_err());
        assert!(store
            .authenticate_in_tenant(Some("globex"), "alice", "pw-acme")
            .is_err());
    }

    #[test]
    fn test_session_carries_tenant() {
        let store = AuthStore::new(test_config());
        store
            .create_user_in_tenant(Some("acme"), "alice", "pw", Role::Admin)
            .unwrap();
        let session = store
            .authenticate_in_tenant(Some("acme"), "alice", "pw")
            .unwrap();
        assert_eq!(session.tenant_id.as_deref(), Some("acme"));

        let (id, role) = store.validate_token_full(&session.token).unwrap();
        assert_eq!(id.tenant.as_deref(), Some("acme"));
        assert_eq!(id.username, "alice");
        assert_eq!(role, Role::Admin);
    }

    #[test]
    fn test_platform_user_has_no_tenant() {
        let store = AuthStore::new(test_config());
        store.create_user("admin", "pw", Role::Admin).unwrap();
        let session = store.authenticate("admin", "pw").unwrap();
        assert!(session.tenant_id.is_none());

        let (id, _) = store.validate_token_full(&session.token).unwrap();
        assert!(id.tenant.is_none());
    }

    #[test]
    fn test_lookup_scram_verifier_global_resolves_platform() {
        let store = AuthStore::new(test_config());
        store.create_user("admin", "pw", Role::Admin).unwrap();
        store
            .create_user_in_tenant(Some("acme"), "admin", "pw", Role::Admin)
            .unwrap();

        // The global helper picks the platform-tenant user only.
        let v = store.lookup_scram_verifier_global("admin");
        assert!(v.is_some());

        // The tenant-scoped user has its own verifier.
        let v_acme = store.lookup_scram_verifier(&UserId::scoped("acme", "admin"));
        assert!(v_acme.is_some());

        // The two verifiers carry independent salts.
        assert_ne!(v.unwrap().salt, v_acme.unwrap().salt);
    }

    #[test]
    fn test_delete_in_tenant_does_not_touch_other_tenant() {
        let store = AuthStore::new(test_config());
        store
            .create_user_in_tenant(Some("acme"), "alice", "pw", Role::Admin)
            .unwrap();
        store
            .create_user_in_tenant(Some("globex"), "alice", "pw", Role::Admin)
            .unwrap();

        store.delete_user_in_tenant(Some("acme"), "alice").unwrap();

        // Globex still alive.
        assert!(store
            .authenticate_in_tenant(Some("globex"), "alice", "pw")
            .is_ok());
        // Acme gone.
        assert!(store
            .authenticate_in_tenant(Some("acme"), "alice", "pw")
            .is_err());
    }

    #[test]
    fn test_user_id_display() {
        assert_eq!(UserId::platform("admin").to_string(), "admin");
        assert_eq!(UserId::scoped("acme", "alice").to_string(), "acme/alice");
    }

    // -----------------------------------------------------------------
    // PolicyEnforcementMode wiring (#712 / S5A)
    // -----------------------------------------------------------------

    fn enforcement_eval_ctx(role: Role) -> EvalContext {
        EvalContext {
            principal_tenant: None,
            current_tenant: None,
            peer_ip: None,
            mfa_present: false,
            now_ms: 1_700_000_000_000,
            principal_is_admin_role: role == Role::Admin,
            principal_is_platform_scoped: true,
        }
    }

    /// Helper: install a single unrelated allow policy so the IAM
    /// path is the authoritative one — without any installed policy
    /// the auth flow short-circuits before we ever consult the mode.
    fn install_unrelated_policy(store: &AuthStore) {
        store
            .put_policy(
                Policy::from_json_str(
                    r#"{"id":"p-unrelated","version":1,"statements":[{"effect":"allow","actions":["select"],"resources":["table:public.other"]}]}"#,
                )
                .unwrap(),
            )
            .unwrap();
    }

    #[test]
    fn enforcement_mode_default_for_new_store_is_legacy_rbac() {
        // Pre-bootstrap construction = "existing install" path.
        // Defaulting to LegacyRbac preserves pre-#712 behaviour on
        // upgrade so an operator that has not yet attached IAM
        // policies does not lose access after the upgrade.
        let store = AuthStore::new(test_config());
        assert_eq!(store.enforcement_mode(), PolicyEnforcementMode::LegacyRbac);
    }

    #[test]
    fn enforcement_mode_set_round_trips_and_returns_previous() {
        let store = AuthStore::new(test_config());
        let prev = store.set_enforcement_mode(PolicyEnforcementMode::PolicyOnly);
        assert_eq!(prev, PolicyEnforcementMode::LegacyRbac);
        assert_eq!(store.enforcement_mode(), PolicyEnforcementMode::PolicyOnly);
        let prev = store.set_enforcement_mode(PolicyEnforcementMode::LegacyRbac);
        assert_eq!(prev, PolicyEnforcementMode::PolicyOnly);
    }

    #[test]
    fn policy_only_mode_treats_no_matching_policy_as_deny() {
        // Acceptance #2: in `policy_only` mode a principal with no
        // matching policy is denied even when the action's role
        // floor would otherwise have allowed it.
        let store = AuthStore::new(test_config());
        store.create_user("alice", "p", Role::Admin).unwrap();
        let uid = UserId::platform("alice");
        install_unrelated_policy(&store);
        store.set_enforcement_mode(PolicyEnforcementMode::PolicyOnly);

        let resource = ResourceRef::new("table", "public.t");
        // Even Role::Admin is denied — DefaultDeny is the deciding
        // signal in `policy_only`, regardless of role.
        assert!(!store.check_policy_authz_with_role(
            &uid,
            "select",
            &resource,
            &enforcement_eval_ctx(Role::Admin),
            Role::Admin,
        ));
    }

    #[test]
    fn legacy_rbac_mode_falls_back_to_role_decision() {
        // Acceptance #3: in `legacy_rbac` mode the same principal
        // falls through to the role-based decision. `select` requires
        // only `Read`, so all three roles satisfy it; a write verb
        // requires `Write`, so `Read` does not.
        let store = AuthStore::new(test_config());
        store.create_user("alice", "p", Role::Read).unwrap();
        let uid = UserId::platform("alice");
        install_unrelated_policy(&store);
        store.set_enforcement_mode(PolicyEnforcementMode::LegacyRbac);

        let table = ResourceRef::new("table", "public.t");
        // Read role + select action: allowed via the legacy fallback.
        assert!(store.check_policy_authz_with_role(
            &uid,
            "select",
            &table,
            &enforcement_eval_ctx(Role::Read),
            Role::Read,
        ));
        // Read role + insert action: legacy decision says no.
        assert!(!store.check_policy_authz_with_role(
            &uid,
            "insert",
            &table,
            &enforcement_eval_ctx(Role::Read),
            Role::Read,
        ));
        // Admin role + insert action: legacy decision says yes.
        assert!(store.check_policy_authz_with_role(
            &uid,
            "insert",
            &table,
            &enforcement_eval_ctx(Role::Admin),
            Role::Admin,
        ));
    }

    #[test]
    fn explicit_deny_wins_irrespective_of_mode_or_role() {
        // Even in `legacy_rbac` with an Admin role, an explicit
        // matching Deny statement always denies. The mode only
        // influences the `DefaultDeny` arm, never an explicit one.
        let store = AuthStore::new(test_config());
        store.create_user("alice", "p", Role::Admin).unwrap();
        let uid = UserId::platform("alice");
        store
            .put_policy(
                Policy::from_json_str(
                    r#"{"id":"p-deny-select","version":1,"statements":[{"effect":"deny","actions":["select"],"resources":["table:public.t"]}]}"#,
                )
                .unwrap(),
            )
            .unwrap();
        store
            .attach_policy(PrincipalRef::User(uid.clone()), "p-deny-select")
            .unwrap();

        let resource = ResourceRef::new("table", "public.t");
        for mode in [
            PolicyEnforcementMode::LegacyRbac,
            PolicyEnforcementMode::PolicyOnly,
        ] {
            store.set_enforcement_mode(mode);
            assert!(
                !store.check_policy_authz_with_role(
                    &uid,
                    "select",
                    &resource,
                    &enforcement_eval_ctx(Role::Admin),
                    Role::Admin,
                ),
                "explicit deny must win under mode {mode:?}"
            );
        }
    }

    #[test]
    fn explicit_allow_wins_irrespective_of_mode_or_role() {
        // Symmetric guarantee: an explicit Allow is honoured even in
        // `policy_only` mode for a role that the legacy fallback
        // would have rejected (Read role + admin-tier action).
        let store = AuthStore::new(test_config());
        store.create_user("alice", "p", Role::Read).unwrap();
        let uid = UserId::platform("alice");
        store
            .put_policy(
                Policy::from_json_str(
                    r#"{"id":"p-allow-create","version":1,"statements":[{"effect":"allow","actions":["create"],"resources":["table:public.t"]}]}"#,
                )
                .unwrap(),
            )
            .unwrap();
        store
            .attach_policy(PrincipalRef::User(uid.clone()), "p-allow-create")
            .unwrap();
        store.set_enforcement_mode(PolicyEnforcementMode::PolicyOnly);

        let resource = ResourceRef::new("table", "public.t");
        assert!(store.check_policy_authz_with_role(
            &uid,
            "create",
            &resource,
            &enforcement_eval_ctx(Role::Read),
            Role::Read,
        ));
    }

    #[test]
    fn take_legacy_rbac_warn_once_is_at_most_once_per_boot() {
        let store = AuthStore::new(test_config());
        // Default mode = LegacyRbac, so the first call wins the token.
        assert!(store.take_legacy_rbac_warn_once());
        // Every subsequent call is a no-op for the lifetime of this
        // process / AuthStore — that is the "once per boot" guarantee.
        for _ in 0..3 {
            assert!(!store.take_legacy_rbac_warn_once());
        }
    }

    #[test]
    fn take_legacy_rbac_warn_once_is_silent_under_policy_only() {
        // Acceptance #5 phrases the warning as "Boot in legacy_rbac
        // mode emits exactly one warn entry per boot" — implicitly,
        // `policy_only` boots emit zero. The token must not be
        // claimable when the mode is strict so the boot path stays
        // quiet.
        let store = AuthStore::new(test_config());
        store.set_enforcement_mode(PolicyEnforcementMode::PolicyOnly);
        assert!(!store.take_legacy_rbac_warn_once());
        // And, importantly, switching back to LegacyRbac later does
        // not "owe" us a warning — the token was never claimed under
        // policy_only, so it is still available the first time the
        // mode is legacy_rbac.
        store.set_enforcement_mode(PolicyEnforcementMode::LegacyRbac);
        assert!(store.take_legacy_rbac_warn_once());
        assert!(!store.take_legacy_rbac_warn_once());
    }

    #[test]
    fn strict_check_policy_authz_ignores_enforcement_mode() {
        // Governance APIs (managed_config, registry, managed_policy)
        // call the strict `check_policy_authz` — it must return
        // `false` on DefaultDeny regardless of mode so the lenient
        // posture cannot accidentally elevate an admin-tier mutation.
        let store = AuthStore::new(test_config());
        store.create_user("alice", "p", Role::Admin).unwrap();
        let uid = UserId::platform("alice");
        install_unrelated_policy(&store);
        store.set_enforcement_mode(PolicyEnforcementMode::LegacyRbac);

        let resource = ResourceRef::new("table", "public.t");
        assert!(!store.check_policy_authz(
            &uid,
            "select",
            &resource,
            &enforcement_eval_ctx(Role::Admin),
        ));
    }
}
