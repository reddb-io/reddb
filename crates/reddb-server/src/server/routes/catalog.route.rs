use crate::server::route_catalog::{
    ListenerSurface, RouteAlias, RouteAudience, RouteAuth, RouteEntry, RouteGroupDefaults,
    RouteMethod, RouteRegistry, RouteStability,
};
use crate::server::routes::common::STANDARD_MIDDLEWARE;

const CATALOG_SURFACES: &[ListenerSurface] = &[ListenerSurface::Public];

const CATALOG_USER: RouteGroupDefaults = RouteGroupDefaults {
    family: "catalog",
    audience: RouteAudience::Client,
    auth: RouteAuth::UserRequired,
    surfaces: CATALOG_SURFACES,
    stability: RouteStability::Stable,
    middlewares: STANDARD_MIDDLEWARE,
};

const CATALOG_DEPRECATED: RouteGroupDefaults = RouteGroupDefaults {
    family: "catalog",
    audience: RouteAudience::Client,
    auth: RouteAuth::UserRequired,
    surfaces: CATALOG_SURFACES,
    stability: RouteStability::Deprecated,
    middlewares: STANDARD_MIDDLEWARE,
};

const CATALOG_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Get,
    "/v1/catalog",
    "canonical v1 catalog path",
)];
const COLLECTION_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Get,
    "/v1/catalog/collections/:name",
    "canonical v1 catalog collection path",
)];

macro_rules! catalog_aliases {
    ($method:expr, $pattern:expr) => {
        &[RouteAlias::canonical(
            $method,
            $pattern,
            "canonical v1 catalog path",
        )]
    };
}

const CATALOG_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases("catalog.snapshot", RouteMethod::Get, "/catalog", CATALOG_ALIASES),
    RouteEntry::with_aliases(
        "catalog.readiness",
        RouteMethod::Get,
        "/catalog/readiness",
        catalog_aliases!(RouteMethod::Get, "/v1/catalog/readiness"),
    ),
    RouteEntry::with_aliases(
        "catalog.attention",
        RouteMethod::Get,
        "/catalog/attention",
        catalog_aliases!(RouteMethod::Get, "/v1/catalog/attention"),
    ),
    RouteEntry::with_aliases(
        "catalog.consistency",
        RouteMethod::Get,
        "/catalog/consistency",
        catalog_aliases!(RouteMethod::Get, "/v1/catalog/consistency"),
    ),
    RouteEntry::with_aliases(
        "catalog.collections.metadata",
        RouteMethod::Get,
        "/catalog/collections/:name",
        COLLECTION_ALIASES,
    ),
];

const DEPRECATED_CATALOG_ROUTES: &[RouteEntry] = &[
    RouteEntry::new(
        "catalog.collections.readiness",
        RouteMethod::Get,
        "/catalog/collections/readiness",
    ),
    RouteEntry::new(
        "catalog.collections.readiness_attention",
        RouteMethod::Get,
        "/catalog/collections/readiness/attention",
    ),
    RouteEntry::new(
        "catalog.indexes.declared",
        RouteMethod::Get,
        "/catalog/indexes/declared",
    ),
    RouteEntry::new(
        "catalog.indexes.operational",
        RouteMethod::Get,
        "/catalog/indexes/operational",
    ),
    RouteEntry::new("catalog.indexes.status", RouteMethod::Get, "/catalog/indexes/status"),
    RouteEntry::new(
        "catalog.indexes.attention",
        RouteMethod::Get,
        "/catalog/indexes/attention",
    ),
    RouteEntry::new(
        "catalog.graph.projections.declared",
        RouteMethod::Get,
        "/catalog/graph/projections/declared",
    ),
    RouteEntry::new(
        "catalog.graph.projections.operational",
        RouteMethod::Get,
        "/catalog/graph/projections/operational",
    ),
    RouteEntry::new(
        "catalog.graph.projections.status",
        RouteMethod::Get,
        "/catalog/graph/projections/status",
    ),
    RouteEntry::new(
        "catalog.graph.projections.attention",
        RouteMethod::Get,
        "/catalog/graph/projections/attention",
    ),
    RouteEntry::new(
        "catalog.analytics_jobs.declared",
        RouteMethod::Get,
        "/catalog/analytics-jobs/declared",
    ),
    RouteEntry::new(
        "catalog.analytics_jobs.operational",
        RouteMethod::Get,
        "/catalog/analytics-jobs/operational",
    ),
    RouteEntry::new(
        "catalog.analytics_jobs.status",
        RouteMethod::Get,
        "/catalog/analytics-jobs/status",
    ),
    RouteEntry::new(
        "catalog.analytics_jobs.attention",
        RouteMethod::Get,
        "/catalog/analytics-jobs/attention",
    ),
];

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.routes(CATALOG_USER, CATALOG_ROUTES);
    registry.routes(CATALOG_DEPRECATED, DEPRECATED_CATALOG_ROUTES);
}
