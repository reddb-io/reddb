use crate::server::route_catalog::{
    ListenerSurface, RouteAlias, RouteAudience, RouteAuth, RouteEntry, RouteGroupDefaults,
    RouteMethod, RouteRegistry, RouteStability,
};
use crate::server::routes::common::STANDARD_MIDDLEWARE;

const PHYSICAL_SURFACES: &[ListenerSurface] = &[ListenerSurface::Public];

const PHYSICAL_USER: RouteGroupDefaults = RouteGroupDefaults {
    family: "physical",
    audience: RouteAudience::Operator,
    auth: RouteAuth::UserRequired,
    surfaces: PHYSICAL_SURFACES,
    stability: RouteStability::Stable,
    middlewares: STANDARD_MIDDLEWARE,
};

const PHYSICAL_METADATA_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Get,
    "/v1/ops/physical/metadata",
    "canonical v1 physical ops path",
)];

macro_rules! physical_aliases {
    ($method:expr, $pattern:expr) => {
        &[RouteAlias::canonical(
            $method,
            $pattern,
            "canonical v1 physical ops path",
        )]
    };
}

const PHYSICAL_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases(
        "physical.metadata",
        RouteMethod::Get,
        "/physical/metadata",
        PHYSICAL_METADATA_ALIASES,
    ),
    RouteEntry::with_aliases(
        "physical.native_header",
        RouteMethod::Get,
        "/physical/native-header",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/native-header"),
    ),
    RouteEntry::with_aliases(
        "physical.native_collection_roots",
        RouteMethod::Get,
        "/physical/native-collection-roots",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/native-collection-roots"),
    ),
    RouteEntry::with_aliases(
        "physical.native_manifest",
        RouteMethod::Get,
        "/physical/native-manifest",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/native-manifest"),
    ),
    RouteEntry::with_aliases(
        "physical.native_registry",
        RouteMethod::Get,
        "/physical/native-registry",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/native-registry"),
    ),
    RouteEntry::with_aliases(
        "physical.native_recovery",
        RouteMethod::Get,
        "/physical/native-recovery",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/native-recovery"),
    ),
    RouteEntry::with_aliases(
        "physical.native_catalog",
        RouteMethod::Get,
        "/physical/native-catalog",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/native-catalog"),
    ),
    RouteEntry::with_aliases(
        "physical.native_metadata_state",
        RouteMethod::Get,
        "/physical/native-metadata-state",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/native-metadata-state"),
    ),
    RouteEntry::with_aliases(
        "physical.authority",
        RouteMethod::Get,
        "/physical/authority",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/authority"),
    ),
    RouteEntry::with_aliases(
        "physical.native_state",
        RouteMethod::Get,
        "/physical/native-state",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/native-state"),
    ),
    RouteEntry::with_aliases(
        "physical.native_vector_artifacts",
        RouteMethod::Get,
        "/physical/native-vector-artifacts",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/native-vector-artifacts"),
    ),
    RouteEntry::with_aliases(
        "physical.native_vector_artifacts.inspect",
        RouteMethod::Get,
        "/physical/native-vector-artifacts/inspect",
        physical_aliases!(
            RouteMethod::Get,
            "/v1/ops/physical/native-vector-artifacts/inspect"
        ),
    ),
    RouteEntry::with_aliases(
        "physical.native_header.repair_policy",
        RouteMethod::Get,
        "/physical/native-header/repair-policy",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/native-header/repair-policy"),
    ),
    RouteEntry::with_aliases(
        "physical.native_header.repair",
        RouteMethod::Post,
        "/physical/native-header/repair",
        physical_aliases!(RouteMethod::Post, "/v1/ops/physical/native-header/repair"),
    ),
    RouteEntry::with_aliases(
        "physical.metadata.rebuild",
        RouteMethod::Post,
        "/physical/metadata/rebuild",
        physical_aliases!(RouteMethod::Post, "/v1/ops/physical/metadata/rebuild"),
    ),
    RouteEntry::with_aliases(
        "physical.native_state.repair",
        RouteMethod::Post,
        "/physical/native-state/repair",
        physical_aliases!(RouteMethod::Post, "/v1/ops/physical/native-state/repair"),
    ),
    RouteEntry::with_aliases(
        "physical.native_vector_artifacts.warmup",
        RouteMethod::Post,
        "/physical/native-vector-artifacts/warmup",
        physical_aliases!(
            RouteMethod::Post,
            "/v1/ops/physical/native-vector-artifacts/warmup"
        ),
    ),
    RouteEntry::with_aliases(
        "physical.collections.vector_artifacts.inspect",
        RouteMethod::Get,
        "/collections/:collection/native-vector-artifacts/inspect",
        physical_aliases!(
            RouteMethod::Get,
            "/v1/ops/physical/collections/:collection/native-vector-artifacts/inspect"
        ),
    ),
    RouteEntry::with_aliases(
        "physical.collections.vector_artifacts.warmup",
        RouteMethod::Post,
        "/collections/:collection/native-vector-artifacts/warmup",
        physical_aliases!(
            RouteMethod::Post,
            "/v1/ops/physical/collections/:collection/native-vector-artifacts/warmup"
        ),
    ),
    RouteEntry::with_aliases(
        "physical.manifest",
        RouteMethod::Get,
        "/manifest",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/manifest"),
    ),
    RouteEntry::with_aliases(
        "physical.roots",
        RouteMethod::Get,
        "/roots",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/roots"),
    ),
    RouteEntry::with_aliases(
        "physical.snapshots",
        RouteMethod::Get,
        "/snapshots",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/snapshots"),
    ),
    RouteEntry::with_aliases(
        "physical.exports",
        RouteMethod::Get,
        "/exports",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/exports"),
    ),
    RouteEntry::with_aliases(
        "physical.indexes",
        RouteMethod::Get,
        "/indexes",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/indexes"),
    ),
    RouteEntry::with_aliases(
        "physical.stats",
        RouteMethod::Get,
        "/stats",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/stats"),
    ),
    RouteEntry::with_aliases(
        "physical.checkpoint",
        RouteMethod::Post,
        "/checkpoint",
        physical_aliases!(RouteMethod::Post, "/v1/ops/physical/checkpoint"),
    ),
    RouteEntry::with_aliases(
        "physical.snapshot.create",
        RouteMethod::Post,
        "/snapshot",
        physical_aliases!(RouteMethod::Post, "/v1/ops/physical/snapshots"),
    ),
    RouteEntry::with_aliases(
        "physical.export.create",
        RouteMethod::Post,
        "/export",
        physical_aliases!(RouteMethod::Post, "/v1/ops/physical/exports"),
    ),
    RouteEntry::with_aliases(
        "physical.indexes.rebuild",
        RouteMethod::Post,
        "/indexes/rebuild",
        physical_aliases!(RouteMethod::Post, "/v1/ops/physical/indexes/rebuild"),
    ),
    RouteEntry::with_aliases(
        "physical.retention.apply",
        RouteMethod::Post,
        "/retention/apply",
        physical_aliases!(RouteMethod::Post, "/v1/ops/physical/retention/apply"),
    ),
    RouteEntry::with_aliases(
        "physical.maintenance",
        RouteMethod::Post,
        "/maintenance",
        physical_aliases!(RouteMethod::Post, "/v1/ops/physical/maintenance"),
    ),
    RouteEntry::with_aliases(
        "physical.collections.indexes",
        RouteMethod::Get,
        "/collections/:collection/indexes",
        physical_aliases!(
            RouteMethod::Get,
            "/v1/ops/physical/collections/:collection/indexes"
        ),
    ),
    RouteEntry::with_aliases(
        "physical.collections.indexes.rebuild",
        RouteMethod::Post,
        "/collections/:collection/indexes/rebuild",
        physical_aliases!(
            RouteMethod::Post,
            "/v1/ops/physical/collections/:collection/indexes/rebuild"
        ),
    ),
];

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.routes(PHYSICAL_USER, PHYSICAL_ROUTES);
}
