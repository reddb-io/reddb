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
    RouteStability,
};
use crate::server::routes::common::{PUBLIC_SURFACES, STANDARD_MIDDLEWARE};

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
    RouteEntry::new("root.index", RouteMethod::Get, "/"),
    // Config document import/export + per-key store. `/config/*` is a
    // terminal wildcard so multi-segment (`a/b`) config keys parse exactly
    // as the legacy `strip_prefix("/config/")` did; bare `/config` is the
    // exact export/import route and wins in the catalog's exact index.
    RouteEntry::new("config.export", RouteMethod::Get, "/config"),
    RouteEntry::new("config.import", RouteMethod::Post, "/config"),
    RouteEntry::new("config.key", RouteMethod::Any, "/config/*"),
    // `/v1/{kv,config,vault}/*` keyed sub-routers. The handlers own their
    // own multi-segment parsing, so a terminal wildcard preserves behavior.
    RouteEntry::new("keyed.v1.kv", RouteMethod::Any, "/v1/kv/*"),
    RouteEntry::new("keyed.v1.config", RouteMethod::Any, "/v1/config/*"),
    RouteEntry::new("keyed.v1.vault", RouteMethod::Any, "/v1/vault/*"),
    // KV collection surface. Wildcards keep the legacy helper parsing
    // (percent-decode + multi-segment keys, `/kv/` watch alias).
    RouteEntry::new("kv.dynamic.kvs", RouteMethod::Any, "/collections/:collection/kvs/*"),
    RouteEntry::new("kv.dynamic.kv", RouteMethod::Any, "/collections/:collection/kv/*"),
    // Log append/query/retention verbs: `/logs/:name/:action`.
    RouteEntry::new("logs.dynamic", RouteMethod::Any, "/logs/:name/:action"),
    // Index lifecycle actions: `/indexes/:name/:action` (POST only in the
    // handler; other methods fall through to the canonical 404).
    RouteEntry::new("indexes.action", RouteMethod::Any, "/indexes/:name/:action"),
    // Vector clustering compute endpoint.
    RouteEntry::new("vectors.cluster", RouteMethod::Post, "/vectors/cluster"),
    // Serverless lifecycle verbs.
    RouteEntry::new("serverless.attach", RouteMethod::Post, "/serverless/attach"),
    RouteEntry::new("serverless.warmup", RouteMethod::Post, "/serverless/warmup"),
    RouteEntry::new("serverless.reclaim", RouteMethod::Post, "/serverless/reclaim"),
    RouteEntry::new("serverless.tick", RouteMethod::Post, "/tick"),
    // Per-collection native-vector-artifact inspect/warmup. The exact
    // `/physical/native-vector-artifacts/{inspect,warmup}` routes keep
    // priority via the catalog's exact index.
    RouteEntry::new(
        "physical.native_vector_artifacts.by_collection",
        RouteMethod::Get,
        "/physical/native-vector-artifacts/:collection",
    ),
    RouteEntry::new(
        "physical.native_vector_artifacts.by_collection.warmup",
        RouteMethod::Post,
        "/physical/native-vector-artifacts/:collection/warmup",
    ),
];

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.routes(LEFTOVERS_USER, LEFTOVER_ROUTES);
}
