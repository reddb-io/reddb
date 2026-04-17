//! Context Index — dedicated cross-structure inverted index for context search.
//!
//! Replaces the `_mm_index.*` / `_mm_field_index.*` metadata hack with a proper
//! inverted index structure that maps tokens and field:value pairs to posting lists.
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────────────┐
//! │                          ContextIndex                                │
//! ├──────────────────────────────────────────────────────────────────────┤
//! │  ┌──────────────────────────────┐  ┌──────────────────────────────┐  │
//! │  │  Token Index                 │  │  Field-Value Index           │  │
//! │  │  BTreeMap<token, postings>   │  │  BTreeMap<(f,v), postings>  │  │
//! │  │                              │  │                              │  │
//! │  │  "00000000000" → [e42, e99]  │  │  ("cpf","081...") → [e42]   │  │
//! │  │  "alice" → [e42, e55]       │  │  ("name","alice") → [e55]   │  │
//! │  └──────────────────────────────┘  └──────────────────────────────┘  │
//! └──────────────────────────────────────────────────────────────────────┘
//! ```

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use parking_lot::RwLock;

use super::entity::{EntityData, EntityId, EntityKind, UnifiedEntity};
use super::tokenization::{push_text_tokens, push_value_tokens, MAX_INDEX_TOKENS};
use crate::storage::schema::Value;

const MAX_FIELD_INDEX_PAIRS: usize = 1024;

// ============================================================================
// Types
// ============================================================================

/// A posting entry pointing to a specific entity in a collection.
#[derive(Debug, Clone)]
pub struct ContextPosting {
    pub entity_id: EntityId,
    pub collection: String,
    pub field: String,
}

/// A search hit from the context index.
#[derive(Debug, Clone)]
pub struct ContextSearchHit {
    pub entity_id: EntityId,
    pub collection: String,
    pub score: f32,
    pub matched_tokens: usize,
    pub total_tokens: usize,
}

/// Statistics about the context index.
#[derive(Debug, Clone, Default)]
pub struct ContextIndexStats {
    pub indexed_entities: usize,
    pub token_count: usize,
    pub field_value_count: usize,
    pub total_postings: usize,
}

/// Keys tracked per entity for O(k) removal.
#[derive(Debug, Clone, Default)]
struct EntityKeys {
    token_keys: Vec<String>,
    field_value_keys: Vec<(String, String)>,
}

// ============================================================================
// ContextIndex
// ============================================================================

/// Dedicated cross-structure inverted index for context search.
///
/// Uses a reverse index (`entity_id → keys`) so that removal is O(k) where k
/// is the number of tokens for that entity, instead of O(total_tokens).
pub struct ContextIndex {
    /// Token → posting list (all entities containing this token)
    tokens: RwLock<BTreeMap<String, Vec<ContextPosting>>>,
    /// (field, value_token) → posting list (field-specific lookups)
    field_values: RwLock<BTreeMap<(String, String), Vec<ContextPosting>>>,
    /// Reverse index: entity_id → keys it was indexed under (for fast removal)
    reverse: RwLock<HashMap<u64, EntityKeys>>,
    /// Set of currently indexed entity IDs (accurate count)
    indexed: RwLock<HashSet<u64>>,
}

impl ContextIndex {
    /// Create a new empty context index.
    pub fn new() -> Self {
        Self {
            tokens: RwLock::new(BTreeMap::new()),
            field_values: RwLock::new(BTreeMap::new()),
            reverse: RwLock::new(HashMap::new()),
            indexed: RwLock::new(HashSet::new()),
        }
    }

    /// Index an entity — extracts tokens and field:value pairs into posting lists.
    pub fn index_entity(&self, collection: &str, entity: &UnifiedEntity) {
        if context_index_disabled() {
            return;
        }
        self.index_entities(collection, std::iter::once(entity));
    }

