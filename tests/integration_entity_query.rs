//! Comprehensive integration tests for RedDB — Entity CRUD operations and query features.
//!
//! Covers row/document/KV/node/edge/vector creation, patching, deletion, SELECT queries
//! with filtering/ordering/pagination, universal queries, EXPLAIN, scan, similarity search,
//! text search, and cross-model entity scenarios.

use reddb::application::{
    CreateDocumentInput, CreateEdgeInput, CreateKvInput, CreateNodeInput, CreateRowInput,
    CreateVectorInput, DeleteEntityInput, ExecuteQueryInput, ExplainQueryInput, PatchEntityInput,
    PatchEntityOperation, PatchEntityOperationType, ScanCollectionInput, SearchSimilarInput,
    SearchTextInput,
};
use reddb::json::Value as JsonValue;
use reddb::storage::schema::Value;
use reddb::MetadataValue;
use reddb::{EntityUseCases, NativeUseCases, QueryUseCases, RedDBRuntime};

fn rt() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("failed to create in-memory runtime")
}

// ===========================================================================
// Entity CRUD Tests
// ===========================================================================

// 1. test_row_create_and_query
#[test]
fn test_row_create_and_query() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    let names = ["Alice", "Bob", "Charlie", "Diana", "Eve"];
    let ages = [30, 25, 35, 28, 40];
    let mut ids = Vec::new();

    for (name, age) in names.iter().zip(ages.iter()) {
        let out = entity.create_row(CreateRowInput {
            collection: "users_create".into(),
            fields: vec![
                ("name".into(), Value::Text(name.to_string())),
                ("age".into(), Value::Integer(*age)),
            ],
            metadata: vec![],
            node_links: vec![],
            vector_links: vec![],
        });
        assert!(
            out.is_ok(),
            "create_row should succeed for {}: {:?}",
            name,
            out.err()
        );
        ids.push(out.unwrap().id);
    }

    // Verify all IDs are unique
    let unique_count = {
        let mut set = std::collections::HashSet::new();
        ids.iter().for_each(|id| {
            set.insert(id.raw());
        });
        set.len()
    };
    assert_eq!(unique_count, 5, "all IDs should be unique");

    // Query with SELECT and verify count
    let result = query.execute(ExecuteQueryInput {
        query: "SELECT * FROM users_create".into(),
    });
    assert!(result.is_ok(), "SELECT should succeed: {:?}", result.err());
    let result = result.unwrap();
    assert_eq!(
        result.result.records.len(),
        5,
        "SELECT should return all 5 rows"
    );

    // Verify columns contain expected fields
    assert!(
        result.result.columns.contains(&"name".to_string()),
        "columns should include 'name', got {:?}",
        result.result.columns
    );
    assert!(
        result.result.columns.contains(&"age".to_string()),
        "columns should include 'age', got {:?}",
        result.result.columns
    );
}

// 2. test_row_patch_set
#[test]
fn test_row_patch_set() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    let out = entity
        .create_row(CreateRowInput {
            collection: "patch_set".into(),
            fields: vec![
                ("name".into(), Value::Text("Alice".into())),
                ("role".into(), Value::Text("developer".into())),
            ],
            metadata: vec![],
            node_links: vec![],
            vector_links: vec![],
        })
        .expect("create_row should succeed");

    // Patch with Set operation to change the role
    let patched = entity.patch(PatchEntityInput {
        collection: "patch_set".into(),
        id: out.id,
        payload: JsonValue::Object(Default::default()),
        operations: vec![PatchEntityOperation {
            op: PatchEntityOperationType::Set,
            path: vec!["fields".into(), "role".into()],
            value: Some(JsonValue::String("architect".into())),
        }],
    });
    assert!(patched.is_ok(), "patch should succeed: {:?}", patched.err());

    // Verify the field changed by querying
    let result = query
        .execute(ExecuteQueryInput {
            query: "SELECT * FROM patch_set".into(),
        })
        .expect("SELECT should succeed");

    assert_eq!(result.result.records.len(), 1, "should have exactly 1 row");

    let record = &result.result.records[0];
    let role_val = record.values.get("role");
    assert!(role_val.is_some(), "record should have 'role' field");
    match role_val.unwrap() {
        Value::Text(s) => assert_eq!(s, "architect", "role should be 'architect' after patch"),
        other => panic!("expected Text('architect'), got {:?}", other),
    }
}

// 3. test_row_patch_unset
#[test]
fn test_row_patch_unset() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    let out = entity
        .create_row(CreateRowInput {
            collection: "patch_unset".into(),
            fields: vec![
                ("name".into(), Value::Text("Bob".into())),
                ("email".into(), Value::Text("bob@example.com".into())),
                ("phone".into(), Value::Text("+1234567890".into())),
            ],
            metadata: vec![],
            node_links: vec![],
            vector_links: vec![],
        })
        .expect("create_row should succeed");

    // Unset the phone field
    let patched = entity.patch(PatchEntityInput {
        collection: "patch_unset".into(),
        id: out.id,
        payload: JsonValue::Object(Default::default()),
        operations: vec![PatchEntityOperation {
            op: PatchEntityOperationType::Unset,
            path: vec!["fields".into(), "phone".into()],
            value: None,
        }],
    });
    assert!(
        patched.is_ok(),
        "unset patch should succeed: {:?}",
        patched.err()
    );

    let result = query
        .execute(ExecuteQueryInput {
            query: "SELECT * FROM patch_unset".into(),
        })
        .expect("SELECT should succeed");

    assert_eq!(result.result.records.len(), 1);
    let record = &result.result.records[0];

    // phone should be absent or null
    let phone_val = record.values.get("phone");
    assert!(
        phone_val.is_none() || matches!(phone_val, Some(Value::Null)),
        "phone should be unset, got {:?}",
        phone_val
    );

    // name and email should still exist
    assert!(
        record.values.get("name").is_some(),
        "name should still exist"
    );
    assert!(
        record.values.get("email").is_some(),
        "email should still exist"
    );
}

// 4. test_row_delete
#[test]
fn test_row_patch_top_level_ttl_payload_expires_entity() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);
    let native = NativeUseCases::new(&rt);

    let out = entity
        .create_row(CreateRowInput {
            collection: "patch_ttl_payload".into(),
            fields: vec![("name".into(), Value::Text("ttl-payload".into()))],
            metadata: vec![],
            node_links: vec![],
            vector_links: vec![],
        })
        .expect("create_row should succeed");

    entity
        .patch(PatchEntityInput {
            collection: "patch_ttl_payload".into(),
            id: out.id,
            payload: JsonValue::Object(
                [("ttl".to_string(), JsonValue::String("0s".into()))]
                    .into_iter()
                    .collect(),
            ),
            operations: vec![],
        })
        .expect("patch with top-level ttl should succeed");

    native
        .apply_retention_policy()
        .expect("apply_retention_policy should succeed");

    let result = query
        .execute(ExecuteQueryInput {
            query: "SELECT * FROM patch_ttl_payload".into(),
        })
        .expect("SELECT should succeed");
    assert_eq!(
        result.result.records.len(),
        0,
        "row should expire after ttl patch"
    );
}

