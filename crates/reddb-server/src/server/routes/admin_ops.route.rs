use crate::server::route_catalog::{
    ListenerSurface, RouteAlias, RouteAudience, RouteAuth, RouteEntry, RouteGroupDefaults,
    RouteMethod, RouteMiddleware, RouteRegistry, RouteRequest, RouteStability,
};
use crate::server::routes::common::{
    ADMIN_TOKEN_MIDDLEWARE, ALL_SURFACES, PUBLIC_ADMIN_SURFACES, PUBLIC_MIDDLEWARE,
    PUBLIC_NO_QUOTA_MIDDLEWARE, PUBLIC_SURFACES, STANDARD_MIDDLEWARE,
};
use crate::server::*;

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
    auth: RouteAuth::AdminToken,
    surfaces: ADMIN_SURFACES,
    stability: RouteStability::Stable,
    middlewares: ADMIN_TOKEN_MIDDLEWARE,
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

const OPS_PUBLIC_PROBE: RouteGroupDefaults = RouteGroupDefaults {
    family: "ops",
    audience: RouteAudience::Infra,
    auth: RouteAuth::Public,
    surfaces: ALL_SURFACES,
    stability: RouteStability::Stable,
    middlewares: PUBLIC_NO_QUOTA_MIDDLEWARE,
};

// Eventual-consistency per-field mutation + status endpoints. The legacy
// dispatcher served these from the untyped `/ec/{collection}/{field}/{action}`
// fallthrough (authenticated user, public listener only, quota-gated, no ops
// policy); the discovered group reproduces that surface/auth/middleware shape.
const OPS_EC_FIELD: RouteGroupDefaults = RouteGroupDefaults {
    family: "ops",
    audience: RouteAudience::Client,
    auth: RouteAuth::UserRequired,
    surfaces: PUBLIC_SURFACES,
    stability: RouteStability::Stable,
    middlewares: STANDARD_MIDDLEWARE,
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
        admin_shutdown,
    ),
    RouteEntry::with_aliases(
        "admin.drain",
        RouteMethod::Post,
        "/admin/drain",
        admin_aliases!(RouteMethod::Post, "/v1/admin/drain"),
        admin_drain,
    ),
    RouteEntry::with_aliases(
        "admin.restore",
        RouteMethod::Post,
        "/admin/restore",
        admin_aliases!(RouteMethod::Post, "/v1/admin/restore"),
        admin_restore,
    ),
    RouteEntry::with_aliases(
        "admin.backup",
        RouteMethod::Post,
        "/admin/backup",
        admin_aliases!(RouteMethod::Post, "/v1/admin/backup"),
        admin_backup,
    ),
    RouteEntry::with_aliases(
        "admin.readonly",
        RouteMethod::Post,
        "/admin/readonly",
        admin_aliases!(RouteMethod::Post, "/v1/admin/readonly"),
        admin_readonly,
    ),
    RouteEntry::with_aliases(
        "admin.blob_cache.sweep",
        RouteMethod::Post,
        "/admin/blob_cache/sweep",
        admin_aliases!(RouteMethod::Post, "/v1/admin/blob-cache/sweep"),
        admin_blob_cache_sweep,
    ),
    RouteEntry::with_aliases(
        "admin.blob_cache.flush_namespace",
        RouteMethod::Post,
        "/admin/blob_cache/flush_namespace",
        admin_aliases!(RouteMethod::Post, "/v1/admin/blob-cache/flush-namespace"),
        admin_blob_cache_flush_namespace,
    ),
    RouteEntry::with_aliases(
        "admin.cache.compare_and_set",
        RouteMethod::Post,
        "/admin/cache/compare-and-set",
        admin_aliases!(RouteMethod::Post, "/v1/admin/cache/compare-and-set"),
        admin_cache_compare_and_set,
    ),
    RouteEntry::with_aliases(
        "admin.failover.promote",
        RouteMethod::Post,
        "/admin/failover/promote",
        admin_aliases!(RouteMethod::Post, "/v1/admin/failover/promote"),
        admin_failover_promote,
    ),
    RouteEntry::with_aliases(
        "admin.replication.confirm_rewind",
        RouteMethod::Post,
        "/admin/replication/rejoin/confirm-rewind",
        admin_aliases!(
            RouteMethod::Post,
            "/v1/admin/replication/rejoin/confirm-rewind"
        ),
        admin_replication_confirm_rewind,
    ),
    // Taking a replication snapshot writes to the backend; it sat in the
    // read-capability group, so any read-role bearer could trigger one.
    RouteEntry::with_aliases(
        "ops.replication.snapshot",
        RouteMethod::Post,
        "/replication/snapshot",
        ops_aliases!(RouteMethod::Post, "/v1/ops/replication/snapshot"),
        ops_replication_snapshot,
    ),
];