    /// Batch variant of `index_entity` that amortizes lock traffic across
    /// multiple rewrites.
    pub fn index_entities<'a, I>(&self, collection: &str, entities: I)
    where
        I: IntoIterator<Item = &'a UnifiedEntity>,
    {
        if context_index_disabled() {
            return;
        }
        let collection = collection.to_string();
        let prepared: Vec<(
            u64,
            EntityKeys,
            Vec<(String, String)>,
            Vec<(String, String)>,
        )> = entities
            .into_iter()
            .map(|entity| {
                let entity_tokens = extract_entity_tokens(entity);
                let field_pairs = extract_field_lookup_pairs(entity);
                let mut keys = EntityKeys::default();
                keys.token_keys = entity_tokens
                    .iter()
                    .map(|(token, _)| token.clone())
                    .collect();
                keys.field_value_keys = field_pairs.clone();
                (entity.id.raw(), keys, entity_tokens, field_pairs)
            })
            .collect();

        if prepared.is_empty() {
            return;
        }

        let previous_keys: Vec<(u64, EntityKeys)> = {
            let mut reverse = self.reverse.write();
            prepared
                .iter()
                .filter_map(|(entity_id, _, _, _)| {
                    reverse.remove(entity_id).map(|keys| (*entity_id, keys))
                })
                .collect()
        };

        {
            let mut index = self.tokens.write();
            for (entity_id, keys) in &previous_keys {
                let entity_id = EntityId::new(*entity_id);
                for key in &keys.token_keys {
                    if let Some(postings) = index.get_mut(key) {
                        postings.retain(|posting| posting.entity_id != entity_id);
                        if postings.is_empty() {
                            index.remove(key);
                        }
                    }
                }
            }

            for (entity_id, _, entity_tokens, _) in &prepared {
                let entity_id = EntityId::new(*entity_id);
                for (token, field) in entity_tokens {
                    index
                        .entry(token.clone())
                        .or_default()
                        .push(ContextPosting {
                            entity_id,
                            collection: collection.clone(),
                            field: field.clone(),
                        });
                }
            }
        }

        {
            let mut index = self.field_values.write();
            for (entity_id, keys) in &previous_keys {
                let entity_id = EntityId::new(*entity_id);
                for key in &keys.field_value_keys {
                    if let Some(postings) = index.get_mut(key) {
                        postings.retain(|posting| posting.entity_id != entity_id);
                        if postings.is_empty() {
                            index.remove(key);
                        }
                    }
                }
            }

            for (entity_id, _, _, field_pairs) in &prepared {
                let entity_id = EntityId::new(*entity_id);
                for (field, value_token) in field_pairs {
                    index
                        .entry((field.clone(), value_token.clone()))
                        .or_default()
                        .push(ContextPosting {
                            entity_id,
                            collection: collection.clone(),
                            field: field.clone(),
                        });
                }
            }
        }

        {
            let mut reverse = self.reverse.write();
            for (entity_id, keys, _, _) in &prepared {
                reverse.insert(*entity_id, keys.clone());
            }
        }

        {
            let mut indexed = self.indexed.write();
            for (entity_id, _, _, _) in &prepared {
                indexed.insert(*entity_id);
            }
        }
    }

    /// Remove all postings for an entity. O(k) where k = entity's token count.
    pub fn remove_entity(&self, entity_id: EntityId) {
        let keys = {
            let mut reverse = self.reverse.write();
            reverse.remove(&entity_id.raw())
        };

        let Some(keys) = keys else {
            return;
        };

        if !keys.token_keys.is_empty() {
            let mut index = self.tokens.write();
            for key in &keys.token_keys {
                if let Some(postings) = index.get_mut(key) {
                    postings.retain(|p| p.entity_id != entity_id);
                    if postings.is_empty() {
                        index.remove(key);
                    }
                }
            }
        }

        if !keys.field_value_keys.is_empty() {
            let mut index = self.field_values.write();
            for key in &keys.field_value_keys {
                if let Some(postings) = index.get_mut(key) {
                    postings.retain(|p| p.entity_id != entity_id);
                    if postings.is_empty() {
                        index.remove(key);
                    }
                }
            }
        }

        {
            let mut indexed = self.indexed.write();
            indexed.remove(&entity_id.raw());
        }
    }

