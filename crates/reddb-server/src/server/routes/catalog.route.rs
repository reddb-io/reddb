use crate::server::route_catalog::{
    ListenerSurface, RouteAlias, RouteAudience, RouteAuth, RouteEntry, RouteGroupDefaults,
    RouteMethod, RouteRegistry, RouteRequest, RouteStability,
};
use crate::server::routes::common::STANDARD_MIDDLEWARE;
use crate::server::*;

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
    RouteEntry::with_aliases(
        "catalog.snapshot",
        RouteMethod::Get,
        "/catalog",
        CATALOG_ALIASES,
        catalog_snapshot,
    ),
    RouteEntry::with_aliases(
        "catalog.readiness",
        RouteMethod::Get,
        "/catalog/readiness",
        catalog_aliases!(RouteMethod::Get, "/v1/catalog/readiness"),
        catalog_readiness,
    ),
    RouteEntry::with_aliases(
        "catalog.attention",
        RouteMethod::Get,
        "/catalog/attention",
        catalog_aliases!(RouteMethod::Get, "/v1/catalog/attention"),
        catalog_attention,
    ),
    RouteEntry::with_aliases(
        "catalog.consistency",
        RouteMethod::Get,
        "/catalog/consistency",
        catalog_aliases!(RouteMethod::Get, "/v1/catalog/consistency"),
        catalog_consistency,
    ),
    RouteEntry::with_aliases(
        "catalog.collections.metadata",
        RouteMethod::Get,
        "/catalog/collections/:name",
        COLLECTION_ALIASES,
        catalog_collections_metadata,
    ),
];

const DEPRECATED_CATALOG_ROUTES: &[RouteEntry] = &[
    RouteEntry::new(
        "catalog.collections.readiness",
        RouteMethod::Get,
        "/catalog/collections/readiness",
        catalog_collections_readiness,
    ),
    RouteEntry::new(
        "catalog.collections.readiness_attention",
        RouteMethod::Get,
        "/catalog/collections/readiness/attention",
        catalog_collections_readiness_attention,
    ),
    RouteEntry::new(
        "catalog.indexes.declared",
        RouteMethod::Get,
        "/catalog/indexes/declared",
        catalog_indexes_declared,
    ),
    RouteEntry::new(
        "catalog.indexes.operational",
        RouteMethod::Get,
        "/catalog/indexes/operational",
        catalog_indexes_operational,
    ),
    RouteEntry::new(
        "catalog.indexes.status",
        RouteMethod::Get,
        "/catalog/indexes/status",
        catalog_indexes_status,
    ),
    RouteEntry::new(
        "catalog.indexes.attention",
        RouteMethod::Get,
        "/catalog/indexes/attention",
        catalog_indexes_attention,
    ),
    RouteEntry::new(
        "catalog.graph.projections.declared",
        RouteMethod::Get,
        "/catalog/graph/projections/declared",
        catalog_graph_projections_declared,
    ),
    RouteEntry::new(
        "catalog.graph.projections.operational",
        RouteMethod::Get,
        "/catalog/graph/projections/operational",
        catalog_graph_projections_operational,
    ),
    RouteEntry::new(
        "catalog.graph.projections.status",
        RouteMethod::Get,
        "/catalog/graph/projections/status",
        catalog_graph_projections_status,
    ),
    RouteEntry::new(
        "catalog.graph.projections.attention",
        RouteMethod::Get,
        "/catalog/graph/projections/attention",
        catalog_graph_projections_attention,
    ),
    RouteEntry::new(
        "catalog.analytics_jobs.declared",
        RouteMethod::Get,
        "/catalog/analytics-jobs/declared",
        catalog_analytics_jobs_declared,
    ),
    RouteEntry::new(
        "catalog.analytics_jobs.operational",
        RouteMethod::Get,
        "/catalog/analytics-jobs/operational",
        catalog_analytics_jobs_operational,
    ),
    RouteEntry::new(
        "catalog.analytics_jobs.status",
        RouteMethod::Get,
        "/catalog/analytics-jobs/status",
        catalog_analytics_jobs_status,
    ),
    RouteEntry::new(
        "catalog.analytics_jobs.attention",
        RouteMethod::Get,
        "/catalog/analytics-jobs/attention",
        catalog_analytics_jobs_attention,
    ),
];

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.routes(CATALOG_USER, CATALOG_ROUTES);
    registry.routes(CATALOG_DEPRECATED, DEPRECATED_CATALOG_ROUTES);
}

// Handlers. Each route above binds one of these by fn pointer, so a
// declared route always has a live handler behind it.

fn catalog_readiness(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let native = server.native_use_cases();
    let readiness = native.readiness();
    let health = native.health();
    let authority = native.physical_authority_status();
    Some(json_response(
        200,
        crate::presentation::ops_json::catalog_readiness_json(
            readiness.query,
            readiness.write,
            readiness.repair,
            &health,
            &authority,
        ),
    ))
}

