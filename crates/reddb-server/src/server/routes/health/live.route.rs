use crate::server::route_catalog::{
    CommandShape, ListenerSurface, RouteAudience, RouteAuth, RouteMethod, RouteMiddleware,
    RouteRegistry, RouteRequest, RouteSpec, RouteStability,
};
use crate::server::*;

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.route(RouteSpec {
        id: "health.live",
        input_shape: CommandShape::Empty,
        output_shape: CommandShape::Structured,
        method: RouteMethod::Get,
        pattern: "/health/live",
        family: "health",
        audience: RouteAudience::Infra,
        auth: RouteAuth::Public,
        surfaces: &[
            ListenerSurface::Public,
            ListenerSurface::Admin,
            ListenerSurface::Metrics,
        ],
        stability: RouteStability::Stable,
        aliases: &[],
        middlewares: &[
            RouteMiddleware::CorsPreflight,
            RouteMiddleware::ListenerSurfaceGate,
            RouteMiddleware::QuotaBypass,
        ],
        handler: Some(health_live),
    });
}

// Handlers. Each route above binds one of these by fn pointer, so a
// declared route always has a live handler behind it.

fn health_live(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_health_live())
}
