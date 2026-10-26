#[path = "support/codec_corpus.rs"]
#[allow(dead_code)]
mod codec_corpus;
#[path = "support/local_read_fixture.rs"]
mod fixture;
#[path = "support/pager.rs"]
#[allow(dead_code)]
mod pager;

use reddb_engine::{
    query::local_read::LocalReadDatabase,
    storage::{manager::ManagerConfig, resident_store::ResidentStore},
};
use reddb_file::EmbeddedRdbFile;
use reddb_types::Value;
use std::collections::HashSet;

fn database() -> LocalReadDatabase<pager::MemoryFile> {
    let mut file = pager::MemoryFile::default();
    fixture::create(pager::BorrowedMemoryFile(&mut file)).expect("native snapshot and WAL");
    LocalReadDatabase::open(
        pager::MemoryFile::from_bytes(file.durable.into_inner()),
        "fixture.rdb",
    )
    .expect("recovered local database")
}

#[test]
fn selects_committed_snapshot_and_wal_without_writes() {
    let mut file = pager::MemoryFile::default();
    fixture::create(pager::BorrowedMemoryFile(&mut file)).expect("native artifact");
    let file = pager::MemoryFile::from_bytes(file.durable.into_inner());
    let operations = file.operations.clone();
    let database = LocalReadDatabase::open(file, "fixture.rdb").expect("open");
    for _ in 0..2 {
        let result = database.query("SELECT p.rid AS id, UPPER(name) AS label, total * 2 AS doubled FROM people AS p WHERE total >= 10 ORDER BY total DESC LIMIT 2").expect("stored SELECT");
        assert_eq!(result.columns, ["id", "label", "doubled"]);
        assert_eq!(
            result.rows,
            [
                vec![
                    Value::UnsignedInteger(2),
                    Value::text("BIA"),
                    Value::Integer(44)
                ],
                vec![
                    Value::UnsignedInteger(fixture::EXACT_ID),
                    Value::text("ANA"),
                    Value::Integer(20)
                ],
            ]
        );
        assert!(result
            .to_json()
            .to_string_compact()
            .contains("9007199254740993"));
    }
    assert!(
        operations.borrow().is_empty(),
        "read-only open/query must not write or flush"
    );
}

#[test]
fn nullable_json_secret_and_wide_page_semantics() {
    let database = database();
    let result = database.query("SELECT name, nickname, payload.score AS score FROM people WHERE nickname IS NULL ORDER BY total DESC LIMIT 4294967296").expect("nullable and JSON");
    assert_eq!(
        result.rows,
        [
            vec![Value::text("Bia"), Value::Null, Value::Integer(7)],
            vec![Value::text("Ana"), Value::Null, Value::Integer(7)]
        ]
    );
    assert!(database
        .query("SELECT name FROM people OFFSET 4294967296")
        .expect("wide offset")
        .rows
        .is_empty());
    let empty = database
        .query("SELECT name, total FROM empty")
        .expect("empty table schema");
    assert_eq!(empty.columns, ["name", "total"]);
    assert!(empty.rows.is_empty());
    let star = database
        .query("SELECT * FROM people")
        .expect("wildcard redacts secrets");
    let token = star
        .columns
        .iter()
        .position(|column| column == "token")
        .expect("secret output");
    assert!(star
        .rows
        .iter()
        .all(|row| row[token].to_json().as_str() == Some("***")));
    let not_null = database
        .query("SELECT name FROM people WHERE NOT (nickname = 'yes')")
        .expect("SQL three-valued logic");
    assert!(not_null.rows.is_empty());
}

#[test]
fn admission_rejects_unsupported_shapes_even_on_empty_tables() {
    let database = database();
    for sql in [
        "INSERT INTO people (name) VALUES ('x')",
        "DELETE FROM people",
        "BEGIN",
        "SELECT DISTINCT name FROM empty",
        "SELECT COUNT(*) FROM empty",
        "SELECT name FROM people GROUP BY name",
        "SELECT ROW_NUMBER() OVER () FROM empty",
        "SELECT name FROM empty LIMIT $1",
        "SELECT name FROM empty WHERE total = $1",
        "SELECT * FROM any",
        "SELECT UNKNOWN_FN(name) FROM empty",
        "SELECT name FROM empty WHERE UNKNOWN_FN(total) = 1",
        "SELECT UPPER() FROM empty",
        "SELECT token FROM people",
        "SELECT name FROM people ORDER BY token",
    ] {
        assert_eq!(
            database.query(sql).expect_err(sql).code(),
            "QUERY_UNSUPPORTED",
            "{sql}"
        );
    }
    for sql in [
        "SELECT missing FROM empty",
        "SELECT other.name FROM empty",
        "SELECT name FROM empty ORDER BY missing",
    ] {
        assert_eq!(
            database.query(sql).expect_err(sql).code(),
            "QUERY_EVALUATION_ERROR",
            "{sql}"
        );
    }
    assert_eq!(
        database
            .query("SELECT name FROM absent")
            .expect_err("missing catalog")
            .code(),
        "CATALOG_ERROR"
    );
    assert_eq!(
        database
            .query("SELECT FROM WHERE")
            .expect_err("parser")
            .code(),
        "QUERY_PARSE_ERROR"
    );
}

