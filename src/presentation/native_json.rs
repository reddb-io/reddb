use crate::json::{Map, Value as JsonValue};
use crate::storage::engine::PhysicalFileHeader;
use crate::storage::unified::store::NativeManifestSummary;
use std::collections::BTreeMap;

pub(crate) fn snapshot_descriptor_json(snapshot: &crate::SnapshotDescriptor) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "snapshot_id".to_string(),
        JsonValue::String(snapshot.snapshot_id.to_string()),
    );
    object.insert(
        "created_at_unix_ms".to_string(),
        JsonValue::String(snapshot.created_at_unix_ms.to_string()),
    );
    object.insert(
        "superblock_sequence".to_string(),
        JsonValue::String(snapshot.superblock_sequence.to_string()),
    );
    object.insert(
        "collection_count".to_string(),
        JsonValue::Number(snapshot.collection_count as f64),
    );
    object.insert(
        "total_entities".to_string(),
        JsonValue::Number(snapshot.total_entities as f64),
    );
    JsonValue::Object(object)
}

pub(crate) fn snapshots_json(snapshots: &[crate::SnapshotDescriptor]) -> JsonValue {
    JsonValue::Array(snapshots.iter().map(snapshot_descriptor_json).collect())
}

pub(crate) fn export_descriptor_json(export: &crate::ExportDescriptor) -> JsonValue {
    let mut object = Map::new();
    object.insert("name".to_string(), JsonValue::String(export.name.clone()));
    object.insert(
        "created_at_unix_ms".to_string(),
        JsonValue::String(export.created_at_unix_ms.to_string()),
    );
    object.insert(
        "snapshot_id".to_string(),
        export
            .snapshot_id
            .map(|value| JsonValue::String(value.to_string()))
            .unwrap_or(JsonValue::Null),
    );
    object.insert(
        "superblock_sequence".to_string(),
        JsonValue::String(export.superblock_sequence.to_string()),
    );
    object.insert(
        "data_path".to_string(),
        JsonValue::String(export.data_path.clone()),
    );
    object.insert(
        "metadata_path".to_string(),
        JsonValue::String(export.metadata_path.clone()),
    );
    object.insert(
        "collection_count".to_string(),
        JsonValue::Number(export.collection_count as f64),
    );
    object.insert(
        "total_entities".to_string(),
        JsonValue::Number(export.total_entities as f64),
    );
    JsonValue::Object(object)
}

pub(crate) fn exports_json(exports: &[crate::ExportDescriptor]) -> JsonValue {
    JsonValue::Array(exports.iter().map(export_descriptor_json).collect())
}

pub(crate) fn manifest_events_json(events: &[crate::ManifestEvent]) -> JsonValue {
    JsonValue::Array(events.iter().map(manifest_event_json).collect())
}

pub(crate) fn manifest_event_json(event: &crate::ManifestEvent) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "collection".to_string(),
        JsonValue::String(event.collection.clone()),
    );
    object.insert(
        "object_key".to_string(),
        JsonValue::String(event.object_key.clone()),
    );
    object.insert(
        "kind".to_string(),
        JsonValue::String(
            match event.kind {
                crate::ManifestEventKind::Insert => "insert",
                crate::ManifestEventKind::Update => "update",
                crate::ManifestEventKind::Remove => "remove",
                crate::ManifestEventKind::Checkpoint => "checkpoint",
            }
            .to_string(),
        ),
    );
    object.insert("block".to_string(), block_reference_json(&event.block));
    object.insert(
        "snapshot_min".to_string(),
        JsonValue::String(event.snapshot_min.to_string()),
    );
    object.insert(
        "snapshot_max".to_string(),
        match event.snapshot_max {
            Some(value) => JsonValue::String(value.to_string()),
            None => JsonValue::Null,
        },
    );
    JsonValue::Object(object)
}

pub(crate) fn collection_roots_json(roots: &BTreeMap<String, u64>) -> JsonValue {
    let mut object = Map::new();
    for (collection, root) in roots {
        object.insert(collection.clone(), JsonValue::String(root.to_string()));
    }
    JsonValue::Object(object)
}

