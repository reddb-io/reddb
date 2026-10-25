//! Test-only admission followed by durable publication; this is not a SQL runtime.
use reddb_engine::catalog::{codec::*, enforcement::*};
use reddb_engine::storage::{
    entity::{EntityData, EntityId, RowData, UnifiedEntity},
    manager::ManagerConfig,
    metadata::Metadata,
    resident_file::EmbeddedResidentStore,
    resident_store::ResidentStore,
    store_wal::StoreWalAction,
};
use reddb_file::{EmbeddedRdbFile, VfsFile};
use reddb_types::Value;
use std::{collections::HashSet, error::Error};

fn action(fields: Vec<(String, Value)>, metadata: Option<&Metadata>) -> StoreWalAction {
    let mut entity = UnifiedEntity::table_row(EntityId::new(1), "orders", 1, vec![]);
    let (names, values) = fields.into_iter().unzip();
    entity.data = EntityData::Row(RowData::with_names(values, names));
    entity.created_at = 123;
    entity.updated_at = 124;
    entity.sequence_id = 1;
    StoreWalAction::upsert_entity("orders", &entity, metadata, 12)
}

fn verify<F: VfsFile>(store: &EmbeddedResidentStore<F>, total: i64) -> Vec<(String, Value)> {
    let (entity, metadata) = store
        .get("orders", EntityId::new(1))
        .expect("healthy store")
        .expect("row");
    assert_eq!(
        metadata.expect("default TTL").get("_ttl_ms"),
        Some(&reddb_engine::storage::metadata::MetadataValue::Int(60_000))
    );
    let record = reddb_engine::query::records::runtime_table_record_from_entity(entity.clone())
        .expect("canonical query record");
    assert_eq!(record.get("total"), Some(&Value::Integer(total)));
    assert_eq!(record.get("final_total"), Some(&Value::Integer(total + 1)));
    let expression =
        reddb_rql::schema_expression::SchemaExpression::parse("total + final_total".into())
            .expect("scalar expression");
    let result = reddb_engine::query::evaluator::evaluate(
        expression.expression(),
        &|field: &reddb_rql::ast::FieldRef| match field {
            reddb_rql::ast::FieldRef::TableColumn { table, column } if table.is_empty() => {
                record.get(column).cloned()
            }
            _ => None,
        },
    )
    .expect("canonical expression over persisted values");
    assert_eq!(result, Value::Integer(total * 2 + 1));
    let EntityData::Row(row) = entity.data else {
        panic!("fixture is a table row")
    };
    row.iter_fields()
        .map(|(name, value)| (name.to_string(), value.clone()))
        .collect()
}

pub fn run<F: VfsFile>(file: F, phase: u32) -> Result<u32, Box<dyn Error>> {
    let mut artifact = if phase == 0 {
        let mut contract = super::enforcement::contract();
        contract.declared_columns[0].unique = true;
        let resident = ResidentStore::new();
        resident.get_or_create_collection("orders", &ManagerConfig::default());
        resident.set_aux_metadata(serialize_collection_contracts(&[contract]));
        let snapshot = resident.snapshot_bytes(&HashSet::new(), &HashSet::new(), || {});
        EmbeddedRdbFile::create(file, "contract-writes.rdb", &snapshot, 123)?
    } else {
        EmbeddedRdbFile::open(file, "contract-writes.rdb")?
    };
    let snapshot = artifact.read_snapshot()?.expect("catalog snapshot");
    let (resident, _) =
        ResidentStore::from_snapshot(&snapshot, &ManagerConfig::default(), true, 1000)
            .map_err(std::io::Error::other)?;
    let contracts = deserialize_collection_contracts(&resident.aux_metadata())?;
    let contract = &contracts[0];
    let enforcer = CollectionContractEnforcer::new(contract);
    let mut store = EmbeddedResidentStore::open(
        artifact.into_inner(),
        "contract-writes.rdb",
        ManagerConfig::default(),
        true,
        1000,
    )?;
    let expected = match phase {
        0 => {
            let fields =
                enforcer.normalize_insert_at(vec![("quantity".into(), Value::text("3"))], 123)?;
            let mut metadata = vec![];
            enforcer.default_ttl_plan().apply(&mut metadata);
            store.publish_actions(&[action(
                fields,
                Some(&Metadata::with_fields(metadata.into_iter().collect())),
            )])?;
            assert!(
                store.state()?.manifest.wal_live_bytes > 0,
                "recover rows from committed WAL"
            );
            15
        }
        1 => {
            let fields = verify(&store, 15);
            let state = store.state()?;
            assert!(enforcer
                .normalize_update_at(vec![("quantity".into(), Value::Integer(0))], 124)
                .is_err());
            let existing = [ContractRow {
                id: EntityId::new(1),
                fields: fields.iter().cloned().collect(),
            }];
            assert!(enforcer
                .enforce_uniqueness(&fields, &existing, None)
                .is_err());
            assert_eq!(
                store.state()?.manifest.wal_live_bytes,
                state.manifest.wal_live_bytes,
                "rejected admission never appends WAL"
            );
            let mut updated = fields;
            updated
                .iter_mut()
                .find(|(name, _)| name == "quantity")
                .expect("quantity")
                .1 = Value::Integer(4);
            let updated = enforcer.normalize_update_at(updated, 124)?;
            enforcer.enforce_uniqueness(&updated, &existing, Some(EntityId::new(1)))?;
            let mut metadata = vec![];
            enforcer.default_ttl_plan().apply(&mut metadata);
            store.publish_actions(&[action(
                updated,
                Some(&Metadata::with_fields(metadata.into_iter().collect())),
            )])?;
            store.checkpoint()?;
            20
        }
        2 => {
            assert_eq!(
                store.state()?.manifest.wal_live_bytes,
                0,
                "recover checkpointed fields"
            );
            20
        }
        _ => return Err(std::io::Error::other("invalid contract-write fixture phase").into()),
    };
    verify(&store, expected);
    Ok(u32::try_from(expected).expect("fixture total fits u32"))
}
