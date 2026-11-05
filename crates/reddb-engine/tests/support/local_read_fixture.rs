//! Native artifact consumed through the public browser API, never shipped in the SDK.
use reddb_engine::{
    catalog::codec::serialize_collection_contracts,
    storage::{
        entity::{EntityData, EntityId, RowData, UnifiedEntity},
        manager::ManagerConfig,
        resident_store::ResidentStore,
        store_wal::StoreWalAction,
    },
};
use reddb_file::{
    EmbeddedRdbFile, PhysicalCollectionContract, PhysicalDeclaredColumnContract, VfsFile,
};
use reddb_types::Value;
use std::{collections::HashSet, error::Error};

pub const EXACT_ID: u64 = 9_007_199_254_740_993;

pub fn contract(name: &str) -> reddb_engine::catalog::CollectionContract {
    reddb_engine::catalog::codec::collection_contract_from_persisted(PhysicalCollectionContract {
        name: name.into(),
        declared_model: "table".into(),
        schema_mode: "strict".into(),
        origin: "explicit".into(),
        declared_columns: [
            ("name", "text"),
            ("total", "integer"),
            ("nickname", "text"),
            ("payload", "json"),
            ("token", "secret"),
        ]
        .into_iter()
        .map(|(name, data_type)| PhysicalDeclaredColumnContract {
            name: name.into(),
            data_type: data_type.into(),
            ..Default::default()
        })
        .collect(),
        ..Default::default()
    })
    .expect("valid local read fixture schema")
}

pub fn row(id: u64, name: &str, total: i64) -> UnifiedEntity {
    let mut entity = UnifiedEntity::table_row(EntityId::new(id), "people", id, vec![]);
    let fields = vec![
        ("name".into(), Value::text(name)),
        ("total".into(), Value::Integer(total)),
        ("payload".into(), Value::Json(br#"{"score":7}"#.to_vec())),
        ("token".into(), Value::Secret(vec![9, 8, 7])),
    ];
    let (names, values) = fields.into_iter().unzip();
    entity.data = EntityData::Row(RowData::with_names(values, names));
    entity
}

pub fn create<F: VfsFile>(file: F) -> Result<(), Box<dyn Error>> {
    let resident = ResidentStore::new();
    let config = ManagerConfig::default();
    resident.get_or_create_collection("people", &config);
    resident.get_or_create_collection("empty", &config);
    resident.set_aux_metadata(serialize_collection_contracts(&[
        contract("people"),
        contract("empty"),
    ]));
    let mut hidden = row(4, "superseded", 999);
    hidden.set_xmin(1);
    hidden.set_xmax(2);
    let mut moderated = row(5, "moderated", 999);
    let EntityData::Row(data) = &mut moderated.data else {
        unreachable!()
    };
    data.named
        .as_mut()
        .expect("fixture named row")
        .insert("__moderation_status".into(), Value::text("pending"));
    for entity in [
        row(EXACT_ID, "Ana", 10),
        row(2, "Bia", 20),
        row(3, "Caio", 30),
        hidden,
        moderated,
    ] {
        let action = StoreWalAction::upsert_entity("people", &entity, None, 12);
        resident.apply_replayed_action(&action, reddb_file::STORE_VERSION_CURRENT, &config)?;
    }
    let snapshot = resident.snapshot_bytes(&HashSet::new(), &HashSet::new(), || {});
    let mut file = EmbeddedRdbFile::create(file, "local-read.rdb", &snapshot, 123)?;
    let actions = [
        StoreWalAction::upsert_entity("people", &row(2, "Bia", 22), None, 12),
        StoreWalAction::DeleteEntityRecord {
            collection: "people".into(),
            entity_id: 3,
        },
    ];
    file.append_wal_payloads(
        &actions
            .iter()
            .map(StoreWalAction::encode)
            .collect::<Vec<_>>(),
    )?;
    Ok(())
}