    /// Search by tokens — tokenizes the query, looks up posting lists, and scores by overlap.
    pub fn search(
        &self,
        query: &str,
        limit: usize,
        allowed_collections: Option<&BTreeSet<String>>,
    ) -> Vec<ContextSearchHit> {
        let query_tokens = tokenize_query(query);
        if query_tokens.is_empty() {
            return Vec::new();
        }

        let index = self.tokens.read();

        let mut scored: HashMap<u64, (String, usize)> = HashMap::new();

        for token in &query_tokens {
            if let Some(postings) = index.get(token) {
                for posting in postings {
                    if allowed_collections
                        .as_ref()
                        .is_some_and(|allowed| !allowed.contains(&posting.collection))
                    {
                        continue;
                    }
                    let entry = scored
                        .entry(posting.entity_id.raw())
                        .or_insert_with(|| (posting.collection.clone(), 0));
                    entry.1 += 1;
                }
            }
        }

        let total_tokens = query_tokens.len();
        let mut results: Vec<ContextSearchHit> = scored
            .into_iter()
            .map(|(entity_id, (collection, overlap))| ContextSearchHit {
                entity_id: EntityId::new(entity_id),
                collection,
                score: (overlap as f32 / total_tokens.max(1) as f32).min(1.0),
                matched_tokens: overlap,
                total_tokens,
            })
            .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.entity_id.raw().cmp(&b.entity_id.raw()))
        });
        results.truncate(limit.max(1));
        results
    }

    /// Search by field:value — direct lookup in the field-value index.
    pub fn search_field(
        &self,
        field: &str,
        value: &str,
        exact: bool,
        limit: usize,
        allowed_collections: Option<&BTreeSet<String>>,
    ) -> Vec<ContextSearchHit> {
        let field_tokens = tokenize_field_name(field);
        let value_tokens = if exact {
            tokenize_exact(value)
        } else {
            tokenize_query(value)
        };

        if field_tokens.is_empty() || value_tokens.is_empty() {
            return Vec::new();
        }

        let index = self.field_values.read();

        let mut scored: HashMap<u64, (String, usize)> = HashMap::new();
        let mut total_pairs = 0usize;

        for field_token in &field_tokens {
            for value_token in &value_tokens {
                total_pairs += 1;
                if let Some(postings) = index.get(&(field_token.clone(), value_token.clone())) {
                    for posting in postings {
                        if allowed_collections
                            .as_ref()
                            .is_some_and(|allowed| !allowed.contains(&posting.collection))
                        {
                            continue;
                        }
                        let entry = scored
                            .entry(posting.entity_id.raw())
                            .or_insert_with(|| (posting.collection.clone(), 0));
                        entry.1 += 1;
                    }
                }
            }
        }

        let mut results: Vec<ContextSearchHit> = scored
            .into_iter()
            .map(|(entity_id, (collection, overlap))| ContextSearchHit {
                entity_id: EntityId::new(entity_id),
                collection,
                score: (overlap as f32 / total_pairs.max(1) as f32).min(1.0),
                matched_tokens: overlap,
                total_tokens: total_pairs,
            })
            .collect();

        results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.entity_id.raw().cmp(&b.entity_id.raw()))
        });
        results.truncate(limit.max(1));
        results
    }

    /// Return index statistics.
    pub fn stats(&self) -> ContextIndexStats {
        let token_count = self.tokens.read().len();
        let field_value_count = self.field_values.read().len();
        let total_postings = self.tokens.read().values().map(|v| v.len()).sum();
        let indexed_entities = self.indexed.read().len();

        ContextIndexStats {
            indexed_entities,
            token_count,
            field_value_count,
            total_postings,
        }
    }
}

