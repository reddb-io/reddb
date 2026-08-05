use crate::server::route_catalog::{
    ListenerSurface, RouteAlias, RouteAudience, RouteAuth, RouteEntry, RouteGroupDefaults,
    RouteMethod, RouteRegistry, RouteRequest, RouteStability,
};
use crate::server::routes::common::STANDARD_MIDDLEWARE;
use crate::server::*;

const COLLECTIONS_SURFACES: &[ListenerSurface] = &[ListenerSurface::Public];

// Collections / entities data-plane routes. The legacy dispatcher served
// these from the untyped `/collections/*`, `/entities/*` and top-level
// (`/export`, `/exports`, `/checkpoint`, `/indexes/rebuild`) arms —
// authenticated user, public listener only, quota-gated, no ops policy;
// per-collection IAM is enforced inline in the id dispatch exactly as the
// legacy arms did via `check_collection_http_policy`. The discovered group
// reproduces that surface/auth/middleware shape.
const COLLECTIONS_USER: RouteGroupDefaults = RouteGroupDefaults {
    family: "collections",
    audience: RouteAudience::Client,
    auth: RouteAuth::UserRequired,
    surfaces: COLLECTIONS_SURFACES,
    stability: RouteStability::Stable,
    middlewares: STANDARD_MIDDLEWARE,
};

macro_rules! collections_aliases {
    ($method:expr, $pattern:expr) => {
        &[RouteAlias::canonical(
            $method,
            $pattern,
            "canonical v1 collections path",
        )]
    };
}

