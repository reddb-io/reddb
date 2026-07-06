//! Per-thread runtime execution context.
//!
//! This module owns connection identity, auth/tenant scope, statement-local
//! config/secret resolvers, and MVCC snapshot visibility helpers.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::api::{RedDBError, RedDBResult};
use crate::storage::schema::Value;
use crate::storage::RedDB;

thread_local! {
    /// Current connection id for the executing statement. Set by the
    /// per-connection wrapper (stdio/gRPC handlers) before dispatching
    /// into `execute_query`; falls back to `0` for embedded callers.
    static CURRENT_CONN_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };

    /// Authenticated user + role for the executing statement (Phase 2.5.2
    /// RLS enforcement). Set by the transport middleware after validating
    /// credentials (password / cert / oauth); unset means "anonymous" /
    /// "embedded" — RLS policies degrade to the role-agnostic subset.
    ///
    /// `None` skips RLS injection entirely; `Some((username, role))`
    /// passes `role` to `matching_rls_policies(table, Some(role), action)`.
    static CURRENT_AUTH_IDENTITY: std::cell::RefCell<Option<(String, crate::auth::Role)>> =
        const { std::cell::RefCell::new(None) };

    /// MVCC snapshot scoped to the currently-executing statement (Phase
    /// 2.3.2d PG parity). `execute_query` captures it on entry and drops
    /// it on exit; every scan consults it via
    /// `entity_visible_under_current_snapshot` to hide tuples whose xmin
    /// hasn't committed or whose xmax already has.
    ///
    /// `None` means "pre-MVCC semantics" — the read path returns every
    /// tuple regardless of xmin/xmax. All embedded callers that bypass
    /// `execute_query` see this default.
    static CURRENT_SNAPSHOT: std::cell::RefCell<Option<SnapshotContext>> =
        const { std::cell::RefCell::new(None) };

    /// Cheap presence flag for `CURRENT_SNAPSHOT`. Scan hot paths
    /// poll this instead of `borrow()`-ing the RefCell on every
    /// row — the common case (autocommit / no MVCC session) reads
    /// one atomic `Cell<bool>` and short-circuits, saving ~10ns × N
    /// rows on aggregate_group / select_range scans.
    static HAS_SNAPSHOT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };

    /// Session-scoped tenant id for the current connection (Phase 2.5.3
    /// multi-tenancy). Populated by `SET TENANT 'id'` or by transport
    /// middleware after resolving tenant from auth claims. Read by the
    /// `CURRENT_TENANT()` scalar function — RLS policies typically
    /// combine it as `USING (tenant_id = CURRENT_TENANT())` to scope
    /// every query to one tenant.
    ///
    /// `None` means "no tenant bound" — `CURRENT_TENANT()` returns
    /// NULL, and RLS policies that gate on it hide every row.
    static CURRENT_TENANT_ID: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };

    /// Statement-local config resolver. SQL expressions materialize the
    /// `red_config` snapshot lazily on the first `$config.*`/`CONFIG()`
    /// access, keeping ordinary statements on the zero-scan path.
    static CURRENT_CONFIG_RESOLVER: std::cell::RefCell<Option<ConfigResolver>> =
        const { std::cell::RefCell::new(None) };

    /// Statement-local secret resolver. SQL expressions materialize the
    /// vault KV snapshot lazily on first `$secret.*` access, then use
    /// lock-free map reads for the rest of the statement.
    static CURRENT_SECRET_RESOLVER: std::cell::RefCell<Option<SecretResolver>> =
        const { std::cell::RefCell::new(None) };

    /// Statement-local plain-KV resolver (#1602). Mirrors the secret
    /// resolver but reads the non-encrypted `plain_kv` store and gates on
    /// `kv:read`. Materialized lazily on the first `$kv.*` access.
    static CURRENT_KV_RESOLVER: std::cell::RefCell<Option<KvResolver>> =
        const { std::cell::RefCell::new(None) };
}

/// Snapshot + manager pair used for read-path visibility checks.
///
/// The manager is needed in addition to the snapshot because `aborted`
/// state mutates after the snapshot is captured — a ROLLBACK by a
/// committed-at-capture-time writer must still hide its tuples. Keeping
/// the Arc around is O(pointer) and the RwLock reads on `is_aborted`
/// are cheap (HashSet lookup under a parking_lot read guard).
///
/// `own_xids` (Phase 2.3.2e) lists the xids belonging to the current
/// connection's transaction — the parent xid plus open and released
/// savepoint sub-xids. The visibility rule promotes rows stamped with
/// these xids to "always visible (unless aborted)" so the writer sees
/// its own nested-savepoint writes even though their xids exceed
/// `snapshot.xid`.
#[derive(Clone)]
pub struct SnapshotContext {
    pub snapshot: crate::storage::transaction::snapshot::Snapshot,
    pub manager: Arc<crate::storage::transaction::snapshot::SnapshotManager>,
    pub own_xids: std::collections::HashSet<crate::storage::transaction::snapshot::Xid>,
    pub requires_index_fallback: bool,
    pub serializable_reader: Option<crate::storage::transaction::snapshot::Xid>,
}

/// Install a connection id on the current thread for the duration of a
/// statement. Transaction state (`RuntimeInner::tx_contexts`) is keyed
/// by this id so different connections can hold independent BEGINs.
///
/// Pub so transports (PG wire, gRPC, HTTP per-request spawners) and
/// tests can emulate per-connection isolation. Call it once when
/// binding the connection's worker thread; pair with
/// `clear_current_connection_id` on teardown.
pub fn set_current_connection_id(id: u64) {
    CURRENT_CONN_ID.with(|c| c.set(id));
}

/// Reset the thread's connection id back to `0` (autocommit).
pub fn clear_current_connection_id() {
    CURRENT_CONN_ID.with(|c| c.set(0));
}

