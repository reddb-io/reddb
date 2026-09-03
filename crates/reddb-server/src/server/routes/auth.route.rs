use crate::server::route_catalog::{
    ListenerSurface, RouteAlias, RouteAudience, RouteAuth, RouteEntry, RouteGroupDefaults,
    RouteMethod, RouteMiddleware, RouteRegistry, RouteRequest, RouteStability,
};
use crate::server::routes::common::{
    ADMIN_TOKEN_MIDDLEWARE, PUBLIC_MIDDLEWARE, STANDARD_MIDDLEWARE,
};
use crate::server::*;

const AUTH_SURFACES: &[ListenerSurface] = &[ListenerSurface::Public];

const AUTH_PUBLIC: RouteGroupDefaults = RouteGroupDefaults {
    family: "auth",
    audience: RouteAudience::Client,
    auth: RouteAuth::Public,
    surfaces: AUTH_SURFACES,
    stability: RouteStability::Stable,
    middlewares: PUBLIC_MIDDLEWARE,
};

const AUTH_USER: RouteGroupDefaults = RouteGroupDefaults {
    family: "auth",
    audience: RouteAudience::Client,
    auth: RouteAuth::UserRequired,
    surfaces: AUTH_SURFACES,
    stability: RouteStability::Stable,
    middlewares: STANDARD_MIDDLEWARE,
};

const AUTH_ADMIN_TOKEN: RouteGroupDefaults = RouteGroupDefaults {
    family: "auth",
    audience: RouteAudience::Operator,
    auth: RouteAuth::AdminToken,
    surfaces: AUTH_SURFACES,
    stability: RouteStability::Internal,
    middlewares: ADMIN_TOKEN_MIDDLEWARE,
};

const BOOTSTRAP_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/auth/bootstrap",
    "canonical v1 auth path",
)];
const LOGIN_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/auth/login",
    "canonical v1 auth path",
)];
const BROWSER_LOGIN_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/auth/browser/login",
    "canonical v1 auth path",
)];
const BROWSER_REFRESH_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/auth/browser/refresh",
    "canonical v1 auth path",
)];
const BROWSER_LOGOUT_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/auth/browser/logout",
    "canonical v1 auth path",
)];
const USERS_GET_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Get,
    "/v1/auth/users",
    "canonical v1 auth path",
)];
const USERS_POST_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/auth/users",
    "canonical v1 auth path",
)];
const USER_DELETE_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Delete,
    "/v1/auth/users/:username",
    "canonical v1 auth path",
)];
const TENANTS_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Get,
    "/v1/auth/tenants",
    "canonical v1 auth path",
)];
const TENANT_USERS_GET_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Get,
    "/v1/auth/tenants/:tenant/users",
    "canonical v1 auth path",
)];
const TENANT_USERS_POST_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/auth/tenants/:tenant/users",
    "canonical v1 auth path",
)];
const TENANT_USER_DELETE_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Delete,
    "/v1/auth/tenants/:tenant/users/:username",
    "canonical v1 auth path",
)];
const POLICIES_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Get,
    "/v1/auth/policies",
    "canonical v1 auth path",
)];
const CAN_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/auth/can",
    "canonical v1 auth path",
)];
const API_KEYS_POST_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/auth/api-keys",
    "canonical v1 auth path",
)];
const API_KEYS_DELETE_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Delete,
    "/v1/auth/api-keys/:key",
    "canonical v1 auth path",
)];
const CHANGE_PASSWORD_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/auth/change-password",
    "canonical v1 auth path",
)];
const WHOAMI_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Get,
    "/v1/auth/whoami",
    "canonical v1 auth path",
)];
const CAPABILITIES_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Get,
    "/v1/auth/capabilities",
    "canonical v1 auth path",
)];
const ADMIN_USERS_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/admin/users",
    "canonical v1 admin path",
)];
const ADMIN_SYSTEM_USERS_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/admin/system-users",
    "canonical v1 admin path",
)];

