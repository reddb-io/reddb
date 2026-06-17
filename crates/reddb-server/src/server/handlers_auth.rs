//! HTTP handlers for authentication and authorization endpoints.
//!
//! Provides REST endpoints for:
//! - Bootstrap (first admin user creation)
//! - Login (session token issuance)
//! - User CRUD (create, list, delete)
//! - API key management (create, revoke)
//! - Password change
//! - Whoami (token introspection)
//!
//! User CRUD is tenant-aware: a tenant-scoped admin can only manage
//! users inside their own tenant; a platform admin (`tenant_id = None`)
//! can target any tenant.

use super::*;
use crate::auth::policies::{Decision, EvalContext, ResourceRef};
use crate::auth::store::SimCtx;
use crate::auth::{AuthError, AuthStore, Role, UserId};
use std::collections::BTreeSet;

/// Resolved caller for an admin-only auth endpoint.
struct AuthCaller {
    /// Owner UserId (tenant + username) carried by the validated token.
    id: UserId,
    role: crate::auth::Role,
}

impl AuthCaller {
    fn is_platform_admin(&self) -> bool {
        self.id.tenant.is_none() && self.role.can_admin()
    }
}

/// Look up the caller behind an `Authorization: Bearer ...` header.
///
/// Returns `None` when:
///   * no header is present,
///   * the token isn't recognized by either OAuth or the AuthStore,
///   * or auth is disabled (the routing layer has already gated this).
///
/// Stays out of Agent A's bearer-extractor lane by re-using the same
/// `Authorization: Bearer` parsing inline; the OAuth-JWT path is
/// shared with `routing::resolve_bearer_role`.
fn resolve_auth_caller(
    server: &RedDBServer,
    headers: &BTreeMap<String, String>,
) -> Option<AuthCaller> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.strip_prefix("Bearer "))?;
    let auth_store = server.auth_store.as_ref()?;
    if super::routing::looks_like_jwt(token) {
        if let Some(validator) = server.runtime.oauth_validator() {
            if let Ok((tenant, username, role)) =
                crate::wire::redwire::auth::validate_oauth_jwt_full(&validator, token)
            {
                return Some(AuthCaller {
                    id: UserId::from_parts(tenant.as_deref(), &username),
                    role,
                });
            }
        }
    }
    let (id, role) = auth_store.validate_token_full(token)?;
    Some(AuthCaller { id, role })
}

fn authorize_user_lifecycle_mutation(
    auth_store: &AuthStore,
    caller: Option<&AuthCaller>,
    action: &str,
    target: &UserId,
) -> Option<HttpResponse> {
    let caller = caller?;
    if auth_store.check_user_lifecycle_authz(&caller.id, caller.role, action, target) {
        None
    } else {
        Some(json_error(
            403,
            format!("policy denied {action} on user:{target}"),
        ))
    }
}

impl RedDBServer {
    /// POST /auth/bootstrap
    ///
    /// Creates the first admin user. One-shot, irreversible.
    /// Body: `{ "username": "admin", "password": "secret" }`
    pub(crate) fn handle_auth_bootstrap(&self, body: Vec<u8>) -> HttpResponse {
        let auth_store = match &self.auth_store {
            Some(store) => store,
            None => return json_error(501, "authentication is not configured"),
        };

        let payload = match parse_json_body(&body) {
            Ok(value) => value,
            Err(resp) => return resp,
        };

        let username = match json_string_field(&payload, "username") {
            Some(u) => u,
            None => return json_error(400, "missing 'username' field"),
        };
        let password = match json_string_field(&payload, "password") {
            Some(p) => p,
            None => return json_error(400, "missing 'password' field"),
        };

        match auth_store.bootstrap(&username, &password) {
            Ok(result) => {
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert(
                    "username".to_string(),
                    JsonValue::String(result.user.username.clone()),
                );
                object.insert(
                    "role".to_string(),
                    JsonValue::String(result.user.role.to_string()),
                );
                object.insert(
                    "api_key".to_string(),
                    JsonValue::String(result.api_key.key.clone()),
                );
                if let Some(ref cert) = result.certificate {
                    object.insert("certificate".to_string(), JsonValue::String(cert.clone()));
                }
                json_response(200, JsonValue::Object(object))
            }
            Err(err) => json_error(403, err.to_string()),
        }
    }

