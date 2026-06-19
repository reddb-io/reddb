use crate::server::route_catalog::{
    ListenerSurface, RouteAlias, RouteAudience, RouteAuth, RouteEntry, RouteGroupDefaults,
    RouteMethod, RouteMiddleware, RouteRegistry, RouteStability,
};
use crate::server::routes::common::{PUBLIC_MIDDLEWARE, STANDARD_MIDDLEWARE};

const QUERY_SURFACES: &[ListenerSurface] = &[ListenerSurface::Public];
const STREAM_MIDDLEWARE: &[RouteMiddleware] = &[
    RouteMiddleware::CorsPreflight,
    RouteMiddleware::ListenerSurfaceGate,
    RouteMiddleware::AuthGate,
    RouteMiddleware::QuotaGate,
    RouteMiddleware::StreamingSlot,
];

const QUERY_PUBLIC: RouteGroupDefaults = RouteGroupDefaults {
    family: "query",
    audience: RouteAudience::Client,
    auth: RouteAuth::Public,
    surfaces: QUERY_SURFACES,
    stability: RouteStability::Stable,
    middlewares: PUBLIC_MIDDLEWARE,
};

const QUERY_USER: RouteGroupDefaults = RouteGroupDefaults {
    family: "query",
    audience: RouteAudience::Client,
    auth: RouteAuth::UserRequired,
    surfaces: QUERY_SURFACES,
    stability: RouteStability::Stable,
    middlewares: STANDARD_MIDDLEWARE,
};

const STREAM_USER: RouteGroupDefaults = RouteGroupDefaults {
    family: "streams",
    audience: RouteAudience::Client,
    auth: RouteAuth::UserRequired,
    surfaces: QUERY_SURFACES,
    stability: RouteStability::Stable,
    middlewares: STREAM_MIDDLEWARE,
};

const QUERY_CONTRACT_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Get,
    "/v1/query",
    "canonical v1 query discovery path",
)];
const QUERY_POST_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/query",
    "canonical v1 query execution path",
)];
const QUERY_EXPLAIN_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/query/explain",
    "canonical v1 query path",
)];
const QUERY_STREAM_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/query/stream",
    "canonical v1 query stream path",
)];
const QUERY_STREAM_CANCEL_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/query/stream/cancel",
    "canonical v1 query stream path",
)];
const INPUT_STREAM_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/streams/input",
    "canonical v1 input stream path",
)];
const SEARCH_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/query/search",
    "canonical v1 query search path",
)];
const CONTEXT_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/query/context",
    "canonical v1 query context path",
)];
const TEXT_SEARCH_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/query/text/search",
    "canonical v1 text search path",
)];
const MULTIMODAL_SEARCH_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/query/multimodal/search",
    "canonical v1 multimodal search path",
)];
const HYBRID_SEARCH_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/query/hybrid/search",
    "canonical v1 hybrid search path",
)];

const QUERY_PUBLIC_ROUTES: &[RouteEntry] = &[RouteEntry::with_aliases(
    "query.contract",
    RouteMethod::Get,
    "/query",
    QUERY_CONTRACT_ALIASES,
)];

const QUERY_USER_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases("query.execute", RouteMethod::Post, "/query", QUERY_POST_ALIASES),
    RouteEntry::with_aliases(
        "query.explain",
        RouteMethod::Post,
        "/query/explain",
        QUERY_EXPLAIN_ALIASES,
    ),
    RouteEntry::with_aliases("query.search", RouteMethod::Post, "/search", SEARCH_ALIASES),
    RouteEntry::with_aliases("query.context", RouteMethod::Post, "/context", CONTEXT_ALIASES),
    RouteEntry::with_aliases(
        "query.text_search",
        RouteMethod::Post,
        "/text/search",
        TEXT_SEARCH_ALIASES,
    ),
    RouteEntry::with_aliases(
        "query.multimodal_search",
        RouteMethod::Post,
        "/multimodal/search",
        MULTIMODAL_SEARCH_ALIASES,
    ),
    RouteEntry::with_aliases(
        "query.hybrid_search",
        RouteMethod::Post,
        "/hybrid/search",
        HYBRID_SEARCH_ALIASES,
    ),
];

const STREAM_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases(
        "streams.input",
        RouteMethod::Post,
        "/streams/input",
        INPUT_STREAM_ALIASES,
    ),
    RouteEntry::with_aliases(
        "streams.query.output",
        RouteMethod::Post,
        "/query/stream",
        QUERY_STREAM_ALIASES,
    ),
    RouteEntry::with_aliases(
        "streams.query.cancel",
        RouteMethod::Post,
        "/query/stream/cancel",
        QUERY_STREAM_CANCEL_ALIASES,
    ),
];

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.routes(QUERY_PUBLIC, QUERY_PUBLIC_ROUTES);
    registry.routes(QUERY_USER, QUERY_USER_ROUTES);
    registry.routes(STREAM_USER, STREAM_ROUTES);
}
