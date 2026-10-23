//! Test-only WASM artifact: execute the same assertions as the native tests.
//! This is built separately and never included in the application WASM bundle.

#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/codec_corpus.rs"]
mod codec_corpus;

#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/portable.rs"]
mod portable;

#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/storage.rs"]
mod storage;

#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/pager.rs"]
mod pager;

#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/transaction.rs"]
mod transaction;

#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/entity.rs"]
mod entity;

#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/entity_codec.rs"]
mod entity_codec;

#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/record_corpus.rs"]
mod record_corpus;
#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/records.rs"]
mod records;

#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/segments.rs"]
mod segments;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name = runConformance)]
pub fn run_conformance() -> String {
    let cases = portable::CASES
        .iter()
        .chain(storage::CASES.iter())
        .chain(pager::CASES.iter())
        .chain(transaction::CASES.iter())
        .chain(entity::CASES.iter())
        .chain(entity_codec::CASES.iter())
        .chain(records::CASES.iter())
        .chain(segments::CASES.iter());
    for (_, run) in cases.clone() {
        run();
    }
    cases.map(|(name, _)| *name).collect::<Vec<_>>().join(",")
}

/// Test-only host-clock probe: JS checks seconds against Date.now before/after.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name = entityTimestamps)]
pub fn entity_timestamps() -> Vec<u64> {
    use reddb_engine::storage::entity::{
        CrossRef, EmbeddingSlot, EntityId, RefType, UnifiedEntity,
    };
    let mut entity = UnifiedEntity::table_row(EntityId::new(1024), "clock", 1, vec![]);
    entity.add_embedding(EmbeddingSlot::new("content", vec![1.0], "fixture"));
    entity.add_cross_ref(CrossRef::new(
        entity.id,
        EntityId::new(1025),
        "clock",
        RefType::RowToNode,
    ));
    vec![
        entity.created_at,
        entity.updated_at,
        entity.embeddings()[0].generated_at,
        entity.cross_refs()[0].created_at,
    ]
}
