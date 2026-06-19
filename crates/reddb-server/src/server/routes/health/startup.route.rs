use crate::server::route_catalog::{
    ListenerSurface, RouteAudience, RouteAuth, RouteMiddleware, RouteMethod, RouteRegistry,
    RouteSpec, RouteStability,
};

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.route(RouteSpec {
        id: "health.startup",
        method: RouteMethod::Get,
        pattern: "/health/startup",
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
    });
}
