use reddb::application::{SearchContextInput, VcsUseCases};
use reddb::auth::Role;
use reddb::runtime::mvcc::{
    clear_current_auth_identity, clear_current_connection_id, set_current_auth_identity,
    set_current_connection_id,
};
use reddb::runtime::{ContextSearchResult, DiscoveryMethod};
use reddb::storage::EntityData;
use reddb::{RedDBOptions, RedDBRuntime};

struct ReadScope;
impl Drop for ReadScope {
    fn drop(&mut self) {
        clear_current_auth_identity();
        clear_current_connection_id();
    }
}

fn execute(runtime: &RedDBRuntime, sql: &str) {
    runtime
        .execute_query(sql)
        .unwrap_or_else(|error| panic!("{sql}: {error}"));
}

fn search(runtime: &RedDBRuntime) -> ContextSearchResult {
    runtime
        .search_context_input(SearchContextInput {
            query: "absentneedle".into(),
            field: None,
            vector: Some(vec![1.0, 0.0]),
            collections: Some(vec!["embeddings".into()]),
            limit: Some(1),
            graph_depth: Some(0),
            graph_max_edges: Some(0),
            max_cross_refs: Some(0),
            follow_cross_refs: Some(false),
            expand_graph: Some(false),
            global_scan: Some(false),
            reindex: Some(false),
            min_score: Some(0.0),
        })
        .expect("context vector expansion")
}

fn assert_content(result: &ContextSearchResult, expected: &str) {
    assert_eq!(result.vectors.len(), 1);
    assert_eq!(result.summary.expanded_via_vector_query, 1);
    assert_eq!(result.vectors[0].collection, "embeddings");
    assert!(matches!(&result.vectors[0].entity.data,
        EntityData::Vector(data) if data.content.as_deref() == Some(expected)));
    let DiscoveryMethod::VectorQuery { similarity } = result.vectors[0].discovery else {
        panic!("vector provenance required");
    };
    assert_eq!(result.vectors[0].score, similarity * 0.9);
}

#[test]
fn context_vectors_keep_policy_before_topk_and_collection_scope() {
    let _scope = ReadScope;
    let runtime = RedDBRuntime::in_memory().expect("runtime");
    for collection in ["embeddings", "outside"] {
        execute(
            &runtime,
            &format!("CREATE VECTOR {collection} DIM 2 METRIC cosine"),
        );
    }
    execute(
        &runtime,
        "INSERT INTO outside VECTOR (dense,content) VALUES ([1,0],'outside')",
    );
    execute(&runtime, "INSERT INTO embeddings VECTOR (dense,content) VALUES ([1,0],'private') WITH METADATA (owner='bob')");
    execute(&runtime, "INSERT INTO embeddings VECTOR (dense,content) VALUES ([0.8,0.6],'allowed') WITH METADATA (owner='alice')");
    execute(
        &runtime,
        "CREATE POLICY own_vectors ON embeddings USING (metadata.owner=CURRENT_USER())",
    );
    execute(&runtime, "ALTER TABLE embeddings ENABLE ROW LEVEL SECURITY");
    set_current_auth_identity("alice".into(), Role::Read);
    assert_content(&search(&runtime), "allowed");
    set_current_auth_identity("nobody".into(), Role::Read);
    assert!(search(&runtime).vectors.is_empty());
    set_current_auth_identity("bob".into(), Role::Read);
    assert_content(&search(&runtime), "private");
    clear_current_auth_identity();
    execute(&runtime, "DROP POLICY own_vectors ON embeddings");
    set_current_auth_identity("alice".into(), Role::Read);
    assert!(
        search(&runtime).vectors.is_empty(),
        "RLS without policies denies by default"
    );
}

#[test]
fn context_vectors_keep_snapshot_savepoint_and_reopen_visibility() {
    let _scope = ReadScope;
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("vectors.rdb");
    {
        let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(&path)).expect("runtime");
        execute(&runtime, "CREATE VECTOR embeddings DIM 2 METRIC cosine");
        VcsUseCases::new(&runtime)
            .set_versioned("embeddings", true)
            .expect("versioned");
        execute(
            &runtime,
            "INSERT INTO embeddings VECTOR (dense,content) VALUES ([1,0],'old')",
        );
        let old_id = search(&runtime).vectors[0].entity.id.raw();
        set_current_connection_id(76001);
        execute(&runtime, "BEGIN");
        assert_content(&search(&runtime), "old");
        set_current_connection_id(76002);
        execute(&runtime, "BEGIN");
        execute(
            &runtime,
            &format!("DELETE FROM embeddings WHERE rid={old_id}"),
        );
        execute(
            &runtime,
            "INSERT INTO embeddings VECTOR (dense,content) VALUES ([1,0],'new')",
        );
        assert_content(&search(&runtime), "new");
        execute(&runtime, "SAVEPOINT keep_new");
        let new_id = search(&runtime).vectors[0].entity.id.raw();
        execute(
            &runtime,
            &format!("DELETE FROM embeddings WHERE rid={new_id}"),
        );
        assert!(search(&runtime).vectors.is_empty());
        execute(&runtime, "ROLLBACK TO SAVEPOINT keep_new");
        assert_content(&search(&runtime), "new");
        set_current_connection_id(76001);
        assert_content(&search(&runtime), "old");
        set_current_connection_id(76002);
        execute(&runtime, "COMMIT");
        set_current_connection_id(76001);
        assert_content(&search(&runtime), "old");
        execute(&runtime, "COMMIT");
        assert_content(&search(&runtime), "new");
        execute(&runtime, "BEGIN");
        execute(
            &runtime,
            &format!("DELETE FROM embeddings WHERE rid={new_id}"),
        );
        assert!(search(&runtime).vectors.is_empty());
        execute(&runtime, "ROLLBACK");
        assert_content(&search(&runtime), "new");
    }
    clear_current_connection_id();
    let runtime = RedDBRuntime::with_options(RedDBOptions::persistent(path)).expect("reopen");
    assert_content(&search(&runtime), "new");
}