#[test]
fn test_row_patch_public_ttl_operation_expires_entity() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);
    let native = NativeUseCases::new(&rt);

    let out = entity
        .create_row(CreateRowInput {
            collection: "patch_ttl_operation".into(),
            fields: vec![("name".into(), Value::Text("ttl-op".into()))],
            metadata: vec![],
            node_links: vec![],
            vector_links: vec![],
        })
        .expect("create_row should succeed");

    entity
        .patch(PatchEntityInput {
            collection: "patch_ttl_operation".into(),
            id: out.id,
            payload: JsonValue::Object(Default::default()),
            operations: vec![PatchEntityOperation {
                op: PatchEntityOperationType::Set,
                path: vec!["ttl".into()],
                value: Some(JsonValue::String("0s".into())),
            }],
        })
        .expect("patch operation with ttl should succeed");

    native
        .apply_retention_policy()
        .expect("apply_retention_policy should succeed");

    let result = query
        .execute(ExecuteQueryInput {
            query: "SELECT * FROM patch_ttl_operation".into(),
        })
        .expect("SELECT should succeed");
    assert_eq!(
        result.result.records.len(),
        0,
        "row should expire after ttl patch"
    );
}

// 4. test_row_delete
#[test]
fn test_row_delete() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    let out = entity
        .create_row(CreateRowInput {
            collection: "delete_test".into(),
            fields: vec![("name".into(), Value::Text("ToBeDeleted".into()))],
            metadata: vec![],
            node_links: vec![],
            vector_links: vec![],
        })
        .expect("create_row should succeed");

    // Verify it exists
    let result = query
        .execute(ExecuteQueryInput {
            query: "SELECT * FROM delete_test".into(),
        })
        .expect("SELECT should succeed");
    assert_eq!(
        result.result.records.len(),
        1,
        "row should exist before delete"
    );

    // Delete it
    let deleted = entity.delete(DeleteEntityInput {
        collection: "delete_test".into(),
        id: out.id,
    });
    assert!(
        deleted.is_ok(),
        "delete should succeed: {:?}",
        deleted.err()
    );
    assert!(
        deleted.unwrap().deleted,
        "delete should report deleted=true"
    );

    // Verify query returns nothing
    let result = query
        .execute(ExecuteQueryInput {
            query: "SELECT * FROM delete_test".into(),
        })
        .expect("SELECT should succeed");
    assert_eq!(
        result.result.records.len(),
        0,
        "query should return 0 rows after delete"
    );
}

// 5. test_document_create_and_flatten
#[test]
fn test_document_create_and_flatten() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    let mut body = reddb::json::Map::new();
    body.insert("name".into(), JsonValue::String("Alice".into()));
    body.insert("age".into(), JsonValue::Number(30.0));
    body.insert("active".into(), JsonValue::Bool(true));

    let mut address = reddb::json::Map::new();
    address.insert("city".into(), JsonValue::String("NYC".into()));
    address.insert("zip".into(), JsonValue::String("10001".into()));
    body.insert("address".into(), JsonValue::Object(address));

    let out = entity.create_document(CreateDocumentInput {
        collection: "doc_flatten".into(),
        body: JsonValue::Object(body),
        metadata: vec![],
        node_links: vec![],
        vector_links: vec![],
    });
    assert!(
        out.is_ok(),
        "create_document should succeed: {:?}",
        out.err()
    );

    let result = query.execute(ExecuteQueryInput {
        query: "SELECT * FROM doc_flatten".into(),
    });
    assert!(
        result.is_ok(),
        "query documents should succeed: {:?}",
        result.err()
    );
    let result = result.unwrap();
    assert_eq!(result.result.records.len(), 1, "should have 1 document");

    let columns = &result.result.columns;
    assert!(
        columns.contains(&"name".to_string()),
        "columns should include 'name': {:?}",
        columns
    );
    assert!(
        columns.contains(&"age".to_string()),
        "columns should include 'age': {:?}",
        columns
    );
}

// 6. test_document_multiple
#[test]
fn test_document_multiple() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    for i in 0..5 {
        let mut body = reddb::json::Map::new();
        body.insert("title".into(), JsonValue::String(format!("Doc {}", i)));
        body.insert("index".into(), JsonValue::Number(i as f64));

        entity
            .create_document(CreateDocumentInput {
                collection: "doc_multi".into(),
                body: JsonValue::Object(body),
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .unwrap_or_else(|e| panic!("create_document {} failed: {:?}", i, e));
    }

    let result = query
        .execute(ExecuteQueryInput {
            query: "SELECT * FROM doc_multi".into(),
        })
        .expect("SELECT should succeed");

    assert_eq!(
        result.result.records.len(),
        5,
        "should have exactly 5 documents"
    );
}

// 7. test_kv_set_get_delete (thorough version with overwrite)
#[test]
fn test_kv_set_get_delete() {
    let rt = rt();
    let uc = EntityUseCases::new(&rt);

    // Set multiple keys
    for i in 0..5 {
        let out = uc.create_kv(CreateKvInput {
            collection: "kv_crud".into(),
            key: format!("key_{}", i),
            value: Value::Text(format!("value_{}", i)),
            metadata: vec![],
        });
        assert!(
            out.is_ok(),
            "create_kv should succeed for key_{}: {:?}",
            i,
            out.err()
        );
    }

    // Get each key
    for i in 0..5 {
        let val = uc
            .get_kv("kv_crud", &format!("key_{}", i))
            .expect("get_kv should succeed")
            .expect("key should exist");
        match &val.0 {
            Value::Text(s) => assert_eq!(s, &format!("value_{}", i)),
            other => panic!("expected Text, got {:?}", other),
        }
    }

    // Overwrite key_0: delete first, then re-create with new value
    let deleted = uc
        .delete_kv("kv_crud", "key_0")
        .expect("delete_kv for overwrite should succeed");
    assert!(deleted, "should have deleted old key_0");
    uc.create_kv(CreateKvInput {
        collection: "kv_crud".into(),
        key: "key_0".into(),
        value: Value::Text("overwritten".into()),
        metadata: vec![],
    })
    .expect("re-create with new value should succeed");

    // Verify overwritten value
    let val = uc
        .get_kv("kv_crud", "key_0")
        .expect("get_kv should succeed")
        .expect("key should still exist");
    match &val.0 {
        Value::Text(s) => assert_eq!(s, "overwritten", "value should be overwritten"),
        other => panic!("expected Text('overwritten'), got {:?}", other),
    }

    // Delete key_0 again
    let deleted = uc
        .delete_kv("kv_crud", "key_0")
        .expect("delete_kv should succeed");
    assert!(deleted, "should have deleted key_0");

    // Confirm deleted
    let val = uc
        .get_kv("kv_crud", "key_0")
        .expect("get_kv should succeed");
    assert!(val.is_none(), "key_0 should be gone after delete");

    // Other keys should still be present
    for i in 1..5 {
        let val = uc
            .get_kv("kv_crud", &format!("key_{}", i))
            .expect("get_kv should succeed");
        assert!(val.is_some(), "key_{} should still exist", i);
    }

    // Delete a non-existent key
    let deleted = uc
        .delete_kv("kv_crud", "nonexistent")
        .expect("delete_kv should succeed");
    assert!(!deleted, "deleting non-existent key should return false");
}

// 8. test_kv_list_all
#[test]
fn test_kv_list_all() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    // Create 10 KV pairs
    for i in 0..10 {
        entity
            .create_kv(CreateKvInput {
                collection: "kv_list".into(),
                key: format!("setting.{}", i),
                value: Value::Integer(i * 100),
                metadata: vec![],
            })
            .expect("create_kv should succeed");
    }

    // Scan the collection
    let page = query.scan(ScanCollectionInput {
        collection: "kv_list".into(),
        offset: 0,
        limit: 20,
    });
    assert!(page.is_ok(), "scan should succeed: {:?}", page.err());
    let page = page.unwrap();
    assert_eq!(page.items.len(), 10, "scan should return all 10 KV pairs");
    assert_eq!(page.total, 10, "total should be 10");
}