    /// POST /v1/_admin/users
    ///
    /// Creates an admin-token managed user. Routing gates this endpoint
    /// with `RED_ADMIN_TOKEN`; regular auth tokens are intentionally ignored.
    /// Body: `{ "username": "system", "password": "secret", "role": "admin",
    ///          "tenant_id": "acme" }`
    pub(crate) fn handle_admin_create_user(&self, body: Vec<u8>) -> HttpResponse {
        let auth_store = match &self.auth_store {
            Some(store) => store,
            None => return json_error(501, "authentication is not configured"),
        };

        let payload = match parse_json_body(&body) {
            Ok(value) => value,
            Err(resp) => return resp,
        };

        let username = match json_string_field(&payload, "username") {
            Some(u) => u,
            None => return json_error(400, "missing 'username' field"),
        };
        let password = match json_string_field(&payload, "password") {
            Some(p) => p,
            None => return json_error(400, "missing 'password' field"),
        };
        let role_str = json_string_field(&payload, "role").unwrap_or_else(|| "read".to_string());
        let role = match crate::auth::Role::from_str(&role_str) {
            Some(r) => r,
            None => {
                return json_error(
                    400,
                    format!("invalid role '{}': must be read, write, or admin", role_str),
                )
            }
        };
        let tenant_id = json_string_field(&payload, "tenant_id");

        match auth_store.create_admin_user(&username, &password, role, tenant_id.as_deref()) {
            Ok(user) => {
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert("username".to_string(), JsonValue::String(user.username));
                object.insert("role".to_string(), JsonValue::String(user.role.to_string()));
                object.insert("enabled".to_string(), JsonValue::Bool(user.enabled));
                if let Some(tenant_id) = user.tenant_id {
                    object.insert("tenant_id".to_string(), JsonValue::String(tenant_id));
                }
                json_response(201, JsonValue::Object(object))
            }
            Err(err) => json_error(400, err.to_string()),
        }
    }

    /// POST /auth/login
    ///
    /// Authenticates with username/password and returns a session token.
    /// Body: `{ "username": "admin", "password": "secret",
    ///          "tenant_id": "acme" }`
    ///
    /// `tenant_id` is optional: omitted = platform tenant. Two users
    /// with the same `username` but different `tenant_id`s are
    /// distinct identities.
    pub(crate) fn handle_auth_login(&self, body: Vec<u8>) -> HttpResponse {
        let auth_store = match &self.auth_store {
            Some(store) => store,
            None => return json_error(501, "authentication is not configured"),
        };

        let payload = match parse_json_body(&body) {
            Ok(value) => value,
            Err(resp) => return resp,
        };

        let username = match json_string_field(&payload, "username") {
            Some(u) => u,
            None => return json_error(400, "missing 'username' field"),
        };
        let password = match json_string_field(&payload, "password") {
            Some(p) => p,
            None => return json_error(400, "missing 'password' field"),
        };
        let tenant_id = json_string_field(&payload, "tenant_id");

        let caller_id = UserId::from_parts(tenant_id.as_deref(), &username);
        match auth_store.authenticate_in_tenant(tenant_id.as_deref(), &username, &password) {
            Ok(session) => {
                tracing::info!(
                    target: "reddb::http_auth",
                    principal = %caller_id,
                    "login ok"
                );
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert(
                    "token".to_string(),
                    JsonValue::String(session.token.clone()),
                );
                object.insert(
                    "username".to_string(),
                    JsonValue::String(session.username.clone()),
                );
                if let Some(t) = &session.tenant_id {
                    object.insert("tenant_id".to_string(), JsonValue::String(t.clone()));
                }
                object.insert(
                    "role".to_string(),
                    JsonValue::String(session.role.to_string()),
                );
                object.insert(
                    "expires_at".to_string(),
                    JsonValue::Number(session.expires_at as f64),
                );
                json_response(200, JsonValue::Object(object))
            }
            Err(err) => {
                tracing::warn!(
                    target: "reddb::http_auth",
                    principal = %caller_id,
                    "login refused"
                );
                json_error(401, err.to_string())
            }
        }
    }