const ADMIN_POLICY_AUDIT_ROUTES: &[RouteEntry] = &[RouteEntry::with_aliases(
    "admin.audit",
    RouteMethod::Get,
    "/admin/audit",
    admin_aliases!(RouteMethod::Get, "/v1/admin/audit"),
    admin_audit,
)];

const ADMIN_POLICY_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases(
        "admin.policies.list",
        RouteMethod::Get,
        "/admin/policies",
        admin_aliases!(RouteMethod::Get, "/v1/admin/policies"),
        admin_policies_list,
    ),
    RouteEntry::with_aliases(
        "admin.policies.simulate",
        RouteMethod::Post,
        "/admin/policies/simulate",
        admin_aliases!(RouteMethod::Post, "/v1/admin/policies/simulate"),
        admin_policies_simulate,
    ),
    RouteEntry::with_aliases(
        "admin.policies.lint",
        RouteMethod::Post,
        "/admin/policies/lint",
        admin_aliases!(RouteMethod::Post, "/v1/admin/policies/lint"),
        admin_policies_lint,
    ),
    RouteEntry::with_aliases(
        "admin.policies.migrate_mode",
        RouteMethod::Post,
        "/admin/policies/migrate-mode",
        admin_aliases!(RouteMethod::Post, "/v1/admin/policies/migrate-mode"),
        admin_policies_migrate_mode,
    ),
    RouteEntry::with_aliases(
        "admin.policies.actions",
        RouteMethod::Get,
        "/admin/policies/actions",
        admin_aliases!(RouteMethod::Get, "/v1/admin/policies/actions"),
        admin_policies_actions,
    ),
    RouteEntry::with_aliases(
        "admin.policies.put",
        RouteMethod::Put,
        "/admin/policies/:id",
        admin_aliases!(RouteMethod::Put, "/v1/admin/policies/:id"),
        admin_policies_put,
    ),
    RouteEntry::with_aliases(
        "admin.policies.get",
        RouteMethod::Get,
        "/admin/policies/:id",
        admin_aliases!(RouteMethod::Get, "/v1/admin/policies/:id"),
        admin_policies_get,
    ),
    RouteEntry::with_aliases(
        "admin.policies.delete",
        RouteMethod::Delete,
        "/admin/policies/:id",
        admin_aliases!(RouteMethod::Delete, "/v1/admin/policies/:id"),
        admin_policies_delete,
    ),
    RouteEntry::with_aliases(
        "admin.users.effective_permissions",
        RouteMethod::Get,
        "/admin/users/:user/effective-permissions",
        admin_aliases!(
            RouteMethod::Get,
            "/v1/admin/users/:user/effective-permissions"
        ),
        admin_users_effective_permissions,
    ),
    RouteEntry::with_aliases(
        "admin.users.groups.add",
        RouteMethod::Put,
        "/admin/users/:user/groups/:group",
        admin_aliases!(RouteMethod::Put, "/v1/admin/users/:user/groups/:group"),
        admin_users_groups_add,
    ),
    RouteEntry::with_aliases(
        "admin.users.groups.remove",
        RouteMethod::Delete,
        "/admin/users/:user/groups/:group",
        admin_aliases!(RouteMethod::Delete, "/v1/admin/users/:user/groups/:group"),
        admin_users_groups_remove,
    ),
    RouteEntry::with_aliases(
        "admin.users.policies.attach",
        RouteMethod::Put,
        "/admin/users/:user/policies/:policy",
        admin_aliases!(RouteMethod::Put, "/v1/admin/users/:user/policies/:policy"),
        admin_users_policies_attach,
    ),
    RouteEntry::with_aliases(
        "admin.users.policies.detach",
        RouteMethod::Delete,
        "/admin/users/:user/policies/:policy",
        admin_aliases!(
            RouteMethod::Delete,
            "/v1/admin/users/:user/policies/:policy"
        ),
        admin_users_policies_detach,
    ),
    RouteEntry::with_aliases(
        "admin.groups.policies.attach",
        RouteMethod::Put,
        "/admin/groups/:group/policies/:policy",
        admin_aliases!(RouteMethod::Put, "/v1/admin/groups/:group/policies/:policy"),
        admin_groups_policies_attach,
    ),
    RouteEntry::with_aliases(
        "admin.groups.policies.detach",
        RouteMethod::Delete,
        "/admin/groups/:group/policies/:policy",
        admin_aliases!(
            RouteMethod::Delete,
            "/v1/admin/groups/:group/policies/:policy"
        ),
        admin_groups_policies_detach,
    ),
];

