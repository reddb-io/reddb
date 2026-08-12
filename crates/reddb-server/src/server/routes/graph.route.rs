use crate::server::route_catalog::{
    ListenerSurface, RouteAlias, RouteAudience, RouteAuth, RouteEntry, RouteGroupDefaults,
    RouteMethod, RouteRegistry, RouteRequest, RouteStability,
};
use crate::server::routes::common::STANDARD_MIDDLEWARE;
use crate::server::*;

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
        graph_neighborhood,
    ),
    RouteEntry::with_aliases(
        "graph.traverse",
        RouteMethod::Post,
        "/graph/traverse",
        graph_aliases!(RouteMethod::Post, "/v1/graph/traverse"),
        graph_traverse,
    ),
    RouteEntry::with_aliases(
        "graph.shortest_path",
        RouteMethod::Post,
        "/graph/shortest-path",
        graph_aliases!(RouteMethod::Post, "/v1/graph/shortest-path"),
        graph_shortest_path,
    ),
    RouteEntry::with_aliases(
        "graph.analytics.components",
        RouteMethod::Post,
        "/graph/analytics/components",
        graph_aliases!(RouteMethod::Post, "/v1/graph/analytics/components"),
        graph_analytics_components,
    ),
    RouteEntry::with_aliases(
        "graph.analytics.centrality",
        RouteMethod::Post,
        "/graph/analytics/centrality",
        graph_aliases!(RouteMethod::Post, "/v1/graph/analytics/centrality"),
        graph_analytics_centrality,
    ),
    RouteEntry::with_aliases(
        "graph.analytics.community",
        RouteMethod::Post,
        "/graph/analytics/community",
        graph_aliases!(RouteMethod::Post, "/v1/graph/analytics/community"),
        graph_analytics_community,
    ),
    RouteEntry::with_aliases(
        "graph.analytics.clustering",
        RouteMethod::Post,
        "/graph/analytics/clustering",
        graph_aliases!(RouteMethod::Post, "/v1/graph/analytics/clustering"),
        graph_analytics_clustering,
    ),
    RouteEntry::with_aliases(
        "graph.analytics.pagerank_personalized",
        RouteMethod::Post,
        "/graph/analytics/pagerank/personalized",
        graph_aliases!(
            RouteMethod::Post,
            "/v1/graph/analytics/pagerank/personalized"
        ),
        graph_analytics_pagerank_personalized,
    ),
    RouteEntry::with_aliases(
        "graph.analytics.hits",
        RouteMethod::Post,
        "/graph/analytics/hits",
        graph_aliases!(RouteMethod::Post, "/v1/graph/analytics/hits"),
        graph_analytics_hits,
    ),
    RouteEntry::with_aliases(
        "graph.analytics.cycles",
        RouteMethod::Post,
        "/graph/analytics/cycles",
        graph_aliases!(RouteMethod::Post, "/v1/graph/analytics/cycles"),
        graph_analytics_cycles,
    ),
    RouteEntry::with_aliases(
        "graph.analytics.topological_sort",
        RouteMethod::Post,
        "/graph/analytics/topological-sort",
        graph_aliases!(RouteMethod::Post, "/v1/graph/analytics/topological-sort"),
        graph_analytics_topological_sort,
    ),
    RouteEntry::with_aliases(
        "graph.analytics.properties",
        RouteMethod::Post,
        "/graph/analytics/properties",
        graph_aliases!(RouteMethod::Post, "/v1/graph/analytics/properties"),
        graph_analytics_properties,
    ),
    RouteEntry::with_aliases(
        "graph.projections.list",
        RouteMethod::Get,
        "/graph/projections",
        graph_aliases!(RouteMethod::Get, "/v1/graph/projections"),
        graph_projections_list,
    ),
    RouteEntry::with_aliases(
        "graph.projections.upsert",
        RouteMethod::Post,
        "/graph/projections",
        graph_aliases!(RouteMethod::Post, "/v1/graph/projections"),
        graph_projections_upsert,
    ),
    RouteEntry::with_aliases(
        "graph.projections.materialize",
        RouteMethod::Post,
        "/graph/projections/:name/materialize",
        graph_aliases!(RouteMethod::Post, "/v1/graph/projections/:name/materialize"),
        graph_projections_materialize,
    ),
    RouteEntry::with_aliases(
        "graph.projections.materializing",
        RouteMethod::Post,
        "/graph/projections/:name/materializing",
        graph_aliases!(
            RouteMethod::Post,
            "/v1/graph/projections/:name/materializing"
        ),
        graph_projections_materializing,
    ),
    RouteEntry::with_aliases(
        "graph.projections.fail",
        RouteMethod::Post,
        "/graph/projections/:name/fail",
        graph_aliases!(RouteMethod::Post, "/v1/graph/projections/:name/fail"),
        graph_projections_fail,
    ),
    RouteEntry::with_aliases(
        "graph.projections.stale",
        RouteMethod::Post,
        "/graph/projections/:name/stale",
        graph_aliases!(RouteMethod::Post, "/v1/graph/projections/:name/stale"),
        graph_projections_stale,
    ),
    RouteEntry::with_aliases(
        "graph.jobs.list",
        RouteMethod::Get,
        "/graph/jobs",
        graph_aliases!(RouteMethod::Get, "/v1/graph/jobs"),
        graph_jobs_list,
    ),
    RouteEntry::with_aliases(
        "graph.jobs.upsert",
        RouteMethod::Post,
        "/graph/jobs",
        graph_aliases!(RouteMethod::Post, "/v1/graph/jobs"),
        graph_jobs_upsert,
    ),
    RouteEntry::with_aliases(
        "graph.jobs.queue",
        RouteMethod::Post,
        "/graph/jobs/queue",
        graph_aliases!(RouteMethod::Post, "/v1/graph/jobs/queue"),
        graph_jobs_queue,
    ),
    RouteEntry::with_aliases(
        "graph.jobs.start",
        RouteMethod::Post,
        "/graph/jobs/start",
        graph_aliases!(RouteMethod::Post, "/v1/graph/jobs/start"),
        graph_jobs_start,
    ),
    RouteEntry::with_aliases(
        "graph.jobs.complete",
        RouteMethod::Post,
        "/graph/jobs/complete",
        graph_aliases!(RouteMethod::Post, "/v1/graph/jobs/complete"),
        graph_jobs_complete,
    ),
    RouteEntry::with_aliases(
        "graph.jobs.stale",
        RouteMethod::Post,
        "/graph/jobs/stale",
        graph_aliases!(RouteMethod::Post, "/v1/graph/jobs/stale"),
        graph_jobs_stale,
    ),
    RouteEntry::with_aliases(
        "graph.jobs.fail",
        RouteMethod::Post,
        "/graph/jobs/fail",
        graph_aliases!(RouteMethod::Post, "/v1/graph/jobs/fail"),
        graph_jobs_fail,
    ),
];

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.routes(GRAPH_USER, GRAPH_ROUTES);
}

