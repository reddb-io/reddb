use crate::server::route_catalog::{
    ListenerSurface, RouteAlias, RouteAudience, RouteAuth, RouteEntry, RouteGroupDefaults,
    RouteMethod, RouteRegistry, RouteStability,
};
use crate::server::routes::common::STANDARD_MIDDLEWARE;

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
    ),
    RouteEntry::with_aliases(
        "collections.create",
        RouteMethod::Post,
        "/collections",
        collections_aliases!(RouteMethod::Post, "/v1/collections"),
    ),
    RouteEntry::with_aliases(
        "collections.drop",
        RouteMethod::Delete,
        "/collections/:collection",
        collections_aliases!(RouteMethod::Delete, "/v1/collections/:collection"),
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
    ),
    RouteEntry::with_aliases(
        "collections.scan",
        RouteMethod::Get,
        "/collections/:collection/scan",
        collections_aliases!(RouteMethod::Get, "/v1/collections/:collection/scan"),
    ),
    RouteEntry::with_aliases(
        "collections.chain_tip",
        RouteMethod::Get,
        "/collections/:collection/chain-tip",
        collections_aliases!(RouteMethod::Get, "/v1/collections/:collection/chain-tip"),
    ),
    // Entity CRUD (`/collections/:collection/entities/:id`).
    RouteEntry::with_aliases(
        "collections.entities.get",
        RouteMethod::Get,
        "/collections/:collection/entities/:id",
        collections_aliases!(RouteMethod::Get, "/v1/collections/:collection/entities/:id"),
    ),
    RouteEntry::with_aliases(
        "collections.entities.patch",
        RouteMethod::Patch,
        "/collections/:collection/entities/:id",
        collections_aliases!(RouteMethod::Patch, "/v1/collections/:collection/entities/:id"),
    ),
    RouteEntry::with_aliases(
        "collections.entities.put",
        RouteMethod::Put,
        "/collections/:collection/entities/:id",
        collections_aliases!(RouteMethod::Put, "/v1/collections/:collection/entities/:id"),
    ),
    RouteEntry::with_aliases(
        "collections.entities.delete",
        RouteMethod::Delete,
        "/collections/:collection/entities/:id",
        collections_aliases!(RouteMethod::Delete, "/v1/collections/:collection/entities/:id"),
    ),
    // Integrity / chain admin actions (admin-token gated inside the handler).
    RouteEntry::with_aliases(
        "collections.verify_chain",
        RouteMethod::Post,
        "/collections/:collection/verify-chain",
        collections_aliases!(RouteMethod::Post, "/v1/collections/:collection/verify-chain"),
    ),
    RouteEntry::with_aliases(
        "collections.clear_integrity_flag",
        RouteMethod::Post,
        "/collections/:collection/clear-integrity-flag",
        collections_aliases!(
            RouteMethod::Post,
            "/v1/collections/:collection/clear-integrity-flag"
        ),
    ),
    // Tree endpoints.
    RouteEntry::with_aliases(
        "collections.trees.create",
        RouteMethod::Post,
        "/collections/:collection/trees",
        collections_aliases!(RouteMethod::Post, "/v1/collections/:collection/trees"),
    ),
    RouteEntry::with_aliases(
        "collections.trees.nodes.insert",
        RouteMethod::Post,
        "/collections/:collection/trees/:tree/nodes",
        collections_aliases!(
            RouteMethod::Post,
            "/v1/collections/:collection/trees/:tree/nodes"
        ),
    ),
    RouteEntry::with_aliases(
        "collections.trees.move",
        RouteMethod::Post,
        "/collections/:collection/trees/:tree/move",
        collections_aliases!(
            RouteMethod::Post,
            "/v1/collections/:collection/trees/:tree/move"
        ),
    ),
    RouteEntry::with_aliases(
        "collections.trees.validate",
        RouteMethod::Post,
        "/collections/:collection/trees/:tree/validate",
        collections_aliases!(
            RouteMethod::Post,
            "/v1/collections/:collection/trees/:tree/validate"
        ),
    ),
    RouteEntry::with_aliases(
        "collections.trees.rebalance",
        RouteMethod::Post,
        "/collections/:collection/trees/:tree/rebalance",
        collections_aliases!(
            RouteMethod::Post,
            "/v1/collections/:collection/trees/:tree/rebalance"
        ),
    ),
    RouteEntry::with_aliases(
        "collections.trees.nodes.delete",
        RouteMethod::Delete,
        "/collections/:collection/trees/:tree/nodes/:node",
        collections_aliases!(
            RouteMethod::Delete,
            "/v1/collections/:collection/trees/:tree/nodes/:node"
        ),
    ),
    RouteEntry::with_aliases(
        "collections.trees.drop",
        RouteMethod::Delete,
        "/collections/:collection/trees/:tree",
        collections_aliases!(
            RouteMethod::Delete,
            "/v1/collections/:collection/trees/:tree"
        ),
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
    ),
    RouteEntry::with_aliases(
        "collections.bulk.rows",
        RouteMethod::Post,
        "/collections/:collection/bulk/rows",
        collections_aliases!(RouteMethod::Post, "/v1/collections/:collection/bulk/rows"),
    ),
    RouteEntry::with_aliases(
        "collections.bulk.nodes",
        RouteMethod::Post,
        "/collections/:collection/bulk/nodes",
        collections_aliases!(RouteMethod::Post, "/v1/collections/:collection/bulk/nodes"),
    ),
    RouteEntry::with_aliases(
        "collections.bulk.edges",
        RouteMethod::Post,
        "/collections/:collection/bulk/edges",
        collections_aliases!(RouteMethod::Post, "/v1/collections/:collection/bulk/edges"),
    ),
    RouteEntry::with_aliases(
        "collections.bulk.vectors",
        RouteMethod::Post,
        "/collections/:collection/bulk/vectors",
        collections_aliases!(
            RouteMethod::Post,
            "/v1/collections/:collection/bulk/vectors"
        ),
    ),
    // Single-item create endpoints.
    RouteEntry::with_aliases(
        "collections.rows.create",
        RouteMethod::Post,
        "/collections/:collection/rows",
        collections_aliases!(RouteMethod::Post, "/v1/collections/:collection/rows"),
    ),
    RouteEntry::with_aliases(
        "collections.batch.insert",
        RouteMethod::Post,
        "/collections/:collection/batch",
        collections_aliases!(RouteMethod::Post, "/v1/collections/:collection/batch"),
    ),
    RouteEntry::with_aliases(
        "collections.nodes.create",
        RouteMethod::Post,
        "/collections/:collection/nodes",
        collections_aliases!(RouteMethod::Post, "/v1/collections/:collection/nodes"),
    ),
    RouteEntry::with_aliases(
        "collections.edges.create",
        RouteMethod::Post,
        "/collections/:collection/edges",
        collections_aliases!(RouteMethod::Post, "/v1/collections/:collection/edges"),
    ),
    RouteEntry::with_aliases(
        "collections.vectors.create",
        RouteMethod::Post,
        "/collections/:collection/vectors",
        collections_aliases!(RouteMethod::Post, "/v1/collections/:collection/vectors"),
    ),
    RouteEntry::with_aliases(
        "collections.documents.create",
        RouteMethod::Post,
        "/collections/:collection/documents",
        collections_aliases!(
            RouteMethod::Post,
            "/v1/collections/:collection/documents"
        ),
    ),
    // Vector search endpoints.
    RouteEntry::with_aliases(
        "collections.similar",
        RouteMethod::Post,
        "/collections/:collection/similar",
        collections_aliases!(RouteMethod::Post, "/v1/collections/:collection/similar"),
    ),
    RouteEntry::with_aliases(
        "collections.ivf.search",
        RouteMethod::Post,
        "/collections/:collection/ivf/search",
        collections_aliases!(
            RouteMethod::Post,
            "/v1/collections/:collection/ivf/search"
        ),
    ),
];

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.routes(COLLECTIONS_USER, COLLECTIONS_ROUTES);
}
