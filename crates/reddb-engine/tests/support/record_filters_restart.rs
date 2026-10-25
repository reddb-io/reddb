//! RQL projection/WHERE/ORDER BY over recovered records, without a public dispatcher.
use reddb_engine::catalog::{
    codec::*, enforcement::CollectionContractEnforcer, CollectionContract,
};
use reddb_engine::query::unified::UnifiedRecord;
use reddb_engine::storage::{
    entity::{EntityData, EntityId, RowData, UnifiedEntity},
    manager::ManagerConfig,
    metadata::Metadata,
    resident_file::EmbeddedResidentStore,
    resident_store::ResidentStore,
    store_wal::StoreWalAction,
};
use reddb_file::{EmbeddedRdbFile, PhysicalDeclaredColumnContract, VfsFile};
use reddb_types::Value;
use std::{collections::HashSet, error::Error};

const IDS: [u64; 4] = [9_007_199_254_740_993, 2, 3, 9_007_199_254_740_992];

fn contract() -> CollectionContract {
    let mut contract = super::enforcement::contract();
    for (name, data_type) in [("nickname", "text"), ("meta", "text"), ("payload", "json")] {
        contract.declared_columns.push(
            declared_column_contract_from_persisted(PhysicalDeclaredColumnContract {
                name: name.into(),
                data_type: data_type.into(),
                ..Default::default()
            })
            .expect("nullable fixture column"),
        );
    }
    contract
}

fn action(id: u64, fields: Vec<(String, Value)>, metadata: Option<&Metadata>) -> StoreWalAction {
    let mut entity = UnifiedEntity::table_row(EntityId::new(id), "orders", id, vec![]);
    let (names, values) = fields.into_iter().unzip();
    entity.data = EntityData::Row(RowData::with_names(values, names));
    entity.created_at = 123;
    entity.updated_at = 124;
    entity.sequence_id = 1;
    StoreWalAction::upsert_entity("orders", &entity, metadata, 12)
}