const PUBLIC_AUTH_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases(
        "auth.bootstrap",
        RouteMethod::Post,
        "/auth/bootstrap",
        BOOTSTRAP_ALIASES,
        auth_bootstrap,
    ),
    RouteEntry::with_aliases(
        "auth.login",
        RouteMethod::Post,
        "/auth/login",
        LOGIN_ALIASES,
        auth_login,
    ),
    RouteEntry::with_aliases(
        "auth.browser.login",
        RouteMethod::Post,
        "/auth/browser/login",
        BROWSER_LOGIN_ALIASES,
        auth_browser_login,
    ),
    RouteEntry::with_aliases(
        "auth.browser.refresh",
        RouteMethod::Post,
        "/auth/browser/refresh",
        BROWSER_REFRESH_ALIASES,
        auth_browser_refresh,
    ),
    RouteEntry::with_aliases(
        "auth.browser.logout",
        RouteMethod::Post,
        "/auth/browser/logout",
        BROWSER_LOGOUT_ALIASES,
        auth_browser_logout,
    ),
    RouteEntry::with_aliases(
        "auth.capabilities",
        RouteMethod::Get,
        "/auth/capabilities",
        CAPABILITIES_ALIASES,
        auth_capabilities,
    ),
];

const USER_AUTH_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases(
        "auth.users.list",
        RouteMethod::Get,
        "/auth/users",
        USERS_GET_ALIASES,
        auth_users_list,
    ),
    RouteEntry::with_aliases(
        "auth.users.create",
        RouteMethod::Post,
        "/auth/users",
        USERS_POST_ALIASES,
        auth_users_create,
    ),
    RouteEntry::with_aliases(
        "auth.users.delete",
        RouteMethod::Delete,
        "/auth/users/:username",
        USER_DELETE_ALIASES,
        auth_users_delete,
    ),
    RouteEntry::with_aliases(
        "auth.tenants.list",
        RouteMethod::Get,
        "/auth/tenants",
        TENANTS_ALIASES,
        auth_tenants_list,
    ),
    RouteEntry::with_aliases(
        "auth.tenant_users.list",
        RouteMethod::Get,
        "/auth/tenants/:tenant/users",
        TENANT_USERS_GET_ALIASES,
        auth_tenant_users_list,
    ),
    RouteEntry::with_aliases(
        "auth.tenant_users.create",
        RouteMethod::Post,
        "/auth/tenants/:tenant/users",
        TENANT_USERS_POST_ALIASES,
        auth_tenant_users_create,
    ),
    RouteEntry::with_aliases(
        "auth.tenant_users.delete",
        RouteMethod::Delete,
        "/auth/tenants/:tenant/users/:username",
        TENANT_USER_DELETE_ALIASES,
        auth_tenant_users_delete,
    ),
    RouteEntry::with_aliases(
        "auth.policies.list",
        RouteMethod::Get,
        "/auth/policies",
        POLICIES_ALIASES,
        auth_policies_list,
    ),
    RouteEntry::with_aliases(
        "auth.can",
        RouteMethod::Post,
        "/auth/can",
        CAN_ALIASES,
        auth_can,
    ),
    RouteEntry::with_aliases(
        "auth.api_keys.create",
        RouteMethod::Post,
        "/auth/api-keys",
        API_KEYS_POST_ALIASES,
        auth_api_keys_create,
    ),
    RouteEntry::with_aliases(
        "auth.api_keys.delete",
        RouteMethod::Delete,
        "/auth/api-keys/:key",
        API_KEYS_DELETE_ALIASES,
        auth_api_keys_delete,
    ),
    RouteEntry::with_aliases(
        "auth.change_password",
        RouteMethod::Post,
        "/auth/change-password",
        CHANGE_PASSWORD_ALIASES,
        auth_change_password,
    ),
    RouteEntry::with_aliases(
        "auth.whoami",
        RouteMethod::Get,
        "/auth/whoami",
        WHOAMI_ALIASES,
        auth_whoami,
    ),
];

