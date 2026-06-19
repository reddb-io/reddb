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

const PHYSICAL_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases(
        "physical.metadata",
        RouteMethod::Get,
        "/physical/metadata",
        PHYSICAL_METADATA_ALIASES,
    ),
    RouteEntry::new("physical.native_header", RouteMethod::Get, "/physical/native-header"),
    RouteEntry::new(
        "physical.native_collection_roots",
        RouteMethod::Get,
        "/physical/native-collection-roots",
    ),
    RouteEntry::new("physical.native_manifest", RouteMethod::Get, "/physical/native-manifest"),
    RouteEntry::new("physical.native_registry", RouteMethod::Get, "/physical/native-registry"),
    RouteEntry::new("physical.native_recovery", RouteMethod::Get, "/physical/native-recovery"),
    RouteEntry::new("physical.native_catalog", RouteMethod::Get, "/physical/native-catalog"),
    RouteEntry::new(
        "physical.native_metadata_state",
        RouteMethod::Get,
        "/physical/native-metadata-state",
    ),
    RouteEntry::new("physical.authority", RouteMethod::Get, "/physical/authority"),
    RouteEntry::new("physical.native_state", RouteMethod::Get, "/physical/native-state"),
    RouteEntry::new(
        "physical.native_vector_artifacts",
        RouteMethod::Get,
        "/physical/native-vector-artifacts",
    ),
    RouteEntry::new(
        "physical.native_vector_artifacts.inspect",
        RouteMethod::Get,
        "/physical/native-vector-artifacts/inspect",
    ),
    RouteEntry::new(
        "physical.native_header.repair_policy",
        RouteMethod::Get,
        "/physical/native-header/repair-policy",
    ),
    RouteEntry::new(
        "physical.native_header.repair",
        RouteMethod::Post,
        "/physical/native-header/repair",
    ),
    RouteEntry::new(
        "physical.metadata.rebuild",
        RouteMethod::Post,
        "/physical/metadata/rebuild",
    ),
    RouteEntry::new(
        "physical.native_state.repair",
        RouteMethod::Post,
        "/physical/native-state/repair",
    ),
    RouteEntry::new(
        "physical.native_vector_artifacts.warmup",
        RouteMethod::Post,
        "/physical/native-vector-artifacts/warmup",
    ),
    RouteEntry::new(
        "physical.collections.vector_artifacts.inspect",
        RouteMethod::Get,
        "/collections/:collection/native-vector-artifacts/inspect",
    ),
    RouteEntry::new(
        "physical.collections.vector_artifacts.warmup",
        RouteMethod::Post,
        "/collections/:collection/native-vector-artifacts/warmup",
    ),
    RouteEntry::new("physical.manifest", RouteMethod::Get, "/manifest"),
    RouteEntry::new("physical.roots", RouteMethod::Get, "/roots"),
    RouteEntry::new("physical.snapshots", RouteMethod::Get, "/snapshots"),
    RouteEntry::new("physical.exports", RouteMethod::Get, "/exports"),
    RouteEntry::new("physical.indexes", RouteMethod::Get, "/indexes"),
    RouteEntry::new("physical.stats", RouteMethod::Get, "/stats"),
    RouteEntry::new("physical.checkpoint", RouteMethod::Post, "/checkpoint"),
    RouteEntry::new("physical.snapshot.create", RouteMethod::Post, "/snapshot"),
    RouteEntry::new("physical.export.create", RouteMethod::Post, "/export"),
    RouteEntry::new("physical.indexes.rebuild", RouteMethod::Post, "/indexes/rebuild"),
    RouteEntry::new("physical.retention.apply", RouteMethod::Post, "/retention/apply"),
    RouteEntry::new("physical.maintenance", RouteMethod::Post, "/maintenance"),
    RouteEntry::new("physical.collections.indexes", RouteMethod::Get, "/collections/:collection/indexes"),
    RouteEntry::new(
        "physical.collections.indexes.rebuild",
        RouteMethod::Post,
        "/collections/:collection/indexes/rebuild",
    ),
];

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.routes(PHYSICAL_USER, PHYSICAL_ROUTES);
}
