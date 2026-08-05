use crate::server::route_catalog::{
    ListenerSurface, RouteAlias, RouteAudience, RouteAuth, RouteEntry, RouteGroupDefaults,
    RouteMethod, RouteRegistry, RouteRequest, RouteStability,
};
use crate::server::routes::common::STANDARD_MIDDLEWARE;
use crate::server::*;

const GEO_SURFACES: &[ListenerSurface] = &[ListenerSurface::Public];

// Geospatial math endpoints. The legacy dispatcher served these from the
// untyped `/geo/*` arms (authenticated user, public listener only,
// quota-gated, no ops policy); the discovered group reproduces that
// surface/auth/middleware shape.
const GEO_USER: RouteGroupDefaults = RouteGroupDefaults {
    family: "geo",
    audience: RouteAudience::Client,
    auth: RouteAuth::UserRequired,
    surfaces: GEO_SURFACES,
    stability: RouteStability::Stable,
    middlewares: STANDARD_MIDDLEWARE,
};

macro_rules! geo_aliases {
    ($method:expr, $pattern:expr) => {
        &[RouteAlias::canonical(
            $method,
            $pattern,
            "canonical v1 geo path",
        )]
    };
}

const GEO_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases(
        "geo.distance",
        RouteMethod::Post,
        "/geo/distance",
        geo_aliases!(RouteMethod::Post, "/v1/geo/distance"),
        geo_distance,
    ),
    RouteEntry::with_aliases(
        "geo.bearing",
        RouteMethod::Post,
        "/geo/bearing",
        geo_aliases!(RouteMethod::Post, "/v1/geo/bearing"),
        geo_bearing,
    ),
    RouteEntry::with_aliases(
        "geo.midpoint",
        RouteMethod::Post,
        "/geo/midpoint",
        geo_aliases!(RouteMethod::Post, "/v1/geo/midpoint"),
        geo_midpoint,
    ),
    RouteEntry::with_aliases(
        "geo.destination",
        RouteMethod::Post,
        "/geo/destination",
        geo_aliases!(RouteMethod::Post, "/v1/geo/destination"),
        geo_destination,
    ),
    RouteEntry::with_aliases(
        "geo.bounding_box",
        RouteMethod::Post,
        "/geo/bounding-box",
        geo_aliases!(RouteMethod::Post, "/v1/geo/bounding-box"),
        geo_bounding_box,
    ),
];

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.routes(GEO_USER, GEO_ROUTES);
}

// Handlers. Each route above binds one of these by fn pointer, so a
// declared route always has a live handler behind it.

fn geo_distance(_server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(handlers_geo::handle_geo_distance(req.body.to_vec()))
}

fn geo_bearing(_server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(handlers_geo::handle_geo_bearing(req.body.to_vec()))
}

fn geo_midpoint(_server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(handlers_geo::handle_geo_midpoint(req.body.to_vec()))
}

fn geo_destination(_server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(handlers_geo::handle_geo_destination(req.body.to_vec()))
}

fn geo_bounding_box(_server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(handlers_geo::handle_geo_bounding_box(req.body.to_vec()))
}
