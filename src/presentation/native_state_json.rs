use crate::json::{Map, Value as JsonValue};
use crate::storage::unified::devx::{
    NativeVectorArtifactBatchInspection, NativeVectorArtifactInspection,
};
use crate::storage::unified::store::{
    NativeCatalogSummary, NativeMetadataStateSummary, NativePhysicalState, NativeRecoverySummary,
    NativeVectorArtifactPageSummary,
};

pub(crate) fn native_vector_artifact_pages_json(
    summaries: &[NativeVectorArtifactPageSummary],
) -> JsonValue {
    JsonValue::Array(
        summaries
            .iter()
            .map(|summary| {
                let mut item = Map::new();
                item.insert(
                    "collection".to_string(),
                    JsonValue::String(summary.collection.clone()),
                );
                item.insert(
                    "artifact_kind".to_string(),
                    JsonValue::String(summary.artifact_kind.clone()),
                );
                item.insert(
                    "root_page".to_string(),
                    JsonValue::Number(summary.root_page as f64),
                );
                item.insert(
                    "page_count".to_string(),
                    JsonValue::Number(summary.page_count as f64),
                );
                item.insert(
                    "byte_len".to_string(),
                    JsonValue::String(summary.byte_len.to_string()),
                );
                item.insert(
                    "checksum".to_string(),
                    JsonValue::String(summary.checksum.to_string()),
                );
                JsonValue::Object(item)
            })
            .collect(),
    )
}