    /// POST /auth/users
    ///
    /// Creates a new user with the specified role.
    /// Body: `{ "username": "alice", "password": "pass",
    ///          "role": "write", "tenant_id": "acme" }`
    ///
    /// Tenant scoping rules:
    ///   * Platform admin (`tenant_id = None`, role `Admin`): may
    ///     create a user under any tenant. The body's `tenant_id` (if
    ///     any) is honored verbatim; absent = platform user.
    ///   * Tenant-scoped admin: the body's `tenant_id` is **forced**
    ///     to the caller's own tenant. Cross-tenant creation is
    ///     forbidden — even when the caller specifies another tenant
    ///     in the body, it's silently overridden.
    pub(crate) fn handle_auth_create_user(
        &self,
        headers: &BTreeMap<String, String>,
        body: Vec<u8>,
        tenant_path_override: Option<&str>,
    ) -> HttpResponse {
        let auth_store = match &self.auth_store {
            Some(store) => store,
            None => return json_error(501, "authentication is not configured"),
        };

        let payload = match parse_json_body(&body) {
            Ok(value) => value,
            Err(resp) => return resp,
        };

        let username = match json_string_field(&payload, "username") {
            Some(u) => u,
            None => return json_error(400, "missing 'username' field"),
        };
        let password = match json_string_field(&payload, "password") {
            Some(p) => p,
            None => return json_error(400, "missing 'password' field"),
        };
        let role_str = json_string_field(&payload, "role").unwrap_or_else(|| "read".to_string());
        let role = match crate::auth::Role::from_str(&role_str) {
            Some(r) => r,
            None => {
                return json_error(
                    400,
                    format!("invalid role '{}': must be read, write, or admin", role_str),
                )
            }
        };

        // Resolve the target tenant. Path override (e.g. POST
        // /auth/tenants/:tenant/users) wins over the body field; the
        // caller's scope then clamps a tenant-scoped admin.
        let body_tenant = json_string_field(&payload, "tenant_id");
        let mut target_tenant = tenant_path_override.map(|s| s.to_string()).or(body_tenant);

        let caller = resolve_auth_caller(self, headers);
        match &caller {
            Some(c) if c.is_platform_admin() => {
                // Platform admin: honor whatever they asked for.
            }
            Some(c) if c.role.can_admin() => {
                // Tenant-scoped admin: clamp to their tenant.
                target_tenant = c.id.tenant.clone();
            }
            Some(_) => {
                return json_error(403, "admin role required to create users");
            }
            None => {
                // No bearer present: only allowed when require_auth is
                // off (the routing gate already handled this for the
                // auth-disabled case). Fall through and treat as
                // platform admin so existing dev workflows work.
                if auth_store.is_enabled() && auth_store.config().require_auth {
                    return json_error(401, "authentication required");
                }
            }
        }

        let target_id = UserId::from_parts(target_tenant.as_deref(), &username);
        if let Some(response) = authorize_user_lifecycle_mutation(
            auth_store,
            caller.as_ref(),
            "user:create",
            &target_id,
        ) {
            return response;
        }

        match auth_store.create_user_in_tenant(target_tenant.as_deref(), &username, &password, role)
        {
            Ok(user) => {
                let principal = UserId::from_parts(user.tenant_id.as_deref(), &user.username);
                tracing::info!(
                    target: "reddb::http_auth",
                    principal = %principal,
                    "create_user ok"
                );
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert(
                    "username".to_string(),
                    JsonValue::String(user.username.clone()),
                );
                if let Some(t) = &user.tenant_id {
                    object.insert("tenant_id".to_string(), JsonValue::String(t.clone()));
                }
                object.insert("role".to_string(), JsonValue::String(user.role.to_string()));
                object.insert("enabled".to_string(), JsonValue::Bool(user.enabled));
                json_response(201, JsonValue::Object(object))
            }
            Err(err) => json_error(409, err.to_string()),
        }
    }

    /// GET /auth/users
    ///
    /// Lists users (password hashes are redacted). Filtering rules:
    ///   * Platform admin sees every user. Pass `?tenant=acme` to
    ///     filter; pass `?tenant=` (empty) to filter to platform
    ///     users only.
    ///   * Tenant-scoped admin sees only their tenant's users; the
    ///     query param is ignored.
    pub(crate) fn handle_auth_list_users(
        &self,
        headers: &BTreeMap<String, String>,
        query: &BTreeMap<String, String>,
    ) -> HttpResponse {
        let auth_store = match &self.auth_store {
            Some(store) => store,
            None => return json_error(501, "authentication is not configured"),
        };

        let caller = resolve_auth_caller(self, headers);
        let tenant_filter: Option<Option<&str>> = match &caller {
            Some(c) if c.is_platform_admin() => {
                // Platform admin: optional ?tenant= filter.
                match query.get("tenant").map(|s| s.as_str()) {
                    None => None,           // all tenants
                    Some("") => Some(None), // platform only
                    Some(t) => Some(Some(t)),
                }
            }
            Some(c) => {
                // Tenant-scoped: clamp to caller's tenant.
                Some(c.id.tenant.as_deref())
            }
            None => None,
        };

        let users = auth_store.list_users_scoped(tenant_filter);
        let user_values: Vec<JsonValue> = users
            .iter()
            .map(|u| {
                let mut obj = Map::new();
                obj.insert(
                    "username".to_string(),
                    JsonValue::String(u.username.clone()),
                );
                if let Some(t) = &u.tenant_id {
                    obj.insert("tenant_id".to_string(), JsonValue::String(t.clone()));
                }
                obj.insert("role".to_string(), JsonValue::String(u.role.to_string()));
                obj.insert("enabled".to_string(), JsonValue::Bool(u.enabled));
                obj.insert(
                    "created_at".to_string(),
                    JsonValue::Number(u.created_at as f64),
                );
                obj.insert(
                    "updated_at".to_string(),
                    JsonValue::Number(u.updated_at as f64),
                );
                let keys: Vec<JsonValue> = u
                    .api_keys
                    .iter()
                    .map(|k| {
                        let mut key_obj = Map::new();
                        key_obj.insert("name".to_string(), JsonValue::String(k.name.clone()));
                        key_obj.insert("role".to_string(), JsonValue::String(k.role.to_string()));
                        key_obj.insert(
                            "created_at".to_string(),
                            JsonValue::Number(k.created_at as f64),
                        );
                        JsonValue::Object(key_obj)
                    })
                    .collect();
                obj.insert("api_keys".to_string(), JsonValue::Array(keys));
                JsonValue::Object(obj)
            })
            .collect();

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert("users".to_string(), JsonValue::Array(user_values));
        json_response(200, JsonValue::Object(object))
    }