// Handlers. Each route above binds one of these by fn pointer, so a
// declared route always has a live handler behind it.

fn graph_neighborhood(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_graph_neighborhood(req.body.to_vec()))
}

fn graph_traverse(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_graph_traverse(req.body.to_vec()))
}

fn graph_shortest_path(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_graph_shortest_path(req.body.to_vec()))
}

fn graph_analytics_components(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(server.handle_graph_components(req.body.to_vec()))
}

fn graph_analytics_centrality(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(server.handle_graph_centrality(req.body.to_vec()))
}

fn graph_analytics_community(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_graph_community(req.body.to_vec()))
}

fn graph_analytics_clustering(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(server.handle_graph_clustering(req.body.to_vec()))
}

fn graph_analytics_pagerank_personalized(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(server.handle_graph_personalized_pagerank(req.body.to_vec()))
}

fn graph_analytics_hits(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_graph_hits(req.body.to_vec()))
}

fn graph_analytics_cycles(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_graph_cycles(req.body.to_vec()))
}

fn graph_analytics_topological_sort(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(server.handle_graph_topological_sort(req.body.to_vec()))
}

fn graph_analytics_properties(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(server.handle_graph_properties(req.body.to_vec()))
}

fn graph_projections_list(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(match server.runtime.graph_projections() {
        Ok(projections) => json_response(
            200,
            crate::presentation::admin_json::graph_projections_json(&projections),
        ),
        Err(err) => json_error(404, err.to_string()),
    })
}

fn graph_projections_upsert(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_graph_projection_upsert(req.body.to_vec()))
}

fn graph_projections_materialize(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let name = req.matched.params.get("name")?;
    Some(server.materialize_graph_projection_transition(name))
}

fn graph_projections_materializing(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let name = req.matched.params.get("name")?;
    Some(
        match server
            .admin_use_cases()
            .mark_graph_projection_materializing(name)
        {
            Ok(projection) => json_response(200, graph_projection_json(&projection)),
            Err(err) => json_error(400, err.to_string()),
        },
    )
}

fn graph_projections_fail(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let name = req.matched.params.get("name")?;
    Some(match server.admin_use_cases().fail_graph_projection(name) {
        Ok(projection) => json_response(200, graph_projection_json(&projection)),
        Err(err) => json_error(400, err.to_string()),
    })
}

fn graph_projections_stale(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let name = req.matched.params.get("name")?;
    Some(
        match server.admin_use_cases().mark_graph_projection_stale(name) {
            Ok(projection) => json_response(200, graph_projection_json(&projection)),
            Err(err) => json_error(400, err.to_string()),
        },
    )
}

fn graph_jobs_list(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(match server.runtime.analytics_jobs() {
        Ok(jobs) => json_response(
            200,
            crate::presentation::admin_json::analytics_jobs_json(&jobs),
        ),
        Err(err) => json_error(404, err.to_string()),
    })
}

fn graph_jobs_upsert(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_analytics_job_upsert(req.body.to_vec()))
}

fn graph_jobs_queue(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_analytics_job_queue(req.body.to_vec()))
}

fn graph_jobs_start(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_analytics_job_start(req.body.to_vec()))
}

fn graph_jobs_complete(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_analytics_job_complete(req.body.to_vec()))
}

fn graph_jobs_stale(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_analytics_job_stale(req.body.to_vec()))
}

fn graph_jobs_fail(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_analytics_job_fail(req.body.to_vec()))
}
