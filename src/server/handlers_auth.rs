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
use crate::auth::UserId;

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
    pub(crate) fn handle_auth_change_password(&self, body: Vec<u8>) -> HttpResponse {
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
}