impl Default for ContextIndex {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Tokenization — extracts tokens from entities for indexing
// ============================================================================

/// Tokenize a query string into normalized tokens for searching.
pub fn tokenize_query(query: &str) -> Vec<String> {
    let mut tokens = BTreeSet::new();
    push_text_tokens(&mut tokens, query, true);
    tokens.into_iter().take(MAX_INDEX_TOKENS).collect()
}

/// Tokenize a value exactly (no word splitting).
pub fn tokenize_exact(value: &str) -> Vec<String> {
    let mut tokens = BTreeSet::new();
    push_text_tokens(&mut tokens, value, false);
    tokens.into_iter().take(MAX_INDEX_TOKENS).collect()
}

/// Tokenize a field name (no word splitting).
pub fn tokenize_field_name(field: &str) -> Vec<String> {
    let mut tokens = BTreeSet::new();
    push_text_tokens(&mut tokens, field, false);
    tokens.into_iter().take(MAX_INDEX_TOKENS).collect()
}

/// Generate sorted tokens for search comparison (used in global scan fallback).
/// Sorted for O(log n) binary_search lookups.
pub fn entity_tokens_for_search(entity: &UnifiedEntity) -> Vec<String> {
    let mut tokens: Vec<String> = extract_entity_tokens(entity)
        .into_iter()
        .map(|(token, _)| token)
        .collect();
    tokens.sort_unstable();
    tokens.dedup();
    tokens
}

/// Extract all indexable tokens from an entity, along with the field they came from.
fn extract_entity_tokens(entity: &UnifiedEntity) -> Vec<(String, String)> {
    let mut token_fields: Vec<(String, String)> = Vec::new();
    let mut tokens = BTreeSet::new();

    // Entity identifiers
    let mut id_tokens = BTreeSet::new();
    push_text_tokens(&mut id_tokens, &entity.id.raw().to_string(), false);
    push_text_tokens(&mut id_tokens, &entity.id.to_string(), false);
    push_text_tokens(&mut id_tokens, entity.kind.collection(), false);
    push_text_tokens(&mut id_tokens, entity.kind.storage_type(), false);
    for t in &id_tokens {
        token_fields.push((t.clone(), "_id".to_string()));
    }
    tokens.extend(id_tokens);

    // Kind-specific tokens
    match &entity.kind {
        EntityKind::TableRow { row_id, .. } => {
            let mut kind_tokens = BTreeSet::new();
            push_text_tokens(&mut kind_tokens, &row_id.to_string(), false);
            push_text_tokens(&mut kind_tokens, &format!("e{row_id}"), false);
            for t in &kind_tokens {
                if tokens.insert(t.clone()) {
                    token_fields.push((t.clone(), "_row_id".to_string()));
                }
            }
        }
        EntityKind::GraphNode(ref node) => {
            let mut kind_tokens = BTreeSet::new();
            push_text_tokens(&mut kind_tokens, &node.label, false);
            push_text_tokens(&mut kind_tokens, &node.node_type, false);
            for t in &kind_tokens {
                if tokens.insert(t.clone()) {
                    token_fields.push((t.clone(), "_label".to_string()));
                }
            }
        }
        EntityKind::GraphEdge(ref edge) => {
            let mut kind_tokens = BTreeSet::new();
            push_text_tokens(&mut kind_tokens, &edge.label, false);
            push_text_tokens(&mut kind_tokens, &edge.from_node, false);
            push_text_tokens(&mut kind_tokens, &edge.to_node, false);
            for t in &kind_tokens {
                if tokens.insert(t.clone()) {
                    token_fields.push((t.clone(), "_edge".to_string()));
                }
            }
        }
        EntityKind::Vector { collection } => {
            let mut kind_tokens = BTreeSet::new();
            push_text_tokens(&mut kind_tokens, collection, false);
            for t in &kind_tokens {
                if tokens.insert(t.clone()) {
                    token_fields.push((t.clone(), "red_collection".to_string()));
                }
            }
        }
        EntityKind::TimeSeriesPoint(_) | EntityKind::QueueMessage { .. } => {}
    }

    // Data field tokens
    match &entity.data {
        EntityData::Row(row) => {
            if let Some(named) = row.named.as_ref() {
                for (key, value) in named {
                    let mut field_tokens = BTreeSet::new();
                    push_text_tokens(&mut field_tokens, key, false);
                    push_value_tokens(&mut field_tokens, value);
                    for t in &field_tokens {
                        if tokens.insert(t.clone()) {
                            token_fields.push((t.clone(), key.clone()));
                        }
                    }
                }
            } else {
                for (i, value) in row.columns.iter().enumerate() {
                    let field_name = format!("_col{i}");
                    let mut field_tokens = BTreeSet::new();
                    push_value_tokens(&mut field_tokens, value);
                    for t in &field_tokens {
                        if tokens.insert(t.clone()) {
                            token_fields.push((t.clone(), field_name.clone()));
                        }
                    }
                }
            }
        }
        EntityData::Node(node) => {
            for (key, value) in &node.properties {
                let mut field_tokens = BTreeSet::new();
                push_text_tokens(&mut field_tokens, key, false);
                push_value_tokens(&mut field_tokens, value);
                for t in &field_tokens {
                    if tokens.insert(t.clone()) {
                        token_fields.push((t.clone(), key.clone()));
                    }
                }
            }
        }
        EntityData::Edge(edge) => {
            let mut field_tokens = BTreeSet::new();
            push_text_tokens(&mut field_tokens, &edge.weight.to_string(), false);
            for t in &field_tokens {
                if tokens.insert(t.clone()) {
                    token_fields.push((t.clone(), "_weight".to_string()));
                }
            }
            for (key, value) in &edge.properties {
                let mut field_tokens = BTreeSet::new();
                push_text_tokens(&mut field_tokens, key, false);
                push_value_tokens(&mut field_tokens, value);
                for t in &field_tokens {
                    if tokens.insert(t.clone()) {
                        token_fields.push((t.clone(), key.clone()));
                    }
                }
            }
        }
        EntityData::Vector(vector) => {
            if let Some(content) = vector.content.as_ref() {
                let mut field_tokens = BTreeSet::new();
                push_text_tokens(&mut field_tokens, content, true);
                for t in &field_tokens {
                    if tokens.insert(t.clone()) {
                        token_fields.push((t.clone(), "content".to_string()));
                    }
                }
            }
        }
        EntityData::TimeSeries(_) | EntityData::QueueMessage(_) => {}
    }

    // Cross-reference tokens
    for xref in entity.cross_refs() {
        let mut xref_tokens = BTreeSet::new();
        push_text_tokens(&mut xref_tokens, &xref.target.raw().to_string(), false);
        push_text_tokens(&mut xref_tokens, &xref.target.to_string(), false);
        push_text_tokens(&mut xref_tokens, &xref.target_collection, false);
        push_text_tokens(&mut xref_tokens, &format!("{:?}", xref.ref_type), false);
        for t in &xref_tokens {
            if tokens.insert(t.clone()) {
                token_fields.push((t.clone(), "_xref".to_string()));
            }
        }
    }

    token_fields.into_iter().take(MAX_INDEX_TOKENS).collect()
}

/// Extract field:value pairs for the field-value index.
fn extract_field_lookup_pairs(entity: &UnifiedEntity) -> Vec<(String, String)> {
    let mut pairs = BTreeSet::new();

    fn push_field_value_pairs(pairs: &mut BTreeSet<(String, String)>, field: &str, value: &Value) {
        if pairs.len() >= MAX_FIELD_INDEX_PAIRS {
            return;
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
                pairs.insert((field_token.clone(), value_token.clone()));
                if pairs.len() >= MAX_FIELD_INDEX_PAIRS {
                    return;
                }
            }
        }
    }