const COLLECTIONS_ROUTES: &[RouteEntry] = &[
    // Collection-level DDL / listing.
    RouteEntry::with_aliases(
        "collections.list",
        RouteMethod::Get,
        "/collections",
        collections_aliases!(RouteMethod::Get, "/v1/collections"),
        collections_list,
    ),
    RouteEntry::with_aliases(
        "collections.create",
        RouteMethod::Post,
        "/collections",
        collections_aliases!(RouteMethod::Post, "/v1/collections"),
        collections_create,
    ),
    RouteEntry::with_aliases(
        "collections.drop",
        RouteMethod::Delete,
        "/collections/:collection",
        collections_aliases!(RouteMethod::Delete, "/v1/collections/:collection"),
        collections_drop,
    ),
    // Per-collection reads. (`/exports`, `/export`, `/checkpoint`,
    // `/indexes/rebuild`, `/collections/:collection/indexes` and
    // `/collections/:collection/indexes/rebuild` are already owned by the
    // physical route family — see physical.route.rs — so they are not
    // re-declared here.)
    RouteEntry::with_aliases(
        "collections.schema",
        RouteMethod::Get,
        "/collections/:collection/schema",
        collections_aliases!(RouteMethod::Get, "/v1/collections/:collection/schema"),
        collections_schema,
    ),
    RouteEntry::with_aliases(
        "collections.scan",
        RouteMethod::Get,
        "/collections/:collection/scan",
        collections_aliases!(RouteMethod::Get, "/v1/collections/:collection/scan"),
        collections_scan,
    ),
    RouteEntry::with_aliases(
        "collections.chain_tip",
        RouteMethod::Get,
        "/collections/:collection/chain-tip",
        collections_aliases!(RouteMethod::Get, "/v1/collections/:collection/chain-tip"),
        collections_chain_tip,
    ),
    // Entity CRUD (`/collections/:collection/entities/:id`).
    RouteEntry::with_aliases(
        "collections.entities.get",
        RouteMethod::Get,
        "/collections/:collection/entities/:id",
        collections_aliases!(RouteMethod::Get, "/v1/collections/:collection/entities/:id"),
        collections_entities_get,
    ),
    RouteEntry::with_aliases(
        "collections.entities.patch",
        RouteMethod::Patch,
        "/collections/:collection/entities/:id",
        collections_aliases!(
            RouteMethod::Patch,
            "/v1/collections/:collection/entities/:id"
        ),
        collections_entities_patch,
    ),
    RouteEntry::with_aliases(
        "collections.entities.put",
        RouteMethod::Put,
        "/collections/:collection/entities/:id",
        collections_aliases!(RouteMethod::Put, "/v1/collections/:collection/entities/:id"),
        collections_entities_put,
    ),
    RouteEntry::with_aliases(
        "collections.entities.delete",
        RouteMethod::Delete,
        "/collections/:collection/entities/:id",
        collections_aliases!(
            RouteMethod::Delete,
            "/v1/collections/:collection/entities/:id"
        ),
        collections_entities_delete,
    ),
    // Integrity / chain admin actions (admin-token gated inside the handler).
    RouteEntry::with_aliases(
        "collections.verify_chain",
        RouteMethod::Post,
        "/collections/:collection/verify-chain",
        collections_aliases!(
            RouteMethod::Post,
            "/v1/collections/:collection/verify-chain"
        ),
        collections_verify_chain,
    ),
    RouteEntry::with_aliases(
        "collections.clear_integrity_flag",
        RouteMethod::Post,
        "/collections/:collection/clear-integrity-flag",
        collections_aliases!(
            RouteMethod::Post,
            "/v1/collections/:collection/clear-integrity-flag"
        ),
        collections_clear_integrity_flag,
    ),
    // Tree endpoints.
    RouteEntry::with_aliases(
        "collections.trees.create",
        RouteMethod::Post,
        "/collections/:collection/trees",
        collections_aliases!(RouteMethod::Post, "/v1/collections/:collection/trees"),
        collections_trees_create,
    ),
    RouteEntry::with_aliases(
        "collections.trees.nodes.insert",
        RouteMethod::Post,
        "/collections/:collection/trees/:tree/nodes",
        collections_aliases!(
            RouteMethod::Post,
            "/v1/collections/:collection/trees/:tree/nodes"
        ),
        collections_trees_nodes_insert,
    ),
    RouteEntry::with_aliases(
        "collections.trees.move",
        RouteMethod::Post,
        "/collections/:collection/trees/:tree/move",
        collections_aliases!(
            RouteMethod::Post,
            "/v1/collections/:collection/trees/:tree/move"
        ),
        collections_trees_move,
    ),
    RouteEntry::with_aliases(
        "collections.trees.validate",
        RouteMethod::Post,
        "/collections/:collection/trees/:tree/validate",
        collections_aliases!(
            RouteMethod::Post,
            "/v1/collections/:collection/trees/:tree/validate"
        ),
        collections_trees_validate,
    ),
    RouteEntry::with_aliases(
        "collections.trees.rebalance",
        RouteMethod::Post,
        "/collections/:collection/trees/:tree/rebalance",
        collections_aliases!(
            RouteMethod::Post,
            "/v1/collections/:collection/trees/:tree/rebalance"
        ),
        collections_trees_rebalance,
    ),
    RouteEntry::with_aliases(
        "collections.trees.nodes.delete",
        RouteMethod::Delete,
        "/collections/:collection/trees/:tree/nodes/:node",
        collections_aliases!(
            RouteMethod::Delete,
            "/v1/collections/:collection/trees/:tree/nodes/:node"
        ),
        collections_trees_nodes_delete,
    ),
    RouteEntry::with_aliases(
        "collections.trees.drop",
        RouteMethod::Delete,
        "/collections/:collection/trees/:tree",
        collections_aliases!(
            RouteMethod::Delete,
            "/v1/collections/:collection/trees/:tree"
        ),
        collections_trees_drop,
    ),
    // Bulk create endpoints.
    RouteEntry::with_aliases(
        "collections.bulk.documents",
        RouteMethod::Post,
        "/collections/:collection/bulk/documents",
        collections_aliases!(
            RouteMethod::Post,
            "/v1/collections/:collection/bulk/documents"
        ),
        collections_bulk_documents,
    ),
    RouteEntry::with_aliases(
        "collections.bulk.rows",
        RouteMethod::Post,
        "/collections/:collection/bulk/rows",
        collections_aliases!(RouteMethod::Post, "/v1/collections/:collection/bulk/rows"),
        collections_bulk_rows,
    ),
    RouteEntry::with_aliases(
        "collections.bulk.nodes",
        RouteMethod::Post,
        "/collections/:collection/bulk/nodes",
        collections_aliases!(RouteMethod::Post, "/v1/collections/:collection/bulk/nodes"),
        collections_bulk_nodes,
    ),
    RouteEntry::with_aliases(
        "collections.bulk.edges",
        RouteMethod::Post,
        "/collections/:collection/bulk/edges",
        collections_aliases!(RouteMethod::Post, "/v1/collections/:collection/bulk/edges"),
        collections_bulk_edges,
    ),
    RouteEntry::with_aliases(
        "collections.bulk.vectors",
        RouteMethod::Post,
        "/collections/:collection/bulk/vectors",
        collections_aliases!(
            RouteMethod::Post,
            "/v1/collections/:collection/bulk/vectors"
        ),
        collections_bulk_vectors,
    ),
    // Single-item create endpoints.
    RouteEntry::with_aliases(
        "collections.rows.create",
        RouteMethod::Post,
        "/collections/:collection/rows",
        collections_aliases!(RouteMethod::Post, "/v1/collections/:collection/rows"),
        collections_rows_create,
    ),
    RouteEntry::with_aliases(
        "collections.batch.insert",
        RouteMethod::Post,
        "/collections/:collection/batch",
        collections_aliases!(RouteMethod::Post, "/v1/collections/:collection/batch"),
        collections_batch_insert,
    ),
    RouteEntry::with_aliases(
        "collections.nodes.create",
        RouteMethod::Post,
        "/collections/:collection/nodes",
        collections_aliases!(RouteMethod::Post, "/v1/collections/:collection/nodes"),
        collections_nodes_create,
    ),
    RouteEntry::with_aliases(
        "collections.edges.create",
        RouteMethod::Post,
        "/collections/:collection/edges",
        collections_aliases!(RouteMethod::Post, "/v1/collections/:collection/edges"),
        collections_edges_create,
    ),
    RouteEntry::with_aliases(
        "collections.vectors.create",
        RouteMethod::Post,
        "/collections/:collection/vectors",
        collections_aliases!(RouteMethod::Post, "/v1/collections/:collection/vectors"),
        collections_vectors_create,
    ),
    RouteEntry::with_aliases(
        "collections.documents.create",
        RouteMethod::Post,
        "/collections/:collection/documents",
        collections_aliases!(RouteMethod::Post, "/v1/collections/:collection/documents"),
        collections_documents_create,
    ),
    // Vector search endpoints.
    RouteEntry::with_aliases(
        "collections.similar",
        RouteMethod::Post,
        "/collections/:collection/similar",
        collections_aliases!(RouteMethod::Post, "/v1/collections/:collection/similar"),
        collections_similar,
    ),
    RouteEntry::with_aliases(
        "collections.ivf.search",
        RouteMethod::Post,
        "/collections/:collection/ivf/search",
        collections_aliases!(RouteMethod::Post, "/v1/collections/:collection/ivf/search"),
        collections_ivf_search,
    ),
];

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.routes(COLLECTIONS_USER, COLLECTIONS_ROUTES);
}