// 9. test_node_create_with_properties
#[test]
fn test_node_create_with_properties() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    let out = entity.create_node(CreateNodeInput {
        collection: "node_props".into(),
        label: "server_alpha".into(),
        node_type: Some("Server".into()),
        properties: vec![
            ("hostname".into(), Value::Text("alpha.example.com".into())),
            ("cpu_cores".into(), Value::Integer(16)),
            ("memory_gb".into(), Value::Float(64.0)),
            ("active".into(), Value::Boolean(true)),
        ],
        metadata: vec![],
        embeddings: vec![],
        table_links: vec![],
        node_links: vec![],
    });
    assert!(out.is_ok(), "create_node should succeed: {:?}", out.err());

    let page = query
        .scan(ScanCollectionInput {
            collection: "node_props".into(),
            offset: 0,
            limit: 10,
        })
        .expect("scan should succeed");
    assert_eq!(page.items.len(), 1, "should have 1 node");

    let item = &page.items[0];
    assert!(item.data.is_node(), "entity should be a node");
    let node_data = item.data.as_node().unwrap();
    assert!(
        node_data.properties.get("hostname").is_some(),
        "node should have 'hostname' property"
    );
}

// 10. test_edge_create_bidirectional
#[test]
fn test_edge_create_bidirectional() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);

    let node_a = entity
        .create_node(CreateNodeInput {
            collection: "edge_net".into(),
            label: "server_a".into(),
            node_type: Some("Server".into()),
            properties: vec![("ip".into(), Value::Text("10.0.0.1".into()))],
            metadata: vec![],
            embeddings: vec![],
            table_links: vec![],
            node_links: vec![],
        })
        .expect("create node_a should succeed");

    let node_b = entity
        .create_node(CreateNodeInput {
            collection: "edge_net".into(),
            label: "server_b".into(),
            node_type: Some("Server".into()),
            properties: vec![("ip".into(), Value::Text("10.0.0.2".into()))],
            metadata: vec![],
            embeddings: vec![],
            table_links: vec![],
            node_links: vec![],
        })
        .expect("create node_b should succeed");

    // Create edge A -> B
    let edge_ab = entity.create_edge(CreateEdgeInput {
        collection: "edge_net".into(),
        label: "connects".into(),
        from: node_a.id,
        to: node_b.id,
        weight: Some(1.0),
        properties: vec![("latency_ms".into(), Value::Integer(5))],
        metadata: vec![],
    });
    assert!(
        edge_ab.is_ok(),
        "edge A->B should succeed: {:?}",
        edge_ab.err()
    );

    // Create edge B -> A (bidirectional)
    let edge_ba = entity.create_edge(CreateEdgeInput {
        collection: "edge_net".into(),
        label: "connects".into(),
        from: node_b.id,
        to: node_a.id,
        weight: Some(1.0),
        properties: vec![("latency_ms".into(), Value::Integer(3))],
        metadata: vec![],
    });
    assert!(
        edge_ba.is_ok(),
        "edge B->A should succeed: {:?}",
        edge_ba.err()
    );

    // Scan — should contain 2 nodes + 2 edges = 4 entities
    let query = QueryUseCases::new(&rt);
    let page = query
        .scan(ScanCollectionInput {
            collection: "edge_net".into(),
            offset: 0,
            limit: 20,
        })
        .expect("scan should succeed");
    assert_eq!(
        page.items.len(),
        4,
        "should have 4 entities (2 nodes + 2 edges), got {}",
        page.items.len()
    );

    let edge_count = page.items.iter().filter(|e| e.data.is_edge()).count();
    assert_eq!(edge_count, 2, "should have exactly 2 edges");

    let node_count = page.items.iter().filter(|e| e.data.is_node()).count();
    assert_eq!(node_count, 2, "should have exactly 2 nodes");
}

// 11. test_vector_create_with_metadata
#[test]
fn test_vector_create_with_metadata() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    let out = entity.create_vector(CreateVectorInput {
        collection: "vec_meta".into(),
        dense: vec![1.0, 0.0, 0.0],
        content: Some("test vector".into()),
        metadata: vec![
            ("source".into(), MetadataValue::String("sensor_1".into())),
            ("confidence".into(), MetadataValue::Float(0.95)),
            (
                "tags".into(),
                MetadataValue::Array(vec![
                    MetadataValue::String("important".into()),
                    MetadataValue::String("verified".into()),
                ]),
            ),
        ],
        link_row: None,
        link_node: None,
    });
    assert!(
        out.is_ok(),
        "create_vector with metadata should succeed: {:?}",
        out.err()
    );

    let page = query
        .scan(ScanCollectionInput {
            collection: "vec_meta".into(),
            offset: 0,
            limit: 10,
        })
        .expect("scan should succeed");
    assert_eq!(page.items.len(), 1, "should have 1 vector");

    let item = &page.items[0];
    assert!(item.data.is_vector(), "entity should be a vector");
    let vec_data = item.data.as_vector().unwrap();
    assert_eq!(vec_data.dense, vec![1.0, 0.0, 0.0]);
    assert_eq!(vec_data.content.as_deref(), Some("test vector"));
}

