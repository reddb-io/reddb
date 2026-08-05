//! Routing 3/3 (#1643) — the final families migrated out of the legacy
//! `match (method, path)` dispatcher into the discovered route catalog.
//!
//! These are the un-versioned "leftover" surfaces that never grew a `/v1`
//! canonical twin during slices 1/2: root discovery, the config document +
//! per-key store, the `/v1/{kv,config,vault}` keyed sub-routers, the KV
//! collection surface, the log append/query/retention verbs, the index
//! lifecycle actions, the serverless lifecycle verbs, vector clustering,
//! and the per-collection native-vector-artifact inspect/warmup routes.
//!
//! All of them are marked [`RouteStability::Internal`] because they carry no
//! `/v1` alias — the routing refactor is behavior-preserving, so no new
//! canonical alias is minted here (see the `stable_product_routes_have_v1_
//! canonical_entry` catalog test, which excludes non-stable routes). Every
//! route dispatches through `route_discovered_buffered`, and the dynamic
//! (`:param` / `*`) handlers re-run the exact legacy path parsing so
//! percent-decoding, multi-segment keys, and 405-vs-404 edges are unchanged.
use crate::server::route_catalog::{
    RouteAudience, RouteAuth, RouteEntry, RouteGroupDefaults, RouteMethod, RouteRegistry,
    RouteRequest, RouteStability,
};
use crate::server::routes::common::{PUBLIC_SURFACES, STANDARD_MIDDLEWARE};
use crate::server::*;

const LEFTOVERS_USER: RouteGroupDefaults = RouteGroupDefaults {
    family: "leftovers",
    audience: RouteAudience::Internal,
    auth: RouteAuth::UserRequired,
    surfaces: PUBLIC_SURFACES,
    stability: RouteStability::Internal,
    middlewares: STANDARD_MIDDLEWARE,
};

const LEFTOVER_ROUTES: &[RouteEntry] = &[
    // Root / discovery. `GET /` serves the UI bundle index when `--ui` is
    // on, otherwise the API discovery document (handled inside the arm).
    RouteEntry::new("root.index", RouteMethod::Get, "/", root_index),
    // Config document import/export + per-key store. `/config/*` is a
    // terminal wildcard so multi-segment (`a/b`) config keys parse exactly
    // as the legacy `strip_prefix("/config/")` did; bare `/config` is the
    // exact export/import route and wins in the catalog's exact index.
    RouteEntry::new("config.export", RouteMethod::Get, "/config", config_export),
    RouteEntry::new("config.import", RouteMethod::Post, "/config", config_import),
    RouteEntry::new("config.key", RouteMethod::Any, "/config/*", config_key),
    // `/v1/{kv,config,vault}/*` keyed sub-routers. The handlers own their
    // own multi-segment parsing, so a terminal wildcard preserves behavior.
    RouteEntry::new("keyed.v1.kv", RouteMethod::Any, "/v1/kv/*", keyed_v1_kv),
    RouteEntry::new(
        "keyed.v1.config",
        RouteMethod::Any,
        "/v1/config/*",
        keyed_v1_kv,
    ),
    RouteEntry::new(
        "keyed.v1.vault",
        RouteMethod::Any,
        "/v1/vault/*",
        keyed_v1_kv,
    ),
    // KV collection surface. Wildcards keep the legacy helper parsing
    // (percent-decode + multi-segment keys, `/kv/` watch alias).
    RouteEntry::new(
        "kv.dynamic.kvs",
        RouteMethod::Any,
        "/collections/:collection/kvs/*",
        kv_dynamic_kvs,
    ),
    RouteEntry::new(
        "kv.dynamic.kv",
        RouteMethod::Any,
        "/collections/:collection/kv/*",
        kv_dynamic_kvs,
    ),
    // Log append/query/retention verbs: `/logs/:name/:action`.
    RouteEntry::new(
        "logs.dynamic",
        RouteMethod::Any,
        "/logs/:name/:action",
        logs_dynamic,
    ),
    // Index lifecycle actions: `/indexes/:name/:action` (POST only in the
    // handler; other methods fall through to the canonical 404).
    RouteEntry::new(
        "indexes.action",
        RouteMethod::Any,
        "/indexes/:name/:action",
        indexes_action,
    ),
    // Vector clustering compute endpoint.
    RouteEntry::new(
        "vectors.cluster",
        RouteMethod::Post,
        "/vectors/cluster",
        vectors_cluster,
    ),
    // Serverless lifecycle verbs.
    RouteEntry::new(
        "serverless.attach",
        RouteMethod::Post,
        "/serverless/attach",
        serverless_attach,
    ),
    RouteEntry::new(
        "serverless.warmup",
        RouteMethod::Post,
        "/serverless/warmup",
        serverless_warmup,
    ),
    RouteEntry::new(
        "serverless.reclaim",
        RouteMethod::Post,
        "/serverless/reclaim",
        serverless_reclaim,
    ),
    RouteEntry::new(
        "serverless.tick",
        RouteMethod::Post,
        "/tick",
        serverless_reclaim,
    ),
    // Per-collection native-vector-artifact inspect/warmup. The exact
    // `/physical/native-vector-artifacts/{inspect,warmup}` routes keep
    // priority via the catalog's exact index.
    RouteEntry::new(
        "physical.native_vector_artifacts.by_collection",
        RouteMethod::Get,
        "/physical/native-vector-artifacts/:collection",
        physical_native_vector_artifacts_by_collection,
    ),
    RouteEntry::new(
        "physical.native_vector_artifacts.by_collection.warmup",
        RouteMethod::Post,
        "/physical/native-vector-artifacts/:collection/warmup",
        physical_native_vector_artifacts_by_collection_warmup,
    ),
];

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.routes(LEFTOVERS_USER, LEFTOVER_ROUTES);
}