    match &entity.data {
        EntityData::Row(row) => {
            if let Some(named) = row.named.as_ref() {
                for (field, value) in named {
                    push_field_value_pairs(&mut pairs, field, value);
                    if pairs.len() >= MAX_FIELD_INDEX_PAIRS {
                        break;
                    }
                }
            }
        }
        EntityData::Node(node) => {
            for (field, value) in &node.properties {
                push_field_value_pairs(&mut pairs, field, value);
                if pairs.len() >= MAX_FIELD_INDEX_PAIRS {
                    break;
                }
            }
        }
        EntityData::Edge(edge) => {
            for (field, value) in &edge.properties {
                push_field_value_pairs(&mut pairs, field, value);
                if pairs.len() >= MAX_FIELD_INDEX_PAIRS {
                    break;
                }
            }
        }
        EntityData::Vector(vector) => {
            if let Some(content) = vector.content.as_ref() {
                let mut value_tokens = BTreeSet::new();
                push_text_tokens(&mut value_tokens, content, true);
                for token in value_tokens {
                    pairs.insert(("content".to_string(), token));
                    if pairs.len() >= MAX_FIELD_INDEX_PAIRS {
                        break;
                    }
                }
            }
        }
        EntityData::TimeSeries(_) | EntityData::QueueMessage(_) => {}
    }

    pairs.into_iter().collect()
}

