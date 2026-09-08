use reddb::auth::Role;
use reddb::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

struct IdentityGuard;
impl Drop for IdentityGuard {
    fn drop(&mut self) {
        clear_current_auth_identity();
    }
}

fn fixture(turbo: bool) -> (tempfile::TempDir, RedDBRuntime) {
    let directory = tempfile::tempdir().expect("fixture directory");
    let runtime =
        RedDBRuntime::with_options(RedDBOptions::persistent(directory.path().join("db.rdb")))
            .expect("fixture runtime");
    runtime
        .execute_query("CREATE VECTOR embeddings DIM 2 METRIC cosine")
        .expect("vectors");
    if !turbo {
        let db = runtime.db();
        let mut contract = db.collection_contract("embeddings").expect("contract");
        contract.name = "legacy".into();
        db.store()
            .create_collection("legacy")
            .expect("legacy collection");
        db.save_collection_contract(contract)
            .expect("legacy contract");
    }
    let collection = if turbo { "embeddings" } else { "legacy" };
    // More denied high-score rows than TurboQuant's normal rerank overfetch.
    for index in 0..64 {
        runtime.execute_query(&format!("INSERT INTO {collection} VECTOR (dense,content) VALUES ([1.0,0.0],'bob_{index}') WITH METADATA (owner='bob',eligible=true)")).expect("private candidate");
    }
    runtime.execute_query(&format!("INSERT INTO {collection} VECTOR (dense,content) VALUES ([0.8,0.6],'alice') WITH METADATA (owner='alice',eligible=true)")).expect("allowed candidate");
    runtime
        .execute_query("CREATE DOCUMENT papers")
        .expect("documents");
    runtime
        .execute_query("INSERT INTO papers DOCUMENT VALUES ({name:'alice'})")
        .expect("public document");
    runtime
        .execute_query("INSERT INTO papers DOCUMENT VALUES ({name:'bob_0'})")
        .expect("public document");
    runtime
        .execute_query(&format!(
            "CREATE POLICY own_vectors ON {collection} USING (metadata.owner=CURRENT_USER())"
        ))
        .expect("legacy policy syntax");
    runtime
        .execute_query(&format!(
            "ALTER TABLE {collection} ENABLE ROW LEVEL SECURITY"
        ))
        .expect("enable RLS");
    (directory, runtime)
}

fn contents(runtime: &RedDBRuntime, sql: &str, field: &str) -> Vec<String> {
    runtime
        .execute_query(sql)
        .expect("query")
        .result
        .records
        .iter()
        .map(|row| match row.get(field) {
            Some(Value::Text(value)) => value.to_string(),
            other => panic!("expected {field}, got {other:?}"),
        })
        .collect()
}

#[test]
fn vector_rls_precedes_topk_on_turbo_and_legacy() {
    let _identity = IdentityGuard;
    for turbo in [true, false] {
        clear_current_auth_identity();
        let (_directory, runtime) = fixture(turbo);
        let collection = if turbo { "embeddings" } else { "legacy" };
        for mode in ["EXACT", "APPROXIMATE"] {
            set_current_auth_identity("alice".into(), Role::Read);
            let sql =
                format!("VECTOR SEARCH {collection} SIMILAR TO [1.0,0.0] MODE {mode} LIMIT 1");
            assert_eq!(
                contents(&runtime, &sql, "content"),
                ["alice"],
                "turbo={turbo}"
            );
            assert_eq!(contents(&runtime, &sql, "content"), ["alice"], "repeat");
            set_current_auth_identity("nobody".into(), Role::Read);
            assert!(contents(&runtime, &sql, "content").is_empty());
            set_current_auth_identity("alice".into(), Role::Read);
            assert_eq!(
                contents(&runtime, &sql, "content"),
                ["alice"],
                "identity switch"
            );
        }
    }
}

#[test]
fn vector_rls_is_enforced_inside_join() {
    let (_directory, runtime) = fixture(true);
    let _identity = IdentityGuard;
    set_current_auth_identity("alice".into(), Role::Read);
    let sql = "SELECT v.content FROM papers d JOIN VECTOR SEARCH embeddings SIMILAR TO [1.0,0.0] LIMIT 1 AS v ON d.name=v.content";
    assert_eq!(contents(&runtime, sql, "v.content"), ["alice"]);
}

