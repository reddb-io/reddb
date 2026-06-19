use crate::server::route_catalog::{
    ListenerSurface, RouteAlias, RouteAudience, RouteAuth, RouteEntry, RouteGroupDefaults,
    RouteMethod, RouteMiddleware, RouteRegistry, RouteStability,
};
use crate::server::routes::common::{
    ADMIN_TOKEN_MIDDLEWARE, ALL_SURFACES, PUBLIC_ADMIN_SURFACES, PUBLIC_MIDDLEWARE,
};

const ADMIN_SURFACES: &[ListenerSurface] = &[ListenerSurface::Public, ListenerSurface::Admin];
const OPS_READ_CLUSTER_MIDDLEWARE: &[RouteMiddleware] = &[
    RouteMiddleware::CorsPreflight,
    RouteMiddleware::ListenerSurfaceGate,
    RouteMiddleware::AuthGate,
    RouteMiddleware::QuotaGate,
    RouteMiddleware::OpsPolicy("ops:read:cluster"),
];
const OPS_ADMIN_MIDDLEWARE: &[RouteMiddleware] = &[
    RouteMiddleware::CorsPreflight,
    RouteMiddleware::ListenerSurfaceGate,
    RouteMiddleware::AuthGate,
    RouteMiddleware::QuotaGate,
    RouteMiddleware::OpsPolicy("ops:admin"),
];

const ADMIN_MUTATION: RouteGroupDefaults = RouteGroupDefaults {
    family: "admin",
    audience: RouteAudience::Operator,
    auth: RouteAuth::AdminToken,
    surfaces: ADMIN_SURFACES,
    stability: RouteStability::Stable,
    middlewares: ADMIN_TOKEN_MIDDLEWARE,
};

const ADMIN_POLICY: RouteGroupDefaults = RouteGroupDefaults {
    family: "admin",
    audience: RouteAudience::Operator,
    auth: RouteAuth::OpsCapability("ops:admin"),
    surfaces: ADMIN_SURFACES,
    stability: RouteStability::Stable,
    middlewares: OPS_ADMIN_MIDDLEWARE,
};

const OPS_READ: RouteGroupDefaults = RouteGroupDefaults {
    family: "ops",
    audience: RouteAudience::Operator,
    auth: RouteAuth::OpsCapability("ops:read:cluster"),
    surfaces: PUBLIC_ADMIN_SURFACES,
    stability: RouteStability::Stable,
    middlewares: OPS_READ_CLUSTER_MIDDLEWARE,
};

const OPS_PUBLIC: RouteGroupDefaults = RouteGroupDefaults {
    family: "ops",
    audience: RouteAudience::Infra,
    auth: RouteAuth::Public,
    surfaces: ALL_SURFACES,
    stability: RouteStability::Stable,
    middlewares: PUBLIC_MIDDLEWARE,
};

const ADMIN_STATUS_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Get,
    "/v1/admin/status",
    "canonical v1 admin status path",
)];
const CLUSTER_STATUS_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Get,
    "/v1/ops/cluster/status",
    "canonical v1 ops cluster path",
)];
const CAPABILITIES_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Get,
    "/v1/capabilities",
    "canonical v1 capabilities path",
)];

const ADMIN_MUTATION_ROUTES: &[RouteEntry] = &[
    RouteEntry::new("admin.shutdown", RouteMethod::Post, "/admin/shutdown"),
    RouteEntry::new("admin.drain", RouteMethod::Post, "/admin/drain"),
    RouteEntry::new("admin.restore", RouteMethod::Post, "/admin/restore"),
    RouteEntry::new("admin.backup", RouteMethod::Post, "/admin/backup"),
    RouteEntry::new("admin.readonly", RouteMethod::Post, "/admin/readonly"),
    RouteEntry::new("admin.blob_cache.sweep", RouteMethod::Post, "/admin/blob_cache/sweep"),
    RouteEntry::new(
        "admin.blob_cache.flush_namespace",
        RouteMethod::Post,
        "/admin/blob_cache/flush_namespace",
    ),
    RouteEntry::new(
        "admin.cache.compare_and_set",
        RouteMethod::Post,
        "/admin/cache/compare-and-set",
    ),
    RouteEntry::new("admin.failover.promote", RouteMethod::Post, "/admin/failover/promote"),
    RouteEntry::new(
        "admin.replication.confirm_rewind",
        RouteMethod::Post,
        "/admin/replication/rejoin/confirm-rewind",
    ),
];

