use super::transport::{json_response, HttpResponse};
use crate::json::{from_slice as json_from_slice, Map, Value as JsonValue};
use crate::runtime::RedDBRuntime;
use crate::storage::engine::clustering;
use crate::storage::schema::Value;
use crate::storage::unified::{MetadataValue, RefType};

struct ClusterVectorItem {
    entity_id: u64,
    vector: Vec<f32>,
    linked_row_id: Option<u64>,
    linked_row_collection: Option<String>,
    content: Option<String>,
}

pub(crate) fn handle_vector_cluster(runtime: &RedDBRuntime, body: Vec<u8>) -> HttpResponse {
    let body: JsonValue = json_from_slice(&body).unwrap_or(JsonValue::Null);
    let obj = match &body {
        JsonValue::Object(o) => o,
        _ => return json_response(400, err("provide collection, field, and algorithm")),
    };

    let collection = str_field(obj, "collection").unwrap_or_default();
    let field = str_field(obj, "field").unwrap_or_else(|| "embedding".to_string());
    let algorithm = str_field(obj, "algorithm").unwrap_or_else(|| "kmeans".to_string());

    // Collect vectors from the collection
    let store = runtime.db().store();
    let manager = match store.get_collection(&collection) {
        Some(m) => m,
        None => return json_response(404, err(&format!("collection '{}' not found", collection))),
    };

    let mut vectors: Vec<ClusterVectorItem> = Vec::new();
    manager.for_each_entity(|entity| {
        let linked_row_from_refs = entity
            .cross_refs()
            .iter()
            .find(|cross_ref| matches!(cross_ref.ref_type, RefType::VectorToRow))
            .map(|cross_ref| (cross_ref.target.raw(), cross_ref.target_collection.clone()));
        let linked_row_from_metadata =
            store
                .get_metadata(&collection, entity.id)
                .and_then(|metadata| {
                    match (
                        metadata.get("_source_row_id"),
                        metadata.get("_source_collection"),
                    ) {
                        (
                            Some(MetadataValue::Int(row_id)),
                            Some(MetadataValue::String(collection)),
                        ) if *row_id >= 0 => Some((*row_id as u64, collection.clone())),
                        (
                            Some(MetadataValue::Timestamp(row_id)),
                            Some(MetadataValue::String(collection)),
                        ) => Some((*row_id, collection.clone())),
                        _ => None,
                    }
                });
        let linked_row = linked_row_from_refs.or(linked_row_from_metadata);

        if let Some(vector) = entity.data.as_vector() {
            if !vector.dense.is_empty() {
                let (linked_row_id, linked_row_collection) = linked_row
                    .map(|(row_id, collection)| (Some(row_id), Some(collection)))
                    .unwrap_or((None, None));
                vectors.push(ClusterVectorItem {
                    entity_id: entity.id.raw(),
                    vector: vector.dense.clone(),
                    linked_row_id,
                    linked_row_collection,
                    content: vector.content.clone(),
                });
                return true;
            }
        }

        let id = entity.id.raw();
        // Try embeddings first
        {
            let embs = entity.embeddings();
            if let Some(emb) = embs.first() {
                vectors.push(ClusterVectorItem {
                    entity_id: id,
                    vector: emb.vector.clone(),
                    linked_row_id: None,
                    linked_row_collection: None,
                    content: None,
                });
                return true;
            }
        }
        // Try field from row data
        if let Some(row) = entity.data.as_row() {
            if let Some(Value::Vector(v)) = row.get_field(&field) {
                vectors.push(ClusterVectorItem {
                    entity_id: id,
                    vector: v.clone(),
                    linked_row_id: None,
                    linked_row_collection: None,
                    content: None,
                });
            }
        }
        true
    });

    if vectors.is_empty() {
        return json_response(400, err("no vectors found in collection"));
    }

    let vecs: Vec<Vec<f32>> = vectors.iter().map(|item| item.vector.clone()).collect();

    let result = match algorithm.as_str() {
        "dbscan" => {
            let eps = num_field(obj, "eps").unwrap_or(0.5) as f32;
            let min_points = num_field(obj, "min_points").unwrap_or(3.0) as usize;
            clustering::dbscan(&vecs, eps, min_points)
        }
        _ => {
            let k = num_field(obj, "k").unwrap_or(5.0) as usize;
            let max_iter = num_field(obj, "max_iterations").unwrap_or(100.0) as usize;
            clustering::kmeans(&vecs, k, max_iter, 0.001)
        }
    };

    // Build response
    let mut out = Map::new();
    out.insert("ok".to_string(), JsonValue::Bool(true));
    out.insert("algorithm".to_string(), JsonValue::String(algorithm));
    out.insert("k".to_string(), JsonValue::Number(result.k as f64));
    out.insert(
        "iterations".to_string(),
        JsonValue::Number(result.iterations as f64),
    );
    out.insert("converged".to_string(), JsonValue::Bool(result.converged));
    out.insert(
        "cluster_sizes".to_string(),
        JsonValue::Array(
            result
                .cluster_sizes
                .iter()
                .map(|&s| JsonValue::Number(s as f64))
                .collect(),
        ),
    );

    // Entity assignments
    let assignments: Vec<JsonValue> = vectors
        .iter()
        .zip(result.assignments.iter())
        .map(|(item, &cluster_id)| {
            let mut assignment = Map::new();
            assignment.insert(
                "entity_id".to_string(),
                JsonValue::Number(item.entity_id as f64),
            );
            assignment.insert(
                "cluster_id".to_string(),
                JsonValue::Number(cluster_id as f64),
            );
            if let Some(linked_row_id) = item.linked_row_id {
                assignment.insert(
                    "linked_row_id".to_string(),
                    JsonValue::Number(linked_row_id as f64),
                );
            }
            if let Some(linked_row_collection) = &item.linked_row_collection {
                assignment.insert(
                    "linked_row_collection".to_string(),
                    JsonValue::String(linked_row_collection.clone()),
                );
            }
            if let Some(content) = &item.content {
                assignment.insert("content".to_string(), JsonValue::String(content.clone()));
            }
            JsonValue::Object(assignment)
        })
        .collect();

    out.insert("assignments".to_string(), JsonValue::Array(assignments));
    out.insert(
        "total_vectors".to_string(),
        JsonValue::Number(vectors.len() as f64),
    );

    json_response(200, JsonValue::Object(out))
}

fn str_field(obj: &Map<std::string::String, JsonValue>, key: &str) -> Option<String> {
    obj.get(key).and_then(|v| match v {
        JsonValue::String(s) => Some(s.clone()),
        _ => None,
    })
}

fn num_field(obj: &Map<std::string::String, JsonValue>, key: &str) -> Option<f64> {
    obj.get(key).and_then(|v| match v {
        JsonValue::Number(n) => Some(*n),
        _ => None,
    })
}

fn err(msg: &str) -> JsonValue {
    let mut obj = Map::<std::string::String, JsonValue>::new();
    obj.insert("ok".to_string(), JsonValue::Bool(false));
    obj.insert("error".to_string(), JsonValue::String(msg.to_string()));
    JsonValue::Object(obj)
}