    /// DELETE /auth/users/:username
    /// DELETE /auth/tenants/:tenant/users/:username
    ///
    /// Deletes a user and revokes all their API keys and sessions.
    /// A tenant-scoped admin can only delete users in their own
    /// tenant; a platform admin can target any tenant via the
    /// `?tenant=` query param or the `/auth/tenants/:tenant/users/:u`
    /// alias.
    pub(crate) fn handle_auth_delete_user(
        &self,
        headers: &BTreeMap<String, String>,
        query: &BTreeMap<String, String>,
        tenant_path_override: Option<&str>,
        username: &str,
    ) -> HttpResponse {
        let auth_store = match &self.auth_store {
            Some(store) => store,
            None => return json_error(501, "authentication is not configured"),
        };

        let caller = resolve_auth_caller(self, headers);
        let target_tenant: Option<String> = match &caller {
            Some(c) if c.is_platform_admin() => tenant_path_override
                .map(|s| s.to_string())
                .or_else(|| query.get("tenant").cloned()),
            Some(c) if c.role.can_admin() => c.id.tenant.clone(),
            Some(_) => {
                return json_error(403, "admin role required");
            }
            None => {
                if auth_store.is_enabled() && auth_store.config().require_auth {
                    return json_error(401, "authentication required");
                }
                tenant_path_override
                    .map(|s| s.to_string())
                    .or_else(|| query.get("tenant").cloned())
            }
        };

        let principal = UserId::from_parts(target_tenant.as_deref(), username);
        if let Some(response) = authorize_user_lifecycle_mutation(
            auth_store,
            caller.as_ref(),
            "user:delete",
            &principal,
        ) {
            return response;
        }
        match auth_store.delete_user_in_tenant(target_tenant.as_deref(), username) {
            Ok(()) => {
                tracing::info!(
                    target: "reddb::http_auth",
                    principal = %principal,
                    "delete_user ok"
                );
                json_ok(format!("user '{}' deleted", principal))
            }
            Err(err) => json_error(404, err.to_string()),
        }
    }

    /// POST /auth/api-keys
    ///
    /// Creates a new API key for a user.
    /// Body: `{ "username": "alice", "name": "ci-deploy", "role": "write" }`
    pub(crate) fn handle_auth_create_api_key(&self, body: Vec<u8>) -> HttpResponse {
        let auth_store = match &self.auth_store {
            Some(store) => store,
            None => return json_error(501, "authentication is not configured"),
        };

        let payload = match parse_json_body(&body) {
            Ok(value) => value,
            Err(resp) => return resp,
        };

        let username = match json_string_field(&payload, "username") {
            Some(u) => u,
            None => return json_error(400, "missing 'username' field"),
        };
        let name = json_string_field(&payload, "name").unwrap_or_else(|| "unnamed".to_string());
        let role_str = json_string_field(&payload, "role").unwrap_or_else(|| "read".to_string());
        let role = match crate::auth::Role::from_str(&role_str) {
            Some(r) => r,
            None => {
                return json_error(
                    400,
                    format!("invalid role '{}': must be read, write, or admin", role_str),
                )
            }
        };

        match auth_store.create_api_key(&username, &name, role) {
            Ok(api_key) => {
                let mut object = Map::new();
                object.insert("ok".to_string(), JsonValue::Bool(true));
                object.insert("key".to_string(), JsonValue::String(api_key.key.clone()));
                object.insert("name".to_string(), JsonValue::String(api_key.name.clone()));
                object.insert(
                    "role".to_string(),
                    JsonValue::String(api_key.role.to_string()),
                );
                json_response(201, JsonValue::Object(object))
            }
            Err(err) => json_error(400, err.to_string()),
        }
    }

    /// DELETE /auth/api-keys/:key
    ///
    /// Revokes an API key.
    pub(crate) fn handle_auth_revoke_api_key(&self, key: &str) -> HttpResponse {
        let auth_store = match &self.auth_store {
            Some(store) => store,
            None => return json_error(501, "authentication is not configured"),
        };

        match auth_store.revoke_api_key(key) {
            Ok(()) => json_ok("API key revoked"),
            Err(err) => json_error(404, err.to_string()),
        }
    }

