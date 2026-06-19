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

macro_rules! admin_aliases {
    ($method:expr, $pattern:expr) => {
        &[RouteAlias::canonical(
            $method,
            $pattern,
            "canonical v1 admin path",
        )]
    };
}

macro_rules! ops_aliases {
    ($method:expr, $pattern:expr) => {
        &[RouteAlias::canonical(
            $method,
            $pattern,
            "canonical v1 ops path",
        )]
    };
}

const ADMIN_MUTATION_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases(
        "admin.shutdown",
        RouteMethod::Post,
        "/admin/shutdown",
        admin_aliases!(RouteMethod::Post, "/v1/admin/shutdown"),
    ),
    RouteEntry::with_aliases(
        "admin.drain",
        RouteMethod::Post,
        "/admin/drain",
        admin_aliases!(RouteMethod::Post, "/v1/admin/drain"),
    ),
    RouteEntry::with_aliases(
        "admin.restore",
        RouteMethod::Post,
        "/admin/restore",
        admin_aliases!(RouteMethod::Post, "/v1/admin/restore"),
    ),
    RouteEntry::with_aliases(
        "admin.backup",
        RouteMethod::Post,
        "/admin/backup",
        admin_aliases!(RouteMethod::Post, "/v1/admin/backup"),
    ),
    RouteEntry::with_aliases(
        "admin.readonly",
        RouteMethod::Post,
        "/admin/readonly",
        admin_aliases!(RouteMethod::Post, "/v1/admin/readonly"),
    ),
    RouteEntry::with_aliases(
        "admin.blob_cache.sweep",
        RouteMethod::Post,
        "/admin/blob_cache/sweep",
        admin_aliases!(RouteMethod::Post, "/v1/admin/blob-cache/sweep"),
    ),
    RouteEntry::with_aliases(
        "admin.blob_cache.flush_namespace",
        RouteMethod::Post,
        "/admin/blob_cache/flush_namespace",
        admin_aliases!(RouteMethod::Post, "/v1/admin/blob-cache/flush-namespace"),
    ),
    RouteEntry::with_aliases(
        "admin.cache.compare_and_set",
        RouteMethod::Post,
        "/admin/cache/compare-and-set",
        admin_aliases!(RouteMethod::Post, "/v1/admin/cache/compare-and-set"),
    ),
    RouteEntry::with_aliases(
        "admin.failover.promote",
        RouteMethod::Post,
        "/admin/failover/promote",
        admin_aliases!(RouteMethod::Post, "/v1/admin/failover/promote"),
    ),
    RouteEntry::with_aliases(
        "admin.replication.confirm_rewind",
        RouteMethod::Post,
        "/admin/replication/rejoin/confirm-rewind",
        admin_aliases!(
            RouteMethod::Post,
            "/v1/admin/replication/rejoin/confirm-rewind"
        ),
    ),
];