const ADMIN_AUTH_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases(
        "auth.admin.users.create",
        RouteMethod::Post,
        "/v1/_admin/users",
        ADMIN_USERS_ALIASES,
        auth_admin_users_create,
    ),
    RouteEntry::with_aliases(
        "auth.admin.system_users.create",
        RouteMethod::Post,
        "/v1/_admin/system-users",
        ADMIN_SYSTEM_USERS_ALIASES,
        auth_admin_users_create,
    ),
];

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.routes(AUTH_PUBLIC, PUBLIC_AUTH_ROUTES);
    registry.routes(AUTH_USER, USER_AUTH_ROUTES);
    registry.routes(AUTH_ADMIN_TOKEN, ADMIN_AUTH_ROUTES);
}

// Handlers. Each route above binds one of these by fn pointer, so a
// declared route always has a live handler behind it.

/// Credential endpoints accept only `Content-Type: application/json`.
/// They parsed whatever arrived, so a cross-site form or `text/plain`
/// POST — the simple-request shapes a browser sends without a preflight —
/// could drive a login or a bootstrap from another origin.
fn require_json_body(req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let content_type = req
        .headers
        .get("content-type")
        .map(String::as_str)
        .unwrap_or_default();
    if content_type
        .trim()
        .to_ascii_lowercase()
        .starts_with("application/json")
    {
        None
    } else {
        Some(json_error(
            415,
            "credential endpoints require Content-Type: application/json",
        ))
    }
}

fn auth_bootstrap(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    if let Some(deny) = require_json_body(req) {
        return Some(deny);
    }
    Some(server.handle_auth_bootstrap(req.headers, req.body.to_vec()))
}

fn auth_login(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    if let Some(deny) = require_json_body(req) {
        return Some(deny);
    }
    Some(server.handle_auth_login(req.body.to_vec()))
}

fn auth_browser_login(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    if let Some(deny) = require_json_body(req) {
        return Some(deny);
    }
    Some(server.handle_browser_login(req.body.to_vec()))
}

fn auth_browser_refresh(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_browser_refresh(req.headers))
}

fn auth_browser_logout(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_browser_logout(req.headers))
}

fn auth_capabilities(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_auth_capabilities(req.headers))
}

fn auth_users_list(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_auth_list_users(req.headers, req.query))
}

fn auth_users_create(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_auth_create_user(req.headers, req.body.to_vec(), None))
}

fn auth_users_delete(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let username = req.matched.params.get("username")?;
    Some(server.handle_auth_delete_user(req.headers, req.query, None, username))
}

fn auth_tenants_list(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_auth_list_tenants(req.headers))
}

fn auth_tenant_users_list(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let tenant = req.matched.params.get("tenant")?;
    let mut tenant_query = req.query.clone();
    tenant_query.insert("tenant".to_string(), tenant.to_string());
    Some(server.handle_auth_list_users(req.headers, &tenant_query))
}

fn auth_tenant_users_create(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let tenant = req.matched.params.get("tenant")?;
    Some(server.handle_auth_create_user(req.headers, req.body.to_vec(), Some(tenant)))
}

fn auth_tenant_users_delete(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let tenant = req.matched.params.get("tenant")?;
    let username = req.matched.params.get("username")?;
    Some(server.handle_auth_delete_user(req.headers, req.query, Some(tenant), username))
}

fn auth_policies_list(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_auth_list_policies(req.headers))
}

fn auth_can(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_auth_can(req.headers, req.body.to_vec()))
}

fn auth_api_keys_create(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_auth_create_api_key(req.headers, req.body.to_vec()))
}

fn auth_api_keys_delete(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let key = req.matched.params.get("key")?;
    Some(server.handle_auth_revoke_api_key(req.headers, key))
}

fn auth_change_password(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_auth_change_password(req.headers, req.body.to_vec()))
}

fn auth_whoami(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_auth_whoami(req.headers))
}

fn auth_admin_users_create(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_admin_create_user(req.body.to_vec()))
}
