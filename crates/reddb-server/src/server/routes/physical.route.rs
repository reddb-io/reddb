use crate::server::route_catalog::{
    ListenerSurface, RouteAlias, RouteAudience, RouteAuth, RouteEntry, RouteGroupDefaults,
    RouteMethod, RouteRegistry, RouteRequest, RouteStability,
};
use crate::server::routes::common::STANDARD_MIDDLEWARE;
use crate::server::*;

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
        physical_metadata,
    ),
    RouteEntry::with_aliases(
        "physical.native_header",
        RouteMethod::Get,
        "/physical/native-header",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/native-header"),
        physical_native_header,
    ),
    RouteEntry::with_aliases(
        "physical.native_collection_roots",
        RouteMethod::Get,
        "/physical/native-collection-roots",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/native-collection-roots"),
        physical_native_collection_roots,
    ),
    RouteEntry::with_aliases(
        "physical.native_manifest",
        RouteMethod::Get,
        "/physical/native-manifest",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/native-manifest"),
        physical_native_manifest,
    ),
    RouteEntry::with_aliases(
        "physical.native_registry",
        RouteMethod::Get,
        "/physical/native-registry",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/native-registry"),
        physical_native_registry,
    ),
    RouteEntry::with_aliases(
        "physical.native_recovery",
        RouteMethod::Get,
        "/physical/native-recovery",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/native-recovery"),
        physical_native_recovery,
    ),
    RouteEntry::with_aliases(
        "physical.native_catalog",
        RouteMethod::Get,
        "/physical/native-catalog",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/native-catalog"),
        physical_native_catalog,
    ),
    RouteEntry::with_aliases(
        "physical.native_metadata_state",
        RouteMethod::Get,
        "/physical/native-metadata-state",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/native-metadata-state"),
        physical_native_metadata_state,
    ),
    RouteEntry::with_aliases(
        "physical.authority",
        RouteMethod::Get,
        "/physical/authority",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/authority"),
        physical_authority,
    ),
    RouteEntry::with_aliases(
        "physical.native_state",
        RouteMethod::Get,
        "/physical/native-state",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/native-state"),
        physical_native_state,
    ),
    RouteEntry::with_aliases(
        "physical.native_vector_artifacts",
        RouteMethod::Get,
        "/physical/native-vector-artifacts",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/native-vector-artifacts"),
        physical_native_vector_artifacts,
    ),
    RouteEntry::with_aliases(
        "physical.native_vector_artifacts.inspect",
        RouteMethod::Get,
        "/physical/native-vector-artifacts/inspect",
        physical_aliases!(
            RouteMethod::Get,
            "/v1/ops/physical/native-vector-artifacts/inspect"
        ),
        physical_native_vector_artifacts_inspect,
    ),
    RouteEntry::with_aliases(
        "physical.native_header.repair_policy",
        RouteMethod::Get,
        "/physical/native-header/repair-policy",
        physical_aliases!(
            RouteMethod::Get,
            "/v1/ops/physical/native-header/repair-policy"
        ),
        physical_native_header_repair_policy,
    ),
    RouteEntry::with_aliases(
        "physical.native_header.repair",
        RouteMethod::Post,
        "/physical/native-header/repair",
        physical_aliases!(RouteMethod::Post, "/v1/ops/physical/native-header/repair"),
        physical_native_header_repair,
    ),
    RouteEntry::with_aliases(
        "physical.metadata.rebuild",
        RouteMethod::Post,
        "/physical/metadata/rebuild",
        physical_aliases!(RouteMethod::Post, "/v1/ops/physical/metadata/rebuild"),
        physical_metadata_rebuild,
    ),
    RouteEntry::with_aliases(
        "physical.native_state.repair",
        RouteMethod::Post,
        "/physical/native-state/repair",
        physical_aliases!(RouteMethod::Post, "/v1/ops/physical/native-state/repair"),
        physical_native_state_repair,
    ),
    RouteEntry::with_aliases(
        "physical.native_vector_artifacts.warmup",
        RouteMethod::Post,
        "/physical/native-vector-artifacts/warmup",
        physical_aliases!(
            RouteMethod::Post,
            "/v1/ops/physical/native-vector-artifacts/warmup"
        ),
        physical_native_vector_artifacts_warmup,
    ),
    RouteEntry::with_aliases(
        "physical.collections.vector_artifacts.inspect",
        RouteMethod::Get,
        "/collections/:collection/native-vector-artifacts/inspect",
        physical_aliases!(
            RouteMethod::Get,
            "/v1/ops/physical/collections/:collection/native-vector-artifacts/inspect"
        ),
        physical_collections_vector_artifacts_inspect,
    ),
    RouteEntry::with_aliases(
        "physical.collections.vector_artifacts.warmup",
        RouteMethod::Post,
        "/collections/:collection/native-vector-artifacts/warmup",
        physical_aliases!(
            RouteMethod::Post,
            "/v1/ops/physical/collections/:collection/native-vector-artifacts/warmup"
        ),
        physical_collections_vector_artifacts_warmup,
    ),
    RouteEntry::with_aliases(
        "physical.manifest",
        RouteMethod::Get,
        "/manifest",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/manifest"),
        physical_manifest,
    ),
    RouteEntry::with_aliases(
        "physical.roots",
        RouteMethod::Get,
        "/roots",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/roots"),
        physical_roots,
    ),
    RouteEntry::with_aliases(
        "physical.snapshots",
        RouteMethod::Get,
        "/snapshots",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/snapshots"),
        physical_snapshots,
    ),
    RouteEntry::with_aliases(
        "physical.exports",
        RouteMethod::Get,
        "/exports",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/exports"),
        physical_exports,
    ),
    RouteEntry::with_aliases(
        "physical.indexes",
        RouteMethod::Get,
        "/indexes",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/indexes"),
        physical_indexes,
    ),
    RouteEntry::with_aliases(
        "physical.stats",
        RouteMethod::Get,
        "/stats",
        physical_aliases!(RouteMethod::Get, "/v1/ops/physical/stats"),
        physical_stats,
    ),
    RouteEntry::with_aliases(
        "physical.checkpoint",
        RouteMethod::Post,
        "/checkpoint",
        physical_aliases!(RouteMethod::Post, "/v1/ops/physical/checkpoint"),
        physical_checkpoint,
    ),
    RouteEntry::with_aliases(
        "physical.snapshot.create",
        RouteMethod::Post,
        "/snapshot",
        physical_aliases!(RouteMethod::Post, "/v1/ops/physical/snapshots"),
        physical_snapshot_create,
    ),
    RouteEntry::with_aliases(
        "physical.export.create",
        RouteMethod::Post,
        "/export",
        physical_aliases!(RouteMethod::Post, "/v1/ops/physical/exports"),
        physical_export_create,
    ),
    RouteEntry::with_aliases(
        "physical.indexes.rebuild",
        RouteMethod::Post,
        "/indexes/rebuild",
        physical_aliases!(RouteMethod::Post, "/v1/ops/physical/indexes/rebuild"),
        physical_indexes_rebuild,
    ),
    RouteEntry::with_aliases(
        "physical.retention.apply",
        RouteMethod::Post,
        "/retention/apply",
        physical_aliases!(RouteMethod::Post, "/v1/ops/physical/retention/apply"),
        physical_retention_apply,
    ),
    RouteEntry::with_aliases(
        "physical.maintenance",
        RouteMethod::Post,
        "/maintenance",
        physical_aliases!(RouteMethod::Post, "/v1/ops/physical/maintenance"),
        physical_maintenance,
    ),
    RouteEntry::with_aliases(
        "physical.collections.indexes",
        RouteMethod::Get,
        "/collections/:collection/indexes",
        physical_aliases!(
            RouteMethod::Get,
            "/v1/ops/physical/collections/:collection/indexes"
        ),
        physical_collections_indexes,
    ),
    RouteEntry::with_aliases(
        "physical.collections.indexes.rebuild",
        RouteMethod::Post,
        "/collections/:collection/indexes/rebuild",
        physical_aliases!(
            RouteMethod::Post,
            "/v1/ops/physical/collections/:collection/indexes/rebuild"
        ),
        physical_collections_indexes_rebuild,
    ),
];

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.routes(PHYSICAL_USER, PHYSICAL_ROUTES);
}

