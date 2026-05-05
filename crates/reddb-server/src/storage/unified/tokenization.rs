//! Shared tokenization utilities for indexing and search.
//!
//! Used by the `ContextIndex` and search paths to ensure consistent
//! token generation across all search operations.

use std::collections::BTreeSet;

use crate::storage::schema::Value;

pub const MAX_INDEX_TOKENS: usize = 256;
pub const MAX_TOKEN_LEN: usize = 160;
pub const MAX_JSON_TOKEN_BUDGET: usize = 128;

pub fn push_value_tokens(tokens: &mut BTreeSet<String>, value: &Value) {
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
        _ => {
            push_text_tokens(tokens, &value.to_string(), false);
        }
    }
}

pub fn push_json_tokens(tokens: &mut BTreeSet<String>, bytes: &[u8]) {
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

pub fn push_text_tokens(tokens: &mut BTreeSet<String>, text: &str, split_words: bool) {
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
        tokens.insert(normalized);
    }

    let canonical = canonical_token(token);
    if !canonical.is_empty() {
        tokens.insert(canonical);
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
