#[allow(dead_code)]
#[path = "../../support/mod.rs"]
mod support;

use std::path::Path;

use reddb::application::{
    CreateDocumentInput, EntityUseCases, PatchEntityInput, PatchEntityOperation,
    PatchEntityOperationType,
};
use reddb::json::json;
use reddb::{PhysicalMetadataFile, RedDBOptions, RedDBRuntime, StorageDeployPreset};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open in-memory")
}

fn persistent_path(prefix: &str) -> support::TempDbFile {
    support::temp_db_file(prefix)
}

fn physical_metadata_options(path: &Path) -> RedDBOptions {
    RedDBOptions::persistent(path)
        .with_storage_profile(StorageDeployPreset::PrimaryReplicaProductionHa.selection())
        .expect("operational profile should persist physical metadata")
}

fn assert_reserved_error(message: &str, field: &str, context: &str) {
    assert!(
        message.contains("reserved system field"),
        "unexpected error: {message}"
    );
    assert!(message.contains(field), "unexpected error: {message}");
    assert!(message.contains(context), "unexpected error: {message}");
    assert!(
        message.contains("rid, collection, kind, tenant, created_at, updated_at"),
        "unexpected error: {message}"
    );
    assert!(
        message.contains("Rename the field in the payload before insert or patch"),
        "unexpected error: {message}"
    );
}

#[test]
fn create_table_rejects_reserved_system_columns() {
    let rt = runtime();

    let err = rt
        .execute_query("CREATE TABLE reserved_rows (rid INT, name TEXT)")
        .expect_err("reserved table column should fail");

    assert_reserved_error(&err.to_string(), "rid", "table 'reserved_rows'");
}

#[test]
fn create_document_rejects_reserved_top_level_body_fields() {
    let rt = runtime();
    rt.execute_query("CREATE DOCUMENT reserved_docs")
        .expect("document collection should be created");

    let err = EntityUseCases::new(&rt)
        .create_document(CreateDocumentInput {
            collection: "reserved_docs".to_string(),
            body: json!({"tenant": "acme", "title": "runbook"}),
            metadata: Vec::new(),
            node_links: Vec::new(),
            vector_links: Vec::new(),
        })
        .expect_err("reserved document field should fail");

    assert_reserved_error(&err.to_string(), "tenant", "document 'reserved_docs'");
}

#[test]
fn sql_document_insert_reserved_error_names_full_envelope_and_recourse() {
    let rt = runtime();
    rt.execute_query("CREATE DOCUMENT sql_reserved_docs")
        .expect("document collection should be created");

    let err = rt
        .execute_query(
            r#"INSERT INTO sql_reserved_docs DOCUMENT VALUES ({"kind":"runbook"})"#,
        )
        .expect_err("reserved document field should fail");

    assert_eq!(
        err.to_string(),
        "query error: reserved system field 'kind' cannot be used as a top-level user field in document 'sql_reserved_docs'. Reserved envelope fields are: rid, collection, kind, tenant, created_at, updated_at. Rename the field in the payload before insert or patch."
    );
}

#[test]
fn document_patch_rejects_reserved_top_level_body_fields() {
    let rt = runtime();
    rt.execute_query("CREATE DOCUMENT patch_reserved_docs")
        .expect("document collection should be created");

    let created = EntityUseCases::new(&rt)
        .create_document(CreateDocumentInput {
            collection: "patch_reserved_docs".to_string(),
            body: json!({"title": "runbook"}),
            metadata: Vec::new(),
            node_links: Vec::new(),
            vector_links: Vec::new(),
        })
        .expect("document should be created");

    let err = EntityUseCases::new(&rt)
        .patch(PatchEntityInput {
            collection: "patch_reserved_docs".to_string(),
            id: created.id,
            payload: json!(null),
            operations: vec![PatchEntityOperation {
                op: PatchEntityOperationType::Set,
                path: vec!["body".to_string(), "tenant".to_string()],
                value: Some(json!("acme")),
            }],
        })
        .expect_err("reserved document patch field should fail");

    assert_reserved_error(&err.to_string(), "tenant", "document 'patch_reserved_docs'");
}

#[test]
fn graph_payloads_reject_reserved_top_level_properties() {
    let rt = runtime();

    let node_err = rt
        .execute_query("INSERT INTO reserved_graph NODE (label, kind) VALUES ('host-a', 'host')")
        .expect_err("reserved node property should fail");
    assert_reserved_error(&node_err.to_string(), "kind", "node 'reserved_graph'");

    rt.execute_query("INSERT INTO reserved_graph NODE (label, name) VALUES ('a', 'A')")
        .expect("source node should be created");
    rt.execute_query("INSERT INTO reserved_graph NODE (label, name) VALUES ('b', 'B')")
        .expect("target node should be created");
    let edge_err = rt
        .execute_query(
            "INSERT INTO reserved_graph EDGE (label, from, to, updated_at) \
             VALUES ('connects', 'a', 'b', 123)",
        )
        .expect_err("reserved edge property should fail");
    assert_reserved_error(&edge_err.to_string(), "updated_at", "edge 'reserved_graph'");
}

#[test]
fn startup_rejects_persisted_table_contract_reserved_columns() {
    let path = persistent_path("reserved_startup");

    {
        let rt = RedDBRuntime::with_options(physical_metadata_options(path.path()))
            .expect("persistent runtime should open");
        rt.execute_query("CREATE TABLE persisted_rows (name TEXT)")
            .expect("table should be created");
        rt.checkpoint().expect("runtime should flush metadata");
    }

    let mut metadata =
        PhysicalMetadataFile::load_for_data_path(&path).expect("metadata should exist");
    let contract = metadata
        .collection_contracts
        .iter_mut()
        .find(|contract| contract.name == "persisted_rows")
        .expect("contract should exist");
    contract
        .declared_columns
        .push(reddb::physical::DeclaredColumnContract {
            name: "collection".to_string(),
            data_type: "TEXT".to_string(),
            sql_type: Some(reddb::storage::schema::SqlTypeName::simple("TEXT")),
            not_null: false,
            default: None,
            compress: None,
            unique: false,
            primary_key: false,
            enum_variants: Vec::new(),
            array_element: None,
            decimal_precision: None,
        });
    metadata
        .save_for_data_path(&path)
        .expect("metadata should be saved");

    let err = match RedDBRuntime::with_options(physical_metadata_options(path.path())) {
        Ok(_) => panic!("reserved persisted contract should fail startup"),
        Err(err) => err,
    };

    assert_reserved_error(&err.to_string(), "collection", "table 'persisted_rows'");
}
