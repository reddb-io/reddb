//! Test-only WASM artifact: execute the same assertions as the native tests.
//! This is built separately and never included in the application WASM bundle.

#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/catalog.rs"]
mod catalog;

#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/enforcement.rs"]
mod enforcement;

#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/enforcement_restart.rs"]
mod enforcement_restart;

#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/record_filters.rs"]
mod record_filters;

#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/record_filters_restart.rs"]
mod record_filters_restart;

#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/record_ordering.rs"]
mod record_ordering;

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
#[path = "../tests/support/metadata_codec.rs"]
mod metadata_codec;
#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/metadata_corpus.rs"]
mod metadata_corpus;

#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/snapshot.rs"]
mod snapshot;

#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/manager.rs"]
mod manager;

#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/read_indexes.rs"]
mod read_indexes;

#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/resident_store.rs"]
mod resident_store;

#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/store_wal.rs"]
mod store_wal;

#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/resident_file.rs"]
mod resident_file;
#[cfg(target_arch = "wasm32")]
#[path = "../tests/support/resident_file_restart.rs"]
mod resident_file_restart;

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
        .chain(segments::CASES.iter())
        .chain(metadata_codec::CASES.iter())
        .chain(snapshot::CASES.iter())
        .chain(manager::CASES.iter())
        .chain(read_indexes::CASES.iter())
        .chain(resident_store::CASES.iter())
        .chain(store_wal::CASES.iter())
        .chain(resident_file::CASES.iter())
        .chain(catalog::CASES.iter())
        .chain(enforcement::CASES.iter())
        .chain(record_filters::CASES.iter())
        .chain(record_ordering::CASES.iter());
    for (_, run) in cases.clone() {
        run();
    }
    cases.map(|(name, _)| *name).collect::<Vec<_>>().join(",")
}

/// Test-only decoder for the native snapshot retained across OPFS restarts.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name = verifyNativeSnapshot)]
pub fn verify_native_snapshot(bytes: &[u8]) -> u32 {
    snapshot::verify_native_snapshot(bytes)
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name = catalogSnapshotFixture)]
pub fn catalog_snapshot_fixture() -> Vec<u8> {
    catalog::snapshot_fixture()
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name = verifyCatalogSnapshot)]
pub fn verify_catalog_snapshot(bytes: &[u8]) -> u32 {
    catalog::verify_snapshot(bytes)
}

/// Test-only encoded native store actions for the offline OPFS recovery fixture.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name = storeWalFixtureCount)]
pub fn store_wal_fixture_count() -> u32 {
    u32::try_from(store_wal::recovery_actions().len()).expect("fixture count fits u32")
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen(js_name = storeWalFixtureAction)]
pub fn store_wal_fixture_action(index: u32) -> Vec<u8> {
    store_wal::recovery_actions()[index as usize].encode()
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
#[derive(Default)]
pub struct StoreWalRecoveryFixture(reddb_engine::storage::resident_store::ResidentStore);

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen::prelude::wasm_bindgen]
impl StoreWalRecoveryFixture {
    #[wasm_bindgen::prelude::wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self(reddb_engine::storage::resident_store::ResidentStore::new())
    }

    pub fn replay(&self, bytes: &[u8]) -> Result<(), wasm_bindgen::JsValue> {
        use reddb_engine::storage::{manager::ManagerConfig, store_wal::StoreWalAction};
        let action = StoreWalAction::decode(bytes)
            .map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))?;
        self.0
            .apply_replayed_action(&action, 12, &ManagerConfig::default())
            .map_err(|error| wasm_bindgen::JsValue::from_str(&error.to_string()))
    }

    pub fn verify(&self) -> u32 {
        store_wal::verify_recovery(&self.0)
    }
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
