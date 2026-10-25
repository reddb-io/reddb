//! Owned native-artifact publication, also executed in browser WASM.
use crate::pager::MemoryFile;
use reddb_engine::storage::{
    entity::{EntityId, UnifiedEntity},
    manager::ManagerConfig,
    metadata::{Metadata, MetadataValue},
    resident_file::EmbeddedResidentStore,
    store_wal::StoreWalAction,
};
use reddb_types::Value;
use std::collections::HashMap;

pub const CASES: &[(&str, fn())] = &[
    (
        "contract_write_durable_publication",
        contract_write_restart_fixture,
    ),
    ("resident_file_models_and_checkpoint", models_and_checkpoint),
    ("resident_file_owned_restart_fixture", owned_restart_fixture),
    (
        "resident_file_rejected_batch_restores_committed_state",
        rejected_batch,
    ),
    (
        "resident_file_publication_failure_boundaries",
        publication_failure_boundaries,
    ),
    (
        "resident_file_checkpoint_failure_boundaries",
        checkpoint_failure_boundaries,
    ),
    (
        "resident_file_wal_capacity_preserves_committed_state",
        wal_capacity,
    ),
];

fn contract_write_restart_fixture() {
    let mut file = MemoryFile::default();
    for (phase, expected) in [(0, 15), (1, 20), (2, 20)] {
        assert_eq!(
            crate::enforcement_restart::run(crate::pager::BorrowedMemoryFile(&mut file), phase)
                .expect("canonical admission and durable publication"),
            expected
        );
        file = MemoryFile::from_bytes(file.durable.into_inner());
    }
}

fn create() -> EmbeddedResidentStore<MemoryFile> {
    EmbeddedResidentStore::create(
        MemoryFile::default(),
        "resident.rdb",
        123,
        ManagerConfig::default(),
        true,
        1000,
    )
    .expect("create resident artifact")
}

fn open(file: MemoryFile) -> EmbeddedResidentStore<MemoryFile> {
    EmbeddedResidentStore::open(file, "resident.rdb", ManagerConfig::default(), true, 1000)
        .expect("recover resident artifact")
}

fn row(id: u64, value: i64) -> StoreWalAction {
    let mut entity =
        UnifiedEntity::table_row(EntityId::new(id), "rows", id, vec![Value::Integer(value)]);
    entity.ensure_table_logical_id();
    entity.created_at = 123;
    entity.updated_at = 124;
    entity.sequence_id = id;
    StoreWalAction::upsert_entity("rows", &entity, None, 12)
}

fn values(store: &EmbeddedResidentStore<MemoryFile>) -> Vec<Value> {
    [1, 2]
        .into_iter()
        .map(|id| {
            let (entity, _) = store
                .get("rows", EntityId::new(id))
                .expect("healthy read")
                .expect("row");
            let reddb_engine::storage::entity::EntityData::Row(row) = entity.data else {
                panic!("table row")
            };
            row.columns[0].clone()
        })
        .collect()
}

fn models_and_checkpoint() {
    let mut store = create();
    let metadata = Metadata::with_fields(HashMap::from([(
        "source".into(),
        MetadataValue::String("owned".into()),
    )]));
    let entities = crate::record_corpus::entities();
    let mut actions = vec![StoreWalAction::CreateCollection {
        name: "empty".into(),
    }];
    for entity in &entities {
        actions.push(StoreWalAction::upsert_entity(
            &format!("model_{}", entity.id.raw()),
            entity,
            Some(&metadata),
            12,
        ));
    }
    store
        .publish_actions(&actions)
        .expect("publish all canonical shapes");
    store = open(store.into_inner());
    store
        .checkpoint()
        .expect("fold logical WAL into native snapshot");
    store = open(store.into_inner());
    assert_eq!(
        store.collection_names().expect("collections").len(),
        entities.len() + 1
    );
    assert_eq!(
        store
            .state()
            .expect("checkpoint root")
            .manifest
            .wal_live_bytes,
        0
    );
    for expected in entities {
        let (found, restored_metadata) = store
            .get(&format!("model_{}", expected.id.raw()), expected.id)
            .expect("healthy store")
            .expect("persisted model");
        assert_eq!(
            reddb_engine::storage::metadata_codec::serialize_entity_record(
                &found,
                restored_metadata.as_ref(),
                12
            ),
            reddb_engine::storage::metadata_codec::serialize_entity_record(
                &expected,
                Some(&metadata),
                12
            ),
        );
    }
}

fn rejected_batch() {
    let mut store = create();
    store
        .publish_actions(&[row(1, 1), row(2, 2)])
        .expect("baseline");
    let generation = store.state().expect("root").selected_superblock.generation;
    assert!(store
        .publish_actions(&[
            row(1, 10),
            StoreWalAction::UpsertEntityRecord {
                collection: "bad".into(),
                record: vec![0xff]
            },
        ])
        .is_err());
    assert_eq!(values(&store), [Value::Integer(1), Value::Integer(2)]);
    assert!(!store
        .collection_names()
        .expect("healthy after reject")
        .contains(&"bad".into()));
    assert_eq!(
        store
            .state()
            .expect("no durable rejection")
            .selected_superblock
            .generation,
        generation
    );
    store
        .publish_actions(&[row(1, 10), row(2, 20)])
        .expect("next valid batch");
    assert_eq!(
        values(&open(store.into_inner())),
        [Value::Integer(10), Value::Integer(20)]
    );
}