/// Read the connection id set by `set_current_connection_id`. Returns
/// `0` when no wrapper installed one — auto-commit path.
pub fn current_connection_id() -> u64 {
    CURRENT_CONN_ID.with(|c| c.get())
}

/// Install the authenticated identity for the current thread (Phase 2.5.2
/// RLS enforcement). Transport layers call this right after resolving
/// auth so the query dispatch can fold RLS policies into the filter.
pub fn set_current_auth_identity(username: String, role: crate::auth::Role) {
    CURRENT_AUTH_IDENTITY.with(|cell| *cell.borrow_mut() = Some((username, role)));
}

/// Clear the thread-local auth identity. Transports call this after the
/// statement completes so pooled threads don't leak identities across
/// requests.
pub fn clear_current_auth_identity() {
    CURRENT_AUTH_IDENTITY.with(|cell| *cell.borrow_mut() = None);
}

/// Read the current-thread auth identity. `None` when no transport
/// installed one (embedded mode / anonymous access).
pub(crate) fn current_auth_identity() -> Option<(String, crate::auth::Role)> {
    CURRENT_AUTH_IDENTITY.with(|cell| cell.borrow().clone())
}

/// Public probe of the thread-local auth identity for callers outside
/// the `runtime` module (e.g. the AI credential resolver, which audits
/// who triggered a secret read on behalf of a query).
pub fn current_auth_identity_for_audit() -> Option<(String, crate::auth::Role)> {
    current_auth_identity()
}

/// Install the session tenant id for the current thread (Phase 2.5.3
/// multi-tenancy). Called by `SET TENANT 'id'` dispatch and by
/// transport middleware that resolves tenant from auth claims (e.g.
/// JWT `tenant` claim, HTTP header, subdomain).
pub fn set_current_tenant(tenant_id: String) {
    CURRENT_TENANT_ID.with(|cell| *cell.borrow_mut() = Some(tenant_id));
}

/// Clear the current-thread tenant — `CURRENT_TENANT()` will then
/// return NULL and any RLS policy gated on it will hide every row.
pub fn clear_current_tenant() {
    CURRENT_TENANT_ID.with(|cell| *cell.borrow_mut() = None);
}

/// Read the current-thread tenant id, applying overrides in priority order:
///   1. `WITHIN TENANT '<id>' …` per-statement override (highest)
///   2. `SET LOCAL TENANT '<id>'` transaction-local override (consulted
///      only when the current connection has an open transaction)
///   3. `SET TENANT '<id>'` session-level thread-local
///   4. `None` (deny-default for RLS).
///
/// The transaction-local layer is read through the runtime; an embedded
/// helper crate that has no `RedDBRuntime` access still gets correct
/// behaviour for layers 1, 3, and 4.
pub fn current_tenant() -> Option<String> {
    let inherited = CURRENT_TENANT_ID.with(|cell| cell.borrow().clone());
    if let Some(over) = current_scope_override() {
        if over.tenant.is_active() {
            return over.tenant.resolve(inherited);
        }
    }
    if let Some(tx_local) = current_tx_local_tenant() {
        return tx_local;
    }
    inherited
}

thread_local! {
    /// Snapshot of the active connection's `tx_local_tenants` entry for
    /// the current `execute_query` call. Outer `Some(_)` means "a
    /// transaction-local tenant override is active for this call";
    /// inner is the override's value (`Some(s)` overrides to `s`,
    /// `None` overrides to NULL/cleared). Refreshed at the top of every
    /// `execute_query` invocation and cleared by the RAII guard on
    /// return so pooled connections cannot leak the override past the
    /// statement that owns it.
    static TX_LOCAL_TENANT: std::cell::RefCell<Option<Option<String>>> =
        const { std::cell::RefCell::new(None) };
}

fn current_tx_local_tenant() -> Option<Option<String>> {
    TX_LOCAL_TENANT.with(|cell| cell.borrow().clone())
}

/// Recognise `SET LOCAL TENANT '<id>'` / `SET LOCAL TENANT NULL` —
/// returns `Ok(Some(Some(id)))` for an explicit value, `Ok(Some(None))`
/// for an explicit NULL clear, `Ok(None)` when the input is not a
/// `SET LOCAL TENANT` statement at all, and `Err` when the prefix
/// matches but the value is malformed.
pub(crate) fn parse_set_local_tenant(query: &str) -> RedDBResult<Option<Option<String>>> {
    let mut tokens = query.split_ascii_whitespace();
    let Some(w1) = tokens.next() else {
        return Ok(None);
    };
    if !w1.eq_ignore_ascii_case("SET") {
        return Ok(None);
    }
    let Some(w2) = tokens.next() else {
        return Ok(None);
    };
    if !w2.eq_ignore_ascii_case("LOCAL") {
        return Ok(None);
    }
    let Some(w3) = tokens.next() else {
        return Ok(None);
    };
    if !w3.eq_ignore_ascii_case("TENANT") {
        return Ok(None);
    }
    let rest: String = tokens.collect::<Vec<_>>().join(" ");
    let rest = rest.trim().trim_end_matches(';').trim();
    let value_str = rest.strip_prefix('=').map(|s| s.trim()).unwrap_or(rest);
    if value_str.is_empty() {
        return Err(RedDBError::Query(
            "SET LOCAL TENANT expects a string literal or NULL".to_string(),
        ));
    }
    if value_str.eq_ignore_ascii_case("NULL") {
        return Ok(Some(None));
    }
    if value_str.starts_with('\'') && value_str.ends_with('\'') && value_str.len() >= 2 {
        let inner = &value_str[1..value_str.len() - 1];
        return Ok(Some(Some(inner.to_string())));
    }
    Err(RedDBError::Query(format!(
        "SET LOCAL TENANT expects a string literal or NULL, got `{value_str}`"
    )))
}

pub(crate) struct TxLocalTenantGuard;