pub(crate) fn native_header_json(header: PhysicalFileHeader) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "format_version".to_string(),
        JsonValue::Number(header.format_version as f64),
    );
    object.insert(
        "sequence".to_string(),
        JsonValue::String(header.sequence.to_string()),
    );
    object.insert(
        "manifest_oldest_root".to_string(),
        JsonValue::String(header.manifest_oldest_root.to_string()),
    );
    object.insert(
        "manifest_root".to_string(),
        JsonValue::String(header.manifest_root.to_string()),
    );
    object.insert(
        "free_set_root".to_string(),
        JsonValue::String(header.free_set_root.to_string()),
    );
    object.insert(
        "manifest_page".to_string(),
        JsonValue::Number(header.manifest_page as f64),
    );
    object.insert(
        "manifest_checksum".to_string(),
        JsonValue::String(header.manifest_checksum.to_string()),
    );
    object.insert(
        "collection_roots_page".to_string(),
        JsonValue::Number(header.collection_roots_page as f64),
    );
    object.insert(
        "collection_roots_checksum".to_string(),
        JsonValue::String(header.collection_roots_checksum.to_string()),
    );
    object.insert(
        "collection_root_count".to_string(),
        JsonValue::Number(header.collection_root_count as f64),
    );
    object.insert(
        "snapshot_count".to_string(),
        JsonValue::Number(header.snapshot_count as f64),
    );
    object.insert(
        "index_count".to_string(),
        JsonValue::Number(header.index_count as f64),
    );
    object.insert(
        "catalog_collection_count".to_string(),
        JsonValue::Number(header.catalog_collection_count as f64),
    );
    object.insert(
        "catalog_total_entities".to_string(),
        JsonValue::String(header.catalog_total_entities.to_string()),
    );
    object.insert(
        "export_count".to_string(),
        JsonValue::Number(header.export_count as f64),
    );
    object.insert(
        "graph_projection_count".to_string(),
        JsonValue::Number(header.graph_projection_count as f64),
    );
    object.insert(
        "analytics_job_count".to_string(),
        JsonValue::Number(header.analytics_job_count as f64),
    );
    object.insert(
        "manifest_event_count".to_string(),
        JsonValue::Number(header.manifest_event_count as f64),
    );
    object.insert(
        "registry_page".to_string(),
        JsonValue::Number(header.registry_page as f64),
    );
    object.insert(
        "registry_checksum".to_string(),
        JsonValue::String(header.registry_checksum.to_string()),
    );
    object.insert(
        "recovery_page".to_string(),
        JsonValue::Number(header.recovery_page as f64),
    );
    object.insert(
        "recovery_checksum".to_string(),
        JsonValue::String(header.recovery_checksum.to_string()),
    );
    object.insert(
        "catalog_page".to_string(),
        JsonValue::Number(header.catalog_page as f64),
    );
    object.insert(
        "catalog_checksum".to_string(),
        JsonValue::String(header.catalog_checksum.to_string()),
    );
    object.insert(
        "metadata_state_page".to_string(),
        JsonValue::Number(header.metadata_state_page as f64),
    );
    object.insert(
        "metadata_state_checksum".to_string(),
        JsonValue::String(header.metadata_state_checksum.to_string()),
    );
    object.insert(
        "vector_artifact_page".to_string(),
        JsonValue::Number(header.vector_artifact_page as f64),
    );
    object.insert(
        "vector_artifact_checksum".to_string(),
        JsonValue::String(header.vector_artifact_checksum.to_string()),
    );
    JsonValue::Object(object)
}

pub(crate) fn repair_policy_json(policy: &str) -> JsonValue {
    let mut object = Map::new();
    object.insert("policy".to_string(), JsonValue::String(policy.to_string()));
    JsonValue::Object(object)
}

pub(crate) fn native_manifest_summary_json(summary: &NativeManifestSummary) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "sequence".to_string(),
        JsonValue::String(summary.sequence.to_string()),
    );
    object.insert(
        "event_count".to_string(),
        JsonValue::Number(summary.event_count as f64),
    );
    object.insert(
        "events_complete".to_string(),
        JsonValue::Bool(summary.events_complete),
    );
    object.insert(
        "omitted_event_count".to_string(),
        JsonValue::Number(summary.omitted_event_count as f64),
    );
    object.insert(
        "recent_events".to_string(),
        JsonValue::Array(
            summary
                .recent_events
                .iter()
                .map(|event| {
                    let mut item = Map::new();
                    item.insert(
                        "collection".to_string(),
                        JsonValue::String(event.collection.clone()),
                    );
                    item.insert(
                        "object_key".to_string(),
                        JsonValue::String(event.object_key.clone()),
                    );
                    item.insert("kind".to_string(), JsonValue::String(event.kind.clone()));
                    item.insert(
                        "block_index".to_string(),
                        JsonValue::String(event.block_index.to_string()),
                    );
                    item.insert(
                        "block_checksum".to_string(),
                        JsonValue::String(event.block_checksum.to_string()),
                    );
                    item.insert(
                        "snapshot_min".to_string(),
                        JsonValue::String(event.snapshot_min.to_string()),
                    );
                    item.insert(
                        "snapshot_max".to_string(),
                        match event.snapshot_max {
                            Some(value) => JsonValue::String(value.to_string()),
                            None => JsonValue::Null,
                        },
                    );
                    JsonValue::Object(item)
                })
                .collect(),
        ),
    );
    JsonValue::Object(object)
}

fn block_reference_json(reference: &crate::BlockReference) -> JsonValue {
    let mut object = Map::new();
    object.insert(
        "index".to_string(),
        JsonValue::String(reference.index.to_string()),
    );
    object.insert(
        "checksum".to_string(),
        JsonValue::String(reference.checksum.to_string()),
    );
    JsonValue::Object(object)
}