// Handlers. Each route above binds one of these by fn pointer, so a
// declared route always has a live handler behind it.

fn physical_metadata(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(match server.native_use_cases().physical_metadata() {
        Ok(metadata) => json_response(200, metadata.to_json_value()),
        Err(err) => json_error(404, err.to_string()),
    })
}

fn physical_native_header(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(match server.native_use_cases().native_header() {
        Ok(header) => json_response(
            200,
            crate::presentation::native_json::native_header_json(header),
        ),
        Err(err) => json_error(404, err.to_string()),
    })
}

fn physical_native_collection_roots(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(match server.native_use_cases().native_collection_roots() {
        Ok(roots) => json_response(
            200,
            crate::presentation::native_json::collection_roots_json(&roots),
        ),
        Err(err) => json_error(404, err.to_string()),
    })
}

fn physical_native_manifest(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(match server.native_use_cases().native_manifest_summary() {
        Ok(summary) => json_response(
            200,
            crate::presentation::native_json::native_manifest_summary_json(&summary),
        ),
        Err(err) => json_error(404, err.to_string()),
    })
}

fn physical_native_registry(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(match server.native_use_cases().native_registry_summary() {
        Ok(summary) => json_response(
            200,
            crate::presentation::ops_json::native_registry_summary_json(&summary),
        ),
        Err(err) => json_error(404, err.to_string()),
    })
}