impl TxLocalTenantGuard {
    pub fn install(value: Option<Option<String>>) -> Self {
        TX_LOCAL_TENANT.with(|cell| *cell.borrow_mut() = value);
        Self
    }
}

impl Drop for TxLocalTenantGuard {
    fn drop(&mut self) {
        TX_LOCAL_TENANT.with(|cell| *cell.borrow_mut() = None);
    }
}

thread_local! {
    /// Stack of `WITHIN ... <stmt>` overrides active on the current
    /// thread. Every entry corresponds to one in-flight `execute_query`
    /// call that started with a `WITHIN` prefix; the entry is pushed
    /// before dispatch and popped before the call returns. The stack
    /// shape supports nested invocations (e.g. a view body that itself
    /// re-enters execute_query).
    static SCOPE_OVERRIDES: std::cell::RefCell<Vec<crate::runtime::within_clause::ScopeOverride>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

pub(crate) fn push_scope_override(over: crate::runtime::within_clause::ScopeOverride) {
    SCOPE_OVERRIDES.with(|cell| cell.borrow_mut().push(over));
}

pub(crate) fn pop_scope_override() {
    SCOPE_OVERRIDES.with(|cell| {
        cell.borrow_mut().pop();
    });
}

pub(crate) fn current_scope_override() -> Option<crate::runtime::within_clause::ScopeOverride> {
    SCOPE_OVERRIDES.with(|cell| cell.borrow().last().cloned())
}

/// Cheap probe: is any `WITHIN …` scope override active on this
/// thread? The fast-path needs to know without paying for the full
/// `.last().cloned()` allocation — just peek at stack length.
pub(crate) fn has_scope_override_active() -> bool {
    SCOPE_OVERRIDES.with(|cell| !cell.borrow().is_empty())
}

/// RAII guard pairing `push_scope_override` with the matching pop, so
/// the stack stays balanced even when the inner `execute_query` returns
/// early via `?`.
pub(crate) struct ScopeOverrideGuard;

impl ScopeOverrideGuard {
    pub fn install(over: crate::runtime::within_clause::ScopeOverride) -> Self {
        push_scope_override(over);
        Self
    }
}

impl Drop for ScopeOverrideGuard {
    fn drop(&mut self) {
        pop_scope_override();
    }
}

/// Read the current-thread auth identity, honouring per-statement
/// `WITHIN ... USER '<u>' AS ROLE '<r>'` overrides. The override only
/// supplies projected strings — it never grants additional privilege —
/// so callers that need to make authorisation decisions must read from
/// the underlying `current_auth_identity()` directly.
pub(crate) fn current_user_projected() -> Option<String> {
    let inherited = current_auth_identity().map(|(u, _)| u);
    if let Some(over) = current_scope_override() {
        if over.user.is_active() {
            return over.user.resolve(inherited);
        }
    }
    inherited
}

pub(crate) fn current_role_projected() -> Option<String> {
    let inherited = current_auth_identity().map(|(_, r)| format!("{r:?}").to_lowercase());
    if let Some(over) = current_scope_override() {
        if over.role.is_active() {
            return over.role.resolve(inherited);
        }
    }
    inherited
}

pub(crate) fn current_secret_value(path: &str) -> Option<String> {
    let key = path.to_ascii_lowercase();
    CURRENT_SECRET_RESOLVER.with(|cell| {
        let mut resolver = cell.borrow_mut();
        let resolver = resolver.as_mut()?;
        if resolver.values.is_none() {
            resolver.values = resolver
                .store
                .as_ref()
                .map(|store| store.vault_kv_snapshot());
        }
        let values = resolver.values.as_ref()?;
        let found = values
            .get(&key)
            .map(|value| (key.as_str(), value))
            .or_else(|| {
                key.strip_prefix("red.vault/").and_then(|rest| {
                    values.get(rest).map(|value| (rest, value)).or_else(|| {
                        let red_secret_key = format!("red.secret.{rest}");
                        values
                            .get_key_value(&red_secret_key)
                            .map(|(key, value)| (key.as_str(), value))
                    })
                })
            })?;
        if !resolver.can_read(found.0) {
            return None;
        }
        Some(found.1.clone())
    })
}

fn secret_value_from_snapshot(values: &HashMap<String, String>, key: &str) -> Option<String> {
    if key.starts_with("red.secret.") {
        return None;
    }
    values.get(key).cloned()
}

struct SecretResolver {
    store: Option<Arc<crate::auth::store::AuthStore>>,
    values: Option<HashMap<String, String>>,
    identity: Option<(String, crate::auth::Role, Option<String>)>,
}

impl SecretResolver {
    fn can_read(&self, key: &str) -> bool {
        // `red.secret.*` is the internal system-secrets namespace. Never
        // expose it via `$secret.X` regardless of IAM role — not even admin.
        if key.starts_with("red.secret.") {
            return false;
        }
        let Some(store) = &self.store else {
            return true;
        };
        let Some((username, role, tenant)) = &self.identity else {
            return true;
        };
        let principal = crate::auth::UserId::from_parts(tenant.as_deref(), username);
        let mut resource =
            crate::auth::policies::ResourceRef::new("secret".to_string(), key.to_string());
        if let Some(tenant) = tenant {
            resource = resource.with_tenant(tenant.clone());
        }
        let ctx = crate::auth::policies::EvalContext {
            principal_tenant: tenant.clone(),
            current_tenant: tenant.clone(),
            peer_ip: None,
            mfa_present: false,
            now_ms: crate::auth::now_ms(),
            principal_is_admin_role: *role == crate::auth::Role::Admin,
            principal_is_platform_scoped: tenant.is_none(),
        };
        store.check_policy_authz_with_role(&principal, "secret:read", &resource, &ctx, *role)
    }
}

pub(crate) struct SecretStoreGuard {
    previous: Option<SecretResolver>,
}

impl SecretStoreGuard {
    pub(super) fn install(store: Option<Arc<crate::auth::store::AuthStore>>) -> Self {
        let previous = CURRENT_SECRET_RESOLVER.with(|cell| {
            cell.replace(Some(SecretResolver {
                store,
                values: None,
                identity: current_auth_identity().map(|(username, role)| {
                    let tenant = current_tenant();
                    (username, role, tenant)
                }),
            }))
        });
        Self { previous }
    }
}

impl Drop for SecretStoreGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        CURRENT_SECRET_RESOLVER.with(|cell| {
            cell.replace(previous);
        });
    }
}