fn catalog_snapshot(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let snapshot = server.runtime.catalog();
    let native = server.native_use_cases();
    let readiness = native.readiness();
    let health = native.health();
    let authority = native.physical_authority_status();
    Some(json_response(
        200,
        crate::presentation::catalog_json::catalog_model_snapshot_with_readiness_json(
            &snapshot,
            crate::presentation::ops_json::catalog_readiness_json(
                readiness.query,
                readiness.write,
                readiness.repair,
                &health,
                &authority,
            ),
        ),
    ))
}

fn catalog_attention(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(json_response(
        200,
        crate::presentation::catalog_json::catalog_attention_summary_json(
            &server.runtime.catalog_attention_summary(),
        ),
    ))
}

fn catalog_consistency(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(json_response(
        200,
        crate::presentation::catalog_json::catalog_consistency_json(
            &server.runtime.catalog_consistency_report(),
        ),
    ))
}

fn catalog_collections_metadata(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let name = req.matched.params.get("name")?;
    if let Some(deny) = server.check_collection_http_policy(req.headers, "select", name) {
        return Some(deny);
    }
    Some(server.handle_collection_ui_metadata(name, req.headers))
}

fn catalog_collections_readiness(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let catalog = server.runtime.catalog();
    Some(deprecated_catalog_response(
        "/catalog/collections/readiness",
        json_response(
            200,
            crate::presentation::catalog_json::catalog_collection_readiness_json(
                &catalog.collections,
            ),
        ),
    ))
}

fn catalog_collections_readiness_attention(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(deprecated_catalog_response(
        "/catalog/collections/readiness/attention",
        json_response(
            200,
            crate::presentation::catalog_json::catalog_collection_attention_json(
                &server.runtime.collection_attention(),
            ),
        ),
    ))
}

fn catalog_indexes_declared(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(deprecated_catalog_response(
        "/catalog/indexes/declared",
        json_response(
            200,
            crate::presentation::admin_json::indexes_json(
                &server.runtime.declared_indexes(),
            ),
        ),
    ))
}

fn catalog_indexes_operational(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(deprecated_catalog_response(
        "/catalog/indexes/operational",
        json_response(
            200,
            crate::presentation::admin_json::indexes_json(&server.runtime.indexes()),
        ),
    ))
}

fn catalog_indexes_status(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(deprecated_catalog_response(
        "/catalog/indexes/status",
        json_response(
            200,
            crate::presentation::catalog_json::catalog_index_statuses_json(
                &server.runtime.index_statuses(),
            ),
        ),
    ))
}

fn catalog_indexes_attention(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(deprecated_catalog_response(
        "/catalog/indexes/attention",
        json_response(
            200,
            crate::presentation::catalog_json::catalog_index_attention_json(
                &server.runtime.index_attention(),
            ),
        ),
    ))
}

fn catalog_graph_projections_declared(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(deprecated_catalog_response(
        "/catalog/graph/projections/declared",
        match server.runtime.graph_projections() {
            Ok(projections) => json_response(
                200,
                crate::presentation::admin_json::graph_projections_json(&projections),
            ),
            Err(err) => json_error(404, err.to_string()),
        },
    ))
}

fn catalog_graph_projections_operational(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(deprecated_catalog_response(
        "/catalog/graph/projections/operational",
        json_response(
            200,
            crate::presentation::admin_json::graph_projections_json(
                &server.runtime.operational_graph_projections(),
            ),
        ),
    ))
}

fn catalog_graph_projections_status(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(deprecated_catalog_response(
        "/catalog/graph/projections/status",
        json_response(
            200,
            crate::presentation::catalog_json::catalog_graph_projection_statuses_json(
                &server.runtime.graph_projection_statuses(),
            ),
        ),
    ))
}

fn catalog_graph_projections_attention(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(deprecated_catalog_response(
        "/catalog/graph/projections/attention",
        json_response(
            200,
            crate::presentation::catalog_json::catalog_graph_projection_attention_json(
                &server.runtime.graph_projection_attention(),
            ),
        ),
    ))
}

fn catalog_analytics_jobs_declared(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(deprecated_catalog_response(
        "/catalog/analytics-jobs/declared",
        match server.runtime.analytics_jobs() {
            Ok(jobs) => json_response(
                200,
                crate::presentation::admin_json::analytics_jobs_json(&jobs),
            ),
            Err(err) => json_error(404, err.to_string()),
        },
    ))
}

fn catalog_analytics_jobs_operational(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(deprecated_catalog_response(
        "/catalog/analytics-jobs/operational",
        json_response(
            200,
            crate::presentation::admin_json::analytics_jobs_json(
                &server.runtime.operational_analytics_jobs(),
            ),
        ),
    ))
}

fn catalog_analytics_jobs_status(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(deprecated_catalog_response(
        "/catalog/analytics-jobs/status",
        json_response(
            200,
            crate::presentation::catalog_json::catalog_analytics_job_statuses_json(
                &server.runtime.analytics_job_statuses(),
            ),
        ),
    ))
}

fn catalog_analytics_jobs_attention(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(deprecated_catalog_response(
        "/catalog/analytics-jobs/attention",
        json_response(
            200,
            crate::presentation::catalog_json::catalog_analytics_job_attention_json(
                &server.runtime.analytics_job_attention(),
            ),
        ),
    ))
}