// Handlers. Each route above binds one of these by fn pointer, so a
// declared route always has a live handler behind it.

fn root_index(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(match server.ui_dir() {
        Some(ui_dir) => crate::server::ui_static::serve_bundle_asset(ui_dir, "/")
            .unwrap_or_else(|| server.handle_root_discovery()),
        None => server.handle_root_discovery(),
    })
}

fn config_export(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_config_export())
}

fn config_import(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_config_import(req.body.to_vec()))
}

fn config_key(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    server.handle_config_key_route(req.method, req.path, req.body)
}

fn keyed_v1_kv(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    server.handle_v1_keyed_route(req.method, req.path, req.query, req.body)
}

fn kv_dynamic_kvs(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    server.handle_collection_kv_route(req.method, req.path, req.query, req.headers, req.body)
}

fn logs_dynamic(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let name = req.matched.params.get("name")?;
    let action = req.matched.params.get("action")?;
    Some(match (req.method, action.as_str()) {
        ("POST", "append") => {
            handlers_log::handle_log_append(&server.runtime, name, req.body.to_vec())
        }
        ("GET", "query") => handlers_log::handle_log_query(&server.runtime, name, req.query),
        ("POST", "retention") => handlers_log::handle_log_retention(&server.runtime, name),
        _ => json_error(405, "method not allowed for log endpoint"),
    })
}

fn indexes_action(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let name = req.matched.params.get("name")?;
    let action = req.matched.params.get("action")?;
    server.handle_index_action_route(req.method, name, action)
}

fn vectors_cluster(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(handlers_vector::handle_vector_cluster(
        &server.runtime,
        req.body.to_vec(),
    ))
}

fn serverless_attach(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_serverless_attach(req.body.to_vec()))
}

fn serverless_warmup(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_serverless_warmup(req.body.to_vec()))
}

fn serverless_reclaim(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_serverless_reclaim(req.body.to_vec()))
}

fn physical_native_vector_artifacts_by_collection(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    Some(
        match server
            .native_use_cases()
            .inspect_vector_artifact(InspectNativeArtifactInput {
                collection: collection.to_string(),
                artifact_kind: req.query.get("kind").cloned(),
            }) {
            Ok(artifact) => json_response(
                200,
                crate::presentation::native_state_json::native_vector_artifact_inspection_json(
                    &artifact,
                ),
            ),
            Err(err) => json_error(404, err.to_string()),
        },
    )
}

fn physical_native_vector_artifacts_by_collection_warmup(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    Some(
        match server
            .native_use_cases()
            .warmup_vector_artifact(InspectNativeArtifactInput {
                collection: collection.to_string(),
                artifact_kind: req.query.get("kind").cloned(),
            }) {
            Ok(artifact) => json_response(
                200,
                crate::presentation::native_state_json::native_vector_artifact_inspection_json(
                    &artifact,
                ),
            ),
            Err(err) => json_error(404, err.to_string()),
        },
    )
}