/// Resolve `$kv.<path>` from the plain (non-encrypted) KV store (#1602).
///
/// The parser desugars `$kv.<path>` to `__KV_REF("red.kv/<path>")`; this
/// strips the `red.kv/` prefix and looks up the bare key in the
/// statement-local snapshot, gating each hit on `kv:read`. Returns `None`
/// (→ SQL NULL) when the key is absent or the principal is denied — never
/// an error, mirroring the secret resolver's deny-to-NULL behaviour.
pub(crate) fn current_kv_value(path: &str) -> Option<String> {
    let key = path.to_ascii_lowercase();
    CURRENT_KV_RESOLVER.with(|cell| {
        let mut resolver = cell.borrow_mut();
        let resolver = resolver.as_mut()?;
        if resolver.values.is_none() {
            resolver.values = resolver
                .store
                .as_ref()
                .map(|store| store.plain_kv_snapshot());
        }
        let values = resolver.values.as_ref()?;
        // Accept both the namespaced `red.kv/<key>` form (from the
        // `$kv.*` desugar) and the bare key (from `update_current_kv_value`
        // after a `SET KV`).
        let found = values
            .get(&key)
            .map(|value| (key.as_str(), value))
            .or_else(|| {
                key.strip_prefix("red.kv/")
                    .and_then(|rest| values.get(rest).map(|value| (rest, value)))
            })?;
        if !resolver.can_read(found.0) {
            return None;
        }
        Some(found.1.clone())
    })
}

struct KvResolver {
    store: Option<Arc<crate::auth::store::AuthStore>>,
    values: Option<HashMap<String, String>>,
    identity: Option<(String, crate::auth::Role, Option<String>)>,
}

impl KvResolver {
    fn can_read(&self, key: &str) -> bool {
        let Some(store) = &self.store else {
            return true;
        };
        let Some((username, role, tenant)) = &self.identity else {
            return true;
        };
        let principal = crate::auth::UserId::from_parts(tenant.as_deref(), username);
        let mut resource =
            crate::auth::policies::ResourceRef::new("kv".to_string(), key.to_string());
        if let Some(tenant) = tenant {
            resource = resource.with_tenant(tenant.clone());
        }
        let ctx = crate::auth::policies::EvalContext {
            principal_tenant: tenant.clone(),
            current_tenant: tenant.clone(),
            peer_ip: None,
            mfa_present: false,
            now_ms: crate::auth::now_ms(),
            principal_is_admin_role: *role == crate::auth::Role::Admin,
            principal_is_platform_scoped: tenant.is_none(),
        };
        store.check_policy_authz_with_role(&principal, "kv:read", &resource, &ctx, *role)
    }
}

pub(crate) struct KvStoreGuard {
    // Boxed to keep the stack footprint of nested guards small. Each
    // StatementFrameGuards is live for the full recursive call depth of a
    // materialized-view refresh chain; unboxed KvResolver (≈120 B) would
    // push those deep paths past the 8 MB stack limit.
    previous: Option<Box<KvResolver>>,
}

impl KvStoreGuard {
    pub(super) fn install(store: Option<Arc<crate::auth::store::AuthStore>>) -> Self {
        let previous = CURRENT_KV_RESOLVER.with(|cell| {
            cell.replace(Some(KvResolver {
                store,
                values: None,
                identity: current_auth_identity().map(|(username, role)| {
                    let tenant = current_tenant();
                    (username, role, tenant)
                }),
            }))
        });
        Self {
            previous: previous.map(Box::new),
        }
    }
}

impl Drop for KvStoreGuard {
    fn drop(&mut self) {
        let previous = self.previous.take().map(|b| *b);
        CURRENT_KV_RESOLVER.with(|cell| {
            cell.replace(previous);
        });
    }
}

/// Update the statement-local KV snapshot after a `SET KV` / `DELETE KV`
/// so a subsequent `$kv.*` read in the same statement observes the write.
pub(crate) fn update_current_kv_value(path: &str, value: Option<String>) {
    let key = path.to_ascii_lowercase();
    CURRENT_KV_RESOLVER.with(|cell| {
        if let Some(resolver) = cell.borrow_mut().as_mut() {
            let Some(values) = resolver.values.as_mut() else {
                return;
            };
            match value {
                Some(value) => {
                    values.insert(key, value);
                }
                None => {
                    values.remove(&key);
                }
            }
        }
    });
}

pub(crate) fn current_config_value(path: &str) -> Option<Value> {
    let key = path.to_ascii_lowercase();
    CURRENT_CONFIG_RESOLVER.with(|cell| {
        let mut resolver = cell.borrow_mut();
        let resolver = resolver.as_mut()?;
        if resolver.values.is_none() {
            resolver.values = Some(latest_config_snapshot(&resolver.db));
        }
        // #1370 — `$config.<path>` desugars to `CONFIG("red.config/<path>")`,
        // but `SET CONFIG <path>` stores under the bare key. Mirror the secret
        // resolver: after the namespaced key, fall back to the stripped bare
        // key, then the dotted `red.config.<rest>` legacy form. Track the key
        // that matched so the `config:read` gate authorizes the real target.
        let found = {
            let values = resolver.values.as_ref()?;
            values
                .get(&key)
                .map(|value| (key.clone(), value.clone()))
                .or_else(|| {
                    key.strip_prefix("red.config/").and_then(|rest| {
                        values
                            .get(rest)
                            .map(|value| (rest.to_string(), value.clone()))
                            .or_else(|| {
                                let legacy = format!("red.config.{rest}");
                                values.get(&legacy).map(|value| (legacy, value.clone()))
                            })
                    })
                })
        }?;
        // #1743 — hard-block the reserved `red.config.*` namespace and gate the
        // read on `config:read`, mirroring the `$secret` sibling. Denied reads
        // resolve to `None` (→ SQL NULL), never an error.
        if !resolver.can_read(&found.0) {
            return None;
        }
        Some(found.1)
    })
}

