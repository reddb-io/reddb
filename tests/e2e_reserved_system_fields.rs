use reddb::application::{CreateDocumentInput, EntityUseCases};
use reddb::json::json;
use reddb::{PhysicalMetadataFile, RedDBOptions, RedDBRuntime};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open in-memory")
}

fn persistent_path(prefix: &str) -> PathBuf {
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("reddb_{prefix}_{unique}.rdb"))
}

fn cleanup_path(path: &Path) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(PhysicalMetadataFile::metadata_path_for(path));
    let _ = std::fs::remove_file(PhysicalMetadataFile::metadata_binary_path_for(path));
}

fn assert_reserved_error(message: &str, field: &str, context: &str) {
    assert!(
        message.contains("reserved system field"),
        "unexpected error: {message}"
    );
    assert!(message.contains(field), "unexpected error: {message}");
    assert!(message.contains(context), "unexpected error: {message}");
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
    cleanup_path(&path);

    {
        let rt = RedDBRuntime::with_options(RedDBOptions::persistent(&path))
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

    let err = match RedDBRuntime::with_options(RedDBOptions::persistent(&path)) {
        Ok(_) => panic!("reserved persisted contract should fail startup"),
        Err(err) => err,
    };

    assert_reserved_error(&err.to_string(), "collection", "table 'persisted_rows'");
    cleanup_path(&path);
}