pub(crate) fn native_vector_artifact_inspection_json(
    artifact: &NativeVectorArtifactInspection,
) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "collection".to_string(),
        JsonValue::String(artifact.collection.clone()),
    );
    object.insert(
        "artifact_kind".to_string(),
        JsonValue::String(artifact.artifact_kind.clone()),
    );
    object.insert(
        "root_page".to_string(),
        JsonValue::Number(artifact.root_page as f64),
    );
    object.insert(
        "page_count".to_string(),
        JsonValue::Number(artifact.page_count as f64),
    );
    object.insert(
        "byte_len".to_string(),
        JsonValue::String(artifact.byte_len.to_string()),
    );
    object.insert(
        "checksum".to_string(),
        JsonValue::String(artifact.checksum.to_string()),
    );
    object.insert(
        "node_count".to_string(),
        JsonValue::String(artifact.node_count.to_string()),
    );
    object.insert(
        "dimension".to_string(),
        JsonValue::Number(artifact.dimension as f64),
    );
    object.insert(
        "max_layer".to_string(),
        JsonValue::Number(artifact.max_layer as f64),
    );
    object.insert(
        "total_connections".to_string(),
        JsonValue::String(artifact.total_connections.to_string()),
    );
    object.insert(
        "avg_connections".to_string(),
        JsonValue::Number(artifact.avg_connections),
    );
    object.insert(
        "entry_point".to_string(),
        match artifact.entry_point {
            Some(value) => JsonValue::String(value.to_string()),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "ivf_n_lists".to_string(),
        match artifact.ivf_n_lists {
            Some(value) => JsonValue::Number(value as f64),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "ivf_non_empty_lists".to_string(),
        match artifact.ivf_non_empty_lists {
            Some(value) => JsonValue::Number(value as f64),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "ivf_trained".to_string(),
        match artifact.ivf_trained {
            Some(value) => JsonValue::Bool(value),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "graph_edge_count".to_string(),
        match artifact.graph_edge_count {
            Some(value) => JsonValue::Number(value as f64),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "graph_node_count".to_string(),
        match artifact.graph_node_count {
            Some(value) => JsonValue::Number(value as f64),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "graph_label_count".to_string(),
        match artifact.graph_label_count {
            Some(value) => JsonValue::Number(value as f64),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "text_doc_count".to_string(),
        match artifact.text_doc_count {
            Some(value) => JsonValue::Number(value as f64),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "text_term_count".to_string(),
        match artifact.text_term_count {
            Some(value) => JsonValue::Number(value as f64),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "text_posting_count".to_string(),
        match artifact.text_posting_count {
            Some(value) => JsonValue::Number(value as f64),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "document_doc_count".to_string(),
        match artifact.document_doc_count {
            Some(value) => JsonValue::Number(value as f64),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "document_path_count".to_string(),
        match artifact.document_path_count {
            Some(value) => JsonValue::Number(value as f64),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "document_value_count".to_string(),
        match artifact.document_value_count {
            Some(value) => JsonValue::Number(value as f64),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "document_unique_value_count".to_string(),
        match artifact.document_unique_value_count {
            Some(value) => JsonValue::Number(value as f64),
            None => JsonValue::Null,
        },
    );
    JsonValue::Object(object)
}

pub(crate) fn native_vector_artifact_batch_json(
    batch: &NativeVectorArtifactBatchInspection,
) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "inspected_count".to_string(),
        JsonValue::Number(batch.inspected_count as f64),
    );
    object.insert(
        "valid_count".to_string(),
        JsonValue::Number(batch.valid_count as f64),
    );
    object.insert(
        "failure_count".to_string(),
        JsonValue::Number(batch.failures.len() as f64),
    );
    object.insert(
        "artifacts".to_string(),
        JsonValue::Array(
            batch
                .artifacts
                .iter()
                .map(native_vector_artifact_inspection_json)
                .collect(),
        ),
    );
    object.insert(
        "failures".to_string(),
        JsonValue::Array(
            batch
                .failures
                .iter()
                .map(|(collection, artifact_kind, error)| {
                    let mut item = Map::new();
                    item.insert(
                        "collection".to_string(),
                        JsonValue::String(collection.clone()),
                    );
                    item.insert(
                        "artifact_kind".to_string(),
                        JsonValue::String(artifact_kind.clone()),
                    );
                    item.insert("error".to_string(), JsonValue::String(error.clone()));
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

pub(crate) fn native_recovery_summary_json(summary: &NativeRecoverySummary) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "snapshot_count".to_string(),
        JsonValue::Number(summary.snapshot_count as f64),
    );
    object.insert(
        "export_count".to_string(),
        JsonValue::Number(summary.export_count as f64),
    );
    object.insert(
        "snapshots_complete".to_string(),
        JsonValue::Bool(summary.snapshots_complete),
    );
    object.insert(
        "exports_complete".to_string(),
        JsonValue::Bool(summary.exports_complete),
    );
    object.insert(
        "omitted_snapshot_count".to_string(),
        JsonValue::Number(summary.omitted_snapshot_count as f64),
    );
    object.insert(
        "omitted_export_count".to_string(),
        JsonValue::Number(summary.omitted_export_count as f64),
    );
    object.insert(
        "snapshots".to_string(),
        JsonValue::Array(
            summary
                .snapshots
                .iter()
                .map(|snapshot| {
                    let mut item = Map::new();
                    item.insert(
                        "snapshot_id".to_string(),
                        JsonValue::String(snapshot.snapshot_id.to_string()),
                    );
                    item.insert(
                        "created_at_unix_ms".to_string(),
                        JsonValue::String(snapshot.created_at_unix_ms.to_string()),
                    );
                    item.insert(
                        "superblock_sequence".to_string(),
                        JsonValue::String(snapshot.superblock_sequence.to_string()),
                    );
                    item.insert(
                        "collection_count".to_string(),
                        JsonValue::Number(snapshot.collection_count as f64),
                    );
                    item.insert(
                        "total_entities".to_string(),
                        JsonValue::String(snapshot.total_entities.to_string()),
                    );
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    object.insert(
        "exports".to_string(),
        JsonValue::Array(
            summary
                .exports
                .iter()
                .map(|export| {
                    let mut item = Map::new();
                    item.insert("name".to_string(), JsonValue::String(export.name.clone()));
                    item.insert(
                        "created_at_unix_ms".to_string(),
                        JsonValue::String(export.created_at_unix_ms.to_string()),
                    );
                    item.insert(
                        "snapshot_id".to_string(),
                        match export.snapshot_id {
                            Some(value) => JsonValue::String(value.to_string()),
                            None => JsonValue::Null,
                        },
                    );
                    item.insert(
                        "superblock_sequence".to_string(),
                        JsonValue::String(export.superblock_sequence.to_string()),
                    );
                    item.insert(
                        "collection_count".to_string(),
                        JsonValue::Number(export.collection_count as f64),
                    );
                    item.insert(
                        "total_entities".to_string(),
                        JsonValue::String(export.total_entities.to_string()),
                    );
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

pub(crate) fn native_catalog_summary_json(summary: &NativeCatalogSummary) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "collection_count".to_string(),
        JsonValue::Number(summary.collection_count as f64),
    );
    object.insert(
        "total_entities".to_string(),
        JsonValue::String(summary.total_entities.to_string()),
    );
    object.insert(
        "collections_complete".to_string(),
        JsonValue::Bool(summary.collections_complete),
    );
    object.insert(
        "omitted_collection_count".to_string(),
        JsonValue::Number(summary.omitted_collection_count as f64),
    );
    object.insert(
        "collections".to_string(),
        JsonValue::Array(
            summary
                .collections
                .iter()
                .map(|collection| {
                    let mut item = Map::new();
                    item.insert(
                        "name".to_string(),
                        JsonValue::String(collection.name.clone()),
                    );
                    item.insert(
                        "entities".to_string(),
                        JsonValue::String(collection.entities.to_string()),
                    );
                    item.insert(
                        "cross_refs".to_string(),
                        JsonValue::String(collection.cross_refs.to_string()),
                    );
                    item.insert(
                        "segments".to_string(),
                        JsonValue::Number(collection.segments as f64),
                    );
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

pub(crate) fn native_physical_state_json<F1, F2, F3, F4>(
    state: &NativePhysicalState,
    header_to_json: F1,
    collection_roots_to_json: F2,
    manifest_to_json: F3,
    registry_to_json: F4,
) -> JsonValue
where
    F1: Fn(crate::storage::engine::PhysicalFileHeader) -> JsonValue,
    F2: Fn(&std::collections::BTreeMap<String, u64>) -> JsonValue,
    F3: Fn(&crate::storage::unified::store::NativeManifestSummary) -> JsonValue,
    F4: Fn(&crate::storage::unified::store::NativeRegistrySummary) -> JsonValue,
{
    let mut object = Map::new();
    object.insert("header".to_string(), header_to_json(state.header));
    object.insert(
        "collection_roots".to_string(),
        collection_roots_to_json(&state.collection_roots),
    );
    object.insert(
        "manifest".to_string(),
        match &state.manifest {
            Some(summary) => manifest_to_json(summary),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "registry".to_string(),
        match &state.registry {
            Some(summary) => registry_to_json(summary),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "recovery".to_string(),
        match &state.recovery {
            Some(summary) => native_recovery_summary_json(summary),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "catalog".to_string(),
        match &state.catalog {
            Some(summary) => native_catalog_summary_json(summary),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "metadata_state".to_string(),
        match &state.metadata_state {
            Some(summary) => native_metadata_state_summary_json(summary),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "vector_artifact_pages".to_string(),
        match &state.vector_artifact_pages {
            Some(summaries) => native_vector_artifact_pages_json(summaries),
            None => JsonValue::Null,
        },
    );
    JsonValue::Object(object)
}

pub(crate) fn native_metadata_state_summary_json(
    summary: &NativeMetadataStateSummary,
) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "protocol_version".to_string(),
        JsonValue::String(summary.protocol_version.clone()),
    );
    object.insert(
        "generated_at_unix_ms".to_string(),
        JsonValue::String(summary.generated_at_unix_ms.to_string()),
    );
    object.insert(
        "last_loaded_from".to_string(),
        match &summary.last_loaded_from {
            Some(value) => JsonValue::String(value.clone()),
            None => JsonValue::Null,
        },
    );
    object.insert(
        "last_healed_at_unix_ms".to_string(),
        match summary.last_healed_at_unix_ms {
            Some(value) => JsonValue::String(value.to_string()),
            None => JsonValue::Null,
        },
    );
    JsonValue::Object(object)
}