pub(crate) fn update_current_config_value(path: &str, value: Value) {
    let key = path.to_ascii_lowercase();
    CURRENT_CONFIG_RESOLVER.with(|cell| {
        if let Some(resolver) = cell.borrow_mut().as_mut() {
            if let Some(values) = resolver.values.as_mut() {
                values.insert(key, value);
            }
        }
    });
}

pub(crate) fn update_current_secret_value(path: &str, value: Option<String>) {
    let key = path.to_ascii_lowercase();
    CURRENT_SECRET_RESOLVER.with(|cell| {
        if let Some(resolver) = cell.borrow_mut().as_mut() {
            let Some(values) = resolver.values.as_mut() else {
                return;
            };
            match value {
                Some(value) => {
                    values.insert(key, value);
                }
                None => {
                    values.remove(&key);
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with_secret_values<T>(values: HashMap<String, String>, f: impl FnOnce() -> T) -> T {
        CURRENT_SECRET_RESOLVER.with(|cell| {
            cell.replace(Some(SecretResolver {
                store: None,
                values: Some(values),
                identity: None,
            }));
        });
        let result = f();
        CURRENT_SECRET_RESOLVER.with(|cell| {
            cell.replace(None);
        });
        result
    }

    #[test]
    fn current_secret_value_does_not_fall_back_to_reserved_red_secret_namespace() {
        let values = HashMap::from([
            (
                "red.secret.aes_key".to_string(),
                "vault-aes-key".to_string(),
            ),
            (
                "red.secret.ai.providers.anthropic.tokens.default".to_string(),
                "provider-key".to_string(),
            ),
            ("acme.key".to_string(), "user-value".to_string()),
        ]);

        with_secret_values(values, || {
            assert_eq!(current_secret_value("red.vault/aes_key"), None);
            assert_eq!(current_secret_value("red.vault/red.secret.aes_key"), None);
            // The AI provider-token namespace is hard-blocked from `$secret`
            // regardless of role, on the new path shape (#1745).
            assert_eq!(
                current_secret_value("red.vault/ai.providers.anthropic.tokens.default"),
                None
            );
            assert_eq!(
                current_secret_value("red.vault/red.secret.ai.providers.anthropic.tokens.default"),
                None
            );
            assert_eq!(
                current_secret_value("red.vault/acme.key").as_deref(),
                Some("user-value")
            );
        });
    }

    fn with_kv_values<T>(values: HashMap<String, String>, f: impl FnOnce() -> T) -> T {
        CURRENT_KV_RESOLVER.with(|cell| {
            cell.replace(Some(KvResolver {
                store: None,
                values: Some(values),
                identity: None,
            }));
        });
        let result = f();
        CURRENT_KV_RESOLVER.with(|cell| {
            cell.replace(None);
        });
        result
    }

    #[test]
    fn current_kv_value_resolves_namespaced_and_bare_keys() {
        let values = HashMap::from([("acme.key".to_string(), "plain-value".to_string())]);

        with_kv_values(values, || {
            // `$kv.acme.key` desugars to `red.kv/acme.key`.
            assert_eq!(
                current_kv_value("red.kv/acme.key").as_deref(),
                Some("plain-value")
            );
            // Bare-key lookup (post-`SET KV` snapshot update) also resolves.
            assert_eq!(current_kv_value("acme.key").as_deref(), Some("plain-value"));
            // Unknown keys resolve to None (→ SQL NULL), never an error.
            assert_eq!(current_kv_value("red.kv/missing.key"), None);
        });
    }

    #[test]
    fn config_key_read_allowed_hard_blocks_reserved_namespace() {
        // The reserved `red.config.*` system namespace is denied role- and
        // store-independently, mirroring the `red.secret.*` block (#1743).
        assert!(!config_key_read_allowed(
            None,
            None,
            "red.config/red.config.aes_key"
        ));
        assert!(!config_key_read_allowed(None, None, "red.config.aes_key"));
        // Ordinary config keys pass when no store is installed (non-IAM path).
        assert!(config_key_read_allowed(
            None,
            None,
            "red.config/ai.openrouter.default.key"
        ));
        assert!(config_key_read_allowed(None, None, "acme.flags.beta"));
    }

    #[test]
    fn is_config_collection_matches_system_config_stores() {
        assert!(is_config_collection("red_config"));
        assert!(is_config_collection("RED_CONFIG"));
        assert!(is_config_collection("red.config"));
        assert!(!is_config_collection("users"));
        assert!(!is_config_collection("red_kv"));
    }

    #[test]
    fn update_current_kv_value_reflects_in_resolver() {
        with_kv_values(HashMap::new(), || {
            assert_eq!(current_kv_value("red.kv/feature.flag"), None);
            update_current_kv_value("feature.flag", Some("on".to_string()));
            assert_eq!(
                current_kv_value("red.kv/feature.flag").as_deref(),
                Some("on")
            );
            update_current_kv_value("feature.flag", None);
            assert_eq!(current_kv_value("red.kv/feature.flag"), None);
        });
    }
}

fn latest_config_snapshot(db: &RedDB) -> HashMap<String, Value> {
    let mut latest: HashMap<String, (u64, Value)> = HashMap::new();

    if let Some(manager) = db.store().get_collection("red_config") {
        manager.for_each_entity(|entity| {
            let Some(row) = entity.data.as_row() else {
                return true;
            };
            let Some(Value::Text(key)) = row.get_field("key") else {
                return true;
            };
            let value = row.get_field("value").cloned().unwrap_or(Value::Null);
            let id = entity.id.raw();
            let key = key.to_ascii_lowercase();
            insert_latest_config_value(&mut latest, key.clone(), id, value.clone());
            if let Some(rest) = key.strip_prefix("red.config.") {
                insert_latest_config_value(&mut latest, format!("red.config/{rest}"), id, value);
            }
            true
        });
    }

    if let Some(manager) = db.store().get_collection("red.config") {
        manager.for_each_entity(|entity| {
            let Some(row) = entity.data.as_row() else {
                return true;
            };
            if matches!(row.get_field("tombstone"), Some(Value::Boolean(true))) {
                return true;
            }
            let Some(Value::Text(key)) = row.get_field("key") else {
                return true;
            };
            let value = row.get_field("value").cloned().unwrap_or(Value::Null);
            insert_latest_config_value(
                &mut latest,
                format!("red.config/{}", key.to_ascii_lowercase()),
                entity.id.raw(),
                value,
            );
            true
        });
    }

    latest
        .into_iter()
        .map(|(key, (_, value))| (key, value))
        .collect()
}

fn insert_latest_config_value(
    latest: &mut HashMap<String, (u64, Value)>,
    key: String,
    id: u64,
    value: Value,
) {
    match latest.get(&key) {
        Some((prev_id, _)) if *prev_id > id => {}
        _ => {
            latest.insert(key, (id, value));
        }
    }
}

struct ConfigResolver {
    db: Arc<RedDB>,
    store: Option<Arc<crate::auth::store::AuthStore>>,
    identity: Option<(String, crate::auth::Role, Option<String>)>,
    values: Option<HashMap<String, Value>>,
}

impl ConfigResolver {
    fn can_read(&self, key: &str) -> bool {
        config_key_read_allowed(self.store.as_ref(), self.identity.as_ref(), key)
    }
}

/// The reserved `red.config.*` system-config namespace. Never expose it via
/// the inline `$config`/`CONFIG()`/`KV()` resolver regardless of IAM role,
/// mirroring the role-independent `red.secret.*` hard-block (#1743).
fn is_reserved_config_key(key: &str) -> bool {
    let key = key.strip_prefix("red.config/").unwrap_or(key);
    key.starts_with("red.config.") || key == "red.config"
}

/// The system config-store collections (`red_config` / `red.config`) reached
/// by the inline resolver. A `KV(<collection>, key)` read of one of these is
/// gated exactly like `$config`; any other collection is unaffected (#1743).
fn is_config_collection(collection: &str) -> bool {
    matches!(
        collection.to_ascii_lowercase().as_str(),
        "red_config" | "red.config"
    )
}

fn config_resource_target(key: &str) -> String {
    let bare = key.strip_prefix("red.config/").unwrap_or(key);
    format!("red.config/{bare}")
}

/// Shared inline-config read gate: a role-independent hard-block on the
/// reserved `red.config.*` namespace, then a `config:read` capability check
/// mirroring `SHOW CONFIG` / the `$secret` sibling (#1743). Absent a store or
/// with IAM authorization disabled, only the hard-block applies so existing
/// non-IAM `$config` reads keep working.
fn config_key_read_allowed(
    store: Option<&Arc<crate::auth::store::AuthStore>>,
    identity: Option<&(String, crate::auth::Role, Option<String>)>,
    key: &str,
) -> bool {
    if is_reserved_config_key(key) {
        return false;
    }
    let Some(store) = store else {
        return true;
    };
    if !store.iam_authorization_enabled() {
        return true;
    }
    let Some((username, role, tenant)) = identity else {
        return true;
    };
    let principal = crate::auth::UserId::from_parts(tenant.as_deref(), username);
    let mut resource =
        crate::auth::policies::ResourceRef::new("config".to_string(), config_resource_target(key));
    if let Some(tenant) = tenant {
        resource = resource.with_tenant(tenant.clone());
    }
    let ctx = crate::auth::policies::EvalContext {
        principal_tenant: tenant.clone(),
        current_tenant: tenant.clone(),
        peer_ip: None,
        mfa_present: false,
        now_ms: crate::auth::now_ms(),
        principal_is_admin_role: *role == crate::auth::Role::Admin,
        principal_is_platform_scoped: tenant.is_none(),
    };
    store.check_policy_authz_with_role(&principal, "config:read", &resource, &ctx, *role)
}

/// Whether the current principal may read config `key` through the inline
/// `$config`/`CONFIG()`/`KV('red_config', …)` resolver. Used by the db-fallback
/// lookup paths that bypass [`current_config_value`] (#1743).
pub(crate) fn config_read_permitted(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    CURRENT_CONFIG_RESOLVER.with(|cell| match cell.borrow().as_ref() {
        Some(resolver) => resolver.can_read(&key),
        None => !is_reserved_config_key(&key),
    })
}

/// Gate a raw `KV(collection, key)` read: config system collections go through
/// the `$config` gate; every other collection is unaffected (#1743).
pub(crate) fn kv_read_permitted(collection: &str, key: &str) -> bool {
    if is_config_collection(collection) {
        config_read_permitted(key)
    } else {
        true
    }
}

pub(crate) struct ConfigSnapshotGuard {
    previous: Option<ConfigResolver>,
}

impl ConfigSnapshotGuard {
    pub(super) fn install(
        db: Arc<RedDB>,
        store: Option<Arc<crate::auth::store::AuthStore>>,
    ) -> Self {
        let identity = current_auth_identity().map(|(username, role)| {
            let tenant = current_tenant();
            (username, role, tenant)
        });
        let previous = CURRENT_CONFIG_RESOLVER.with(|cell| {
            cell.replace(Some(ConfigResolver {
                db,
                store,
                identity,
                values: None,
            }))
        });
        Self { previous }
    }
}

impl Drop for ConfigSnapshotGuard {
    fn drop(&mut self) {
        let previous = self.previous.take();
        CURRENT_CONFIG_RESOLVER.with(|cell| {
            cell.replace(previous);
        });
    }
}

/// Install the MVCC snapshot used by the current thread for the duration
/// of one statement. Paired with `clear_current_snapshot()` — callers
/// should prefer the `CurrentSnapshotGuard` RAII wrapper so early returns
/// still clean up.
pub fn set_current_snapshot(ctx: SnapshotContext) {
    CURRENT_SNAPSHOT.with(|cell| *cell.borrow_mut() = Some(ctx));
    HAS_SNAPSHOT.with(|c| c.set(true));
}

pub fn clear_current_snapshot() {
    CURRENT_SNAPSHOT.with(|cell| *cell.borrow_mut() = None);
    HAS_SNAPSHOT.with(|c| c.set(false));
}

/// Drop-guard that restores the previous snapshot on scope exit. Safe to
/// nest — each statement saves the caller's snapshot and puts it back
/// instead of blindly clearing, so a top-level `execute_query` called
/// from inside another statement dispatch (e.g. vector source subqueries)
/// doesn't strip visibility from the outer scan.
pub(crate) struct CurrentSnapshotGuard {
    previous: Option<SnapshotContext>,
}

impl CurrentSnapshotGuard {
    pub(crate) fn install(ctx: SnapshotContext) -> Self {
        let previous = CURRENT_SNAPSHOT.with(|cell| cell.borrow().clone());
        set_current_snapshot(ctx);
        Self { previous }
    }
}

impl Drop for CurrentSnapshotGuard {
    fn drop(&mut self) {
        let prev = self.previous.take();
        let has = prev.is_some();
        CURRENT_SNAPSHOT.with(|cell| *cell.borrow_mut() = prev);
        HAS_SNAPSHOT.with(|c| c.set(has));
    }
}

/// Is this entity visible under the current thread's MVCC snapshot?
///
/// Returns `true` (no filtering) when no snapshot is installed — that
/// path is used by embedded callers and by operations that intentionally
/// bypass MVCC (VACUUM, snapshot export, admin introspection).
///
/// When a snapshot is installed the result is
///   `snapshot.sees(xmin, xmax) && !mgr.is_aborted(xmin) && !xmax_half_abort`
/// where `xmax_half_abort` re-grants visibility for tuples whose
/// deleting transaction rolled back.
#[inline]
pub fn entity_visible_under_current_snapshot(
    entity: &crate::storage::unified::entity::UnifiedEntity,
) -> bool {
    // Moderation visibility gate (#1274, ADR 0057). A row carrying the
    // moderation status marker — quarantine-pending or rejected-tombstone
    // — is hidden from every normal read, on top of MVCC visibility. The
    // marker lives on the row itself, so the check is a single field probe
    // and rides the existing per-row visibility chokepoint rather than
    // adding a separate filter pass to each scan call-site.
    if crate::runtime::ai::moderation::entity_moderation_hidden(entity) {
        return false;
    }
    // Fast path — one `Cell<bool>` read, no RefCell borrow. Autocommit
    // reads (no active MVCC transaction) still hide superseded physical
    // versions while avoiding a full snapshot-context lookup.
    // This runs on every row of every scan; the slow path only fires
    // inside an explicit transaction.
    if !HAS_SNAPSHOT.with(|c| c.get()) {
        return entity.xmax == 0;
    }
    CURRENT_SNAPSHOT.with(|cell| {
        let guard = cell.borrow();
        let Some(ctx) = guard.as_ref() else {
            return true;
        };
        let visible = visibility_check(ctx, entity.xmin, entity.xmax);
        if visible {
            record_serializable_read(ctx, entity);
        }
        visible
    })
}

/// Direct visibility check from raw `(xmin, xmax)` — bypasses the
/// entity borrow for callers that already decomposed the tuple (e.g.
/// pre-materialized scan caches). Same semantics as
/// `entity_visible_under_current_snapshot`.
#[inline]
pub(crate) fn xids_visible_under_current_snapshot(xmin: u64, xmax: u64) -> bool {
    if !HAS_SNAPSHOT.with(|c| c.get()) {
        return true;
    }
    CURRENT_SNAPSHOT.with(|cell| {
        let guard = cell.borrow();
        let Some(ctx) = guard.as_ref() else {
            return true;
        };
        visibility_check(ctx, xmin, xmax)
    })
}

/// Clone the current thread's snapshot context. Parallel scan paths
/// (`query_all_zoned` with `std::thread::scope`) call this on the main
/// thread *before* spawning workers so the captured `SnapshotContext`
/// can be moved into every worker closure. Worker threads do not
/// inherit thread-locals, so calling `entity_visible_under_current_snapshot`
/// from inside a spawned closure would silently skip the filter.
pub fn capture_current_snapshot() -> Option<SnapshotContext> {
    CURRENT_SNAPSHOT.with(|cell| cell.borrow().clone())
}

/// Whether the active read snapshot may need historical tuple versions
/// that the current secondary indexes cannot prove. Index paths can still
/// recheck visible candidates, but only a heap scan can discover versions
/// whose indexed value was changed or deleted after this snapshot.
pub(crate) fn current_snapshot_requires_index_fallback() -> bool {
    if !HAS_SNAPSHOT.with(|c| c.get()) {
        return false;
    }
    CURRENT_SNAPSHOT.with(|cell| {
        cell.borrow()
            .as_ref()
            .is_some_and(|ctx| ctx.requires_index_fallback)
    })
}

/// Frozen MVCC + identity context for callers that need to reinstall
/// the same view across thread-local boundaries — long-lived cursors,
/// background batchers, anything that detaches from the dispatch path
/// and re-enters later.
///
/// The bundle bakes in the three thread-locals every read path
/// consults: `SnapshotContext` (MVCC visibility), the auth identity
/// (RLS policy gate), and the tenant id (RLS scope). A FETCH that
/// reinstalls the bundle sees exactly the same rows as the DECLARE
/// would have, regardless of writes that landed in between.
///
/// Cheap to clone — `SnapshotContext` is a clone of three
/// `Arc`-backed fields, identity is a `(String, Role)`, tenant is a
/// `String`. None of these contend with the read path.
#[derive(Clone, Default)]
pub struct SnapshotBundle {
    pub snapshot: Option<SnapshotContext>,
    pub auth: Option<(String, crate::auth::Role)>,
    pub tenant: Option<String>,
}

/// Capture the three read-path thread-locals into a `SnapshotBundle`.
/// Pairs with `with_snapshot_bundle` for re-entry.
pub fn snapshot_bundle() -> SnapshotBundle {
    SnapshotBundle {
        snapshot: capture_current_snapshot(),
        auth: current_auth_identity(),
        tenant: CURRENT_TENANT_ID.with(|cell| cell.borrow().clone()),
    }
}

/// Reinstall a captured `SnapshotBundle` for the duration of `f`.
/// Restores the caller's previous thread-locals on exit (panic-safe via
/// the explicit guard struct so a panic in `f` cannot leak the
/// installed identity into the worker's next request).
pub fn with_snapshot_bundle<R>(bundle: &SnapshotBundle, f: impl FnOnce() -> R) -> R {
    struct Guard {
        prev_snapshot: Option<SnapshotContext>,
        prev_auth: Option<(String, crate::auth::Role)>,
        prev_tenant: Option<String>,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            let snap = self.prev_snapshot.take();
            let has = snap.is_some();
            CURRENT_SNAPSHOT.with(|cell| *cell.borrow_mut() = snap);
            HAS_SNAPSHOT.with(|c| c.set(has));
            CURRENT_AUTH_IDENTITY.with(|cell| *cell.borrow_mut() = self.prev_auth.take());
            CURRENT_TENANT_ID.with(|cell| *cell.borrow_mut() = self.prev_tenant.take());
        }
    }

    let _guard = {
        let prev_snapshot = CURRENT_SNAPSHOT.with(|cell| cell.borrow().clone());
        let prev_auth = CURRENT_AUTH_IDENTITY.with(|cell| cell.borrow().clone());
        let prev_tenant = CURRENT_TENANT_ID.with(|cell| cell.borrow().clone());

        match bundle.snapshot.clone() {
            Some(ctx) => set_current_snapshot(ctx),
            None => clear_current_snapshot(),
        }
        CURRENT_AUTH_IDENTITY.with(|cell| *cell.borrow_mut() = bundle.auth.clone());
        CURRENT_TENANT_ID.with(|cell| *cell.borrow_mut() = bundle.tenant.clone());

        Guard {
            prev_snapshot,
            prev_auth,
            prev_tenant,
        }
    };
    f()
}

/// Apply the same visibility rules used by the thread-local helpers
/// against a caller-provided context. Intended for parallel workers
/// that captured the snapshot with `capture_current_snapshot()`.
#[inline]
pub fn entity_visible_with_context(
    ctx: Option<&SnapshotContext>,
    entity: &crate::storage::unified::entity::UnifiedEntity,
) -> bool {
    // Same moderation visibility gate as the thread-local path (#1274):
    // parallel scan workers capture the snapshot context but must apply
    // the moderation marker check identically.
    if crate::runtime::ai::moderation::entity_moderation_hidden(entity) {
        return false;
    }
    match ctx {
        Some(ctx) => {
            let visible = visibility_check(ctx, entity.xmin, entity.xmax);
            if visible {
                record_serializable_read(ctx, entity);
            }
            visible
        }
        None => true,
    }
}

fn record_serializable_read(
    ctx: &SnapshotContext,
    entity: &crate::storage::unified::entity::UnifiedEntity,
) {
    let Some(reader) = ctx.serializable_reader else {
        return;
    };
    if !matches!(
        &entity.kind,
        crate::storage::unified::entity::EntityKind::TableRow { .. }
    ) {
        return;
    }
    ctx.manager
        .record_serializable_read(reader, entity.kind.collection(), entity.logical_id());
}

#[inline]
fn visibility_check(ctx: &SnapshotContext, xmin: u64, xmax: u64) -> bool {
    // Writer aborted → tuple never existed from any future reader's view.
    // Checked *before* the own-xids fast path so an aborted own-sub-xid
    // (rolled-back savepoint) stays hidden from the parent.
    if xmin != 0 && ctx.manager.is_aborted(xmin) {
        return false;
    }
    // Deleter aborted → treat xmax as unset; fall back to xmin-only check.
    let effective_xmax = if xmax != 0 && ctx.manager.is_aborted(xmax) {
        0
    } else {
        xmax
    };
    // Phase 2.3.2e: own-tx writes are always visible to the connection
    // that stamped them, even when xmin/xmax exceed `snapshot.xid` (as
    // happens for sub-xids allocated by SAVEPOINT after BEGIN).
    let own_xmin = xmin != 0 && ctx.own_xids.contains(&xmin);
    let own_xmax = effective_xmax != 0 && ctx.own_xids.contains(&effective_xmax);
    if own_xmax {
        // This connection deleted the row via this xid — hide it from self.
        return false;
    }
    if own_xmin {
        return true;
    }
    ctx.snapshot.sees(xmin, effective_xmax)
}