    /// POST /auth/change-password
    ///
    /// Changes a user's password.
    /// Body: `{ "username": "alice", "old_password": "old", "new_password": "new" }`
    pub(crate) fn handle_auth_change_password(
        &self,
        headers: &BTreeMap<String, String>,
        body: Vec<u8>,
    ) -> HttpResponse {
        let auth_store = match &self.auth_store {
            Some(store) => store,
            None => return json_error(501, "authentication is not configured"),
        };

        let payload = match parse_json_body(&body) {
            Ok(value) => value,
            Err(resp) => return resp,
        };

        let username = match json_string_field(&payload, "username") {
            Some(u) => u,
            None => return json_error(400, "missing 'username' field"),
        };
        let old_password = match json_string_field(&payload, "old_password") {
            Some(p) => p,
            None => return json_error(400, "missing 'old_password' field"),
        };
        let new_password = match json_string_field(&payload, "new_password") {
            Some(p) => p,
            None => return json_error(400, "missing 'new_password' field"),
        };

        let caller = resolve_auth_caller(self, headers);
        let target = UserId::platform(username.clone());
        let changes_other_user = caller
            .as_ref()
            .map(|caller| caller.id != target)
            .unwrap_or(false);
        if changes_other_user {
            if let Some(response) = authorize_user_lifecycle_mutation(
                auth_store,
                caller.as_ref(),
                "user:password:change",
                &target,
            ) {
                return response;
            }
        }

        match auth_store.change_password(&username, &old_password, &new_password) {
            Ok(()) => json_ok("password changed"),
            Err(err) => json_error(400, err.to_string()),
        }
    }

    /// GET /auth/whoami
    ///
    /// Returns the current user's identity based on the auth token.
    pub(crate) fn handle_auth_whoami(&self, headers: &BTreeMap<String, String>) -> HttpResponse {
        let auth_store = match &self.auth_store {
            Some(store) => store,
            None => return json_error(501, "authentication is not configured"),
        };

        let token = headers
            .get("authorization")
            .and_then(|v| v.strip_prefix("Bearer "));

        match token {
            Some(tok) => {
                // Try OAuth-JWT first when token shape matches AND a
                // validator is configured; fall through to AuthStore
                // for opaque API keys / session tokens. Mirrors the
                // path in `routing::resolve_bearer_role` so /whoami
                // surfaces the same identity the rest of the surface
                // already authenticated.
                if super::routing::looks_like_jwt(tok) {
                    if let Some(validator) = self.runtime.oauth_validator() {
                        match crate::wire::redwire::auth::validate_oauth_jwt_full(&validator, tok) {
                            Ok((tenant, username, role)) => {
                                let mut object = Map::new();
                                object.insert("ok".to_string(), JsonValue::Bool(true));
                                object.insert("username".to_string(), JsonValue::String(username));
                                if let Some(t) = tenant {
                                    object.insert("tenant_id".to_string(), JsonValue::String(t));
                                }
                                object.insert(
                                    "role".to_string(),
                                    JsonValue::String(role.to_string()),
                                );
                                object.insert("authenticated".to_string(), JsonValue::Bool(true));
                                object.insert(
                                    "method".to_string(),
                                    JsonValue::String("oauth_jwt".into()),
                                );
                                return json_response(200, JsonValue::Object(object));
                            }
                            Err(_) => {
                                return json_error(401, "invalid or expired token");
                            }
                        }
                    }
                }
                match auth_store.validate_token_full(tok) {
                    Some((id, role)) => {
                        let mut object = Map::new();
                        object.insert("ok".to_string(), JsonValue::Bool(true));
                        object.insert(
                            "username".to_string(),
                            JsonValue::String(id.username.clone()),
                        );
                        if let Some(t) = id.tenant.clone() {
                            object.insert("tenant_id".to_string(), JsonValue::String(t));
                        }
                        object.insert("role".to_string(), JsonValue::String(role.to_string()));
                        object.insert("authenticated".to_string(), JsonValue::Bool(true));
                        json_response(200, JsonValue::Object(object))
                    }
                    None => json_error(401, "invalid or expired token"),
                }
            }
            None => {
                if auth_store.is_enabled() {
                    json_error(401, "no authorization token provided")
                } else {
                    let mut object = Map::new();
                    object.insert("ok".to_string(), JsonValue::Bool(true));
                    object.insert(
                        "username".to_string(),
                        JsonValue::String("anonymous".to_string()),
                    );
                    object.insert("role".to_string(), JsonValue::String("admin".to_string()));
                    object.insert("authenticated".to_string(), JsonValue::Bool(false));
                    object.insert(
                        "note".to_string(),
                        JsonValue::String(
                            "auth is disabled; all requests have admin access".to_string(),
                        ),
                    );
                    json_response(200, JsonValue::Object(object))
                }
            }
        }
    }

