use crate::server::route_catalog::{
    RouteAlias, RouteAudience, RouteAuth, RouteEntry, RouteGroupDefaults, RouteMethod,
    RouteRegistry, RouteRequest, RouteStability,
};
use crate::server::routes::common::{PUBLIC_SURFACES, STANDARD_MIDDLEWARE};
use crate::server::{HttpResponse, RedDBServer};

const CACHE_USER: RouteGroupDefaults = RouteGroupDefaults {
    family: "cache",
    audience: RouteAudience::Client,
    auth: RouteAuth::UserRequired,
    surfaces: PUBLIC_SURFACES,
    stability: RouteStability::Stable,
    middlewares: STANDARD_MIDDLEWARE,
};

macro_rules! cache_aliases {
    ($method:expr, $pattern:expr) => {
        &[RouteAlias::canonical(
            $method,
            $pattern,
            "canonical v1 cache path",
        )]
    };
}

const CACHE_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases(
        "cache.get",
        RouteMethod::Get,
        "/cache/ns/:namespace/:key",
        cache_aliases!(RouteMethod::Get, "/v1/cache/ns/:namespace/:key"),
        get,
    ),
    RouteEntry::with_aliases(
        "cache.put",
        RouteMethod::Put,
        "/cache/ns/:namespace/:key",
        cache_aliases!(RouteMethod::Put, "/v1/cache/ns/:namespace/:key"),
        put,
    ),
    RouteEntry::with_aliases(
        "cache.exists",
        RouteMethod::Get,
        "/cache/ns/:namespace/:key/exists",
        cache_aliases!(RouteMethod::Get, "/v1/cache/ns/:namespace/:key/exists"),
        exists,
    ),
    RouteEntry::with_aliases(
        "cache.invalidate",
        RouteMethod::Delete,
        "/cache/ns/:namespace/:key",
        cache_aliases!(RouteMethod::Delete, "/v1/cache/ns/:namespace/:key"),
        invalidate,
    ),
    RouteEntry::with_aliases(
        "cache.invalidate_prefix",
        RouteMethod::Delete,
        "/cache/ns/:namespace",
        cache_aliases!(RouteMethod::Delete, "/v1/cache/ns/:namespace"),
        invalidate_prefix,
    ),
    RouteEntry::with_aliases(
        "cache.invalidate_tags",
        RouteMethod::Post,
        "/cache/ns/:namespace/invalidate-tags",
        cache_aliases!(RouteMethod::Post, "/v1/cache/ns/:namespace/invalidate-tags"),
        invalidate_tags,
    ),
    RouteEntry::with_aliases(
        "cache.flush_namespace",
        RouteMethod::Post,
        "/cache/ns/:namespace/flush",
        cache_aliases!(RouteMethod::Post, "/v1/cache/ns/:namespace/flush"),
        flush,
    ),
];

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.routes(CACHE_USER, CACHE_ROUTES);
}

macro_rules! cache_handler {
    ($name:ident, $operation:literal, $capability:literal) => {
        fn $name(server: &RedDBServer, request: &RouteRequest<'_>) -> Option<HttpResponse> {
            Some(server.handle_cache(request, $operation, $capability))
        }
    };
}

cache_handler!(get, "get", "cache:read");
cache_handler!(put, "put", "cache:write");
cache_handler!(exists, "exists", "cache:read");
cache_handler!(invalidate, "invalidate", "cache:invalidate");
cache_handler!(invalidate_prefix, "invalidate_prefix", "cache:invalidate");
cache_handler!(invalidate_tags, "invalidate_tags", "cache:invalidate");
cache_handler!(flush, "flush", "cache:invalidate");