// 12. test_vector_create_and_search
#[test]
fn test_vector_create_and_search() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    // Create 50 vectors spread across directions
    for i in 0..50 {
        let angle = (i as f32) * std::f32::consts::PI * 2.0 / 50.0;
        entity
            .create_vector(CreateVectorInput {
                collection: "vec_search".into(),
                dense: vec![angle.cos(), angle.sin(), 0.0],
                content: Some(format!("vector_{}", i)),
                metadata: vec![],
                link_row: None,
                link_node: None,
            })
            .unwrap_or_else(|e| panic!("create_vector {} failed: {:?}", i, e));
    }

    // Search for vectors similar to [1, 0, 0]
    let results = query.search_similar(SearchSimilarInput {
        collection: "vec_search".into(),
        vector: vec![1.0, 0.0, 0.0],
        k: 5,
        min_score: 0.0,
        text: None,
        provider: None,
    });
    assert!(
        results.is_ok(),
        "search_similar should succeed: {:?}",
        results.err()
    );
    let results = results.unwrap();

    assert!(!results.is_empty(), "should find similar vectors");
    assert!(results.len() <= 5, "should return at most k=5 results");

    // Verify results are sorted by score (descending)
    for window in results.windows(2) {
        assert!(
            window[0].score >= window[1].score,
            "results should be sorted by score descending: {} >= {}",
            window[0].score,
            window[1].score
        );
    }

    // The closest vector to [1,0,0] should have a high score
    assert!(
        results[0].score > 0.9,
        "closest vector should have score > 0.9, got {}",
        results[0].score
    );
}

// ===========================================================================
// Query Tests
// ===========================================================================