    /// GET /auth/tenants
    ///
    /// Red UI tenant-list contract. Returns the tenants visible to the
    /// current principal so the UI can render its tenant switcher and
    /// tenant-scoped tables without probing user/policy listings.
    ///
    /// Visibility rules (hybrid tenant model — see #739 thread):
    ///   * Platform admin (`tenant_id = None`, role `Admin`): every
    ///     tenant the AuthStore has *evidence* of. Tenants are derived
    ///     from the union of user `tenant_id`s and policy `tenant`
    ///     fields, deduplicated and sorted. The implicit platform
    ///     tenant is exposed as a separate entry with `"id": null`.
    ///   * Tenant-scoped principal: exactly their own tenant.
    ///   * Authenticated platform-scoped non-admin: only the platform
    ///     entry (`id: null`).
    ///   * Auth disabled / no token (when allowed): platform admin
    ///     treatment — every visible tenant.
    pub(crate) fn handle_auth_list_tenants(
        &self,
        headers: &BTreeMap<String, String>,
    ) -> HttpResponse {
        let auth_store = match &self.auth_store {
            Some(store) => store,
            None => return json_error(501, "authentication is not configured"),
        };

        let caller = resolve_auth_caller(self, headers);
        if caller.is_none() && auth_store.is_enabled() && auth_store.config().require_auth {
            return json_error(401, "authentication required");
        }

        // Compute the universe of known tenants from the AuthStore
        // (users + policies). Platform (`None`) is always represented
        // as a separate entry so the UI can render a stable row for it.
        let mut platform_seen = false;
        let mut tenants: BTreeSet<String> = BTreeSet::new();
        for user in auth_store.list_users() {
            match user.tenant_id {
                Some(t) => {
                    tenants.insert(t);
                }
                None => platform_seen = true,
            }
        }
        for policy in auth_store.list_policies() {
            match &policy.tenant {
                Some(t) => {
                    tenants.insert(t.clone());
                }
                None => platform_seen = true,
            }
        }

        let visible: Vec<Option<String>> = match &caller {
            Some(c) if c.is_platform_admin() => {
                let mut out: Vec<Option<String>> = Vec::new();
                if platform_seen {
                    out.push(None);
                }
                out.extend(tenants.into_iter().map(Some));
                out
            }
            Some(c) => match &c.id.tenant {
                Some(t) => vec![Some(t.clone())],
                None => vec![None],
            },
            None => {
                // Auth disabled (or require_auth off and no token) —
                // mirror platform-admin visibility.
                let mut out: Vec<Option<String>> = Vec::new();
                if platform_seen {
                    out.push(None);
                }
                out.extend(tenants.into_iter().map(Some));
                out
            }
        };

        let tenant_values: Vec<JsonValue> = visible
            .into_iter()
            .map(|id| {
                let mut obj = Map::new();
                match id {
                    Some(t) => {
                        obj.insert("id".to_string(), JsonValue::String(t));
                        obj.insert("scope".to_string(), JsonValue::String("tenant".to_string()));
                    }
                    None => {
                        obj.insert("id".to_string(), JsonValue::Null);
                        obj.insert(
                            "scope".to_string(),
                            JsonValue::String("platform".to_string()),
                        );
                    }
                }
                JsonValue::Object(obj)
            })
            .collect();

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert("tenants".to_string(), JsonValue::Array(tenant_values));
        json_response(200, JsonValue::Object(object))
    }