/// Perf escape hatch: `REDDB_DISABLE_CONTEXT_INDEX=1` skips the
/// per-insert tokenisation + three-way RwLock write storm the
/// context index does on every mutation.
///
/// Default is still "enabled" so `SEARCH CONTEXT` and `ASK` keep
/// working out of the box. OLTP-only deployments that never query
/// via the context index (point lookups, range scans, RLS-gated
/// SELECTs) can flip the flag and recover the 40–60 % of insert
/// latency the indexer costs. Result: up to ~2× faster inserts
/// and ~1.5× faster batch writes at the cost of `SEARCH CONTEXT`
/// returning empty.
///
/// Read once via a `OnceLock<AtomicBool>` so the check is a single
/// relaxed atomic load on the hot path — cheap enough to be
/// unconditional.
fn context_index_disabled() -> bool {
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::sync::OnceLock;

    static CELL: OnceLock<AtomicU8> = OnceLock::new();
    let atomic = CELL.get_or_init(|| {
        let initial = match std::env::var("REDDB_DISABLE_CONTEXT_INDEX") {
            Ok(v) if matches!(v.as_str(), "1" | "true" | "TRUE" | "yes") => 1,
            _ => 0,
        };
        AtomicU8::new(initial)
    });
    atomic.load(Ordering::Relaxed) != 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::unified::entity::{EntityData, EntityKind, RowData};
    use std::collections::HashMap;

    fn make_row_entity(id: u64, table: &str, fields: Vec<(&str, Value)>) -> UnifiedEntity {
        let named: HashMap<String, Value> = fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        UnifiedEntity::new(
            EntityId::new(id),
            EntityKind::TableRow {
                table: std::sync::Arc::from(table),
                row_id: id,
            },
            EntityData::Row(RowData {
                columns: Vec::new(),
                named: Some(named),
                schema: None,
            }),
        )
    }

    #[test]
    fn test_index_and_search() {
        let index = ContextIndex::new();
        let entity = make_row_entity(
            1,
            "customers",
            vec![
                ("name", Value::Text("Alice".to_string())),
                ("cpf", Value::Text("000.000.000-00".to_string())),
            ],
        );
        index.index_entity("customers", &entity);

        let results = index.search("000.000.000-00", 10, None);
        assert!(!results.is_empty());
        assert_eq!(results[0].entity_id, EntityId::new(1));
        assert_eq!(results[0].collection, "customers");
    }

    #[test]
    fn test_field_search() {
        let index = ContextIndex::new();
        let entity = make_row_entity(
            42,
            "customers",
            vec![("cpf", Value::Text("000.000.000-00".to_string()))],
        );
        index.index_entity("customers", &entity);

        let results = index.search_field("cpf", "000.000.000-00", true, 10, None);
        assert!(!results.is_empty());
        assert_eq!(results[0].entity_id, EntityId::new(42));
    }

    #[test]
    fn test_remove_entity() {
        let index = ContextIndex::new();
        let entity = make_row_entity(1, "test", vec![("key", Value::Text("value".to_string()))]);
        index.index_entity("test", &entity);

        assert!(!index.search("value", 10, None).is_empty());

        index.remove_entity(EntityId::new(1));
        assert!(index.search("value", 10, None).is_empty());
    }

    #[test]
    fn test_collection_filtering() {
        let index = ContextIndex::new();
        let e1 = make_row_entity(1, "col_a", vec![("name", Value::Text("Alice".to_string()))]);
        let e2 = make_row_entity(2, "col_b", vec![("name", Value::Text("Alice".to_string()))]);
        index.index_entity("col_a", &e1);
        index.index_entity("col_b", &e2);

        let all = index.search("alice", 10, None);
        assert_eq!(all.len(), 2);

        let allowed: BTreeSet<String> = ["col_a".to_string()].into();
        let filtered = index.search("alice", 10, Some(&allowed));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].collection, "col_a");
    }

    #[test]
    fn test_stats() {
        let index = ContextIndex::new();
        let entity = make_row_entity(1, "test", vec![("k", Value::Text("v".to_string()))]);
        index.index_entity("test", &entity);

        let stats = index.stats();
        assert_eq!(stats.indexed_entities, 1);
        assert!(stats.token_count > 0);
    }
}