// Handlers. Each route above binds one of these by fn pointer, so a
// declared route always has a live handler behind it.

fn collections_list(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let values = server
        .catalog_use_cases()
        .collections()
        .into_iter()
        .map(JsonValue::String)
        .collect();
    let mut object = Map::new();
    object.insert("collections".to_string(), JsonValue::Array(values));
    Some(json_response(200, JsonValue::Object(object)))
}

fn collections_create(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_create_collection(req.body.to_vec()))
}

fn collections_drop(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    Some(server.handle_drop_collection(collection))
}

fn collections_schema(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    if let Some(deny) = server.check_collection_http_policy(req.headers, "select", collection) {
        return Some(deny);
    }
    Some(server.handle_describe_collection(collection))
}

fn collections_scan(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    if let Some(deny) = server.check_collection_http_policy(req.headers, "select", collection) {
        return Some(deny);
    }
    Some(server.handle_scan(collection, req.query))
}

fn collections_chain_tip(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    Some(handle_chain_tip(&server.runtime, collection))
}

fn collections_entities_get(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    let id = req.matched.params.get("id")?.parse::<u64>().ok()?;
    if let Some(deny) = server.check_collection_http_policy(req.headers, "select", collection) {
        return Some(deny);
    }
    Some(server.handle_get_entity(collection, id))
}

fn collections_entities_patch(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    let id = req.matched.params.get("id")?.parse::<u64>().ok()?;
    if let Some(deny) = server.check_collection_http_policy(req.headers, "update", collection) {
        return Some(deny);
    }
    Some(server.handle_patch_entity(collection, id, req.body.to_vec()))
}

fn collections_entities_put(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    let id = req.matched.params.get("id")?.parse::<u64>().ok()?;
    if let Some(deny) = server.check_collection_http_policy(req.headers, "update", collection) {
        return Some(deny);
    }
    Some(server.handle_replace_document(collection, id, req.body.to_vec()))
}

fn collections_entities_delete(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    let id = req.matched.params.get("id")?.parse::<u64>().ok()?;
    if let Some(deny) = server.check_collection_http_policy(req.headers, "delete", collection) {
        return Some(deny);
    }
    Some(server.handle_delete_entity(collection, id))
}

fn collections_verify_chain(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    if !admin_token_ok(req.headers) {
        return Some(json_error(401, "verify-chain requires admin token"));
    }
    Some(handle_verify_chain(&server.runtime, collection))
}