// 13. test_select_with_filter
#[test]
fn test_select_with_filter() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    let entries = vec![
        ("Alice", "engineering", 30),
        ("Bob", "marketing", 25),
        ("Charlie", "engineering", 35),
        ("Diana", "sales", 28),
        ("Eve", "engineering", 40),
    ];

    for (name, dept, age) in &entries {
        entity
            .create_row(CreateRowInput {
                collection: "filter_test".into(),
                fields: vec![
                    ("name".into(), Value::Text(name.to_string())),
                    ("dept".into(), Value::Text(dept.to_string())),
                    ("age".into(), Value::Integer(*age)),
                ],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .expect("create_row should succeed");
    }

    let result = query.execute(ExecuteQueryInput {
        query: "SELECT * FROM filter_test WHERE dept = 'engineering'".into(),
    });
    assert!(
        result.is_ok(),
        "filtered SELECT should succeed: {:?}",
        result.err()
    );
    let result = result.unwrap();
    assert_eq!(
        result.result.records.len(),
        3,
        "should return 3 engineering rows, got {}",
        result.result.records.len()
    );

    for record in &result.result.records {
        let dept = record.values.get("dept");
        match dept {
            Some(Value::Text(s)) => assert_eq!(s, "engineering"),
            other => panic!("expected dept='engineering', got {:?}", other),
        }
    }
}

// 14. test_select_with_order_by
#[test]
fn test_select_with_order_by() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    let ages = [30, 25, 35, 28, 40];
    for (i, age) in ages.iter().enumerate() {
        entity
            .create_row(CreateRowInput {
                collection: "order_test".into(),
                fields: vec![
                    ("name".into(), Value::Text(format!("user_{}", i))),
                    ("age".into(), Value::Integer(*age)),
                ],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .expect("create_row should succeed");
    }

    let result = query.execute(ExecuteQueryInput {
        query: "SELECT * FROM order_test ORDER BY age ASC".into(),
    });
    assert!(
        result.is_ok(),
        "ordered SELECT should succeed: {:?}",
        result.err()
    );
    let result = result.unwrap();
    assert_eq!(result.result.records.len(), 5);

    let result_ages: Vec<i64> = result
        .result
        .records
        .iter()
        .filter_map(|r| match r.values.get("age") {
            Some(Value::Integer(n)) => Some(*n),
            Some(Value::Float(f)) => Some(*f as i64),
            _ => None,
        })
        .collect();

    for window in result_ages.windows(2) {
        assert!(
            window[0] <= window[1],
            "ages should be ascending: {} <= {}",
            window[0],
            window[1]
        );
    }
}

// 15. test_select_with_limit_offset
#[test]
fn test_select_with_limit_offset() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    for i in 0..20 {
        entity
            .create_row(CreateRowInput {
                collection: "paginate".into(),
                fields: vec![
                    ("index".into(), Value::Integer(i)),
                    ("name".into(), Value::Text(format!("item_{}", i))),
                ],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .expect("create_row should succeed");
    }

    let result = query.execute(ExecuteQueryInput {
        query: "SELECT * FROM paginate LIMIT 5 OFFSET 3".into(),
    });
    assert!(
        result.is_ok(),
        "paginated SELECT should succeed: {:?}",
        result.err()
    );
    let result = result.unwrap();

    assert!(
        result.result.records.len() <= 5,
        "LIMIT 5 should return at most 5 rows, got {}",
        result.result.records.len()
    );

    // Verify LIMIT alone
    let result_limit = query
        .execute(ExecuteQueryInput {
            query: "SELECT * FROM paginate LIMIT 3".into(),
        })
        .expect("SELECT with LIMIT should succeed");
    assert_eq!(
        result_limit.result.records.len(),
        3,
        "LIMIT 3 should return 3 rows"
    );
}

// 16. test_select_universal_from_any
#[test]
fn test_select_universal_from_any() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    // Create a mix of rows, nodes, and vectors
    entity
        .create_row(CreateRowInput {
            collection: "universal_mix".into(),
            fields: vec![("kind".into(), Value::Text("row".into()))],
            metadata: vec![],
            node_links: vec![],
            vector_links: vec![],
        })
        .expect("create row");

    entity
        .create_node(CreateNodeInput {
            collection: "universal_mix".into(),
            label: "node_one".into(),
            node_type: Some("Type".into()),
            properties: vec![("kind".into(), Value::Text("node".into()))],
            metadata: vec![],
            embeddings: vec![],
            table_links: vec![],
            node_links: vec![],
        })
        .expect("create node");

    entity
        .create_vector(CreateVectorInput {
            collection: "universal_mix".into(),
            dense: vec![1.0, 0.0, 0.0],
            content: Some("a vector".into()),
            metadata: vec![],
            link_row: None,
            link_node: None,
        })
        .expect("create vector");

    let result = query.execute(ExecuteQueryInput {
        query: "SELECT * FROM any".into(),
    });
    assert!(
        result.is_ok(),
        "SELECT * FROM any should succeed: {:?}",
        result.err()
    );
    let result = result.unwrap();
    assert!(
        result.result.records.len() >= 3,
        "universal query should return at least 3 entities, got {}",
        result.result.records.len()
    );
}

// 17. test_select_universal_with_filter
#[test]
fn test_select_universal_with_filter() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    for i in 0..3 {
        entity
            .create_row(CreateRowInput {
                collection: "univ_a".into(),
                fields: vec![("val".into(), Value::Integer(i))],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .expect("create row in univ_a");
    }
    for i in 0..2 {
        entity
            .create_row(CreateRowInput {
                collection: "univ_b".into(),
                fields: vec![("val".into(), Value::Integer(i))],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .expect("create row in univ_b");
    }

    // In universal queries, the collection field is stored as "_collection"
    let result = query.execute(ExecuteQueryInput {
        query: "SELECT * FROM any WHERE _collection = 'univ_a'".into(),
    });
    assert!(
        result.is_ok(),
        "universal filtered SELECT should succeed: {:?}",
        result.err()
    );
    let result = result.unwrap();
    assert_eq!(
        result.result.records.len(),
        3,
        "should return 3 rows from univ_a, got {}",
        result.result.records.len()
    );
}

// 18. test_explain_query
#[test]
fn test_explain_query() {
    let rt = rt();
    let query = QueryUseCases::new(&rt);

    let explain = query.explain(ExplainQueryInput {
        query: "SELECT * FROM any".into(),
    });
    assert!(
        explain.is_ok(),
        "explain should succeed: {:?}",
        explain.err()
    );
    let explain = explain.unwrap();

    assert!(explain.is_universal, "FROM any should be universal");
    // statement reflects the parsed expression type (e.g. "table", "graph", "vector")
    assert_eq!(
        explain.statement, "table",
        "statement should be 'table' for SELECT queries"
    );
    assert!(!explain.query.is_empty(), "query field should be populated");

    // Explain a non-universal query
    let explain2 = query.explain(ExplainQueryInput {
        query: "SELECT * FROM some_table".into(),
    });
    assert!(
        explain2.is_ok(),
        "explain non-universal should succeed: {:?}",
        explain2.err()
    );
    let explain2 = explain2.unwrap();
    assert!(
        !explain2.is_universal,
        "FROM some_table should NOT be universal"
    );
    assert_eq!(
        explain2.statement, "table",
        "statement for table query should be 'table'"
    );
}

// 19. test_scan_collection
#[test]
fn test_scan_collection() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    for i in 0..15 {
        entity
            .create_row(CreateRowInput {
                collection: "scan_test".into(),
                fields: vec![("idx".into(), Value::Integer(i))],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .expect("create_row should succeed");
    }

    // Scan with offset=0, limit=5
    let page1 = query
        .scan(ScanCollectionInput {
            collection: "scan_test".into(),
            offset: 0,
            limit: 5,
        })
        .expect("scan page 1 should succeed");
    assert_eq!(page1.items.len(), 5, "first page should have 5 items");
    assert_eq!(page1.total, 15, "total should be 15");
    assert!(page1.next.is_some(), "should have a next cursor");

    // Scan with offset=5, limit=5
    let page2 = query
        .scan(ScanCollectionInput {
            collection: "scan_test".into(),
            offset: 5,
            limit: 5,
        })
        .expect("scan page 2 should succeed");
    assert_eq!(page2.items.len(), 5, "second page should have 5 items");

    // Scan with offset=10, limit=5
    let page3 = query
        .scan(ScanCollectionInput {
            collection: "scan_test".into(),
            offset: 10,
            limit: 5,
        })
        .expect("scan page 3 should succeed");
    assert_eq!(page3.items.len(), 5, "third page should have 5 items");

    // Scan with offset=14, limit=5
    let page_last = query
        .scan(ScanCollectionInput {
            collection: "scan_test".into(),
            offset: 14,
            limit: 5,
        })
        .expect("scan last page should succeed");
    assert_eq!(page_last.items.len(), 1, "last page should have 1 item");

    // All pages together should cover all entities
    let total_scanned = page1.items.len() + page2.items.len() + page3.items.len();
    assert_eq!(total_scanned, 15, "all pages should sum to 15 items");
}

// 20. test_search_similar
#[test]
fn test_search_similar() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    let vectors: Vec<Vec<f32>> = vec![
        vec![1.0, 0.0, 0.0],
        vec![0.0, 1.0, 0.0],
        vec![0.0, 0.0, 1.0],
        vec![0.9, 0.1, 0.0],
        vec![0.8, 0.2, 0.0],
        vec![0.1, 0.9, 0.0],
        vec![-1.0, 0.0, 0.0],
    ];

    for (i, v) in vectors.iter().enumerate() {
        entity
            .create_vector(CreateVectorInput {
                collection: "sim_search".into(),
                dense: v.clone(),
                content: Some(format!("vec_{}", i)),
                metadata: vec![],
                link_row: None,
                link_node: None,
            })
            .expect("create_vector should succeed");
    }

    let results = query
        .search_similar(SearchSimilarInput {
            collection: "sim_search".into(),
            vector: vec![1.0, 0.0, 0.0],
            k: 3,
            min_score: 0.0,
            text: None,
            provider: None,
        })
        .expect("search_similar should succeed");

    assert_eq!(results.len(), 3, "should return k=3 results");

    assert!(
        results[0].score > 0.99,
        "exact match should have score ~1.0, got {}",
        results[0].score
    );

    // Test with min_score filtering
    let results_filtered = query
        .search_similar(SearchSimilarInput {
            collection: "sim_search".into(),
            vector: vec![1.0, 0.0, 0.0],
            k: 10,
            min_score: 0.8,
            text: None,
            provider: None,
        })
        .expect("search_similar with min_score should succeed");

    for r in &results_filtered {
        assert!(
            r.score >= 0.8,
            "all results should have score >= 0.8, got {}",
            r.score
        );
    }
}

// 21. test_search_text
#[test]
fn test_search_text() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    let texts = [
        (
            "Database Management Systems",
            "Explains relational databases and SQL",
        ),
        (
            "Network Security Fundamentals",
            "Covers firewalls and intrusion detection",
        ),
        (
            "Machine Learning Basics",
            "Introduction to neural networks and deep learning",
        ),
        (
            "Database Optimization Techniques",
            "Indexing and query optimization for databases",
        ),
        ("Web Development Guide", "HTML CSS and JavaScript tutorials"),
    ];

    for (title, description) in &texts {
        entity
            .create_row(CreateRowInput {
                collection: "text_search".into(),
                fields: vec![
                    ("title".into(), Value::Text(title.to_string())),
                    ("description".into(), Value::Text(description.to_string())),
                ],
                metadata: vec![],
                node_links: vec![],
                vector_links: vec![],
            })
            .expect("create_row should succeed");
    }

    let result = query.search_text(SearchTextInput {
        query: "database".into(),
        collections: Some(vec!["text_search".into()]),
        entity_types: None,
        capabilities: None,
        fields: None,
        limit: Some(10),
        fuzzy: false,
    });
    assert!(
        result.is_ok(),
        "search_text should succeed: {:?}",
        result.err()
    );
    let result = result.unwrap();
    assert!(
        !result.matches.is_empty(),
        "should find matches for 'database'"
    );
    assert!(
        result.matches.len() >= 2,
        "should find at least 2 database-related entries, got {}",
        result.matches.len()
    );

    // Fuzzy search should also work
    let fuzzy_result = query.search_text(SearchTextInput {
        query: "databse".into(),
        collections: Some(vec!["text_search".into()]),
        entity_types: None,
        capabilities: None,
        fields: None,
        limit: Some(10),
        fuzzy: true,
    });
    assert!(
        fuzzy_result.is_ok(),
        "fuzzy search_text should succeed: {:?}",
        fuzzy_result.err()
    );
}

// ===========================================================================
// Cross-model Tests
// ===========================================================================

// 22. test_node_with_embedding
#[test]
fn test_node_with_embedding() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    use reddb::application::CreateNodeEmbeddingInput;

    let out = entity.create_node(CreateNodeInput {
        collection: "embed_nodes".into(),
        label: "concept_ai".into(),
        node_type: Some("Concept".into()),
        properties: vec![
            ("name".into(), Value::Text("Artificial Intelligence".into())),
            ("field".into(), Value::Text("computer_science".into())),
        ],
        metadata: vec![],
        embeddings: vec![CreateNodeEmbeddingInput {
            name: "semantic".into(),
            vector: vec![0.5, 0.3, 0.8, 0.1],
            model: Some("test-model".into()),
        }],
        table_links: vec![],
        node_links: vec![],
    });
    assert!(
        out.is_ok(),
        "create_node with embedding should succeed: {:?}",
        out.err()
    );

    let page = query
        .scan(ScanCollectionInput {
            collection: "embed_nodes".into(),
            offset: 0,
            limit: 10,
        })
        .expect("scan should succeed");
    assert_eq!(page.items.len(), 1, "should have 1 node");

    let item = &page.items[0];
    assert!(item.data.is_node(), "entity should be a node");

    assert!(
        !item.embeddings().is_empty(),
        "node should have at least one embedding slot"
    );
}

// 23. test_row_linked_to_node
#[test]
fn test_row_linked_to_node() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);

    let node_out = entity
        .create_node(CreateNodeInput {
            collection: "linked".into(),
            label: "product_alpha".into(),
            node_type: Some("Product".into()),
            properties: vec![("sku".into(), Value::Text("SKU-001".into()))],
            metadata: vec![],
            embeddings: vec![],
            table_links: vec![],
            node_links: vec![],
        })
        .expect("create_node should succeed");

    let row_out = entity
        .create_row(CreateRowInput {
            collection: "linked".into(),
            fields: vec![
                (
                    "description".into(),
                    Value::Text("Alpha product review".into()),
                ),
                ("rating".into(), Value::Integer(5)),
            ],
            metadata: vec![],
            node_links: vec![],
            vector_links: vec![],
        })
        .expect("create_row should succeed");

    // Both entities should be in the same collection
    let page = query
        .scan(ScanCollectionInput {
            collection: "linked".into(),
            offset: 0,
            limit: 10,
        })
        .expect("scan should succeed");

    assert_eq!(
        page.items.len(),
        2,
        "collection should have both node and row, got {}",
        page.items.len()
    );

    let has_node = page.items.iter().any(|e| e.data.is_node());
    let has_row = page.items.iter().any(|e| e.data.is_row());
    assert!(has_node, "should contain a node");
    assert!(has_row, "should contain a row");

    assert_ne!(
        node_out.id, row_out.id,
        "node and row should have different IDs"
    );
}

// ---------------------------------------------------------------------------
// 24. SQL TTL in Row Insert
// ---------------------------------------------------------------------------

#[test]
fn test_sql_insert_row_ttl_ms_expiration() {
    let rt = rt();
    let query = QueryUseCases::new(&rt);
    let native = NativeUseCases::new(&rt);

    let inserted = query.execute(ExecuteQueryInput {
        query: "INSERT INTO ttl_rows (name, _ttl_ms) VALUES ('row-with-ttl-ms', 1)".into(),
    });
    assert!(
        inserted.is_ok(),
        "row INSERT with _ttl_ms should succeed: {:?}",
        inserted.err()
    );

    let before = query
        .execute(ExecuteQueryInput {
            query: "SELECT * FROM ttl_rows".into(),
        })
        .expect("SELECT before retention should succeed");
    assert_eq!(
        before.result.records.len(),
        1,
        "row should exist before retention sweep"
    );

    native
        .apply_retention_policy()
        .expect("apply_retention_policy should succeed");

    let after = query
        .execute(ExecuteQueryInput {
            query: "SELECT * FROM ttl_rows".into(),
        })
        .expect("SELECT after retention should succeed");
    assert_eq!(
        after.result.records.len(),
        0,
        "row should be deleted after _ttl_ms expiration"
    );
}

// ---------------------------------------------------------------------------
// 25. SQL TTL in Node Insert
// ---------------------------------------------------------------------------

#[test]
fn test_sql_insert_node_ttl_expiration() {
    let rt = rt();
    let query = QueryUseCases::new(&rt);
    let native = NativeUseCases::new(&rt);

    let inserted = query.execute(ExecuteQueryInput {
        query: "INSERT INTO ttl_nodes NODE (label, node_type, ip, _ttl_ms) VALUES ('node-ttl-ms', 'Host', '10.0.0.1', 1)"
            .into(),
    });
    assert!(
        inserted.is_ok(),
        "node INSERT with ttl should succeed: {:?}",
        inserted.err()
    );

    let before_scan = query
        .scan(ScanCollectionInput {
            collection: "ttl_nodes".into(),
            offset: 0,
            limit: 10,
        })
        .expect("scan before retention should succeed");
    assert_eq!(
        before_scan
            .items
            .iter()
            .filter(|item| item.data.is_node())
            .count(),
        1,
        "node should exist before retention sweep"
    );

    native
        .apply_retention_policy()
        .expect("apply_retention_policy should succeed");

    let after_scan = query
        .scan(ScanCollectionInput {
            collection: "ttl_nodes".into(),
            offset: 0,
            limit: 10,
        })
        .expect("scan after retention should succeed");
    assert_eq!(
        after_scan
            .items
            .iter()
            .filter(|item| item.data.is_node())
            .count(),
        0,
        "ttl-based node records should be deleted"
    );
}

// ---------------------------------------------------------------------------
// 26. SQL TTL in Edge Insert
// ---------------------------------------------------------------------------

#[test]
fn test_sql_insert_edge_ttl_expiration() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);
    let native = NativeUseCases::new(&rt);

    let from = entity.create_node(CreateNodeInput {
        collection: "ttl_edges".into(),
        label: "edge-node-a".into(),
        node_type: Some("Host".into()),
        properties: vec![("name".into(), Value::Text("node-a".into()))],
        metadata: vec![],
        embeddings: vec![],
        table_links: vec![],
        node_links: vec![],
    });
    assert!(
        from.is_ok(),
        "from node should be created: {:?}",
        from.err()
    );
    let to = entity.create_node(CreateNodeInput {
        collection: "ttl_edges".into(),
        label: "edge-node-b".into(),
        node_type: Some("Host".into()),
        properties: vec![("name".into(), Value::Text("node-b".into()))],
        metadata: vec![],
        embeddings: vec![],
        table_links: vec![],
        node_links: vec![],
    });
    assert!(to.is_ok(), "to node should be created: {:?}", to.err());
    let from = from.unwrap();
    let to = to.unwrap();

    let inserted = query.execute(ExecuteQueryInput {
        query: format!(
            "INSERT INTO ttl_edges EDGE (label, from, to, weight, _ttl_ms) VALUES ('connects', {}, {}, 0.9, 1)",
            from.id.raw(),
            to.id.raw()
        )
        .into(),
    });
    assert!(
        inserted.is_ok(),
        "edge INSERT with _ttl_ms should succeed: {:?}",
        inserted.err()
    );

    let before_scan = query
        .scan(ScanCollectionInput {
            collection: "ttl_edges".into(),
            offset: 0,
            limit: 20,
        })
        .expect("scan before retention should succeed");
    assert_eq!(
        before_scan
            .items
            .iter()
            .filter(|item| item.data.is_edge())
            .count(),
        1,
        "edge should exist before retention sweep"
    );

    native
        .apply_retention_policy()
        .expect("apply_retention_policy should succeed");

    let after_scan = query
        .scan(ScanCollectionInput {
            collection: "ttl_edges".into(),
            offset: 0,
            limit: 20,
        })
        .expect("scan after retention should succeed");
    assert_eq!(
        after_scan
            .items
            .iter()
            .filter(|item| item.data.is_edge())
            .count(),
        0,
        "edge should be deleted after ttl expiration"
    );
}