#[test]
fn vector_rls_applies_to_typed_execution_and_existing_filter() {
    use reddb_rql::ast::{MetadataFilter, QueryExpr, VectorQuery, VectorSource};
    let (_directory, runtime) = fixture(true);
    let _identity = IdentityGuard;
    set_current_auth_identity("alice".into(), Role::Read);
    let query = VectorQuery::new("embeddings", VectorSource::literal(vec![1.0, 0.0]))
        .limit(1)
        .with_filter(MetadataFilter::eq("eligible", true));
    let result = runtime
        .execute_query_expr(QueryExpr::Vector(query))
        .expect("typed search");
    assert_eq!(result.result.records.len(), 1);
    assert_eq!(
        result.result.records[0].get("content"),
        Some(&Value::text("alice"))
    );
    let sql = "VECTOR SEARCH embeddings SIMILAR TO [1.0,0.0] WHERE eligible=false LIMIT 1";
    assert!(
        contents(&runtime, sql, "content").is_empty(),
        "RLS must AND the caller filter"
    );
}

#[test]
fn vector_rls_revocation_invalidates_warm_results() {
    let (_directory, runtime) = fixture(true);
    let _identity = IdentityGuard;
    let sql = "VECTOR SEARCH embeddings SIMILAR TO [1.0,0.0] LIMIT 1";
    set_current_auth_identity("alice".into(), Role::Read);
    assert_eq!(contents(&runtime, sql, "content"), ["alice"]);
    assert_eq!(contents(&runtime, sql, "content"), ["alice"]);
    clear_current_auth_identity();
    runtime
        .execute_query("DROP POLICY own_vectors ON embeddings")
        .expect("revoke");
    set_current_auth_identity("alice".into(), Role::Read);
    assert!(
        contents(&runtime, sql, "content").is_empty(),
        "RLS with no policy denies all"
    );
}

#[test]
fn vector_rls_analysis_preserves_active_transaction_and_excludes_private_scoring() {
    let (_directory, runtime) = fixture(true);
    let _identity = IdentityGuard;
    runtime
        .execute_query("CREATE TABLE changes (id INT)")
        .expect("table");
    runtime.execute_query("BEGIN").expect("begin");
    runtime
        .execute_query("INSERT INTO changes (id) VALUES (1)")
        .expect("pending write");
    set_current_auth_identity("alice".into(), Role::Read);
    let result = runtime
        .execute_query("EXPLAIN ANALYZE VECTOR SEARCH embeddings SIMILAR TO [1.0,0.0] LIMIT 1")
        .expect("analyze");
    let stats = result.result.stats.vector.expect("vector counters");
    assert_eq!(stats.rows_returned, 1);
    assert_eq!(stats.rls_rejected, 64);
    assert_eq!(
        stats.exact_distance_evaluations, 1,
        "private vectors must not reach exact scoring"
    );
    clear_current_auth_identity();
    runtime
        .execute_query("ROLLBACK")
        .expect("transaction remains active");
    assert!(runtime
        .execute_query("SELECT * FROM changes")
        .expect("rolled back")
        .result
        .records
        .is_empty());
}

#[test]
fn vector_rls_is_enforced_in_hybrid_and_vector_source_subqueries() {
    use reddb_rql::ast::{
        FusionStrategy, HybridQuery, QueryExpr, TableQuery, VectorQuery, VectorSource,
    };
    let (_directory, runtime) = fixture(true);
    runtime
        .execute_query("CREATE TABLE empty_rows (id INT)")
        .expect("empty structured source");
    let _identity = IdentityGuard;
    set_current_auth_identity("alice".into(), Role::Read);
    let vector = VectorQuery::new("embeddings", VectorSource::literal(vec![1.0, 0.0])).limit(1);
    let mut hybrid = HybridQuery::new(
        QueryExpr::Table(TableQuery::new("empty_rows")),
        vector.clone(),
    );
    hybrid.fusion = FusionStrategy::Union {
        structured_weight: 0.0,
        vector_weight: 1.0,
    };
    let result = runtime
        .execute_query_expr(QueryExpr::Hybrid(hybrid))
        .expect("hybrid");
    assert_eq!(result.result.records.len(), 1);
    assert_eq!(
        result.result.records[0].get("content"),
        Some(&Value::text("alice"))
    );
    let nested = VectorQuery::new(
        "embeddings",
        VectorSource::Subquery(Box::new(QueryExpr::Vector(vector))),
    )
    .limit(1);
    let result = runtime
        .execute_query_expr(QueryExpr::Vector(nested.clone()))
        .expect("vector subquery");
    assert_eq!(result.result.records.len(), 1);
    assert_eq!(
        result.result.records[0].get("content"),
        Some(&Value::text("alice"))
    );
    set_current_auth_identity("nobody".into(), Role::Read);
    assert!(
        runtime
            .execute_query_expr(QueryExpr::Vector(nested))
            .is_err(),
        "unauthorized source must not supply an embedding"
    );
}