fn collections_clear_integrity_flag(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    if !admin_token_ok(req.headers) {
        return Some(json_error(401, "clear-integrity-flag requires admin token"));
    }
    Some(handle_clear_integrity_flag(&server.runtime, collection))
}

fn collections_trees_create(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    Some(server.handle_create_tree(collection, req.body.to_vec()))
}

fn collections_trees_nodes_insert(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    let tree = req.matched.params.get("tree")?;
    Some(server.handle_tree_insert_node(collection, tree, req.body.to_vec()))
}

fn collections_trees_move(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    let tree = req.matched.params.get("tree")?;
    Some(server.handle_tree_move(collection, tree, req.body.to_vec()))
}

fn collections_trees_validate(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    let tree = req.matched.params.get("tree")?;
    Some(server.handle_tree_validate(collection, tree))
}

fn collections_trees_rebalance(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    let tree = req.matched.params.get("tree")?;
    Some(server.handle_tree_rebalance(collection, tree, req.body.to_vec()))
}

fn collections_trees_nodes_delete(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    let tree = req.matched.params.get("tree")?;
    let node = req.matched.params.get("node")?.parse::<u64>().ok()?;
    Some(server.handle_tree_delete_node(collection, tree, node))
}

fn collections_trees_drop(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    let tree = req.matched.params.get("tree")?;
    Some(server.handle_drop_tree(collection, tree))
}

fn collections_bulk_documents(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    if let Some(deny) = server.check_collection_http_policy(req.headers, "insert", collection) {
        return Some(deny);
    }
    Some(server.handle_bulk_create(
        collection,
        req.body.to_vec(),
        RedDBServer::handle_create_document,
    ))
}

fn collections_bulk_rows(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    if let Some(deny) = server.check_collection_http_policy(req.headers, "insert", collection) {
        return Some(deny);
    }
    Some(server.handle_bulk_create_rows_fast(collection, req.body.to_vec()))
}

fn collections_bulk_nodes(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    if let Some(deny) = server.check_collection_http_policy(req.headers, "insert", collection) {
        return Some(deny);
    }
    Some(server.handle_bulk_create(
        collection,
        req.body.to_vec(),
        RedDBServer::handle_create_node,
    ))
}

fn collections_bulk_edges(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    if let Some(deny) = server.check_collection_http_policy(req.headers, "insert", collection) {
        return Some(deny);
    }
    Some(server.handle_bulk_create(
        collection,
        req.body.to_vec(),
        RedDBServer::handle_create_edge,
    ))
}

fn collections_bulk_vectors(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    if let Some(deny) = server.check_collection_http_policy(req.headers, "insert", collection) {
        return Some(deny);
    }
    Some(server.handle_bulk_create(
        collection,
        req.body.to_vec(),
        RedDBServer::handle_create_vector,
    ))
}

fn collections_rows_create(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    if let Some(deny) = server.check_collection_http_policy(req.headers, "insert", collection) {
        return Some(deny);
    }
    Some(server.handle_create_row(collection, req.body.to_vec()))
}

fn collections_batch_insert(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    if let Some(deny) = server.check_collection_http_policy(req.headers, "insert", collection) {
        return Some(deny);
    }
    let idempotency_key = req
        .headers
        .get("idempotency-key")
        .map(|value| value.as_str());
    Some(server.handle_batch_insert(collection, req.body.to_vec(), idempotency_key))
}

fn collections_nodes_create(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    if let Some(deny) = server.check_collection_http_policy(req.headers, "insert", collection) {
        return Some(deny);
    }
    Some(server.handle_create_node(collection, req.body.to_vec()))
}

fn collections_edges_create(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    if let Some(deny) = server.check_collection_http_policy(req.headers, "insert", collection) {
        return Some(deny);
    }
    Some(server.handle_create_edge(collection, req.body.to_vec()))
}

fn collections_vectors_create(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    if let Some(deny) = server.check_collection_http_policy(req.headers, "insert", collection) {
        return Some(deny);
    }
    Some(server.handle_create_vector(collection, req.body.to_vec()))
}

fn collections_documents_create(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    if let Some(deny) = server.check_collection_http_policy(req.headers, "insert", collection) {
        return Some(deny);
    }
    Some(server.handle_create_document(collection, req.body.to_vec()))
}

fn collections_similar(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    Some(server.handle_similar(collection, req.body.to_vec()))
}

fn collections_ivf_search(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    Some(server.handle_ivf_search(collection, req.body.to_vec()))
}
