//! Helper functions for query execution
//!
//! Utility functions for filtering, similarity calculation, and text extraction.

use crate::storage::schema::Value;

use super::super::entity::{EntityData, EntityKind, UnifiedEntity};
use super::filters::{Filter, FilterOp, FilterValue};

/// Apply all filters to an entity
pub fn apply_filters(entity: &UnifiedEntity, filters: &[Filter]) -> bool {
    for filter in filters {
        let value = get_entity_field(entity, &filter.field);
        if !match_filter(&value, &filter.op, &filter.value) {
            return false;
        }
    }
    true
}

/// Get a field value from an entity
pub fn get_entity_field(entity: &UnifiedEntity, field: &str) -> Option<Value> {
    // Check special fields first
    match field {
        "id" => return Some(Value::Integer(entity.id.raw() as i64)),
        "created_at" => return Some(Value::Integer(entity.created_at as i64)),
        "updated_at" => return Some(Value::Integer(entity.updated_at as i64)),
        _ => {}
    }

    // Check entity data properties
    match &entity.data {
        EntityData::Node(node) => node.get(field).cloned(),
        EntityData::Edge(edge) => edge.get(field).cloned(),
        EntityData::Row(row) => row.get_by_name(field).cloned(),
        EntityData::Vector(vec) => {
            if field == "content" {
                vec.content.as_ref().map(|c| Value::Text(c.clone()))
            } else {
                None
            }
        }
        EntityData::TimeSeries(_) => None,
        EntityData::QueueMessage(_) => None,
    }
}

/// Match a filter against a value
pub fn match_filter(value: &Option<Value>, op: &FilterOp, filter_value: &FilterValue) -> bool {
    match (value, op, filter_value) {
        (Some(Value::Text(s)), FilterOp::Equals, FilterValue::String(fs)) => s == fs,
        (Some(Value::Integer(i)), FilterOp::Equals, FilterValue::Int(fi)) => *i == *fi,
        (Some(Value::Float(f)), FilterOp::Equals, FilterValue::Float(ff)) => {
            (*f - *ff).abs() < 0.0001
        }
        (Some(Value::Boolean(b)), FilterOp::Equals, FilterValue::Bool(fb)) => *b == *fb,

        (Some(Value::Text(s)), FilterOp::Contains, FilterValue::String(fs)) => {
            s.contains(fs.as_str())
        }
        (Some(Value::Text(s)), FilterOp::StartsWith, FilterValue::String(fs)) => {
            s.starts_with(fs.as_str())
        }
        (Some(Value::Text(s)), FilterOp::EndsWith, FilterValue::String(fs)) => {
            s.ends_with(fs.as_str())
        }

        (Some(Value::Integer(i)), FilterOp::GreaterThan, FilterValue::Int(fi)) => *i > *fi,
        (Some(Value::Integer(i)), FilterOp::LessThan, FilterValue::Int(fi)) => *i < *fi,
        (Some(Value::Float(f)), FilterOp::GreaterThan, FilterValue::Float(ff)) => *f > *ff,
        (Some(Value::Float(f)), FilterOp::LessThan, FilterValue::Float(ff)) => *f < *ff,

        (None, FilterOp::IsNull, _) => true,
        (Some(_), FilterOp::IsNotNull, _) => true,
        (None, FilterOp::IsNotNull, _) => false,
        (Some(_), FilterOp::IsNull, _) => false,

        _ => false,
    }
}

/// Calculate vector similarity between an entity and a query vector
pub fn calculate_entity_similarity(
    entity: &UnifiedEntity,
    query: &[f32],
    slot: &Option<String>,
) -> f32 {
    let mut best_similarity = 0.0f32;

    // Check entity embeddings
    for emb in &entity.embeddings {
        if let Some(ref slot_name) = slot {
            if &emb.name != slot_name {
                continue;
            }
        }
        let sim = cosine_similarity(query, &emb.vector);
        best_similarity = best_similarity.max(sim);
    }

    // Check vector data if entity is a vector
    if let EntityData::Vector(ref vec_data) = entity.data {
        let sim = cosine_similarity(query, &vec_data.dense);
        best_similarity = best_similarity.max(sim);
    }

    best_similarity
}

/// Calculate cosine similarity between two vectors
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    for i in 0..a.len() {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }

    let denom = (norm_a * norm_b).sqrt();
    if denom > 0.0 {
        dot / denom
    } else {
        0.0
    }
}

/// Extract searchable text from an entity
pub fn extract_searchable_text(entity: &UnifiedEntity) -> String {
    let mut parts = Vec::new();

    // Add kind info
    match &entity.kind {
        EntityKind::GraphNode { label, node_type } => {
            parts.push(label.clone());
            parts.push(node_type.clone());
        }
        EntityKind::GraphEdge { label, .. } => {
            parts.push(label.clone());
        }
        EntityKind::TableRow { table, .. } => {
            parts.push(table.clone());
        }
        EntityKind::Vector { collection } => {
            parts.push(collection.clone());
        }
        EntityKind::TimeSeriesPoint { series, metric } => {
            parts.push(series.clone());
            parts.push(metric.clone());
        }
        EntityKind::QueueMessage { queue, .. } => {
            parts.push(queue.clone());
        }
    }

    // Add data properties
    match &entity.data {
        EntityData::Node(node) => {
            for (k, v) in &node.properties {
                parts.push(k.clone());
                if let Value::Text(s) = v {
                    parts.push(s.clone());
                }
            }
        }
        EntityData::Edge(edge) => {
            for (k, v) in &edge.properties {
                parts.push(k.clone());
                if let Value::Text(s) = v {
                    parts.push(s.clone());
                }
            }
        }
        EntityData::Row(row) => {
            for col in &row.columns {
                if let Value::Text(s) = col {
                    parts.push(s.clone());
                }
            }
        }
        EntityData::Vector(vec) => {
            if let Some(ref content) = vec.content {
                parts.push(content.clone());
            }
        }
        EntityData::TimeSeries(ts) => {
            parts.push(ts.metric.clone());
        }
        EntityData::QueueMessage(_) => {}
    }

    parts.join(" ")
}