// ---------------------------------------------------------------------------
// 27. SQL TTL Update to Any Resource
// ---------------------------------------------------------------------------

#[test]
fn test_sql_update_ttl_after_insert() {
    let rt = rt();
    let query = QueryUseCases::new(&rt);
    let native = NativeUseCases::new(&rt);

    let inserted = query.execute(ExecuteQueryInput {
        query: "INSERT INTO ttl_updates (name, status) VALUES ('target-row', 'alive')".into(),
    });
    assert!(
        inserted.is_ok(),
        "initial INSERT without ttl should succeed: {:?}",
        inserted.err()
    );

    let updated = query.execute(ExecuteQueryInput {
        query: "UPDATE ttl_updates SET _ttl = 0 WHERE name = 'target-row'".into(),
    });
    assert!(
        updated.is_ok(),
        "UPDATE with _ttl should succeed: {:?}",
        updated.err()
    );

    let before = query
        .execute(ExecuteQueryInput {
            query: "SELECT * FROM ttl_updates".into(),
        })
        .expect("SELECT before retention should succeed");
    assert_eq!(
        before.result.records.len(),
        1,
        "row should exist before retention sweep"
    );

    native
        .apply_retention_policy()
        .expect("apply_retention_policy should succeed");

    let after = query
        .execute(ExecuteQueryInput {
            query: "SELECT * FROM ttl_updates".into(),
        })
        .expect("SELECT after retention should succeed");
    assert_eq!(
        after.result.records.len(),
        0,
        "row should be removed after SQL UPDATE _ttl=0"
    );
}

