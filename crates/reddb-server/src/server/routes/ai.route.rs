use crate::server::route_catalog::{
    ListenerSurface, RouteAlias, RouteAudience, RouteAuth, RouteEntry, RouteGroupDefaults,
    RouteMethod, RouteRegistry, RouteRequest, RouteStability,
};
use crate::server::routes::common::STANDARD_MIDDLEWARE;
use crate::server::*;

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
    RouteEntry::with_aliases("ai.ask", RouteMethod::Post, "/ai/ask", ASK_ALIASES, ai_ask),
    RouteEntry::with_aliases(
        "ai.embeddings",
        RouteMethod::Post,
        "/ai/embeddings",
        EMBEDDINGS_ALIASES,
        ai_embeddings,
    ),
    RouteEntry::with_aliases(
        "ai.prompt",
        RouteMethod::Post,
        "/ai/prompt",
        PROMPT_ALIASES,
        ai_prompt,
    ),
    RouteEntry::with_aliases(
        "ai.credentials",
        RouteMethod::Post,
        "/ai/credentials",
        CREDENTIALS_ALIASES,
        ai_credentials,
    ),
    RouteEntry::with_aliases(
        "ai.models.list",
        RouteMethod::Get,
        "/ai/models",
        MODELS_GET_ALIASES,
        ai_models_list,
    ),
    RouteEntry::with_aliases(
        "ai.models.register",
        RouteMethod::Post,
        "/ai/models",
        MODELS_POST_ALIASES,
        ai_models_register,
    ),
    RouteEntry::with_aliases(
        "ai.models.get",
        RouteMethod::Get,
        "/ai/models/:name",
        MODEL_GET_ALIASES,
        ai_models_get,
    ),
    RouteEntry::with_aliases(
        "ai.models.update",
        RouteMethod::Put,
        "/ai/models/:name",
        MODEL_PUT_ALIASES,
        ai_models_update,
    ),
    RouteEntry::with_aliases(
        "ai.models.pull",
        RouteMethod::Post,
        "/ai/models/:name/pull",
        MODEL_PULL_ALIASES,
        ai_models_pull,
    ),
    RouteEntry::with_aliases(
        "ai.models.cache_status",
        RouteMethod::Get,
        "/ai/models/:name/cache",
        MODEL_CACHE_GET_ALIASES,
        ai_models_cache_status,
    ),
    RouteEntry::with_aliases(
        "ai.models.cache_drop",
        RouteMethod::Delete,
        "/ai/models/:name/cache",
        MODEL_CACHE_DELETE_ALIASES,
        ai_models_cache_drop,
    ),
];

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.routes(AI_USER, AI_ROUTES);
}

// Handlers. Each route above binds one of these by fn pointer, so a
// declared route always has a live handler behind it.

fn ai_ask(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_ai_ask(req.body.to_vec()))
}

fn ai_embeddings(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_ai_embeddings(req.body.to_vec()))
}

fn ai_prompt(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_ai_prompt(req.body.to_vec()))
}

/// Mutations of provider credentials and the local model cache touch the
/// server filesystem and decide which secret is sent to which host; they
/// are operator actions, not user actions. The dispatcher installed the
/// caller's identity before routing, so an authenticated non-admin is
/// refused here (anonymous / auth-disabled deployments keep working).
fn require_admin_identity() -> Option<HttpResponse> {
    match crate::runtime::execution_context::current_auth_identity_for_audit() {
        Some((_, role)) if !role.can_admin() => {
            Some(json_error(403, "admin role required"))
        }
        _ => None,
    }
}

fn ai_credentials(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    if let Some(denied) = require_admin_identity() {
        return Some(denied);
    }
    Some(server.handle_ai_credentials(req.body.to_vec()))
}

fn ai_models_list(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_ai_model_list())
}

fn ai_models_register(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    if let Some(denied) = require_admin_identity() {
        return Some(denied);
    }
    Some(server.handle_ai_model_register(req.body.to_vec()))
}

fn ai_models_get(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let name = req.matched.params.get("name")?;
    Some(server.handle_ai_model_get(name))
}

fn ai_models_update(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    if let Some(denied) = require_admin_identity() {
        return Some(denied);
    }
    let name = req.matched.params.get("name")?;
    Some(server.handle_ai_model_update(name, req.body.to_vec()))
}

fn ai_models_pull(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    if let Some(denied) = require_admin_identity() {
        return Some(denied);
    }
    let name = req.matched.params.get("name")?;
    Some(server.handle_ai_model_pull(name, req.body.to_vec()))
}

fn ai_models_cache_status(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let name = req.matched.params.get("name")?;
    Some(server.handle_ai_model_cache_status(name))
}

fn ai_models_cache_drop(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    if let Some(denied) = require_admin_identity() {
        return Some(denied);
    }
    let name = req.matched.params.get("name")?;
    Some(server.handle_ai_model_cache_drop(name))
}