const OPS_READ_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases(
        "admin.status",
        RouteMethod::Get,
        "/admin/status",
        ADMIN_STATUS_ALIASES,
        admin_status,
    ),
    RouteEntry::with_aliases(
        "admin.blob_cache.stats",
        RouteMethod::Get,
        "/admin/blob_cache/stats",
        ops_aliases!(RouteMethod::Get, "/v1/ops/blob-cache/stats"),
        admin_blob_cache_stats,
    ),
    RouteEntry::with_aliases(
        "ops.ec.status",
        RouteMethod::Get,
        "/ec/status",
        ops_aliases!(RouteMethod::Get, "/v1/ops/ec/status"),
        ops_ec_status,
    ),
    RouteEntry::with_aliases(
        "ops.backup.status",
        RouteMethod::Get,
        "/backup/status",
        ops_aliases!(RouteMethod::Get, "/v1/ops/backup/status"),
        ops_backup_status,
    ),
    RouteEntry::with_aliases(
        "ops.backup.trigger",
        RouteMethod::Post,
        "/backup/trigger",
        ops_aliases!(RouteMethod::Post, "/v1/ops/backup/trigger"),
        ops_backup_trigger,
    ),
    RouteEntry::with_aliases(
        "ops.recovery.restore_points",
        RouteMethod::Get,
        "/recovery/restore-points",
        ops_aliases!(RouteMethod::Get, "/v1/ops/recovery/restore-points"),
        ops_recovery_restore_points,
    ),
    RouteEntry::with_aliases(
        "ops.replication.status",
        RouteMethod::Get,
        "/replication/status",
        ops_aliases!(RouteMethod::Get, "/v1/ops/replication/status"),
        ops_replication_status,
    ),
    RouteEntry::with_aliases(
        "ops.topology.graph",
        RouteMethod::Get,
        "/v1/topology/graph",
        ops_aliases!(RouteMethod::Get, "/v1/ops/topology/graph"),
        ops_topology_graph,
    ),
    RouteEntry::with_aliases(
        "ops.cluster.status",
        RouteMethod::Get,
        "/cluster/status",
        CLUSTER_STATUS_ALIASES,
        ops_cluster_status,
    ),
    RouteEntry::with_aliases(
        "ops.deployment.profiles",
        RouteMethod::Get,
        "/deployment/profiles",
        ops_aliases!(RouteMethod::Get, "/v1/ops/deployment/profiles"),
        ops_deployment_profiles,
    ),
    RouteEntry::new(
        "ops.grpc.discovery",
        RouteMethod::Get,
        "/grpc",
        ops_grpc_discovery,
    ),
    RouteEntry::with_aliases(
        "ops.cdc.changes",
        RouteMethod::Get,
        "/changes",
        ops_aliases!(RouteMethod::Get, "/v1/ops/cdc/changes"),
        ops_cdc_changes,
    ),
];

