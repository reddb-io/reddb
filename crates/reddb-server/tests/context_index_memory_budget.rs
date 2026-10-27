//! Context indexing must share the same reported and enforced memory budget.
use reddb_server::application::SearchContextInput;
use reddb_server::storage::memory_pools::MemoryPool;
use reddb_server::{RedDBOptions, RedDBRuntime};

#[test]
fn enabled_context_postings_are_reported_beyond_native_row_indexes() {
    let runtime =
        RedDBRuntime::with_options(RedDBOptions::in_memory().with_memory_budget(8 * 1024 * 1024))
            .expect("runtime");
    runtime
        .execute_query("CREATE TABLE context_rows (id INTEGER PRIMARY KEY,body TEXT) WITH context_index = true")
        .expect("opt-in collection");
    for id in 0..100 {
        runtime
            .execute_query(&format!(
                "INSERT INTO context_rows (id,body) VALUES ({id},'customer{id} reference{id} alpha beta gamma delta')"
            ))
            .expect("within budget");
    }
    runtime.refresh_memory_accounting();
    let native = runtime.index_store_ref().memory_bytes();
    let reported = runtime
        .memory_accounting()
        .used_bytes(MemoryPool::IndexMemory);
    assert!(
        reported > native + 64 * 1024,
        "opted-in postings must be charged: reported={reported}, native={native}"
    );
}

#[test]
fn context_growth_is_rejected_before_publishing_an_over_budget_row() {
    let runtime =
        RedDBRuntime::with_options(RedDBOptions::in_memory().with_memory_budget(128 * 1024))
            .expect("runtime");
    runtime
        .execute_query("CREATE TABLE context_rows (id INTEGER PRIMARY KEY,body TEXT) WITH context_index = true")
        .expect("opt-in collection");
    // Establish a positive control before the larger indexed rows exhaust the
    // tiny budget. Resident directories now participate in the same ceiling.
    runtime
        .execute_query("INSERT INTO context_rows (id,body) VALUES (0,'seed')")
        .expect("small positive control fits the unchanged 128 KiB budget");
    let mut accepted = 1;
    let mut rejection = None;
    for id in 1..100 {
        match runtime.execute_query(&format!(
            "INSERT INTO context_rows (id,body) VALUES ({id},'customer{id} reference{id} alpha beta gamma delta')"
        )) {
            Ok(_) => accepted += 1,
            Err(error) => {
                rejection = Some(error);
                break;
            }
        }
    }
    let error = rejection.expect("the context footprint must exhaust this small shared budget");
    assert!(error.to_string().contains("over budget"), "{error}");
    assert!(accepted > 0);
    let rows = runtime
        .execute_query("SELECT id FROM context_rows ORDER BY id")
        .expect("accepted data stays readable")
        .result
        .records;
    assert_eq!(rows.len(), accepted);
    for (id, row) in rows.iter().enumerate() {
        assert_eq!(row.get("id").expect("id").to_string(), id.to_string());
    }
    runtime.refresh_memory_accounting();
    assert!(runtime.memory_accounting().total_used_bytes() <= 128 * 1024);
}

#[test]
fn context_update_admission_preserves_the_original_row_and_search_hits() {
    let body = (0..250)
        .map(|n| format!("different{n}"))
        .collect::<Vec<_>>()
        .join(" ");
    for enabled in [false, true] {
        let runtime =
            RedDBRuntime::with_options(RedDBOptions::in_memory().with_memory_budget(128 * 1024))
                .expect("runtime");
        runtime.execute_query(&format!(
            "CREATE TABLE context_rows (id INTEGER PRIMARY KEY,body TEXT) WITH context_index = {enabled}"
        )).expect("table");
        runtime
            .execute_query("INSERT INTO context_rows (id,body) VALUES (1,'original')")
            .expect("seed");
        let result = runtime.execute_query(&format!(
            "UPDATE context_rows SET body = '{body}' WHERE id = 1"
        ));
        if enabled {
            let error = result.expect_err("context postings exceed the remaining budget");
            assert!(error.to_string().contains("over budget"), "{error}");
            let store = runtime.db().store();
            let index = store.context_index();
            assert_eq!(
                index.search_field("body", "original", true, 10, None).len(),
                1
            );
            assert!(index
                .search_field("body", "different1", true, 10, None)
                .is_empty());
        } else {
            result.expect("the same payload fits without the optional index");
        }
        let records = runtime
            .execute_query("SELECT body FROM context_rows")
            .expect("readback")
            .result
            .records;
        let expected = if enabled { "original" } else { body.as_str() };
        assert_eq!(
            records[0].get("body"),
            Some(&reddb_types::Value::text(expected))
        );
        runtime.refresh_memory_accounting();
        assert!(runtime.memory_accounting().total_used_bytes() <= 128 * 1024);
    }
}

#[test]
fn context_bulk_admission_rejects_the_entire_statement() {
    let runtime =
        RedDBRuntime::with_options(RedDBOptions::in_memory().with_memory_budget(128 * 1024))
            .expect("runtime");
    runtime.execute_query(
        "CREATE TABLE context_rows (id INTEGER PRIMARY KEY,body TEXT) WITH context_index = true"
    ).expect("table");
    let values = (0..100)
        .map(|id| format!("({id},'customer{id} reference{id} alpha beta gamma delta')"))
        .collect::<Vec<_>>()
        .join(",");
    let error = runtime
        .execute_query(&format!(
            "INSERT INTO context_rows (id,body) VALUES {values}"
        ))
        .expect_err("reserve the full statement before publication");
    assert!(error.to_string().contains("over budget"), "{error}");
    assert!(matches!(
        error,
        reddb_server::RedDBError::InvalidOperation(_)
    ));
    assert!(runtime
        .execute_query("SELECT id FROM context_rows")
        .expect("readback")
        .result
        .records
        .is_empty());
    assert_eq!(
        runtime
            .db()
            .store()
            .context_index()
            .stats()
            .indexed_entities,
        0
    );
}

#[test]
fn lazy_context_reindex_cannot_bypass_admission() {
    let runtime =
        RedDBRuntime::with_options(RedDBOptions::in_memory().with_memory_budget(128 * 1024))
            .expect("runtime");
    runtime
        .execute_query("CREATE TABLE docs (id INTEGER PRIMARY KEY,body TEXT)")
        .expect("table");
    let body = (0..250)
        .map(|n| format!("different{n}"))
        .collect::<Vec<_>>()
        .join(" ");
    runtime
        .execute_query(&format!("INSERT INTO docs (id,body) VALUES (1,'{body}')"))
        .expect("the payload fits without context postings");
    let store = runtime.db().store();
    store.context_index().set_collection_enabled("docs", true);
    let input = |reindex| SearchContextInput {
        query: "different1".into(),
        field: None,
        vector: None,
        collections: Some(vec!["docs".into()]),
        graph_depth: None,
        graph_max_edges: None,
        max_cross_refs: None,
        follow_cross_refs: Some(false),
        expand_graph: Some(false),
        global_scan: Some(true),
        reindex: Some(reindex),
        limit: Some(1),
        min_score: None,
    };
    runtime
        .search_context(input(false))
        .expect("read-only fallback");
    let error = runtime
        .search_context(input(true))
        .expect_err("optional reindexing must reserve its resident postings");
    assert!(error.to_string().contains("over budget"), "{error}");
    assert_eq!(store.context_index().stats().indexed_entities, 0);
    assert_eq!(
        runtime
            .execute_query("SELECT body FROM docs")
            .expect("original data")
            .result
            .records[0]
            .get("body"),
        Some(&reddb_types::Value::text(body))
    );
}
