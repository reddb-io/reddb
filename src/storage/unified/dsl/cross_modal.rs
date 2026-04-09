use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use super::super::entity::{EntityData, EntityId, EntityKind, RefType, UnifiedEntity};
use super::super::store::UnifiedStore;
use super::builders::{CrossModalMatch, TraversalDirection};

pub(crate) fn merge_cross_modal_match(
    results: &mut HashMap<EntityId, CrossModalMatch>,
    id: EntityId,
    candidate: CrossModalMatch,
) {
    match results.get_mut(&id) {
        Some(existing) => {
            existing.vector_score = existing.vector_score.max(candidate.vector_score);
            existing.graph_score = existing.graph_score.max(candidate.graph_score);
            existing.table_score = existing.table_score.max(candidate.table_score);
            let should_replace_path = match (existing.path.is_empty(), candidate.path.is_empty()) {
                (true, false) => true,
                (false, true) => false,
                (false, false) => candidate.path.len() < existing.path.len(),
                (true, true) => false,
            };
            if should_replace_path {
                existing.path = candidate.path;
            }
        }
        None => {
            results.insert(id, candidate);
        }
    }
}

pub(crate) fn cross_modal_normalize_token(token: &str) -> String {
    token
        .trim()
        .to_ascii_lowercase()
        .replace('-', "_")
        .replace(' ', "_")
}

pub(crate) fn cross_modal_ref_matches_edge_label(
    ref_type: RefType,
    edge_label: Option<&str>,
) -> bool {
    let Some(edge_label) = edge_label else {
        return true;
    };

    let requested = cross_modal_normalize_token(edge_label);
    let actual = cross_modal_ref_type_label(ref_type);

    requested == actual || requested.replace('_', "") == actual.replace('_', "")
}

pub(crate) fn cross_modal_ref_type_label(ref_type: RefType) -> String {
    let raw = format!("{ref_type:?}");
    let mut snake = String::with_capacity(raw.len() + 4);

    for (index, ch) in raw.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 {
                snake.push('_');
            }
            snake.push(ch.to_ascii_lowercase());
        } else {
            snake.push(ch);
        }
    }

    snake
}

pub(crate) fn cross_modal_token_matches_entity_id(token: &str, entity_id: EntityId) -> bool {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return false;
    }

    trimmed == entity_id.to_string()
        || trimmed == entity_id.raw().to_string()
        || trimmed
            .strip_prefix('e')
            .and_then(|value| value.parse::<u64>().ok())
            .is_some_and(|value| value == entity_id.raw())
}

pub(crate) fn cross_modal_parse_entity_id_token(token: &str) -> Option<EntityId> {
    let trimmed = token.trim();
    if trimmed.is_empty() {
        return None;
    }

    trimmed
        .parse::<u64>()
        .ok()
        .or_else(|| {
            trimmed
                .strip_prefix('e')
                .and_then(|value| value.parse::<u64>().ok())
        })
        .map(EntityId::new)
}

pub(crate) fn cross_modal_value_matches_token(
    value: &crate::storage::schema::Value,
    token: &str,
) -> bool {
    match value {
        crate::storage::schema::Value::UnsignedInteger(value) => {
            let token = token.trim();
            token == value.to_string()
                || token
                    .strip_prefix('e')
                    .and_then(|item| item.parse::<u64>().ok())
                    .is_some_and(|item| item == *value)
        }
        crate::storage::schema::Value::Integer(value) => {
            let token = token.trim();
            token == value.to_string()
                || (*value >= 0
                    && token
                        .strip_prefix('e')
                        .and_then(|item| item.parse::<u64>().ok())
                        .is_some_and(|item| item == *value as u64))
        }
        crate::storage::schema::Value::Text(_)
        | crate::storage::schema::Value::NodeRef(_)
        | crate::storage::schema::Value::EdgeRef(_)
        | crate::storage::schema::Value::VectorRef(_, _)
        | crate::storage::schema::Value::RowRef(_, _) => cross_modal_value_tokens(value)
            .into_iter()
            .any(|item| cross_modal_normalize_token(&item) == cross_modal_normalize_token(token)),
        _ => cross_modal_normalize_token(&value.to_string()) == cross_modal_normalize_token(token),
    }
}