fn publication_failure_boundaries() {
    let mut store = create();
    store
        .publish_actions(&[row(1, 1), row(2, 2)])
        .expect("baseline");
    let baseline = store.into_inner().durable.into_inner();
    let successful_file = MemoryFile::from_bytes(baseline.clone());
    let operations = successful_file.operations.clone();
    let mut successful = open(successful_file);
    successful
        .publish_actions(&[row(1, 10), row(2, 20)])
        .expect("count publication barriers");
    let count = operations.borrow().len();
    assert!(count >= 4, "WAL and root write/sync barriers");
    for fail_at in 1..=count {
        let mut file = MemoryFile::from_bytes(baseline.clone());
        file.fail_at = Some(fail_at);
        let mut interrupted = open(file);
        assert!(
            interrupted
                .publish_actions(&[row(1, 10), row(2, 20)])
                .is_err(),
            "failure at {fail_at}"
        );
        assert!(interrupted.get("rows", EntityId::new(1)).is_err());
        assert!(interrupted.collection_names().is_err());
        assert!(interrupted.checkpoint().is_err());
        assert!(interrupted.publish_actions(&[row(1, 30)]).is_err());
        let file = interrupted.into_inner();
        for image in [file.durable.into_inner(), file.bytes.into_inner()] {
            let reopened = open(MemoryFile::from_bytes(image));
            let actual = values(&reopened);
            assert!(
                actual == [Value::Integer(1), Value::Integer(2)]
                    || actual == [Value::Integer(10), Value::Integer(20)],
                "no partial batch after barrier {fail_at}: {actual:?}"
            );
        }
    }
}

fn checkpoint_failure_boundaries() {
    let mut store = create();
    store
        .publish_actions(&[row(1, 1), row(2, 2)])
        .expect("baseline");
    let baseline = store.into_inner().durable.into_inner();
    let successful_file = MemoryFile::from_bytes(baseline.clone());
    let operations = successful_file.operations.clone();
    let mut successful = open(successful_file);
    successful.checkpoint().expect("count checkpoint barriers");
    let count = operations.borrow().len();
    assert!(count > 0);
    for fail_at in 1..=count {
        let mut file = MemoryFile::from_bytes(baseline.clone());
        file.fail_at = Some(fail_at);
        let mut interrupted = open(file);
        assert!(interrupted.checkpoint().is_err(), "failure at {fail_at}");
        assert!(interrupted.collection_names().is_err());
        let file = interrupted.into_inner();
        for image in [file.durable.into_inner(), file.bytes.into_inner()] {
            assert_eq!(
                values(&open(MemoryFile::from_bytes(image))),
                [Value::Integer(1), Value::Integer(2)]
            );
        }
    }
}

fn wal_capacity() {
    let mut store = create();
    store
        .publish_actions(&[row(1, 1), row(2, 2)])
        .expect("baseline");
    let before = store.state().expect("root");
    let length = usize::try_from(before.manifest.wal_region_bytes).expect("fixture length") + 64;
    let mut state = 0x1234_5678_9abc_def0_u64;
    let payload: Vec<_> = (0..length)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state.to_le_bytes()[7]
        })
        .collect();
    let mut entity =
        UnifiedEntity::table_row(EntityId::new(3), "rows", 3, vec![Value::Blob(payload)]);
    entity.ensure_table_logical_id();
    store
        .publish_actions(&[StoreWalAction::upsert_entity("rows", &entity, None, 12)])
        .expect("grow before staging large batch");
    assert!(
        store.state().expect("grown root").manifest.wal_region_bytes
            > before.manifest.wal_region_bytes
    );
    let reopened = open(store.into_inner());
    assert_eq!(values(&reopened), [Value::Integer(1), Value::Integer(2)]);
    let found = reopened
        .get("rows", EntityId::new(3))
        .expect("healthy")
        .expect("large row")
        .0;
    let reddb_engine::storage::entity::EntityData::Row(found) = found.data else {
        panic!("large row")
    };
    let reddb_engine::storage::entity::EntityData::Row(expected) = entity.data else {
        panic!("source row")
    };
    assert_eq!(found.columns, expected.columns);
}

fn owned_restart_fixture() {
    let mut file = MemoryFile::default();
    crate::resident_file_restart::run(crate::pager::BorrowedMemoryFile(&mut file), 0)
        .expect("create shared fixture");
    file.operations.borrow_mut().clear();
    file.fail_at = Some(2);
    assert!(
        crate::resident_file_restart::run(crate::pager::BorrowedMemoryFile(&mut file), 1).is_err()
    );
    let mut recovered = MemoryFile::from_bytes(file.bytes.into_inner());
    crate::resident_file_restart::run(crate::pager::BorrowedMemoryFile(&mut recovered), 2)
        .expect("resume shared fixture");
    crate::resident_file_restart::run(crate::pager::BorrowedMemoryFile(&mut recovered), 3)
        .expect("verify shared checkpoint");
}
