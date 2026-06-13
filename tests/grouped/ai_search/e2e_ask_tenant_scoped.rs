//! Phase 3 ASK tenant-scoped — RLS-aware SEARCH CONTEXT / ASK corpus.
//!
//! When a session binds a tenant (`SET TENANT 'acme'`) and a table
//! declares `TENANT BY (col)`, the `search_context` pipeline (which
//! feeds `ASK`) must restrict the corpus to rows that the auto-RLS
//! policy accepts — identical to the SELECT path, so the LLM never
//! reasons over data the caller cannot see.

use reddb::application::SearchContextInput;
use reddb::runtime::mvcc::{
    clear_current_connection_id, set_current_connection_id, set_current_tenant,
};
use reddb::{RedDBOptions, RedDBRuntime};

fn open_runtime() -> RedDBRuntime {
    RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime should open in-memory")
}

fn exec(rt: &RedDBRuntime, sql: &str) {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("{sql}: {err:?}"));
}

#[test]
fn ask_corpus_scoped_to_current_tenant() {
    let rt = open_runtime();

    // Tenant-scoped table + RLS auto-policy.
    exec(
        &rt,
        "CREATE TABLE notes (id INT, body TEXT, tenant_id TEXT) TENANT BY (tenant_id)",
    );

    // Seed rows for two tenants in the same table.
    exec(
        &rt,
        "INSERT INTO notes (id, body, tenant_id) VALUES \
           (1, 'acme launch plan', 'acme'), \
           (2, 'globex quarterly revenue', 'globex')",
    );

    set_current_connection_id(100);

    // Acme session: search for 'revenue' (only globex has it) — must be empty.
    set_current_tenant("acme".to_string());
    let result = rt
        .search_context(SearchContextInput {
            query: "revenue".to_string(),
            field: None,
            vector: None,
            collections: None,
            limit: Some(10),
            graph_depth: None,
            graph_max_edges: None,
            max_cross_refs: None,
            follow_cross_refs: None,
            expand_graph: None,
            global_scan: Some(true),
            reindex: Some(false),
            min_score: Some(0.0),
        })
        .expect("search_context acme/revenue");
    assert!(
        total_matches(&result) == 0,
        "acme session must not see globex rows — found {} matches",
        total_matches(&result)
    );

    // Acme session: search for 'launch' — must surface its own row.
    let result = rt
        .search_context(SearchContextInput {
            query: "launch".to_string(),
            field: None,
            vector: None,
            collections: None,
            limit: Some(10),
            graph_depth: None,
            graph_max_edges: None,
            max_cross_refs: None,
            follow_cross_refs: None,
            expand_graph: None,
            global_scan: Some(true),
            reindex: Some(false),
            min_score: Some(0.0),
        })
        .expect("search_context acme/launch");
    assert!(
        total_matches(&result) > 0,
        "acme session must see its own 'launch' row"
    );

    // Globex session: search for 'launch' — zero (acme only).
    set_current_tenant("globex".to_string());
    let result = rt
        .search_context(SearchContextInput {
            query: "launch".to_string(),
            field: None,
            vector: None,
            collections: None,
            limit: Some(10),
            graph_depth: None,
            graph_max_edges: None,
            max_cross_refs: None,
            follow_cross_refs: None,
            expand_graph: None,
            global_scan: Some(true),
            reindex: Some(false),
            min_score: Some(0.0),
        })
        .expect("search_context globex/launch");
    assert!(
        total_matches(&result) == 0,
        "globex session must not see acme rows — found {} matches",
        total_matches(&result)
    );

    // Globex session: search for 'revenue' — surfaces its own row.
    let result = rt
        .search_context(SearchContextInput {
            query: "revenue".to_string(),
            field: None,
            vector: None,
            collections: None,
            limit: Some(10),
            graph_depth: None,
            graph_max_edges: None,
            max_cross_refs: None,
            follow_cross_refs: None,
            expand_graph: None,
            global_scan: Some(true),
            reindex: Some(false),
            min_score: Some(0.0),
        })
        .expect("search_context globex/revenue");
    assert!(
        total_matches(&result) > 0,
        "globex session must see its own 'revenue' row"
    );

    clear_current_connection_id();
}

fn total_matches(result: &reddb::runtime::ContextSearchResult) -> usize {
    result.tables.len() + result.vectors.len() + result.documents.len() + result.key_values.len()
}