pub(crate) fn cross_modal_value_matches_entity_id(
    value: &crate::storage::schema::Value,
    entity_id: EntityId,
) -> bool {
    match value {
        crate::storage::schema::Value::UnsignedInteger(value) => *value == entity_id.raw(),
        crate::storage::schema::Value::Integer(value) if *value >= 0 => {
            *value as u64 == entity_id.raw()
        }
        crate::storage::schema::Value::Text(_)
        | crate::storage::schema::Value::NodeRef(_)
        | crate::storage::schema::Value::EdgeRef(_)
        | crate::storage::schema::Value::VectorRef(_, _)
        | crate::storage::schema::Value::RowRef(_, _) => cross_modal_value_tokens(value)
            .into_iter()
            .any(|item| cross_modal_token_matches_entity_id(&item, entity_id)),
        _ => {
            let rendered = value.to_string();
            cross_modal_token_matches_entity_id(&rendered, entity_id)
        }
    }
}

pub(crate) fn cross_modal_value_tokens(value: &crate::storage::schema::Value) -> Vec<String> {
    match value {
        crate::storage::schema::Value::UnsignedInteger(value) => {
            vec![value.to_string(), format!("e{value}")]
        }
        crate::storage::schema::Value::Integer(value) if *value >= 0 => {
            vec![value.to_string(), format!("e{value}")]
        }
        crate::storage::schema::Value::Integer(value) => vec![value.to_string()],
        crate::storage::schema::Value::Text(value) => {
            let trimmed = value.trim();
            let mut tokens = vec![trimmed.to_string()];
            if trimmed.contains([',', ';', '|']) {
                tokens.extend(
                    trimmed
                        .split([',', ';', '|'])
                        .map(str::trim)
                        .filter(|item| !item.is_empty())
                        .map(str::to_string),
                );
            }
            tokens
        }
        crate::storage::schema::Value::NodeRef(value)
        | crate::storage::schema::Value::EdgeRef(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                Vec::new()
            } else {
                vec![trimmed.to_string()]
            }
        }
        crate::storage::schema::Value::RowRef(collection, row_id) => vec![
            row_id.to_string(),
            format!("e{row_id}"),
            collection.trim().to_string(),
            format!("{}:{row_id}", collection.trim()),
            format!("{}:e{row_id}", collection.trim()),
        ],
        crate::storage::schema::Value::VectorRef(collection, vector_id) => vec![
            vector_id.to_string(),
            format!("e{vector_id}"),
            collection.trim().to_string(),
            format!("{}:{vector_id}", collection.trim()),
            format!("{}:e{vector_id}", collection.trim()),
        ],
        crate::storage::schema::Value::Json(bytes) => {
            fn push(tokens: &mut Vec<String>, seen: &mut HashSet<String>, token: String) {
                let normalized = cross_modal_normalize_token(&token);
                if !normalized.is_empty() && seen.insert(normalized) {
                    tokens.push(token);
                }
            }

            fn collect(
                value: &crate::serde_json::Value,
                depth: usize,
                budget: &mut usize,
                tokens: &mut Vec<String>,
                seen: &mut HashSet<String>,
            ) {
                if depth > 6 || *budget == 0 {
                    return;
                }

                match value {
                    crate::serde_json::Value::String(value) => {
                        let trimmed = value.trim();
                        if trimmed.is_empty() {
                            return;
                        }
                        push(tokens, seen, trimmed.to_string());
                        if trimmed.contains([',', ';', '|']) {
                            for item in trimmed
                                .split([',', ';', '|'])
                                .map(str::trim)
                                .filter(|item| !item.is_empty())
                            {
                                push(tokens, seen, item.to_string());
                            }
                        }
                        *budget = budget.saturating_sub(1);
                    }
                    crate::serde_json::Value::Number(value) => {
                        let i = *value as i64;
                        if *value >= 0.0 && value.fract().abs() < f64::EPSILON {
                            let u = *value as u64;
                            push(tokens, seen, u.to_string());
                            push(tokens, seen, format!("e{u}"));
                        } else if value.fract().abs() < f64::EPSILON {
                            push(tokens, seen, i.to_string());
                            if i >= 0 {
                                push(tokens, seen, format!("e{i}"));
                            }
                        }
                        *budget = budget.saturating_sub(1);
                    }
                    crate::serde_json::Value::Array(items) => {
                        for item in items {
                            collect(item, depth + 1, budget, tokens, seen);
                            if *budget == 0 {
                                break;
                            }
                        }
                    }
                    crate::serde_json::Value::Object(fields) => {
                        for item in fields.values() {
                            collect(item, depth + 1, budget, tokens, seen);
                            if *budget == 0 {
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }

            let mut tokens = Vec::new();
            if let Ok(value) = crate::serde_json::from_slice::<crate::serde_json::Value>(bytes) {
                let mut seen = HashSet::new();
                let mut budget = 64usize;
                collect(&value, 0, &mut budget, &mut tokens, &mut seen);
            }
            tokens
        }
        _ => vec![value.to_string().trim().to_string()],
    }
}

pub(crate) fn cross_modal_extend_reference_tokens(
    tokens: &mut Vec<String>,
    seen: &mut HashSet<String>,
    value: &crate::storage::schema::Value,
) {
    for token in cross_modal_value_tokens(value) {
        let normalized = cross_modal_normalize_token(&token);
        if !normalized.is_empty() && seen.insert(normalized) {
            tokens.push(token);
        }
    }
}

pub(crate) fn cross_modal_entity_reference_tokens(entity: &UnifiedEntity) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut tokens = Vec::new();

    for token in [entity.id.raw().to_string(), entity.id.to_string()] {
        let normalized = cross_modal_normalize_token(&token);
        if seen.insert(normalized) {
            tokens.push(token);
        }
    }

    if let EntityKind::TableRow { row_id, .. } = &entity.kind {
        for token in [row_id.to_string(), format!("e{row_id}")] {
            let normalized = cross_modal_normalize_token(&token);
            if seen.insert(normalized) {
                tokens.push(token);
            }
        }
    }

    if let EntityKind::GraphNode { label, .. } = &entity.kind {
        let label = label.trim();
        let normalized = cross_modal_normalize_token(label);
        if !normalized.is_empty() && seen.insert(normalized) {
            tokens.push(label.to_string());
        }
    }

    match &entity.data {
        EntityData::Row(row) => {
            if let Some(named) = row.named.as_ref() {
                for key in [
                    "id",
                    "_id",
                    "row_id",
                    "entity_id",
                    "_entity_id",
                    "node_id",
                    "edge_id",
                    "from",
                    "to",
                    "from_node",
                    "to_node",
                ] {
                    if let Some(value) = named.get(key) {
                        cross_modal_extend_reference_tokens(&mut tokens, &mut seen, value);
                    }
                }
            }
        }
        EntityData::Node(node) => {
            for key in ["id", "_id", "node_id", "entity_id", "_entity_id"] {
                if let Some(value) = node.properties.get(key) {
                    cross_modal_extend_reference_tokens(&mut tokens, &mut seen, value);
                }
            }
        }
        EntityData::Edge(edge) => {
            if let EntityKind::GraphEdge {
                label,
                from_node,
                to_node,
                ..
            } = &entity.kind
            {
                for token in [label.trim(), from_node.trim(), to_node.trim()] {
                    let normalized = cross_modal_normalize_token(token);
                    if !normalized.is_empty() && seen.insert(normalized) {
                        tokens.push(token.to_string());
                    }
                }
            }
            for key in [
                "id",
                "_id",
                "edge_id",
                "entity_id",
                "_entity_id",
                "from",
                "to",
                "from_node",
                "to_node",
            ] {
                if let Some(value) = edge.properties.get(key) {
                    cross_modal_extend_reference_tokens(&mut tokens, &mut seen, value);
                }
            }
        }
        EntityData::Vector(data) => {
            if let EntityKind::Vector { collection } = &entity.kind {
                let collection = collection.trim();
                let normalized = cross_modal_normalize_token(collection);
                if !normalized.is_empty() && seen.insert(normalized) {
                    tokens.push(collection.to_string());
                }
            }

            if let Some(content) = data.content.as_ref() {
                let content = content.trim();
                let normalized = cross_modal_normalize_token(content);
                if !normalized.is_empty() && seen.insert(normalized) {
                    tokens.push(content.to_string());
                }
            }
        }
    }

    tokens
}

pub(crate) fn cross_modal_value_matches_entity(
    value: &crate::storage::schema::Value,
    entity: &UnifiedEntity,
) -> bool {
    if cross_modal_value_matches_entity_id(value, entity.id) {
        return true;
    }

    cross_modal_entity_reference_tokens(entity)
        .into_iter()
        .any(|token| cross_modal_value_matches_token(value, &token))
}

pub(crate) fn cross_modal_graph_node_matches_ref(entity: &UnifiedEntity, node_ref: &str) -> bool {
    if !matches!(entity.kind, EntityKind::GraphNode { .. }) {
        return false;
    }

    let normalized_ref = cross_modal_normalize_token(node_ref);
    if cross_modal_entity_reference_tokens(entity)
        .into_iter()
        .any(|token| cross_modal_normalize_token(&token) == normalized_ref)
    {
        return true;
    }

    match &entity.data {
        EntityData::Node(node) => node
            .properties
            .get("id")
            .or_else(|| node.properties.get("_id"))
            .or_else(|| node.properties.get("node_id"))
            .or_else(|| node.properties.get("entity_id"))
            .or_else(|| node.properties.get("_entity_id"))
            .is_some_and(|value| cross_modal_value_matches_token(value, node_ref)),
        _ => false,
    }
}

pub(crate) fn cross_modal_lookup_graph_nodes_by_ref(
    store: &Arc<UnifiedStore>,
    node_ref: &str,
) -> Vec<UnifiedEntity> {
    let mut seen = HashSet::new();
    let mut nodes = Vec::new();

    if let Some(entity_id) = cross_modal_parse_entity_id_token(node_ref) {
        if let Some((_, entity)) = store.get_any(entity_id) {
            if cross_modal_graph_node_matches_ref(&entity, node_ref) && seen.insert(entity.id) {
                nodes.push(entity);
            }
        }
    }

    for collection in store.list_collections() {
        let Some(manager) = store.get_collection(&collection) else {
            continue;
        };

        for entity in
            manager.query_all(|candidate| matches!(&candidate.kind, EntityKind::GraphNode { .. }))
        {
            if cross_modal_graph_node_matches_ref(&entity, node_ref) && seen.insert(entity.id) {
                nodes.push(entity);
            }
        }
    }

    nodes
}

pub(crate) fn cross_modal_graph_neighbors(
    store: &Arc<UnifiedStore>,
    node_id: EntityId,
    direction: &TraversalDirection,
    edge_label: Option<&str>,
) -> Vec<UnifiedEntity> {
    let Some((_, current_node)) = store.get_any(node_id) else {
        return Vec::new();
    };
    let mut seen = HashSet::new();
    let mut neighbors = Vec::new();

    for collection in store.list_collections() {
        let Some(manager) = store.get_collection(&collection) else {
            continue;
        };

        for entity in
            manager.query_all(|candidate| matches!(&candidate.kind, EntityKind::GraphEdge { .. }))
        {
            let EntityKind::GraphEdge {
                label,
                from_node,
                to_node,
                ..
            } = &entity.kind
            else {
                continue;
            };

            if edge_label.is_some_and(|requested| {
                cross_modal_normalize_token(requested) != cross_modal_normalize_token(label)
            }) {
                continue;
            }

            let neighbor_ref = match direction {
                TraversalDirection::Out
                    if cross_modal_graph_node_matches_ref(&current_node, from_node) =>
                {
                    Some(to_node.as_str())
                }
                TraversalDirection::In
                    if cross_modal_graph_node_matches_ref(&current_node, to_node) =>
                {
                    Some(from_node.as_str())
                }
                TraversalDirection::Both
                    if cross_modal_graph_node_matches_ref(&current_node, from_node) =>
                {
                    Some(to_node.as_str())
                }
                TraversalDirection::Both
                    if cross_modal_graph_node_matches_ref(&current_node, to_node) =>
                {
                    Some(from_node.as_str())
                }
                _ => None,
            };

            let Some(neighbor_ref) = neighbor_ref else {
                continue;
            };

            for neighbor in cross_modal_lookup_graph_nodes_by_ref(store, neighbor_ref) {
                if seen.insert(neighbor.id) {
                    neighbors.push(neighbor);
                }
            }
        }
    }

    neighbors
}

pub(crate) fn cross_modal_entity_vectors(entity: &UnifiedEntity) -> Vec<Vec<f32>> {
    let mut vectors = entity
        .embeddings
        .iter()
        .filter_map(|embedding| {
            if embedding.vector.is_empty() {
                None
            } else {
                Some(embedding.vector.clone())
            }
        })
        .collect::<Vec<_>>();

    if let EntityData::Vector(data) = &entity.data {
        if !data.dense.is_empty() {
            vectors.push(data.dense.clone());
        }
    }

    vectors
}
