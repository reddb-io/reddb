use crate::server::route_catalog::{ListenerSurface, RouteMiddleware};

pub(crate) const PUBLIC_SURFACES: &[ListenerSurface] = &[ListenerSurface::Public];

pub(crate) const ALL_SURFACES: &[ListenerSurface] = &[
    ListenerSurface::Public,
    ListenerSurface::Admin,
    ListenerSurface::Metrics,
];

pub(crate) const PUBLIC_ADMIN_SURFACES: &[ListenerSurface] =
    &[ListenerSurface::Public, ListenerSurface::Admin];

pub(crate) const METRICS_SURFACES: &[ListenerSurface] =
    &[ListenerSurface::Public, ListenerSurface::Metrics];

pub(crate) const STANDARD_MIDDLEWARE: &[RouteMiddleware] = &[
    RouteMiddleware::CorsPreflight,
    RouteMiddleware::ListenerSurfaceGate,
    RouteMiddleware::AuthGate,
    RouteMiddleware::QuotaGate,
];

pub(crate) const PUBLIC_MIDDLEWARE: &[RouteMiddleware] = &[
    RouteMiddleware::CorsPreflight,
    RouteMiddleware::ListenerSurfaceGate,
    RouteMiddleware::QuotaGate,
];

pub(crate) const PUBLIC_NO_QUOTA_MIDDLEWARE: &[RouteMiddleware] = &[
    RouteMiddleware::CorsPreflight,
    RouteMiddleware::ListenerSurfaceGate,
    RouteMiddleware::QuotaBypass,
];

pub(crate) const ADMIN_TOKEN_MIDDLEWARE: &[RouteMiddleware] = &[
    RouteMiddleware::CorsPreflight,
    RouteMiddleware::ListenerSurfaceGate,
    RouteMiddleware::AuthGate,
    RouteMiddleware::QuotaGate,
];
