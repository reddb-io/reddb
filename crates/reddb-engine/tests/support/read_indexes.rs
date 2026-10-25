//! Resident read-index and cache behavior, shared by native and WASM hosts.
use reddb_engine::storage::context_index::{ContextIndex, ContextSearchHit};
use reddb_engine::storage::entity::{EntityData, EntityId, RowData, UnifiedEntity};
use reddb_engine::storage::entity_cache::EntityCache;
use reddb_engine::storage::tokenization::{push_value_tokens, MAX_INDEX_TOKENS};
use reddb_types::Value;
use std::collections::{BTreeSet, HashMap};

pub const CASES: &[(&str, fn())] = &[
    ("resident_index_document_tokens", document_tokens),
    (
        "resident_index_replacement_and_scope",
        replacement_and_scope,
    ),
    ("resident_entity_cache_invalidation", cache_invalidation),
];

fn row(id: u64, collection: &str, field: &str, value: Value) -> UnifiedEntity {
    let mut entity = UnifiedEntity::table_row(EntityId::new(id), collection, id, vec![]);
    entity.data = EntityData::Row(RowData::with_names(vec![value], vec![field.into()]));
    entity
}

fn ids(hits: Vec<ContextSearchHit>) -> Vec<u64> {
    let mut ids: Vec<_> = hits.into_iter().map(|hit| hit.entity_id.raw()).collect();
    ids.sort_unstable();
    ids
}

fn document_tokens() {
    let fields = [
        ("name", Value::text("Alice Example")),
        ("count", Value::UnsignedInteger(9_007_199_254_740_993)),
        ("active", Value::Boolean(true)),
    ];
    let mut binary = vec![];
    reddb_types::document_body_codec::encode(
        &fields
            .iter()
            .map(|(key, value)| (*key, value))
            .collect::<Vec<_>>(),
        &mut binary,
    )
    .expect("native document body");
    let textual = br#"{"name":"Alice Example","count":{"$uint":"9007199254740993"},"active":true}"#;
    let tokenize = |bytes: Vec<u8>| {
        let mut tokens = BTreeSet::new();
        push_value_tokens(&mut tokens, &Value::Json(bytes));
        tokens
    };
    let tokens = tokenize(binary.clone());
    assert_eq!(
        tokens,
        tokenize(textual.to_vec()),
        "binary and canonical tagged JSON bodies share tokens"
    );
    for expected in ["alice", "example", "9007199254740993", "true"] {
        assert!(tokens.contains(expected), "missing token {expected}");
    }
    let mut bounded = BTreeSet::new();
    push_value_tokens(
        &mut bounded,
        &Value::Array((0..10_000).map(Value::Integer).collect()),
    );
    assert!(bounded.len() <= MAX_INDEX_TOKENS);
    let index = ContextIndex::new();
    index.set_collection_enabled("docs", true);
    index.index_entity("docs", &row(1, "docs", "body", Value::Json(binary)));
    assert_eq!(ids(index.search("alice", 10, None)), [1]);
    assert_eq!(ids(index.search("9007199254740993", 10, None)), [1]);
}

fn replacement_and_scope() {
    let index = ContextIndex::new();
    let specific = row(1, "a", "body", Value::text("passport renewal"));
    index.index_entity("a", &specific);
    assert!(
        index.search("passport", 10, None).is_empty(),
        "indexing is opt-in"
    );
    for collection in ["a", "b"] {
        index.set_collection_enabled(collection, true);
    }
    let broad = row(
        2,
        "a",
        "body",
        Value::text("passport renewal travel hotel airline visa luggage itinerary"),
    );
    index.index_entities("a", [&specific, &broad]);
    index.index_entity("b", &row(3, "b", "body", Value::text("passport renewal")));
    let scope = BTreeSet::from(["a".into()]);
    let ranked = index.search_bm25("passport renewal", 10, Some(&scope));
    assert_eq!(ranked[0].entity_id.raw(), 1);
    assert!(
        ranked[0].score > ranked[1].score,
        "specific document ranks first"
    );
    assert_eq!(ids(index.search("passport", 10, None)), [1, 2, 3]);
    index.index_entity("a", &row(1, "a", "body", Value::text("updated")));
    assert_eq!(
        ids(index.search("passport", 10, Some(&scope))),
        [2],
        "replace removes old postings"
    );
    assert_eq!(
        ids(index.search_field("body", "updated", true, 10, None)),
        [1]
    );
    index.remove_entity(EntityId::new(2));
    assert!(index.search("passport", 10, Some(&scope)).is_empty());
    index.set_collection_enabled("a", false);
    index.index_entity("a", &row(4, "a", "body", Value::text("disabled")));
    assert!(
        index.search("disabled", 10, None).is_empty(),
        "disabled collections admit no new postings"
    );
    assert_eq!(
        ids(index.search("updated", 10, None)),
        [1],
        "existing physical candidates remain until invalidated"
    );
    index.remove_entity(EntityId::new(1));
    assert!(index.search("updated", 10, None).is_empty());
    assert_eq!(ids(index.search("passport", 10, None)), [3]);
}

fn cache_invalidation() {
    let cache = EntityCache::with_capacity(32); // Two entries in each of 16 native shards.
    assert_eq!(cache.hit_rate(), None);
    for id in [1, 17] {
        cache.insert(
            id,
            ("a".into(), row(id, "a", "value", Value::Integer(id as i64))),
        );
    }
    assert_eq!(cache.get(1).expect("cached row").0, "a");
    cache.insert(33, ("a".into(), row(33, "a", "value", Value::Null)));
    assert!(
        cache.get(17).is_none(),
        "least recently used entry evicted within shard"
    );
    assert!(cache.get(1).is_some());
    assert!(cache.get(33).is_some());
    assert_eq!(cache.stats().evictions, 1);
    cache.remove_many([1, 33, 999, 1]);
    assert_eq!(
        cache.len(),
        0,
        "batch invalidation tolerates missing/repeated keys"
    );
    assert!(!cache.remove(1));
    for (id, collection) in [(2, "a"), (3, "b")] {
        cache.insert(
            id,
            (
                collection.into(),
                UnifiedEntity::graph_node(EntityId::new(id), collection, "Person", HashMap::new()),
            ),
        );
    }
    cache.retain(|_, (collection, _)| collection != "a");
    assert!(cache.get(2).is_none());
    assert_eq!(cache.get(3).expect("other collection retained").0, "b");
    assert_eq!(cache.len(), 1);
}
