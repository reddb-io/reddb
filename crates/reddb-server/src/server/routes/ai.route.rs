use crate::server::route_catalog::{
    ListenerSurface, RouteAlias, RouteAudience, RouteAuth, RouteEntry, RouteGroupDefaults,
    RouteMethod, RouteRegistry, RouteStability,
};
use crate::server::routes::common::STANDARD_MIDDLEWARE;

const AI_SURFACES: &[ListenerSurface] = &[ListenerSurface::Public];

const AI_USER: RouteGroupDefaults = RouteGroupDefaults {
    family: "ai",
    audience: RouteAudience::Client,
    auth: RouteAuth::UserRequired,
    surfaces: AI_SURFACES,
    stability: RouteStability::Stable,
    middlewares: STANDARD_MIDDLEWARE,
};

const ASK_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/ai/ask",
    "canonical v1 AI path",
)];
const EMBEDDINGS_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/ai/embeddings",
    "canonical v1 AI path",
)];
const PROMPT_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/ai/prompt",
    "canonical v1 AI path",
)];
const CREDENTIALS_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/ai/credentials",
    "canonical v1 AI path",
)];
const MODELS_GET_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Get,
    "/v1/ai/models",
    "canonical v1 AI path",
)];
const MODELS_POST_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/ai/models",
    "canonical v1 AI path",
)];
const MODEL_GET_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Get,
    "/v1/ai/models/:name",
    "canonical v1 AI path",
)];
const MODEL_PUT_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Put,
    "/v1/ai/models/:name",
    "canonical v1 AI path",
)];
const MODEL_PULL_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Post,
    "/v1/ai/models/:name/pull",
    "canonical v1 AI path",
)];
const MODEL_CACHE_GET_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Get,
    "/v1/ai/models/:name/cache",
    "canonical v1 AI path",
)];
const MODEL_CACHE_DELETE_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Delete,
    "/v1/ai/models/:name/cache",
    "canonical v1 AI path",
)];

const AI_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases("ai.ask", RouteMethod::Post, "/ai/ask", ASK_ALIASES),
    RouteEntry::with_aliases(
        "ai.embeddings",
        RouteMethod::Post,
        "/ai/embeddings",
        EMBEDDINGS_ALIASES,
    ),
    RouteEntry::with_aliases("ai.prompt", RouteMethod::Post, "/ai/prompt", PROMPT_ALIASES),
    RouteEntry::with_aliases(
        "ai.credentials",
        RouteMethod::Post,
        "/ai/credentials",
        CREDENTIALS_ALIASES,
    ),
    RouteEntry::with_aliases("ai.models.list", RouteMethod::Get, "/ai/models", MODELS_GET_ALIASES),
    RouteEntry::with_aliases(
        "ai.models.register",
        RouteMethod::Post,
        "/ai/models",
        MODELS_POST_ALIASES,
    ),
    RouteEntry::with_aliases("ai.models.get", RouteMethod::Get, "/ai/models/:name", MODEL_GET_ALIASES),
    RouteEntry::with_aliases(
        "ai.models.update",
        RouteMethod::Put,
        "/ai/models/:name",
        MODEL_PUT_ALIASES,
    ),
    RouteEntry::with_aliases(
        "ai.models.pull",
        RouteMethod::Post,
        "/ai/models/:name/pull",
        MODEL_PULL_ALIASES,
    ),
    RouteEntry::with_aliases(
        "ai.models.cache_status",
        RouteMethod::Get,
        "/ai/models/:name/cache",
        MODEL_CACHE_GET_ALIASES,
    ),
    RouteEntry::with_aliases(
        "ai.models.cache_drop",
        RouteMethod::Delete,
        "/ai/models/:name/cache",
        MODEL_CACHE_DELETE_ALIASES,
    ),
];

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.routes(AI_USER, AI_ROUTES);
}