const OPS_PUBLIC_PROBE_ROUTES: &[RouteEntry] = &[
    RouteEntry::new(
        "ops.health.aggregate",
        RouteMethod::Get,
        "/health",
        ops_health_aggregate,
    ),
    RouteEntry::new(
        "ops.ready.aggregate",
        RouteMethod::Get,
        "/ready",
        ops_ready_aggregate,
    ),
    RouteEntry::new(
        "ops.ready.query",
        RouteMethod::Get,
        "/ready/query",
        ops_ready_query,
    ),
    RouteEntry::new(
        "ops.ready.write",
        RouteMethod::Get,
        "/ready/write",
        ops_ready_write,
    ),
    RouteEntry::new(
        "ops.ready.repair",
        RouteMethod::Get,
        "/ready/repair",
        ops_ready_repair,
    ),
    RouteEntry::new(
        "ops.ready.serverless",
        RouteMethod::Get,
        "/ready/serverless",
        ops_ready_serverless,
    ),
    RouteEntry::new(
        "ops.ready.serverless.query",
        RouteMethod::Get,
        "/ready/serverless/query",
        ops_ready_serverless_query,
    ),
    RouteEntry::new(
        "ops.ready.serverless.write",
        RouteMethod::Get,
        "/ready/serverless/write",
        ops_ready_serverless_write,
    ),
    RouteEntry::new(
        "ops.ready.serverless.repair",
        RouteMethod::Get,
        "/ready/serverless/repair",
        ops_ready_serverless_repair,
    ),
];

const OPS_PUBLIC_ROUTES: &[RouteEntry] = &[RouteEntry::with_aliases(
    "ops.capabilities",
    RouteMethod::Get,
    "/capabilities",
    CAPABILITIES_ALIASES,
    ops_capabilities,
)];

const OPS_EC_FIELD_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases(
        "ops.ec.add",
        RouteMethod::Post,
        "/ec/:collection/:field/add",
        ops_aliases!(RouteMethod::Post, "/v1/ops/ec/:collection/:field/add"),
        ops_ec_add,
    ),
    RouteEntry::with_aliases(
        "ops.ec.sub",
        RouteMethod::Post,
        "/ec/:collection/:field/sub",
        ops_aliases!(RouteMethod::Post, "/v1/ops/ec/:collection/:field/sub"),
        ops_ec_add,
    ),
    RouteEntry::with_aliases(
        "ops.ec.set",
        RouteMethod::Post,
        "/ec/:collection/:field/set",
        ops_aliases!(RouteMethod::Post, "/v1/ops/ec/:collection/:field/set"),
        ops_ec_add,
    ),
    RouteEntry::with_aliases(
        "ops.ec.consolidate",
        RouteMethod::Post,
        "/ec/:collection/:field/consolidate",
        ops_aliases!(
            RouteMethod::Post,
            "/v1/ops/ec/:collection/:field/consolidate"
        ),
        ops_ec_consolidate,
    ),
    RouteEntry::with_aliases(
        "ops.ec.field_status",
        RouteMethod::Get,
        "/ec/:collection/:field/status",
        ops_aliases!(RouteMethod::Get, "/v1/ops/ec/:collection/:field/status"),
        ops_ec_field_status,
    ),
];

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.routes(ADMIN_MUTATION, ADMIN_MUTATION_ROUTES);
    registry.routes(
        RouteGroupDefaults {
            auth: RouteAuth::OpsCapability("ops:admin"),
            middlewares: OPS_ADMIN_MIDDLEWARE,
            ..ADMIN_POLICY
        },
        ADMIN_POLICY_AUDIT_ROUTES,
    );
    registry.routes(ADMIN_POLICY, ADMIN_POLICY_ROUTES);
    registry.routes(OPS_READ, OPS_READ_ROUTES);
    registry.routes(OPS_PUBLIC_PROBE, OPS_PUBLIC_PROBE_ROUTES);
    registry.routes(OPS_PUBLIC, OPS_PUBLIC_ROUTES);
    registry.routes(OPS_EC_FIELD, OPS_EC_FIELD_ROUTES);
}

// Handlers. Each route above binds one of these by fn pointer, so a
// declared route always has a live handler behind it.

fn admin_shutdown(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_admin_shutdown())
}

fn admin_drain(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_admin_drain())
}

