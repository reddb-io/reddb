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

const GRAPH_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases(
        "graph.neighborhood",
        RouteMethod::Post,
        "/graph/neighborhood",
        GRAPH_NEIGHBORHOOD_ALIASES,
    ),
    RouteEntry::new("graph.traverse", RouteMethod::Post, "/graph/traverse"),
    RouteEntry::new("graph.shortest_path", RouteMethod::Post, "/graph/shortest-path"),
    RouteEntry::new(
        "graph.analytics.components",
        RouteMethod::Post,
        "/graph/analytics/components",
    ),
    RouteEntry::new(
        "graph.analytics.centrality",
        RouteMethod::Post,
        "/graph/analytics/centrality",
    ),
    RouteEntry::new(
        "graph.analytics.community",
        RouteMethod::Post,
        "/graph/analytics/community",
    ),
    RouteEntry::new(
        "graph.analytics.clustering",
        RouteMethod::Post,
        "/graph/analytics/clustering",
    ),
    RouteEntry::new(
        "graph.analytics.pagerank_personalized",
        RouteMethod::Post,
        "/graph/analytics/pagerank/personalized",
    ),
    RouteEntry::new("graph.analytics.hits", RouteMethod::Post, "/graph/analytics/hits"),
    RouteEntry::new("graph.analytics.cycles", RouteMethod::Post, "/graph/analytics/cycles"),
    RouteEntry::new(
        "graph.analytics.topological_sort",
        RouteMethod::Post,
        "/graph/analytics/topological-sort",
    ),
    RouteEntry::new(
        "graph.analytics.properties",
        RouteMethod::Post,
        "/graph/analytics/properties",
    ),
    RouteEntry::new("graph.projections.list", RouteMethod::Get, "/graph/projections"),
    RouteEntry::new("graph.projections.upsert", RouteMethod::Post, "/graph/projections"),
    RouteEntry::new(
        "graph.projections.materialize",
        RouteMethod::Post,
        "/graph/projections/:name/materialize",
    ),
    RouteEntry::new(
        "graph.projections.materializing",
        RouteMethod::Post,
        "/graph/projections/:name/materializing",
    ),
    RouteEntry::new(
        "graph.projections.fail",
        RouteMethod::Post,
        "/graph/projections/:name/fail",
    ),
    RouteEntry::new(
        "graph.projections.stale",
        RouteMethod::Post,
        "/graph/projections/:name/stale",
    ),
    RouteEntry::new("graph.jobs.list", RouteMethod::Get, "/graph/jobs"),
    RouteEntry::new("graph.jobs.upsert", RouteMethod::Post, "/graph/jobs"),
    RouteEntry::new("graph.jobs.queue", RouteMethod::Post, "/graph/jobs/queue"),
    RouteEntry::new("graph.jobs.start", RouteMethod::Post, "/graph/jobs/start"),
    RouteEntry::new("graph.jobs.complete", RouteMethod::Post, "/graph/jobs/complete"),
    RouteEntry::new("graph.jobs.stale", RouteMethod::Post, "/graph/jobs/stale"),
    RouteEntry::new("graph.jobs.fail", RouteMethod::Post, "/graph/jobs/fail"),
];

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.routes(GRAPH_USER, GRAPH_ROUTES);
}
