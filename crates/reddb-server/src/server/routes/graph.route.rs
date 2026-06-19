use crate::server::route_catalog::{
    ListenerSurface, RouteAlias, RouteAudience, RouteAuth, RouteEntry, RouteGroupDefaults,
    RouteMethod, RouteRegistry, RouteStability,
};
use crate::server::routes::common::STANDARD_MIDDLEWARE;

const GRAPH_SURFACES: &[ListenerSurface] = &[ListenerSurface::Public];

const GRAPH_USER: RouteGroupDefaults = RouteGroupDefaults {
    family: "graph",
    audience: RouteAudience::Client,
    auth: RouteAuth::UserRequired,
    surfaces: GRAPH_SURFACES,
    stability: RouteStability::Stable,
    middlewares: STANDARD_MIDDLEWARE,
};

const GRAPH_NEIGHBORHOOD_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/graph/neighborhood",
    "canonical v1 graph path",
)];

macro_rules! graph_aliases {
    ($method:expr, $pattern:expr) => {
        &[RouteAlias::canonical(
            $method,
            $pattern,
            "canonical v1 graph path",
        )]
    };
}

const GRAPH_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases(
        "graph.neighborhood",
        RouteMethod::Post,
        "/graph/neighborhood",
        GRAPH_NEIGHBORHOOD_ALIASES,
    ),
    RouteEntry::with_aliases(
        "graph.traverse",
        RouteMethod::Post,
        "/graph/traverse",
        graph_aliases!(RouteMethod::Post, "/v1/graph/traverse"),
    ),
    RouteEntry::with_aliases(
        "graph.shortest_path",
        RouteMethod::Post,
        "/graph/shortest-path",
        graph_aliases!(RouteMethod::Post, "/v1/graph/shortest-path"),
    ),
    RouteEntry::with_aliases(
        "graph.analytics.components",
        RouteMethod::Post,
        "/graph/analytics/components",
        graph_aliases!(RouteMethod::Post, "/v1/graph/analytics/components"),
    ),
    RouteEntry::with_aliases(
        "graph.analytics.centrality",
        RouteMethod::Post,
        "/graph/analytics/centrality",
        graph_aliases!(RouteMethod::Post, "/v1/graph/analytics/centrality"),
    ),
    RouteEntry::with_aliases(
        "graph.analytics.community",
        RouteMethod::Post,
        "/graph/analytics/community",
        graph_aliases!(RouteMethod::Post, "/v1/graph/analytics/community"),
    ),
    RouteEntry::with_aliases(
        "graph.analytics.clustering",
        RouteMethod::Post,
        "/graph/analytics/clustering",
        graph_aliases!(RouteMethod::Post, "/v1/graph/analytics/clustering"),
    ),
    RouteEntry::with_aliases(
        "graph.analytics.pagerank_personalized",
        RouteMethod::Post,
        "/graph/analytics/pagerank/personalized",
        graph_aliases!(RouteMethod::Post, "/v1/graph/analytics/pagerank/personalized"),
    ),
    RouteEntry::with_aliases(
        "graph.analytics.hits",
        RouteMethod::Post,
        "/graph/analytics/hits",
        graph_aliases!(RouteMethod::Post, "/v1/graph/analytics/hits"),
    ),
    RouteEntry::with_aliases(
        "graph.analytics.cycles",
        RouteMethod::Post,
        "/graph/analytics/cycles",
        graph_aliases!(RouteMethod::Post, "/v1/graph/analytics/cycles"),
    ),
    RouteEntry::with_aliases(
        "graph.analytics.topological_sort",
        RouteMethod::Post,
        "/graph/analytics/topological-sort",
        graph_aliases!(RouteMethod::Post, "/v1/graph/analytics/topological-sort"),
    ),
    RouteEntry::with_aliases(
        "graph.analytics.properties",
        RouteMethod::Post,
        "/graph/analytics/properties",
        graph_aliases!(RouteMethod::Post, "/v1/graph/analytics/properties"),
    ),
    RouteEntry::with_aliases(
        "graph.projections.list",
        RouteMethod::Get,
        "/graph/projections",
        graph_aliases!(RouteMethod::Get, "/v1/graph/projections"),
    ),
    RouteEntry::with_aliases(
        "graph.projections.upsert",
        RouteMethod::Post,
        "/graph/projections",
        graph_aliases!(RouteMethod::Post, "/v1/graph/projections"),
    ),
    RouteEntry::with_aliases(
        "graph.projections.materialize",
        RouteMethod::Post,
        "/graph/projections/:name/materialize",
        graph_aliases!(RouteMethod::Post, "/v1/graph/projections/:name/materialize"),
    ),
    RouteEntry::with_aliases(
        "graph.projections.materializing",
        RouteMethod::Post,
        "/graph/projections/:name/materializing",
        graph_aliases!(
            RouteMethod::Post,
            "/v1/graph/projections/:name/materializing"
        ),
    ),
    RouteEntry::with_aliases(
        "graph.projections.fail",
        RouteMethod::Post,
        "/graph/projections/:name/fail",
        graph_aliases!(RouteMethod::Post, "/v1/graph/projections/:name/fail"),
    ),
    RouteEntry::with_aliases(
        "graph.projections.stale",
        RouteMethod::Post,
        "/graph/projections/:name/stale",
        graph_aliases!(RouteMethod::Post, "/v1/graph/projections/:name/stale"),
    ),
    RouteEntry::with_aliases(
        "graph.jobs.list",
        RouteMethod::Get,
        "/graph/jobs",
        graph_aliases!(RouteMethod::Get, "/v1/graph/jobs"),
    ),
    RouteEntry::with_aliases(
        "graph.jobs.upsert",
        RouteMethod::Post,
        "/graph/jobs",
        graph_aliases!(RouteMethod::Post, "/v1/graph/jobs"),
    ),
    RouteEntry::with_aliases(
        "graph.jobs.queue",
        RouteMethod::Post,
        "/graph/jobs/queue",
        graph_aliases!(RouteMethod::Post, "/v1/graph/jobs/queue"),
    ),
    RouteEntry::with_aliases(
        "graph.jobs.start",
        RouteMethod::Post,
        "/graph/jobs/start",
        graph_aliases!(RouteMethod::Post, "/v1/graph/jobs/start"),
    ),
    RouteEntry::with_aliases(
        "graph.jobs.complete",
        RouteMethod::Post,
        "/graph/jobs/complete",
        graph_aliases!(RouteMethod::Post, "/v1/graph/jobs/complete"),
    ),
    RouteEntry::with_aliases(
        "graph.jobs.stale",
        RouteMethod::Post,
        "/graph/jobs/stale",
        graph_aliases!(RouteMethod::Post, "/v1/graph/jobs/stale"),
    ),
    RouteEntry::with_aliases(
        "graph.jobs.fail",
        RouteMethod::Post,
        "/graph/jobs/fail",
        graph_aliases!(RouteMethod::Post, "/v1/graph/jobs/fail"),
    ),
];

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.routes(GRAPH_USER, GRAPH_ROUTES);
}