fn admin_restore(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_admin_restore(req.body.to_vec()))
}

fn admin_backup(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_admin_backup(req.query))
}

fn admin_readonly(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_admin_readonly(req.body.to_vec()))
}

fn admin_blob_cache_sweep(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_admin_blob_cache_sweep(req.body.to_vec()))
}

fn admin_blob_cache_flush_namespace(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(server.handle_admin_blob_cache_flush_namespace(req.body.to_vec()))
}

fn admin_cache_compare_and_set(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(server.handle_admin_blob_cache_compare_and_set(req.body.to_vec()))
}

fn admin_failover_promote(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_admin_failover_promote(req.body.to_vec()))
}

fn admin_replication_confirm_rewind(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(server.handle_admin_replication_confirm_rewind(req.body.to_vec()))
}

fn admin_audit(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_admin_audit_query(req.query))
}

fn admin_policies_list(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_iam_policy_list())
}

fn admin_policies_put(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let id = req.matched.params.get("id")?;
    Some(server.handle_iam_policy_put(req.headers, id, req.body.to_vec()))
}

fn admin_policies_get(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let id = req.matched.params.get("id")?;
    Some(server.handle_iam_policy_get(id))
}

fn admin_policies_delete(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let id = req.matched.params.get("id")?;
    Some(server.handle_iam_policy_delete(req.headers, id))
}

fn admin_policies_simulate(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_iam_simulate(req.body.to_vec()))
}

fn admin_policies_lint(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_iam_policy_lint(req.body.to_vec()))
}

fn admin_policies_migrate_mode(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(server.handle_iam_policy_migrate_mode(req.body.to_vec()))
}

fn admin_policies_actions(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_iam_policy_actions())
}

fn admin_users_effective_permissions(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let user = req.matched.params.get("user")?;
    Some(server.handle_iam_effective_permissions(user, req.query))
}

fn admin_users_groups_add(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let user = req.matched.params.get("user")?;
    let group = req.matched.params.get("group")?;
    Some(server.handle_iam_add_user_group(user, group))
}

fn admin_users_groups_remove(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let user = req.matched.params.get("user")?;
    let group = req.matched.params.get("group")?;
    Some(server.handle_iam_remove_user_group(user, group))
}

fn admin_users_policies_attach(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let user = req.matched.params.get("user")?;
    let policy = req.matched.params.get("policy")?;
    Some(server.handle_iam_attach_user(req.headers, user, policy))
}

fn admin_users_policies_detach(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let user = req.matched.params.get("user")?;
    let policy = req.matched.params.get("policy")?;
    Some(server.handle_iam_detach_user(req.headers, user, policy))
}

fn admin_groups_policies_attach(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let group = req.matched.params.get("group")?;
    let policy = req.matched.params.get("policy")?;
    Some(server.handle_iam_attach_group(req.headers, group, policy))
}

fn admin_groups_policies_detach(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let group = req.matched.params.get("group")?;
    let policy = req.matched.params.get("policy")?;
    Some(server.handle_iam_detach_group(req.headers, group, policy))
}

fn admin_status(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_admin_status())
}

fn admin_blob_cache_stats(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_admin_blob_cache_stats(req.query))
}

fn ops_ec_status(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(handlers_ec::handle_ec_global_status(&server.runtime))
}

fn ops_ec_add(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    let field = req.matched.params.get("field")?;
    let operation = match req.matched.spec.id {
        "ops.ec.add" => "add",
        "ops.ec.sub" => "sub",
        _ => "set",
    };
    Some(handlers_ec::handle_ec_mutate(
        &server.runtime,
        collection,
        field,
        operation,
        req.body.to_vec(),
    ))
}

fn ops_ec_consolidate(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    let field = req.matched.params.get("field")?;
    Some(handlers_ec::handle_ec_consolidate(
        &server.runtime,
        collection,
        field,
    ))
}

fn ops_ec_field_status(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    let field = req.matched.params.get("field")?;
    Some(handlers_ec::handle_ec_status(
        &server.runtime,
        collection,
        field,
        req.query,
    ))
}

fn ops_backup_status(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_backup_status())
}

