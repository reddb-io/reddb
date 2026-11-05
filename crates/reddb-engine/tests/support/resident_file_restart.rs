//! Identical owned-store restart fixture for native memory I/O and actual OPFS.
use reddb_engine::storage::{
    entity::UnifiedEntity,
    error::StoreError,
    manager::ManagerConfig,
    metadata::{Metadata, MetadataValue},
    metadata_codec::serialize_entity_record,
    resident_file::EmbeddedResidentStore,
    store_wal::StoreWalAction,
};
use reddb_file::VfsFile;
use std::collections::HashMap;

fn metadata() -> Metadata {
    Metadata::with_fields(HashMap::from([(
        "source".into(),
        MetadataValue::String("opfs".into()),
    )]))
}

fn entities() -> Vec<UnifiedEntity> {
    crate::record_corpus::entities()
}

fn collection(entity: &UnifiedEntity) -> String {
    format!("model_{}", entity.id.raw())
}

fn verify<F: VfsFile>(store: &EmbeddedResidentStore<F>) -> Result<u32, StoreError> {
    let expected = entities();
    assert_eq!(store.collection_names()?.len(), expected.len() + 1);
    for entity in &expected {
        let (found, found_metadata) = store
            .get(&collection(entity), entity.id)?
            .expect("fixture model");
        assert_eq!(
            serialize_entity_record(&found, found_metadata.as_ref(), 12),
            serialize_entity_record(entity, Some(&metadata()), 12),
        );
    }
    Ok(u32::try_from(expected.len()).expect("fixture count fits u32"))
}

pub fn run<F: VfsFile>(file: F, phase: u32) -> Result<u32, StoreError> {
    let mut store = if phase == 0 {
        EmbeddedResidentStore::create(
            file,
            "owned-resident.rdb",
            123,
            ManagerConfig::default(),
            true,
            1000,
        )?
    } else {
        EmbeddedResidentStore::open(
            file,
            "owned-resident.rdb",
            ManagerConfig::default(),
            true,
            1000,
        )?
    };
    match phase {
        0 => {
            let mut actions = vec![StoreWalAction::CreateCollection {
                name: "empty".into(),
            }];
            actions.extend(entities().iter().map(|entity| {
                StoreWalAction::upsert_entity(&collection(entity), entity, Some(&metadata()), 12)
            }));
            store.publish_actions(&actions)?;
            store.checkpoint()?;
        }
        1 => {
            verify(&store)?;
            let actions: Vec<_> = entities()
                .into_iter()
                .take(2)
                .map(|entity| StoreWalAction::DropCollection {
                    name: collection(&entity),
                })
                .collect();
            let result = store.publish_actions(&actions);
            assert!(result.is_err(), "host interrupts the first WAL flush");
            assert!(
                store.collection_names().is_err(),
                "uncertain publication fences reads"
            );
            result?;
        }
        2 => {
            verify(&store)?;
            let entity = entities().into_iter().next().expect("fixture entity");
            store.publish_actions(&[
                StoreWalAction::DeleteEntityRecord {
                    collection: collection(&entity),
                    entity_id: entity.id.raw(),
                },
                StoreWalAction::upsert_entity(&collection(&entity), &entity, Some(&metadata()), 12),
            ])?;
            store.checkpoint()?;
        }
        3 => assert_eq!(store.state()?.manifest.wal_live_bytes, 0),
        _ => {
            return Err(StoreError::Internal(
                "invalid owned-store fixture phase".into(),
            ))
        }
    }
    verify(&store)
}