const ADMIN_POLICY_ROUTES: &[RouteEntry] = &[
    RouteEntry::new("admin.audit", RouteMethod::Get, "/admin/audit"),
    RouteEntry::new("admin.policies.list", RouteMethod::Get, "/admin/policies"),
    RouteEntry::new("admin.policies.put", RouteMethod::Put, "/admin/policies/:id"),
    RouteEntry::new("admin.policies.get", RouteMethod::Get, "/admin/policies/:id"),
    RouteEntry::new("admin.policies.delete", RouteMethod::Delete, "/admin/policies/:id"),
    RouteEntry::new("admin.policies.simulate", RouteMethod::Post, "/admin/policies/simulate"),
    RouteEntry::new("admin.policies.lint", RouteMethod::Post, "/admin/policies/lint"),
    RouteEntry::new(
        "admin.policies.migrate_mode",
        RouteMethod::Post,
        "/admin/policies/migrate-mode",
    ),
    RouteEntry::new("admin.policies.actions", RouteMethod::Get, "/admin/policies/actions"),
    RouteEntry::new(
        "admin.users.effective_permissions",
        RouteMethod::Get,
        "/admin/users/:user/effective-permissions",
    ),
    RouteEntry::new(
        "admin.users.groups.add",
        RouteMethod::Put,
        "/admin/users/:user/groups/:group",
    ),
    RouteEntry::new(
        "admin.users.groups.remove",
        RouteMethod::Delete,
        "/admin/users/:user/groups/:group",
    ),
    RouteEntry::new(
        "admin.users.policies.attach",
        RouteMethod::Put,
        "/admin/users/:user/policies/:policy",
    ),
    RouteEntry::new(
        "admin.users.policies.detach",
        RouteMethod::Delete,
        "/admin/users/:user/policies/:policy",
    ),
    RouteEntry::new(
        "admin.groups.policies.attach",
        RouteMethod::Put,
        "/admin/groups/:group/policies/:policy",
    ),
    RouteEntry::new(
        "admin.groups.policies.detach",
        RouteMethod::Delete,
        "/admin/groups/:group/policies/:policy",
    ),
];

const OPS_READ_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases(
        "admin.status",
        RouteMethod::Get,
        "/admin/status",
        ADMIN_STATUS_ALIASES,
    ),
    RouteEntry::new("admin.blob_cache.stats", RouteMethod::Get, "/admin/blob_cache/stats"),
    RouteEntry::new("ops.ec.status", RouteMethod::Get, "/ec/status"),
    RouteEntry::new("ops.backup.status", RouteMethod::Get, "/backup/status"),
    RouteEntry::new("ops.backup.trigger", RouteMethod::Post, "/backup/trigger"),
    RouteEntry::new("ops.recovery.restore_points", RouteMethod::Get, "/recovery/restore-points"),
    RouteEntry::new("ops.replication.status", RouteMethod::Get, "/replication/status"),
    RouteEntry::new("ops.replication.snapshot", RouteMethod::Post, "/replication/snapshot"),
    RouteEntry::new("ops.topology.graph", RouteMethod::Get, "/v1/topology/graph"),
    RouteEntry::with_aliases(
        "ops.cluster.status",
        RouteMethod::Get,
        "/cluster/status",
        CLUSTER_STATUS_ALIASES,
    ),
    RouteEntry::new("ops.deployment.profiles", RouteMethod::Get, "/deployment/profiles"),
    RouteEntry::new("ops.grpc.discovery", RouteMethod::Get, "/grpc"),
    RouteEntry::new("ops.cdc.changes", RouteMethod::Get, "/changes"),
];

const OPS_PUBLIC_ROUTES: &[RouteEntry] = &[
    RouteEntry::new("ops.health.aggregate", RouteMethod::Get, "/health"),
    RouteEntry::new("ops.ready.aggregate", RouteMethod::Get, "/ready"),
    RouteEntry::new("ops.ready.query", RouteMethod::Get, "/ready/query"),
    RouteEntry::new("ops.ready.write", RouteMethod::Get, "/ready/write"),
    RouteEntry::new("ops.ready.repair", RouteMethod::Get, "/ready/repair"),
    RouteEntry::new("ops.ready.serverless", RouteMethod::Get, "/ready/serverless"),
    RouteEntry::new(
        "ops.ready.serverless.query",
        RouteMethod::Get,
        "/ready/serverless/query",
    ),
    RouteEntry::new(
        "ops.ready.serverless.write",
        RouteMethod::Get,
        "/ready/serverless/write",
    ),
    RouteEntry::new(
        "ops.ready.serverless.repair",
        RouteMethod::Get,
        "/ready/serverless/repair",
    ),
    RouteEntry::with_aliases(
        "ops.capabilities",
        RouteMethod::Get,
        "/capabilities",
        CAPABILITIES_ALIASES,
    ),
];

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.routes(ADMIN_MUTATION, ADMIN_MUTATION_ROUTES);
    registry.routes(ADMIN_POLICY, ADMIN_POLICY_ROUTES);
    registry.routes(OPS_READ, OPS_READ_ROUTES);
    registry.routes(OPS_PUBLIC, OPS_PUBLIC_ROUTES);
}