// ---------------------------------------------------------------------------
// 28. SQL TTL Update on Node
// ---------------------------------------------------------------------------

#[test]
fn test_sql_update_ttl_on_node_after_insert() {
    let rt = rt();
    let query = QueryUseCases::new(&rt);
    let native = NativeUseCases::new(&rt);

    let inserted = query.execute(ExecuteQueryInput {
        query: "INSERT INTO ttl_node_updates NODE (label, node_type, ip) VALUES ('node-update-target', 'Host', '10.0.0.8')"
            .into(),
    });
    assert!(
        inserted.is_ok(),
        "initial node INSERT without ttl should succeed: {:?}",
        inserted.err()
    );

    let updated = query.execute(ExecuteQueryInput {
        query: "UPDATE ttl_node_updates SET _ttl = 0 WHERE label = 'node-update-target'".into(),
    });
    assert!(
        updated.is_ok(),
        "node UPDATE with _ttl should succeed: {:?}",
        updated.err()
    );

    let before_scan = query
        .scan(ScanCollectionInput {
            collection: "ttl_node_updates".into(),
            offset: 0,
            limit: 10,
        })
        .expect("scan before retention should succeed");
    assert_eq!(
        before_scan
            .items
            .iter()
            .filter(|item| item.data.is_node())
            .count(),
        1,
        "node should exist before retention sweep"
    );

    native
        .apply_retention_policy()
        .expect("apply_retention_policy should succeed");

    let after_scan = query
        .scan(ScanCollectionInput {
            collection: "ttl_node_updates".into(),
            offset: 0,
            limit: 10,
        })
        .expect("scan after retention should succeed");
    assert_eq!(
        after_scan
            .items
            .iter()
            .filter(|item| item.data.is_node())
            .count(),
        0,
        "node should be removed after SQL UPDATE _ttl=0"
    );
}

// ---------------------------------------------------------------------------
// 29. SQL DELETE on Node
// ---------------------------------------------------------------------------

#[test]
fn test_sql_delete_node_with_where_clause() {
    let rt = rt();
    let query = QueryUseCases::new(&rt);

    let inserted = query.execute(ExecuteQueryInput {
        query: "INSERT INTO delete_nodes NODE (label, node_type, ip) VALUES ('delete-me', 'Host', '10.0.0.9')"
            .into(),
    });
    assert!(
        inserted.is_ok(),
        "initial node INSERT should succeed: {:?}",
        inserted.err()
    );

    let deleted = query.execute(ExecuteQueryInput {
        query: "DELETE FROM delete_nodes WHERE label = 'delete-me'".into(),
    });
    assert!(
        deleted.is_ok(),
        "node DELETE should succeed: {:?}",
        deleted.err()
    );
    assert_eq!(
        deleted.unwrap().affected_rows,
        1,
        "node DELETE should affect exactly one entity"
    );

    let after_scan = query
        .scan(ScanCollectionInput {
            collection: "delete_nodes".into(),
            offset: 0,
            limit: 10,
        })
        .expect("scan after delete should succeed");
    assert_eq!(
        after_scan
            .items
            .iter()
            .filter(|item| item.data.is_node())
            .count(),
        0,
        "node should be removed after SQL DELETE"
    );
}

// ---------------------------------------------------------------------------
// 30. CREATE TABLE WITH TTL applies default retention to inserts
// ---------------------------------------------------------------------------

