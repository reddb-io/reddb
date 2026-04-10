use std::collections::BTreeSet;

use crate::storage::schema::Value;
use crate::storage::unified::{Metadata, MetadataValue};
use crate::storage::{EntityData, EntityKind, UnifiedEntity};

pub(crate) const INTERNAL_MULTIMODAL_INDEX_PREFIX: &str = "_mm_index.";
pub(crate) const INTERNAL_MULTIMODAL_FIELD_INDEX_PREFIX: &str = "_mm_field_index.";
const MAX_INDEX_TOKENS: usize = 256;
const MAX_FIELD_INDEX_PAIRS: usize = 1024;
const MAX_TOKEN_LEN: usize = 160;
const MAX_JSON_TOKEN_BUDGET: usize = 128;

pub(crate) fn metadata_key_for_multimodal_token(token: &str) -> String {
    format!("{INTERNAL_MULTIMODAL_INDEX_PREFIX}{token}")
}

pub(crate) fn metadata_key_for_field_lookup(index: &str, value: &str) -> String {
    format!("{INTERNAL_MULTIMODAL_FIELD_INDEX_PREFIX}{index}.{value}")
}

pub(crate) fn rebuild_entity_multimodal_index(metadata: &mut Metadata, entity: &UnifiedEntity) {
    metadata.fields.retain(|key, _| {
        !key.starts_with(INTERNAL_MULTIMODAL_INDEX_PREFIX)
            && !key.starts_with(INTERNAL_MULTIMODAL_FIELD_INDEX_PREFIX)
    });

    for token in entity_multimodal_tokens(entity) {
        metadata.set(
            metadata_key_for_multimodal_token(&token),
            MetadataValue::Bool(true),
        );
    }

    for (index, value) in entity_field_lookup_pairs(entity) {
        metadata.set(
            metadata_key_for_field_lookup(&index, &value),
            MetadataValue::Bool(true),
        );
    }
}

pub(crate) fn query_multimodal_tokens(query: &str) -> Vec<String> {
    let mut tokens = BTreeSet::new();
    push_text_tokens(&mut tokens, query, true);
    tokens.into_iter().take(MAX_INDEX_TOKENS).collect()
}

pub(crate) fn query_multimodal_tokens_exact(value: &str) -> Vec<String> {
    let mut tokens = BTreeSet::new();
    push_text_tokens(&mut tokens, value, false);
    tokens.into_iter().take(MAX_INDEX_TOKENS).collect()
}

pub(crate) fn query_lookup_index_tokens(index: &str) -> Vec<String> {
    let mut tokens = BTreeSet::new();
    push_text_tokens(&mut tokens, index, false);
    tokens.into_iter().take(MAX_INDEX_TOKENS).collect()
}

fn entity_multimodal_tokens(entity: &UnifiedEntity) -> Vec<String> {
    let mut tokens = BTreeSet::new();

    push_text_tokens(&mut tokens, &entity.id.raw().to_string(), false);
    push_text_tokens(&mut tokens, &entity.id.to_string(), false);
    push_text_tokens(&mut tokens, entity.kind.collection(), false);
    push_text_tokens(&mut tokens, entity.kind.storage_type(), false);

    match &entity.kind {
        EntityKind::TableRow { row_id, .. } => {
            push_text_tokens(&mut tokens, &row_id.to_string(), false);
            push_text_tokens(&mut tokens, &format!("e{row_id}"), false);
        }
        EntityKind::GraphNode { label, node_type } => {
            push_text_tokens(&mut tokens, label, false);
            push_text_tokens(&mut tokens, node_type, false);
        }
        EntityKind::GraphEdge {
            label,
            from_node,
            to_node,
            ..
        } => {
            push_text_tokens(&mut tokens, label, false);
            push_text_tokens(&mut tokens, from_node, false);
            push_text_tokens(&mut tokens, to_node, false);
        }
        EntityKind::Vector { collection } => {
            push_text_tokens(&mut tokens, collection, false);
        }
    }

    match &entity.data {
        EntityData::Row(row) => {
            if let Some(named) = row.named.as_ref() {
                for (key, value) in named {
                    push_text_tokens(&mut tokens, key, false);
                    push_value_tokens(&mut tokens, value);
                }
            } else {
                for value in &row.columns {
                    push_value_tokens(&mut tokens, value);
                }
            }
        }
        EntityData::Node(node) => {
            for (key, value) in &node.properties {
                push_text_tokens(&mut tokens, key, false);
                push_value_tokens(&mut tokens, value);
            }
        }
        EntityData::Edge(edge) => {
            push_text_tokens(&mut tokens, &edge.weight.to_string(), false);
            for (key, value) in &edge.properties {
                push_text_tokens(&mut tokens, key, false);
                push_value_tokens(&mut tokens, value);
            }
        }
        EntityData::Vector(vector) => {
            if let Some(content) = vector.content.as_ref() {
                push_text_tokens(&mut tokens, content, true);
            }
            push_text_tokens(&mut tokens, &vector.dense.len().to_string(), false);
        }
    }

    for xref in &entity.cross_refs {
        push_text_tokens(&mut tokens, &xref.target.raw().to_string(), false);
        push_text_tokens(&mut tokens, &xref.target.to_string(), false);
        push_text_tokens(&mut tokens, &xref.target_collection, false);
        push_text_tokens(&mut tokens, &format!("{:?}", xref.ref_type), false);
    }

    tokens.into_iter().take(MAX_INDEX_TOKENS).collect()
}