    /// GET /auth/policies
    ///
    /// Red UI policy-list contract. Returns policy summaries visible to
    /// the current principal. Statement bodies are intentionally
    /// summarised (counts only) — full policy bodies are an admin-only
    /// detail surface left to a future slice.
    ///
    /// Visibility rules:
    ///   * Platform admin: every policy.
    ///   * Tenant-scoped admin: tenant-attached policies for their
    ///     tenant + platform-wide policies (`tenant = None`) since
    ///     platform-wide rules affect their tenant too.
    ///   * Non-admin authenticated principal: only the policies that
    ///     resolve to *them* via `effective_policies` (group + user
    ///     attachments). This lets a user see which rules govern them
    ///     without leaking the full registry.
    ///   * Auth disabled: every policy.
    pub(crate) fn handle_auth_list_policies(
        &self,
        headers: &BTreeMap<String, String>,
    ) -> HttpResponse {
        let auth_store = match &self.auth_store {
            Some(store) => store,
            None => return json_error(501, "authentication is not configured"),
        };

        let caller = resolve_auth_caller(self, headers);
        if caller.is_none() && auth_store.is_enabled() && auth_store.config().require_auth {
            return json_error(401, "authentication required");
        }

        let all_policies = auth_store.list_policies();

        let visible: Vec<_> = match &caller {
            Some(c) if c.is_platform_admin() => all_policies,
            Some(c) if c.role.can_admin() => {
                let tenant = c.id.tenant.clone();
                all_policies
                    .into_iter()
                    .filter(|p| p.tenant.is_none() || p.tenant == tenant)
                    .collect()
            }
            Some(c) => {
                let effective = auth_store.effective_policies(&c.id);
                let allowed_ids: BTreeSet<String> =
                    effective.iter().map(|p| p.id.clone()).collect();
                all_policies
                    .into_iter()
                    .filter(|p| allowed_ids.contains(&p.id))
                    .collect()
            }
            None => all_policies,
        };

        let policy_values: Vec<JsonValue> = visible
            .iter()
            .map(|p| {
                let mut obj = Map::new();
                obj.insert("id".to_string(), JsonValue::String(p.id.clone()));
                obj.insert("version".to_string(), JsonValue::Number(p.version as f64));
                match &p.tenant {
                    Some(t) => {
                        obj.insert("tenant".to_string(), JsonValue::String(t.clone()));
                        obj.insert("scope".to_string(), JsonValue::String("tenant".to_string()));
                    }
                    None => {
                        obj.insert("tenant".to_string(), JsonValue::Null);
                        obj.insert(
                            "scope".to_string(),
                            JsonValue::String("platform".to_string()),
                        );
                    }
                }
                obj.insert(
                    "statement_count".to_string(),
                    JsonValue::Number(p.statements.len() as f64),
                );
                obj.insert(
                    "created_at".to_string(),
                    JsonValue::Number(p.created_at as f64),
                );
                obj.insert(
                    "updated_at".to_string(),
                    JsonValue::Number(p.updated_at as f64),
                );
                JsonValue::Object(obj)
            })
            .collect();

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert("policies".to_string(), JsonValue::Array(policy_values));
        json_response(200, JsonValue::Object(object))
    }

    /// POST /auth/can
    ///
    /// Batch authorization probe for the current principal.
    ///
    /// Body shape:
    /// ```text
    /// { "checks": [
    ///     { "action": "read", "resource": { "kind": "collection", "name": "accounts" } },
    ///     { "action": "write", "resource": { "kind": "collection", "name": "audit" },
    ///       "current_tenant": "acme" }
    ///   ]
    /// }
    /// ```
    /// A trivial single-check form is also accepted by promoting the
    /// top-level `action`/`resource`/`current_tenant` keys to a one-element
    /// `checks` array.
    ///
    /// Each result is `{ allowed, reason, action, resource }` where
    /// `reason` is a UI-safe explanation produced by the policy
    /// simulator (e.g. `"allow at p1.statement[0]"`,
    /// `"no statement matched (default deny)"`).
    pub(crate) fn handle_auth_can(
        &self,
        headers: &BTreeMap<String, String>,
        body: Vec<u8>,
    ) -> HttpResponse {
        let auth_store = match &self.auth_store {
            Some(store) => store,
            None => return json_error(501, "authentication is not configured"),
        };

        let payload = match parse_json_body(&body) {
            Ok(v) => v,
            Err(resp) => return resp,
        };

        // Resolve the caller, with auth-disabled treated as a platform-admin
        // probe (the UI uses this to introspect what's possible without a
        // bound principal during dev).
        let caller = resolve_auth_caller(self, headers);
        if caller.is_none() && auth_store.is_enabled() && auth_store.config().require_auth {
            return json_error(401, "authentication required");
        }

        // Extract the checks array — fall back to a single-check form
        // built from top-level fields.
        let checks: Vec<JsonValue> = match payload.get("checks") {
            Some(JsonValue::Array(arr)) => arr.clone(),
            Some(_) => {
                return json_error(400, "'checks' must be an array");
            }
            None => {
                if payload.get("action").is_some() {
                    vec![payload.clone()]
                } else {
                    return json_error(400, "missing 'checks' array or top-level 'action'");
                }
            }
        };

        if checks.is_empty() {
            return json_error(400, "'checks' must not be empty");
        }
        const MAX_CHECKS: usize = 64;
        if checks.len() > MAX_CHECKS {
            return json_error(
                400,
                format!("too many checks: {} > {MAX_CHECKS}", checks.len()),
            );
        }

        // The principal used for evaluation. When auth is disabled and
        // no caller is present, fabricate a synthetic platform-admin
        // principal so the probe still produces meaningful answers.
        let (principal, role): (UserId, Role) = match &caller {
            Some(c) => (c.id.clone(), c.role),
            None => (UserId::from_parts(None, "anonymous"), Role::Admin),
        };

        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);