fn physical_native_recovery(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(match server.native_use_cases().native_recovery_summary() {
        Ok(summary) => json_response(
            200,
            crate::presentation::native_state_json::native_recovery_summary_json(&summary),
        ),
        Err(err) => json_error(404, err.to_string()),
    })
}

fn physical_native_catalog(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(match server.native_use_cases().native_catalog_summary() {
        Ok(summary) => json_response(
            200,
            crate::presentation::native_state_json::native_catalog_summary_json(&summary),
        ),
        Err(err) => json_error(404, err.to_string()),
    })
}

fn physical_native_metadata_state(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(
        match server.native_use_cases().native_metadata_state_summary() {
            Ok(summary) => json_response(
                200,
                crate::presentation::native_state_json::native_metadata_state_summary_json(
                    &summary,
                ),
            ),
            Err(err) => json_error(404, err.to_string()),
        },
    )
}

fn physical_authority(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(json_response(
        200,
        crate::presentation::ops_json::physical_authority_status_json(
            &server.native_use_cases().physical_authority_status(),
        ),
    ))
}

fn physical_native_state(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(match server.native_use_cases().native_physical_state() {
        Ok(state) => json_response(
            200,
            crate::presentation::native_state_json::native_physical_state_json(
                &state,
                crate::presentation::native_json::native_header_json,
                crate::presentation::native_json::collection_roots_json,
                crate::presentation::native_json::native_manifest_summary_json,
                crate::presentation::ops_json::native_registry_summary_json,
            ),
        ),
        Err(err) => json_error(404, err.to_string()),
    })
}

fn physical_native_vector_artifacts(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(
        match server.native_use_cases().native_vector_artifact_pages() {
            Ok(summaries) => json_response(
                200,
                crate::presentation::native_state_json::native_vector_artifact_pages_json(
                    &summaries,
                ),
            ),
            Err(err) => json_error(404, err.to_string()),
        },
    )
}

fn physical_native_vector_artifacts_inspect(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(match server.native_use_cases().inspect_vector_artifacts() {
        Ok(batch) => json_response(
            200,
            crate::presentation::native_state_json::native_vector_artifact_batch_json(&batch),
        ),
        Err(err) => json_error(404, err.to_string()),
    })
}

fn physical_native_header_repair_policy(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(
        match server.native_use_cases().native_header_repair_policy() {
            Ok(policy) => json_response(
                200,
                crate::presentation::native_json::repair_policy_json(&policy),
            ),
            Err(err) => json_error(404, err.to_string()),
        },
    )
}

fn physical_manifest(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(
        match server.native_use_cases().manifest_events_filtered(
            req.query.get("collection").map(String::as_str),
            req.query.get("kind").map(String::as_str),
            req.query
                .get("since_snapshot")
                .and_then(|value| value.parse::<u64>().ok()),
        ) {
            Ok(events) => json_response(
                200,
                crate::presentation::native_json::manifest_events_json(&events),
            ),
            Err(err) => json_error(404, err.to_string()),
        },
    )
}