pub(crate) fn entity_multimodal_tokens_for_search(entity: &UnifiedEntity) -> Vec<String> {
    entity_multimodal_tokens(entity)
}

fn entity_field_lookup_pairs(entity: &UnifiedEntity) -> Vec<(String, String)> {
    let mut pairs = BTreeSet::new();

    fn push_pairs(pairs: &mut BTreeSet<(String, String)>, field: &str, value: &Value) -> bool {
        if pairs.len() >= MAX_FIELD_INDEX_PAIRS {
            return true;
        }
        let mut field_tokens = BTreeSet::new();
        push_text_tokens(&mut field_tokens, field, false);

        let mut value_tokens = BTreeSet::new();
        push_value_tokens(&mut value_tokens, value);

        for field_token in &field_tokens {
            for value_token in &value_tokens {
                if field_token.is_empty() || value_token.is_empty() {
                    continue;
                }
                let _ = pairs.insert((field_token.clone(), value_token.clone()));
                if pairs.len() >= MAX_FIELD_INDEX_PAIRS {
                    return true;
                }
            }
        }
        pairs.len() >= MAX_FIELD_INDEX_PAIRS
    }

    match &entity.data {
        EntityData::Row(row) => {
            if let Some(named) = row.named.as_ref() {
                for (field, value) in named {
                    if push_pairs(&mut pairs, field, value) {
                        break;
                    }
                }
            }
        }
        EntityData::Node(node) => {
            for (field, value) in &node.properties {
                if push_pairs(&mut pairs, field, value) {
                    break;
                }
            }
        }
        EntityData::Edge(edge) => {
            for (field, value) in &edge.properties {
                if push_pairs(&mut pairs, field, value) {
                    break;
                }
            }
        }
        EntityData::Vector(vector) => {
            if let Some(content) = vector.content.as_ref() {
                let mut value = BTreeSet::new();
                push_text_tokens(&mut value, content, true);
                for token in value {
                    let _ = pairs.insert(("content".to_string(), token));
                    if pairs.len() >= MAX_FIELD_INDEX_PAIRS {
                        break;
                    }
                }
            }
        }
    }

    pairs.into_iter().collect()
}