#[test]
fn test_create_table_with_ttl_applies_default_to_api_insert() {
    let rt = rt();
    let entity = EntityUseCases::new(&rt);
    let query = QueryUseCases::new(&rt);
    let native = NativeUseCases::new(&rt);

    let created = query.execute(ExecuteQueryInput {
        query: "CREATE TABLE sessions (token TEXT, user_id TEXT) WITH TTL 0s".into(),
    });
    assert!(
        created.is_ok(),
        "CREATE TABLE ... WITH TTL should succeed: {:?}",
        created.err()
    );

    let inserted = entity.create_row(CreateRowInput {
        collection: "sessions".into(),
        fields: vec![
            ("token".into(), Value::Text("t-1".into())),
            ("user_id".into(), Value::Text("u-1".into())),
        ],
        metadata: vec![],
        node_links: vec![],
        vector_links: vec![],
    });
    assert!(
        inserted.is_ok(),
        "row insert into table with default TTL should succeed: {:?}",
        inserted.err()
    );

    let before = query
        .execute(ExecuteQueryInput {
            query: "SELECT * FROM sessions".into(),
        })
        .expect("SELECT before retention should succeed");
    assert_eq!(
        before.result.records.len(),
        1,
        "row should exist before retention sweep"
    );

    native
        .apply_retention_policy()
        .expect("apply_retention_policy should succeed");

    let after = query
        .execute(ExecuteQueryInput {
            query: "SELECT * FROM sessions".into(),
        })
        .expect("SELECT after retention should succeed");
    assert_eq!(
        after.result.records.len(),
        0,
        "default table TTL should be applied to inserted rows"
    );
}

#[test]
fn test_password_hash_and_verify() {
    let rt = rt();
    let query = QueryUseCases::new(&rt);

    // INSERT with PASSWORD('plaintext') literal should store an
    // argon2id hash, not the plaintext.
    let insert = query.execute(ExecuteQueryInput {
        query: "INSERT INTO accounts (username, pw) VALUES ('alice', PASSWORD('MyP@ss123'))".into(),
    });
    assert!(
        insert.is_ok(),
        "INSERT with PASSWORD() literal should succeed: {:?}",
        insert.err()
    );

    // VERIFY_PASSWORD is exposed as a projection (scalar function).
    // Correct candidate → true; wrong candidate → false.
    let matching = query
        .execute(ExecuteQueryInput {
            query: "SELECT VERIFY_PASSWORD(pw, 'MyP@ss123') AS ok FROM accounts".into(),
        })
        .expect("VERIFY_PASSWORD SELECT should succeed");
    let row = matching
        .result
        .records
        .first()
        .expect("at least one row for matching candidate");
    let ok = row
        .values
        .get("ok")
        .cloned()
        .unwrap_or(Value::Boolean(false));
    assert_eq!(
        ok,
        Value::Boolean(true),
        "correct password must return Boolean(true), got {ok:?}"
    );

    let non_matching = query
        .execute(ExecuteQueryInput {
            query: "SELECT VERIFY_PASSWORD(pw, 'wrong') AS ok FROM accounts".into(),
        })
        .expect("VERIFY_PASSWORD SELECT (wrong candidate) should succeed");
    let row = non_matching
        .result
        .records
        .first()
        .expect("at least one row for wrong candidate");
    let ok = row
        .values
        .get("ok")
        .cloned()
        .unwrap_or(Value::Boolean(true));
    assert_eq!(
        ok,
        Value::Boolean(false),
        "wrong password must return Boolean(false), got {ok:?}"
    );

    // Raw SELECT of the password column must never surface the
    // plaintext — the stored hash is wrapped in Value::Password and
    // the Display impl masks it. The hash itself still lives in the
    // record so VERIFY_PASSWORD can compare against it internally.
    let raw = query
        .execute(ExecuteQueryInput {
            query: "SELECT pw FROM accounts".into(),
        })
        .expect("raw SELECT of password should succeed");
    let row = raw.result.records.first().expect("at least one row");
    if let Some(pw_value) = row.values.get("pw") {
        // The value must be wrapped in Value::Password (not raw Text
        // that would leak the hash or plaintext through formatters).
        match pw_value {
            Value::Password(h) => {
                assert!(
                    h.starts_with("argon2id$"),
                    "stored password must be an argon2id hash, got: {h}"
                );
                assert!(
                    !h.contains("MyP@ss123"),
                    "plaintext must not be in the stored hash"
                );
            }
            other => panic!("expected Value::Password, got {other:?}"),
        }
        // Display must mask the value.
        assert_eq!(format!("{pw_value}"), "***");
    }
}

#[test]
fn test_secret_encrypt_and_decrypt() {
    use std::sync::Arc;

    let rt = rt();

    // Without an AuthStore wired into the runtime, `SECRET('...')`
    // in INSERT must fail with a clear error — the AES key lives in
    // the vault and is inaccessible here.
    let without_vault = QueryUseCases::new(&rt).execute(ExecuteQueryInput {
        query: "INSERT INTO creds (name, token) VALUES ('stripe', SECRET('sk_live_abc'))".into(),
    });
    assert!(without_vault.is_err(), "SECRET() without a vault must fail");

    // Wire a bootstrapped AuthStore with an auto-generated AES key.
    let auth = Arc::new(reddb::prelude::AuthStore::new(
        reddb::prelude::AuthConfig::default(),
    ));
    auth.ensure_vault_secret_key();
    rt.set_auth_store(Arc::clone(&auth));

    let query = QueryUseCases::new(&rt);

    // Happy path: INSERT encrypts, SELECT decrypts (auto_decrypt=true default).
    query
        .execute(ExecuteQueryInput {
            query: "INSERT INTO creds (name, token) VALUES ('stripe', SECRET('sk_live_abc'))"
                .into(),
        })
        .expect("INSERT with SECRET() must succeed once the vault is wired");

    let decrypted = query
        .execute(ExecuteQueryInput {
            query: "SELECT name, token FROM creds".into(),
        })
        .expect("SELECT should succeed");
    let row = decrypted.result.records.first().expect("at least one row");
    let tok_val = row.values.get("token").expect("token column present");
    assert_eq!(
        tok_val,
        &Value::Text("sk_live_abc".to_string()),
        "auto_decrypt=true should surface plaintext, got {tok_val:?}"
    );

    // Flip auto_decrypt off: SELECT should return the raw Value::Secret
    // (bytes) which formatters mask.
    query
        .execute(ExecuteQueryInput {
            query: "SET CONFIG red.config.secret.auto_decrypt = false".into(),
        })
        .expect("SET CONFIG should succeed");

    let masked = query
        .execute(ExecuteQueryInput {
            query: "SELECT name, token FROM creds".into(),
        })
        .expect("SELECT with auto_decrypt=false should succeed");
    let row = masked.result.records.first().expect("at least one row");
    let tok_val = row.values.get("token").expect("token column present");
    match tok_val {
        Value::Secret(bytes) => {
            assert!(
                !String::from_utf8_lossy(bytes).contains("sk_live_abc"),
                "ciphertext must not reveal plaintext"
            );
        }
        other => panic!("expected Value::Secret when auto_decrypt is off, got {other:?}"),
    }
}