#[test]
fn vector_rls_prevents_private_reference_as_query_source() {
    use reddb_rql::ast::{QueryExpr, VectorQuery, VectorSource};
    let (_directory, runtime) = fixture(true);
    // Bob can obtain a reference to his own vector; Alice cannot resolve it.
    let _identity = IdentityGuard;
    set_current_auth_identity("bob".into(), Role::Read);
    let result = runtime
        .execute_query("VECTOR SEARCH embeddings SIMILAR TO [1.0,0.0] LIMIT 1")
        .expect("bob search");
    let vector_id = match result.result.records[0].get("entity_id") {
        Some(Value::UnsignedInteger(id)) => *id,
        other => panic!("expected vector identity, got {other:?}"),
    };
    let query = QueryExpr::Vector(VectorQuery::new(
        "embeddings",
        VectorSource::reference("embeddings", vector_id),
    ));
    assert!(runtime.execute_query_expr(query.clone()).is_ok());
    set_current_auth_identity("alice".into(), Role::Read);
    assert!(
        runtime.execute_query_expr(query).is_err(),
        "private source must not influence Alice's search"
    );
}

#[test]
fn vector_rls_applies_to_similar_and_ivf_runtime_entries() {
    use reddb::storage::EntityData;
    let (_directory, runtime) = fixture(true);
    let _identity = IdentityGuard;
    set_current_auth_identity("alice".into(), Role::Read);
    let matches = runtime
        .search_similar("embeddings", &[1.0, 0.0], 1, 0.0)
        .expect("similar");
    assert_eq!(matches.len(), 1);
    assert!(
        matches!(&matches[0].entity.data, EntityData::Vector(data) if data.content.as_deref() == Some("alice"))
    );
    let matches = runtime
        .search_ivf("embeddings", &[1.0, 0.0], 1, 1, Some(1))
        .expect("ivf")
        .matches;
    assert_eq!(matches.len(), 1);
    assert!(
        matches!(&matches[0].entity.as_ref().expect("entity").data, EntityData::Vector(data) if data.content.as_deref() == Some("alice"))
    );
}

#[test]
fn vector_rls_toggle_does_not_reuse_unrestricted_cached_rows() {
    let (_directory, runtime) = fixture(true);
    let _identity = IdentityGuard;
    let sql = "VECTOR SEARCH embeddings SIMILAR TO [1.0,0.0] LIMIT 1";
    runtime
        .execute_query("ALTER TABLE embeddings DISABLE ROW LEVEL SECURITY")
        .expect("disable");
    set_current_auth_identity("alice".into(), Role::Read);
    assert!(contents(&runtime, sql, "content")[0].starts_with("bob_"));
    clear_current_auth_identity();
    runtime
        .execute_query("ALTER TABLE embeddings ENABLE ROW LEVEL SECURITY")
        .expect("enable");
    set_current_auth_identity("alice".into(), Role::Read);
    assert_eq!(contents(&runtime, sql, "content"), ["alice"]);
    clear_current_auth_identity();
    runtime
        .execute_query("CREATE POLICY own_vectors ON embeddings USING (false)")
        .expect("replace policy");
    set_current_auth_identity("alice".into(), Role::Read);
    assert!(contents(&runtime, sql, "content").is_empty());
}