fn push_value_tokens(tokens: &mut BTreeSet<String>, value: &Value) {
    match value {
        Value::Null => {}
        Value::Integer(v) => {
            push_text_tokens(tokens, &v.to_string(), false);
            if *v >= 0 {
                push_text_tokens(tokens, &format!("e{v}"), false);
            }
        }
        Value::UnsignedInteger(v) => {
            push_text_tokens(tokens, &v.to_string(), false);
            push_text_tokens(tokens, &format!("e{v}"), false);
        }
        Value::Float(v) => {
            if v.is_finite() {
                push_text_tokens(tokens, &v.to_string(), false);
            }
        }
        Value::Text(v) => push_text_tokens(tokens, v, true),
        Value::Boolean(v) => push_text_tokens(tokens, if *v { "true" } else { "false" }, false),
        Value::Timestamp(v)
        | Value::Duration(v)
        | Value::TimestampMs(v)
        | Value::BigInt(v)
        | Value::Decimal(v) => push_text_tokens(tokens, &v.to_string(), false),
        Value::Phone(v) => push_text_tokens(tokens, &v.to_string(), false),
        Value::Port(v) => push_text_tokens(tokens, &v.to_string(), false),
        Value::NodeRef(v) | Value::EdgeRef(v) | Value::Email(v) | Value::Url(v) => {
            push_text_tokens(tokens, v, true)
        }
        Value::RowRef(collection, id)
        | Value::VectorRef(collection, id)
        | Value::DocRef(collection, id) => {
            push_text_tokens(tokens, collection, false);
            push_text_tokens(tokens, &id.to_string(), false);
            push_text_tokens(tokens, &format!("e{id}"), false);
            push_text_tokens(tokens, &format!("{collection}:{id}"), false);
        }
        Value::KeyRef(collection, key) => {
            push_text_tokens(tokens, collection, false);
            push_text_tokens(tokens, key, true);
            push_text_tokens(tokens, &format!("{collection}:{key}"), true);
        }
        Value::TableRef(table) => push_text_tokens(tokens, table, false),
        Value::Json(bytes) => push_json_tokens(tokens, bytes),
        Value::Array(values) => {
            for item in values {
                push_value_tokens(tokens, item);
                if tokens.len() >= MAX_INDEX_TOKENS {
                    break;
                }
            }
        }
        Value::Blob(_)
        | Value::IpAddr(_)
        | Value::MacAddr(_)
        | Value::Vector(_)
        | Value::Uuid(_)
        | Value::Color(_)
        | Value::Semver(_)
        | Value::Cidr(_, _)
        | Value::Date(_)
        | Value::Time(_)
        | Value::EnumValue(_)
        | Value::Ipv4(_)
        | Value::Ipv6(_)
        | Value::Subnet(_, _)
        | Value::Latitude(_)
        | Value::Longitude(_)
        | Value::GeoPoint(_, _)
        | Value::Country2(_)
        | Value::Country3(_)
        | Value::Lang2(_)
        | Value::Lang5(_)
        | Value::Currency(_)
        | Value::ColorAlpha(_)
        | Value::PageRef(_) => {
            push_text_tokens(tokens, &value.to_string(), false);
        }
    }
}

fn push_json_tokens(tokens: &mut BTreeSet<String>, bytes: &[u8]) {
    fn collect(
        value: &crate::serde_json::Value,
        tokens: &mut BTreeSet<String>,
        budget: &mut usize,
    ) {
        if *budget == 0 || tokens.len() >= MAX_INDEX_TOKENS {
            return;
        }
        match value {
            crate::serde_json::Value::Null => {}
            crate::serde_json::Value::Bool(v) => {
                push_text_tokens(tokens, if *v { "true" } else { "false" }, false);
                *budget = budget.saturating_sub(1);
            }
            crate::serde_json::Value::Number(v) => {
                push_text_tokens(tokens, &v.to_string(), false);
                *budget = budget.saturating_sub(1);
            }
            crate::serde_json::Value::String(v) => {
                push_text_tokens(tokens, v, true);
                *budget = budget.saturating_sub(1);
            }
            crate::serde_json::Value::Array(values) => {
                for item in values {
                    collect(item, tokens, budget);
                    if *budget == 0 || tokens.len() >= MAX_INDEX_TOKENS {
                        break;
                    }
                }
            }
            crate::serde_json::Value::Object(fields) => {
                for (key, item) in fields {
                    push_text_tokens(tokens, key, false);
                    collect(item, tokens, budget);
                    if *budget == 0 || tokens.len() >= MAX_INDEX_TOKENS {
                        break;
                    }
                }
            }
        }
    }

    if let Ok(value) = crate::serde_json::from_slice::<crate::serde_json::Value>(bytes) {
        let mut budget = MAX_JSON_TOKEN_BUDGET;
        collect(&value, tokens, &mut budget);
    }
}

fn push_text_tokens(tokens: &mut BTreeSet<String>, text: &str, split_words: bool) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }

    push_token_variant(tokens, trimmed);

    if let Some((_, rhs)) = trimmed.split_once(':') {
        let rhs = rhs.trim();
        if !rhs.is_empty() {
            push_token_variant(tokens, rhs);
        }
    }

    if split_words {
        for word in trimmed
            .split(|ch: char| ch.is_ascii_whitespace() || [',', ';', '|'].contains(&ch))
            .map(str::trim)
            .filter(|word| !word.is_empty())
        {
            push_token_variant(tokens, word);
        }
    }
}

fn push_token_variant(tokens: &mut BTreeSet<String>, token: &str) {
    let normalized = normalize_token(token);
    if !normalized.is_empty() {
        let _ = tokens.insert(normalized);
    }

    let canonical = canonical_token(token);
    if !canonical.is_empty() {
        let _ = tokens.insert(canonical);
    }
}

fn normalize_token(token: &str) -> String {
    token
        .trim()
        .to_ascii_lowercase()
        .replace(['-', ' '], "_")
        .chars()
        .take(MAX_TOKEN_LEN)
        .collect()
}

fn canonical_token(token: &str) -> String {
    token
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(|ch| ch.to_lowercase())
        .take(MAX_TOKEN_LEN)
        .collect()
}