const ADMIN_POLICY_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases(
        "admin.audit",
        RouteMethod::Get,
        "/admin/audit",
        admin_aliases!(RouteMethod::Get, "/v1/admin/audit"),
    ),
    RouteEntry::with_aliases(
        "admin.policies.list",
        RouteMethod::Get,
        "/admin/policies",
        admin_aliases!(RouteMethod::Get, "/v1/admin/policies"),
    ),
    RouteEntry::with_aliases(
        "admin.policies.simulate",
        RouteMethod::Post,
        "/admin/policies/simulate",
        admin_aliases!(RouteMethod::Post, "/v1/admin/policies/simulate"),
    ),
    RouteEntry::with_aliases(
        "admin.policies.lint",
        RouteMethod::Post,
        "/admin/policies/lint",
        admin_aliases!(RouteMethod::Post, "/v1/admin/policies/lint"),
    ),
    RouteEntry::with_aliases(
        "admin.policies.migrate_mode",
        RouteMethod::Post,
        "/admin/policies/migrate-mode",
        admin_aliases!(RouteMethod::Post, "/v1/admin/policies/migrate-mode"),
    ),
    RouteEntry::with_aliases(
        "admin.policies.actions",
        RouteMethod::Get,
        "/admin/policies/actions",
        admin_aliases!(RouteMethod::Get, "/v1/admin/policies/actions"),
    ),
    RouteEntry::with_aliases(
        "admin.policies.put",
        RouteMethod::Put,
        "/admin/policies/:id",
        admin_aliases!(RouteMethod::Put, "/v1/admin/policies/:id"),
    ),
    RouteEntry::with_aliases(
        "admin.policies.get",
        RouteMethod::Get,
        "/admin/policies/:id",
        admin_aliases!(RouteMethod::Get, "/v1/admin/policies/:id"),
    ),
    RouteEntry::with_aliases(
        "admin.policies.delete",
        RouteMethod::Delete,
        "/admin/policies/:id",
        admin_aliases!(RouteMethod::Delete, "/v1/admin/policies/:id"),
    ),
    RouteEntry::with_aliases(
        "admin.users.effective_permissions",
        RouteMethod::Get,
        "/admin/users/:user/effective-permissions",
        admin_aliases!(
            RouteMethod::Get,
            "/v1/admin/users/:user/effective-permissions"
        ),
    ),
    RouteEntry::with_aliases(
        "admin.users.groups.add",
        RouteMethod::Put,
        "/admin/users/:user/groups/:group",
        admin_aliases!(RouteMethod::Put, "/v1/admin/users/:user/groups/:group"),
    ),
    RouteEntry::with_aliases(
        "admin.users.groups.remove",
        RouteMethod::Delete,
        "/admin/users/:user/groups/:group",
        admin_aliases!(RouteMethod::Delete, "/v1/admin/users/:user/groups/:group"),
    ),
    RouteEntry::with_aliases(
        "admin.users.policies.attach",
        RouteMethod::Put,
        "/admin/users/:user/policies/:policy",
        admin_aliases!(RouteMethod::Put, "/v1/admin/users/:user/policies/:policy"),
    ),
    RouteEntry::with_aliases(
        "admin.users.policies.detach",
        RouteMethod::Delete,
        "/admin/users/:user/policies/:policy",
        admin_aliases!(
            RouteMethod::Delete,
            "/v1/admin/users/:user/policies/:policy"
        ),
    ),
    RouteEntry::with_aliases(
        "admin.groups.policies.attach",
        RouteMethod::Put,
        "/admin/groups/:group/policies/:policy",
        admin_aliases!(
            RouteMethod::Put,
            "/v1/admin/groups/:group/policies/:policy"
        ),
    ),
    RouteEntry::with_aliases(
        "admin.groups.policies.detach",
        RouteMethod::Delete,
        "/admin/groups/:group/policies/:policy",
        admin_aliases!(
            RouteMethod::Delete,
            "/v1/admin/groups/:group/policies/:policy"
        ),
    ),
];

const OPS_READ_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases(
        "admin.status",
        RouteMethod::Get,
        "/admin/status",
        ADMIN_STATUS_ALIASES,
    ),
    RouteEntry::with_aliases(
        "admin.blob_cache.stats",
        RouteMethod::Get,
        "/admin/blob_cache/stats",
        ops_aliases!(RouteMethod::Get, "/v1/ops/blob-cache/stats"),
    ),
    RouteEntry::with_aliases(
        "ops.ec.status",
        RouteMethod::Get,
        "/ec/status",
        ops_aliases!(RouteMethod::Get, "/v1/ops/ec/status"),
    ),
    RouteEntry::with_aliases(
        "ops.backup.status",
        RouteMethod::Get,
        "/backup/status",
        ops_aliases!(RouteMethod::Get, "/v1/ops/backup/status"),
    ),
    RouteEntry::with_aliases(
        "ops.backup.trigger",
        RouteMethod::Post,
        "/backup/trigger",
        ops_aliases!(RouteMethod::Post, "/v1/ops/backup/trigger"),
    ),
    RouteEntry::with_aliases(
        "ops.recovery.restore_points",
        RouteMethod::Get,
        "/recovery/restore-points",
        ops_aliases!(RouteMethod::Get, "/v1/ops/recovery/restore-points"),
    ),
    RouteEntry::with_aliases(
        "ops.replication.status",
        RouteMethod::Get,
        "/replication/status",
        ops_aliases!(RouteMethod::Get, "/v1/ops/replication/status"),
    ),
    RouteEntry::with_aliases(
        "ops.replication.snapshot",
        RouteMethod::Post,
        "/replication/snapshot",
        ops_aliases!(RouteMethod::Post, "/v1/ops/replication/snapshot"),
    ),
    RouteEntry::with_aliases(
        "ops.topology.graph",
        RouteMethod::Get,
        "/v1/topology/graph",
        ops_aliases!(RouteMethod::Get, "/v1/ops/topology/graph"),
    ),
    RouteEntry::with_aliases(
        "ops.cluster.status",
        RouteMethod::Get,
        "/cluster/status",
        CLUSTER_STATUS_ALIASES,
    ),
    RouteEntry::with_aliases(
        "ops.deployment.profiles",
        RouteMethod::Get,
        "/deployment/profiles",
        ops_aliases!(RouteMethod::Get, "/v1/ops/deployment/profiles"),
    ),
    RouteEntry::new("ops.grpc.discovery", RouteMethod::Get, "/grpc"),
    RouteEntry::with_aliases(
        "ops.cdc.changes",
        RouteMethod::Get,
        "/changes",
        ops_aliases!(RouteMethod::Get, "/v1/ops/cdc/changes"),
    ),
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