#[test]
fn data_errors_propagate_from_projection_filter_and_sort() {
    let database = database();
    for sql in [
        "SELECT total / 0 FROM people",
        "SELECT name FROM people WHERE total / 0 > 1",
        "SELECT name FROM people ORDER BY total / 0",
        "SELECT UPPER(total) FROM people",
    ] {
        assert_eq!(
            database.query(sql).expect_err(sql).code(),
            "QUERY_EVALUATION_ERROR"
        );
    }
}

#[test]
fn missing_or_corrupt_catalog_is_never_inferred_from_rows() {
    for metadata in [
        vec![],
        b"broken".to_vec(),
        reddb_engine::catalog::codec::serialize_collection_contracts(&[
            fixture::contract("people"),
            fixture::contract("people"),
        ]),
    ] {
        let resident = ResidentStore::new();
        resident.get_or_create_collection("people", &ManagerConfig::default());
        resident.set_aux_metadata(metadata.clone());
        let snapshot = resident.snapshot_bytes(&HashSet::new(), &HashSet::new(), || {});
        let mut file = pager::MemoryFile::default();
        EmbeddedRdbFile::create(
            pager::BorrowedMemoryFile(&mut file),
            "catalog.rdb",
            &snapshot,
            123,
        )
        .expect("artifact");
        let opened = LocalReadDatabase::open(file, "catalog.rdb");
        if metadata.is_empty() {
            assert_eq!(
                opened
                    .expect("legacy file opens without inferred tables")
                    .query("SELECT * FROM people")
                    .expect_err("no contract")
                    .code(),
                "CATALOG_ERROR"
            );
        } else {
            assert!(
                opened.is_err(),
                "corrupt or duplicate authoritative catalog"
            );
        }
    }
}

fn open_resident(resident: &ResidentStore) -> LocalReadDatabase<pager::MemoryFile> {
    let snapshot = resident.snapshot_bytes(&HashSet::new(), &HashSet::new(), || {});
    let mut file = pager::MemoryFile::default();
    EmbeddedRdbFile::create(
        pager::BorrowedMemoryFile(&mut file),
        "local.rdb",
        &snapshot,
        123,
    )
    .expect("canonical artifact");
    LocalReadDatabase::open(file, "local.rdb").expect("committed read view")
}

#[test]
fn ttl_retention_and_other_models_are_not_silently_treated_as_plain_tables() {
    use reddb_engine::{
        catalog::{codec::serialize_collection_contracts, CollectionModel},
        storage::{
            entity::EntityId,
            metadata::{Metadata, MetadataValue},
        },
    };
    for kind in ["ttl", "retention", "metadata", "model"] {
        let resident = ResidentStore::new();
        let manager = resident.get_or_create_collection("people", &ManagerConfig::default());
        manager.insert(fixture::row(1, "x", 1)).expect("row");
        let mut contract = fixture::contract("people");
        match kind {
            "ttl" => contract.default_ttl_ms = Some(1),
            "retention" => contract.retention_duration_ms = Some(1),
            "model" => contract.declared_model = CollectionModel::Document,
            "metadata" => {
                manager
                    .set_metadata(
                        EntityId::new(1),
                        Metadata::with_fields(std::collections::HashMap::from([(
                            "_expires_at".into(),
                            MetadataValue::Timestamp(1),
                        )])),
                    )
                    .expect("row expiry metadata");
            }
            _ => unreachable!(),
        }
        resident.set_aux_metadata(serialize_collection_contracts(&[contract]));
        assert_eq!(
            open_resident(&resident)
                .query("SELECT name FROM people WHERE total < 0 LIMIT 0")
                .expect_err(kind)
                .code(),
            "QUERY_UNSUPPORTED"
        );
    }
}

#[test]
fn materialization_cap_cannot_be_bypassed_by_limit_or_where() {
    use reddb_engine::{
        catalog::codec::serialize_collection_contracts,
        query::local_read::LOCAL_READ_ROWS_MAX,
        storage::entity::{EntityId, UnifiedEntity},
    };
    let resident = ResidentStore::new();
    let manager = resident.get_or_create_collection("people", &ManagerConfig::default());
    for id in 1..=u64::try_from(LOCAL_READ_ROWS_MAX + 1).expect("test count fits u64") {
        manager
            .insert(UnifiedEntity::table_row(
                EntityId::new(id),
                "people",
                id,
                vec![Value::text("row")],
            ))
            .expect("bounded source row");
    }
    resident.set_aux_metadata(serialize_collection_contracts(&[fixture::contract(
        "people",
    )]));
    let database = open_resident(&resident);
    assert_eq!(
        database
            .query("SELECT name FROM people WHERE FALSE LIMIT 0")
            .expect_err("bound source materialization before LIMIT")
            .code(),
        "QUERY_RESOURCE_LIMIT"
    );
}