fn physical_roots(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(match server.native_use_cases().collection_roots() {
        Ok(roots) => json_response(
            200,
            crate::presentation::native_json::collection_roots_json(&roots),
        ),
        Err(err) => json_error(404, err.to_string()),
    })
}

fn physical_snapshots(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(match server.native_use_cases().snapshots() {
        Ok(snapshots) => json_response(
            200,
            crate::presentation::native_json::snapshots_json(&snapshots),
        ),
        Err(err) => json_error(404, err.to_string()),
    })
}

fn physical_exports(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(match server.native_use_cases().exports() {
        Ok(exports) => json_response(
            200,
            crate::presentation::native_json::exports_json(&exports),
        ),
        Err(err) => json_error(404, err.to_string()),
    })
}

fn physical_indexes(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(json_response(
        200,
        crate::presentation::admin_json::indexes_json(&server.catalog_use_cases().indexes()),
    ))
}

fn physical_stats(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(json_response(
        200,
        crate::presentation::query_result_json::runtime_stats_json(
            &server.catalog_use_cases().stats(),
        ),
    ))
}

fn physical_checkpoint(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(match server.native_use_cases().checkpoint() {
        Ok(()) => json_ok("checkpoint completed"),
        Err(err) => json_error(500, err.to_string()),
    })
}

fn physical_snapshot_create(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(match server.native_use_cases().create_snapshot() {
        Ok(snapshot) => json_response(
            200,
            crate::presentation::native_json::snapshot_descriptor_json(&snapshot),
        ),
        Err(err) => json_error(500, err.to_string()),
    })
}

fn physical_export_create(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_export(req.body.to_vec()))
}

fn physical_indexes_rebuild(server: &RedDBServer, req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(server.handle_rebuild_indexes(req.body.to_vec(), None))
}

fn physical_retention_apply(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(match server.native_use_cases().apply_retention_policy() {
        Ok(()) => json_ok("retention policy applied"),
        Err(err) => json_error(500, err.to_string()),
    })
}

fn physical_maintenance(server: &RedDBServer, _req: &RouteRequest<'_>) -> Option<HttpResponse> {
    Some(match server.native_use_cases().run_maintenance() {
        Ok(()) => json_ok("maintenance completed"),
        Err(err) => json_error(500, err.to_string()),
    })
}

fn physical_native_header_repair(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(
        match server
            .native_use_cases()
            .repair_native_header_from_metadata()
        {
            Ok(policy) => json_response(
                200,
                crate::presentation::native_json::repair_policy_json(&policy),
            ),
            Err(err) => json_error(500, err.to_string()),
        },
    )
}

fn physical_metadata_rebuild(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(
        match server
            .native_use_cases()
            .rebuild_physical_metadata_from_native_state()
        {
            Ok(true) => json_ok("physical metadata rebuilt from native state"),
            Ok(false) => json_error(409, "native state is not available for metadata rebuild"),
            Err(err) => json_error(500, err.to_string()),
        },
    )
}

fn physical_native_state_repair(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(
        match server
            .native_use_cases()
            .repair_native_physical_state_from_metadata()
        {
            Ok(true) => json_ok("native physical state republished from physical metadata"),
            Ok(false) => json_error(
                409,
                "native physical state repair is not available in this mode",
            ),
            Err(err) => json_error(500, err.to_string()),
        },
    )
}

fn physical_native_vector_artifacts_warmup(
    server: &RedDBServer,
    _req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    Some(match server.native_use_cases().warmup_vector_artifacts() {
        Ok(batch) => json_response(
            200,
            crate::presentation::native_state_json::native_vector_artifact_batch_json(&batch),
        ),
        Err(err) => json_error(500, err.to_string()),
    })
}

fn physical_collections_vector_artifacts_inspect(
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

fn physical_collections_vector_artifacts_warmup(
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

fn physical_collections_indexes(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    Some(json_response(
        200,
        crate::presentation::admin_json::indexes_json(
            &server
                .catalog_use_cases()
                .indexes_for_collection(collection),
        ),
    ))
}

fn physical_collections_indexes_rebuild(
    server: &RedDBServer,
    req: &RouteRequest<'_>,
) -> Option<HttpResponse> {
    let collection = req.matched.params.get("collection")?;
    Some(server.handle_rebuild_indexes(req.body.to_vec(), Some(collection)))
}