fn seed<F: VfsFile>(
    store: &mut EmbeddedResidentStore<F>,
    contract: &CollectionContract,
) -> Result<(), Box<dyn Error>> {
    let enforcer = CollectionContractEnforcer::new(contract);
    let mut metadata = vec![];
    enforcer.default_ttl_plan().apply(&mut metadata);
    let metadata = Metadata::with_fields(metadata.into_iter().collect());
    let mut actions = vec![];
    for (id, quantity, tenant, nickname) in [
        (IDS[0], 1, "red", None),
        (IDS[1], 3, "blue", Some("Bee")),
        (IDS[2], 4, "red", None),
        (IDS[3], 2, "blue", Some("Other")),
    ] {
        let mut binary = vec![];
        reddb_types::document_body_codec::encode(
            &[("score", &Value::Integer(quantity * 5))],
            &mut binary,
        )?;
        let mut fields = vec![
            ("quantity".into(), Value::Integer(quantity)),
            (
                "meta".into(),
                Value::text(format!(r#"{{"tenant":"{tenant}"}}"#)),
            ),
            ("payload".into(), Value::Json(binary)),
        ];
        if let Some(nickname) = nickname {
            fields.push(("nickname".into(), Value::text(nickname)));
        }
        actions.push(action(
            id,
            enforcer.normalize_insert_at(fields, 123)?,
            Some(&metadata),
        ));
    }
    store.publish_actions(&actions)?;
    Ok(())
}

fn verify<F: VfsFile>(
    store: &EmbeddedResidentStore<F>,
    contract: &CollectionContract,
    updated: bool,
) -> Vec<u32> {
    let records: Vec<UnifiedRecord> = IDS
        .iter()
        .map(|id| {
            let (entity, _) = store
                .get("orders", EntityId::new(*id))
                .expect("healthy store")
                .expect("fixture row");
            reddb_engine::query::records::runtime_table_record_from_entity(entity)
                .expect("canonical record")
        })
        .collect();
    super::record_ordering::verify_recovered(&records, contract, updated);
    super::record_projection::verify_recovered(&records, contract, updated);
    let mut counts = vec![];
    for predicate in [
        "rid = 9007199254740993",
        "total >= 15 AND o.meta.tenant = 'red'",
        "nickname IS NULL AND total < 20",
        "quantity * 5 = total",
        "payload.score = total",
        "rid IN (9007199254740993, 2)",
        "COALESCE(nickname, 'none') = 'none'",
    ] {
        let query = super::record_filters::table_query(&format!(
            "SELECT * FROM orders AS o WHERE {predicate}"
        ));
        let count = records
            .iter()
            .filter(|record| {
                super::record_filters::matches_where(record, &query, Some(contract))
                    .expect("WHERE evaluation")
            })
            .count();
        counts.push(u32::try_from(count).expect("fixture count fits u32"));
    }
    let query = super::record_filters::table_query("SELECT * FROM orders WHERE total / 0 > 1");
    assert!(
        matches!(
            super::record_filters::matches_where(&records[0], &query, Some(contract)),
            Err(reddb_engine::query::evaluator::EvalError::DivisionByZero)
        ),
        "data errors cannot become an empty result"
    );
    counts
}

pub fn run<F: VfsFile>(file: F, phase: u32) -> Result<Vec<u32>, Box<dyn Error>> {
    let mut artifact = if phase == 0 {
        let resident = ResidentStore::new();
        resident.get_or_create_collection("orders", &ManagerConfig::default());
        resident.set_aux_metadata(serialize_collection_contracts(&[contract()]));
        let snapshot = resident.snapshot_bytes(&HashSet::new(), &HashSet::new(), || {});
        EmbeddedRdbFile::create(file, "record-filters.rdb", &snapshot, 123)?
    } else {
        EmbeddedRdbFile::open(file, "record-filters.rdb")?
    };
    let snapshot = artifact.read_snapshot()?.expect("schema snapshot");
    let (resident, _) =
        ResidentStore::from_snapshot(&snapshot, &ManagerConfig::default(), true, 1000)
            .map_err(std::io::Error::other)?;
    let contracts = deserialize_collection_contracts(&resident.aux_metadata())?;
    let contract = &contracts[0];
    let mut store = EmbeddedResidentStore::open(
        artifact.into_inner(),
        "record-filters.rdb",
        ManagerConfig::default(),
        true,
        1000,
    )?;
    match phase {
        0 => {
            seed(&mut store, contract)?;
            assert!(store.state()?.manifest.wal_live_bytes > 0);
        }
        1 => {
            assert_eq!(verify(&store, contract, false), [1, 1, 1, 4, 4, 2, 2]);
            let (entity, metadata) = store
                .get("orders", EntityId::new(IDS[0]))?
                .expect("row to update");
            let EntityData::Row(row) = entity.data else {
                panic!("fixture row")
            };
            let mut fields: Vec<_> = row
                .iter_fields()
                .map(|(name, value)| (name.to_string(), value.clone()))
                .collect();
            fields
                .iter_mut()
                .find(|(name, _)| name == "quantity")
                .expect("quantity")
                .1 = Value::Integer(5);
            let fields =
                CollectionContractEnforcer::new(contract).normalize_update_at(fields, 124)?;
            store.publish_actions(&[action(IDS[0], fields, metadata.as_ref())])?;
            store.checkpoint()?;
        }
        2 => assert_eq!(store.state()?.manifest.wal_live_bytes, 0),
        _ => return Err(std::io::Error::other("invalid record-filter fixture phase").into()),
    }
    let counts = verify(&store, contract, phase != 0);
    assert_eq!(
        counts,
        if phase == 0 {
            vec![1, 1, 1, 4, 4, 2, 2]
        } else {
            vec![1, 2, 0, 4, 3, 2, 2]
        }
    );
    Ok(counts)
}
