use crate::server::route_catalog::{
    RouteAlias, RouteAudience, RouteAuth, RouteEntry, RouteGroupDefaults, RouteMethod,
    RouteMiddleware, RouteRegistry, RouteStability,
};
use crate::server::routes::common::{METRICS_SURFACES, STANDARD_MIDDLEWARE};

const OPS_READ_CLUSTER_MIDDLEWARE: &[RouteMiddleware] = &[
    RouteMiddleware::CorsPreflight,
    RouteMiddleware::ListenerSurfaceGate,
    RouteMiddleware::AuthGate,
    RouteMiddleware::QuotaGate,
    RouteMiddleware::OpsPolicy("ops:read:cluster"),
];

const METRICS_OPERATOR: RouteGroupDefaults = RouteGroupDefaults {
    family: "metrics",
    audience: RouteAudience::Operator,
    auth: RouteAuth::OpsCapability("ops:read:cluster"),
    surfaces: METRICS_SURFACES,
    stability: RouteStability::Stable,
    middlewares: OPS_READ_CLUSTER_MIDDLEWARE,
};

const PROM_COMPAT: RouteGroupDefaults = RouteGroupDefaults {
    family: "prometheus",
    audience: RouteAudience::CompatibilityAdapter,
    auth: RouteAuth::UserRequired,
    surfaces: METRICS_SURFACES,
    stability: RouteStability::Compatibility,
    middlewares: STANDARD_MIDDLEWARE,
};

const METRICS_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Get,
    "/v1/ops/metrics",
    "canonical v1 operator metrics path",
)];
const PROM_QUERY_GET_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Get,
    "/prometheus/api/v1/query",
    "explicit Prometheus adapter namespace",
)];
const PROM_QUERY_POST_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/prometheus/api/v1/query",
    "explicit Prometheus adapter namespace",
)];
const PROM_QUERY_RANGE_GET_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Get,
    "/prometheus/api/v1/query_range",
    "explicit Prometheus adapter namespace",
)];
const PROM_QUERY_RANGE_POST_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/prometheus/api/v1/query_range",
    "explicit Prometheus adapter namespace",
)];
const PROM_REMOTE_WRITE_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/prometheus/api/v1/write",
    "explicit Prometheus adapter namespace",
)];

const METRICS_ROUTES: &[RouteEntry] = &[RouteEntry::with_aliases(
    "metrics.scrape",
    RouteMethod::Get,
    "/metrics",
    METRICS_ALIASES,
)];

const PROMETHEUS_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases(
        "prometheus.query.get",
        RouteMethod::Get,
        "/api/v1/query",
        PROM_QUERY_GET_ALIASES,
    ),
    RouteEntry::with_aliases(
        "prometheus.query.post",
        RouteMethod::Post,
        "/api/v1/query",
        PROM_QUERY_POST_ALIASES,
    ),
    RouteEntry::with_aliases(
        "prometheus.query_range.get",
        RouteMethod::Get,
        "/api/v1/query_range",
        PROM_QUERY_RANGE_GET_ALIASES,
    ),
    RouteEntry::with_aliases(
        "prometheus.query_range.post",
        RouteMethod::Post,
        "/api/v1/query_range",
        PROM_QUERY_RANGE_POST_ALIASES,
    ),
    RouteEntry::with_aliases(
        "prometheus.remote_write",
        RouteMethod::Post,
        "/api/v1/write",
        PROM_REMOTE_WRITE_ALIASES,
    ),
];

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.routes(METRICS_OPERATOR, METRICS_ROUTES);
    registry.routes(PROM_COMPAT, PROMETHEUS_ROUTES);
}