fn ops_backup_trigger(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_backup_trigger())
}

fn ops_recovery_restore_points(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(server.handle_restore_points())
}

fn ops_replication_status(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_replication_status())
}

fn ops_replication_snapshot(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_replication_snapshot())
}

fn ops_topology_graph(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_topology_graph())
}

fn ops_cluster_status(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_cluster_status())
}

fn ops_deployment_profiles(_server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let profile = req
        .query
        .get("profile")
        .and_then(|value| deployment_profile_from_token(value.as_str()));
    Some(json_response(
        200,
        match profile {
            Some(profile) => {
                crate::presentation::deployment_json::deployment_profile_json(match profile {
                    DeploymentProfile::Embedded => {
                        crate::presentation::deployment_json::DeploymentProfileView::Embedded
                    }
                    DeploymentProfile::Server => {
                        crate::presentation::deployment_json::DeploymentProfileView::Server
                    }
                    DeploymentProfile::Serverless => {
                        crate::presentation::deployment_json::DeploymentProfileView::Serverless
                    }
                })
            }
            None => crate::presentation::deployment_json::deployment_profiles_catalog_json(
                &[
                    crate::presentation::deployment_json::DeploymentProfileView::Embedded,
                    crate::presentation::deployment_json::DeploymentProfileView::Server,
                    crate::presentation::deployment_json::DeploymentProfileView::Serverless,
                ],
                "Use /deployment/profiles?profile=serverless to get the exact serverless contract.",
            ),
        },
    ))
}

fn ops_grpc_discovery(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_grpc_discovery())
}

fn ops_cdc_changes(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_cdc_poll(req.query))
}

fn ops_health_aggregate(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let report = server.native_use_cases().health();
    let status = if report.allows_serving_traffic() {
        200
    } else {
        503
    };
    Some(json_response(
        status,
        server.health_json_with_transport(&report),
    ))
}

fn ops_ready_aggregate(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let report = server.native_use_cases().health();
    let status = if report.allows_serving_traffic() {
        200
    } else {
        503
    };
    Some(json_response(
        status,
        server.health_json_with_transport(&report),
    ))
}

fn ops_ready_query(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let ready = server.native_use_cases().readiness().query;
    Some(json_response(
        if ready { 200 } else { 503 },
        crate::presentation::catalog_json::readiness_json("query", ready),
    ))
}

fn ops_ready_write(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let ready = server.native_use_cases().readiness().write;
    Some(json_response(
        if ready { 200 } else { 503 },
        crate::presentation::catalog_json::readiness_json("write", ready),
    ))
}

fn ops_ready_repair(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let ready = server.native_use_cases().readiness().repair;
    Some(json_response(
        if ready { 200 } else { 503 },
        crate::presentation::catalog_json::readiness_json("repair", ready),
    ))
}

fn ops_ready_serverless(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let native = server.native_use_cases();
    let readiness = native.readiness();
    let health = native.health();
    let authority = native.physical_authority_status();
    let (query_ready, write_ready, repair_ready) = (
        readiness.query_serverless,
        readiness.write_serverless,
        readiness.repair_serverless,
    );
    let ready = query_ready && write_ready && repair_ready;
    Some(json_response(
        if ready { 200 } else { 503 },
        serverless_readiness_summary_to_json(
            query_ready,
            write_ready,
            repair_ready,
            &health,
            &authority,
        ),
    ))
}

fn ops_ready_serverless_query(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let ready = server.native_use_cases().readiness().query_serverless;
    Some(json_response(
        if ready { 200 } else { 503 },
        crate::presentation::catalog_json::readiness_json("query", ready),
    ))
}

fn ops_ready_serverless_write(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let ready = server.native_use_cases().readiness().write_serverless;
    Some(json_response(
        if ready { 200 } else { 503 },
        crate::presentation::catalog_json::readiness_json("write", ready),
    ))
}

fn ops_ready_serverless_repair(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let ready = server.native_use_cases().readiness().repair_serverless;
    Some(json_response(
        if ready { 200 } else { 503 },
        crate::presentation::catalog_json::readiness_json("repair", ready),
    ))
}

fn ops_capabilities(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_capabilities())
}
