use crate::server::route_catalog::{
    ListenerSurface, RouteAlias, RouteAudience, RouteAuth, RouteEntry, RouteGroupDefaults,
    RouteMethod, RouteMiddleware, RouteRegistry, RouteStability,
};
use crate::server::routes::common::{ADMIN_TOKEN_MIDDLEWARE, PUBLIC_MIDDLEWARE, STANDARD_MIDDLEWARE};

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
    ),
    RouteEntry::with_aliases("auth.login", RouteMethod::Post, "/auth/login", LOGIN_ALIASES),
    RouteEntry::with_aliases(
        "auth.browser.login",
        RouteMethod::Post,
        "/auth/browser/login",
        BROWSER_LOGIN_ALIASES,
    ),
    RouteEntry::with_aliases(
        "auth.browser.refresh",
        RouteMethod::Post,
        "/auth/browser/refresh",
        BROWSER_REFRESH_ALIASES,
    ),
    RouteEntry::with_aliases(
        "auth.browser.logout",
        RouteMethod::Post,
        "/auth/browser/logout",
        BROWSER_LOGOUT_ALIASES,
    ),
    RouteEntry::with_aliases(
        "auth.capabilities",
        RouteMethod::Get,
        "/auth/capabilities",
        CAPABILITIES_ALIASES,
    ),
];

const USER_AUTH_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases("auth.users.list", RouteMethod::Get, "/auth/users", USERS_GET_ALIASES),
    RouteEntry::with_aliases(
        "auth.users.create",
        RouteMethod::Post,
        "/auth/users",
        USERS_POST_ALIASES,
    ),
    RouteEntry::with_aliases(
        "auth.users.delete",
        RouteMethod::Delete,
        "/auth/users/:username",
        USER_DELETE_ALIASES,
    ),
    RouteEntry::with_aliases("auth.tenants.list", RouteMethod::Get, "/auth/tenants", TENANTS_ALIASES),
    RouteEntry::with_aliases(
        "auth.tenant_users.list",
        RouteMethod::Get,
        "/auth/tenants/:tenant/users",
        TENANT_USERS_GET_ALIASES,
    ),
    RouteEntry::with_aliases(
        "auth.tenant_users.create",
        RouteMethod::Post,
        "/auth/tenants/:tenant/users",
        TENANT_USERS_POST_ALIASES,
    ),
    RouteEntry::with_aliases(
        "auth.tenant_users.delete",
        RouteMethod::Delete,
        "/auth/tenants/:tenant/users/:username",
        TENANT_USER_DELETE_ALIASES,
    ),
    RouteEntry::with_aliases(
        "auth.policies.list",
        RouteMethod::Get,
        "/auth/policies",
        POLICIES_ALIASES,
    ),
    RouteEntry::with_aliases("auth.can", RouteMethod::Post, "/auth/can", CAN_ALIASES),
    RouteEntry::with_aliases(
        "auth.api_keys.create",
        RouteMethod::Post,
        "/auth/api-keys",
        API_KEYS_POST_ALIASES,
    ),
    RouteEntry::with_aliases(
        "auth.api_keys.delete",
        RouteMethod::Delete,
        "/auth/api-keys/:key",
        API_KEYS_DELETE_ALIASES,
    ),
    RouteEntry::with_aliases(
        "auth.change_password",
        RouteMethod::Post,
        "/auth/change-password",
        CHANGE_PASSWORD_ALIASES,
    ),
    RouteEntry::with_aliases("auth.whoami", RouteMethod::Get, "/auth/whoami", WHOAMI_ALIASES),
];

const ADMIN_AUTH_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases(
        "auth.admin.users.create",
        RouteMethod::Post,
        "/v1/_admin/users",
        ADMIN_USERS_ALIASES,
    ),
    RouteEntry::with_aliases(
        "auth.admin.system_users.create",
        RouteMethod::Post,
        "/v1/_admin/system-users",
        ADMIN_SYSTEM_USERS_ALIASES,
    ),
];

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.routes(AUTH_PUBLIC, PUBLIC_AUTH_ROUTES);
    registry.routes(AUTH_USER, USER_AUTH_ROUTES);
    registry.routes(AUTH_ADMIN_TOKEN, ADMIN_AUTH_ROUTES);
}