        let mut results: Vec<JsonValue> = Vec::with_capacity(checks.len());
        for (idx, raw) in checks.iter().enumerate() {
            let obj = match raw {
                JsonValue::Object(m) => m,
                _ => {
                    results.push(check_error_json(idx, "check entries must be objects"));
                    continue;
                }
            };

            let action = match obj.get("action").and_then(JsonValue::as_str) {
                Some(s) if !s.is_empty() => s.to_string(),
                _ => {
                    results.push(check_error_json(idx, "missing 'action'"));
                    continue;
                }
            };

            let (kind, name, res_tenant) = match obj.get("resource") {
                Some(JsonValue::Object(res)) => {
                    let kind = res
                        .get("kind")
                        .and_then(JsonValue::as_str)
                        .unwrap_or("")
                        .to_string();
                    let name = res
                        .get("name")
                        .and_then(JsonValue::as_str)
                        .unwrap_or("")
                        .to_string();
                    let tenant = res
                        .get("tenant")
                        .and_then(JsonValue::as_str)
                        .map(|s| s.to_string());
                    if kind.is_empty() || name.is_empty() {
                        results.push(check_error_json(
                            idx,
                            "resource requires non-empty 'kind' and 'name'",
                        ));
                        continue;
                    }
                    (kind, name, tenant)
                }
                _ => {
                    results.push(check_error_json(idx, "missing 'resource' object"));
                    continue;
                }
            };

            let mut resource = ResourceRef::new(kind.clone(), name.clone());
            if let Some(t) = res_tenant.clone() {
                resource = resource.with_tenant(t);
            }

            let current_tenant = obj
                .get("current_tenant")
                .and_then(JsonValue::as_str)
                .map(|s| s.to_string())
                .or_else(|| principal.tenant.clone());

            let principal_is_admin_role = role == Role::Admin;
            let ctx = EvalContext {
                principal_tenant: principal.tenant.clone(),
                current_tenant: current_tenant.clone(),
                peer_ip: None,
                mfa_present: false,
                now_ms,
                principal_is_admin_role,
                principal_is_platform_scoped: principal.tenant.is_none(),
            };

            let allowed =
                auth_store.check_policy_authz_with_role(&principal, &action, &resource, &ctx, role);

            let sim = auth_store.simulate(
                &principal,
                &action,
                &resource,
                SimCtx {
                    current_tenant,
                    peer_ip: None,
                    mfa_present: false,
                    now_ms: Some(now_ms),
                },
            );
            let reason = match (&sim.decision, allowed) {
                // Simulator agrees with the runtime evaluator.
                (Decision::Allow { .. }, true) | (Decision::Deny { .. }, false) => sim.reason,
                // Default-deny but runtime granted via LegacyRbac role fallback.
                (Decision::DefaultDeny, true) => {
                    "allowed by role-based legacy fallback (no matching policy)".to_string()
                }
                (Decision::DefaultDeny, false) => sim.reason,
                // Should not happen — admin-bypass was retired — but
                // surface honestly rather than asserting.
                (Decision::AdminBypass, _) => "admin bypass".to_string(),
                // Simulator says allow but evaluator says deny (or vice
                // versa). Both are conceivable when mode + role disagree.
                (Decision::Allow { .. }, false) => {
                    format!("denied by enforcement policy ({})", sim.reason)
                }
                (Decision::Deny { .. }, true) => sim.reason,
            };

            let mut entry = Map::new();
            entry.insert("action".to_string(), JsonValue::String(action));
            let mut res_obj = Map::new();
            res_obj.insert("kind".to_string(), JsonValue::String(kind));
            res_obj.insert("name".to_string(), JsonValue::String(name));
            if let Some(t) = res_tenant {
                res_obj.insert("tenant".to_string(), JsonValue::String(t));
            }
            entry.insert("resource".to_string(), JsonValue::Object(res_obj));
            entry.insert("allowed".to_string(), JsonValue::Bool(allowed));
            entry.insert(
                "reason".to_string(),
                crate::json_field::SerializedJsonField::tainted(&reason),
            );
            results.push(JsonValue::Object(entry));
        }

        let mut object = Map::new();
        object.insert("ok".to_string(), JsonValue::Bool(true));
        object.insert("results".to_string(), JsonValue::Array(results));
        json_response(200, JsonValue::Object(object))
    }
}

fn check_error_json(index: usize, message: &str) -> JsonValue {
    let mut obj = Map::new();
    obj.insert("index".to_string(), JsonValue::Number(index as f64));
    obj.insert("allowed".to_string(), JsonValue::Bool(false));
    obj.insert(
        "error".to_string(),
        crate::json_field::SerializedJsonField::tainted(message),
    );
    JsonValue::Object(obj)
}
