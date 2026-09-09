//! Cross-model policy resolution and policy-change regressions.
use reddb::auth::Role;
use reddb::json::{Map, Value as Json};
use reddb::runtime::mvcc::{clear_current_auth_identity, set_current_auth_identity};
use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

fn capture(rt: &RedDBRuntime, name: &str, sql: &str, field: &str, expected: &[&str]) -> Json {
    let mut row = Map::new();
    row.insert("name".into(), Json::String(name.into()));
    row.insert("sql".into(), Json::String(sql.into()));
    row.insert(
        "expected".into(),
        Json::Array(expected.iter().map(|s| Json::String((*s).into())).collect()),
    );
    match rt.execute_query(sql) {
        Ok(result) => {
            let mut actual: Vec<String> = result
                .result
                .records
                .iter()
                .map(|record| match record.get(field) {
                    Some(Value::Text(value)) => value.to_string(),
                    other => format!("unexpected field: {other:?}"),
                })
                .collect();
            actual.sort();
            let mut wanted: Vec<String> = expected.iter().map(|s| (*s).to_string()).collect();
            wanted.sort();
            row.insert("valid".into(), Json::Bool(actual == wanted));
            row.insert(
                "actual".into(),
                Json::Array(actual.into_iter().map(Json::String).collect()),
            );
        }
        Err(error) => {
            row.insert("valid".into(), Json::Bool(false));
            row.insert("error".into(), Json::String(error.to_string()));
        }
    }
    Json::Object(row)
}

fn node_rid(rt: &RedDBRuntime, name: &str) -> Result<u64, Box<dyn std::error::Error>> {
    let result = rt.execute_query(&format!("SELECT rid FROM links WHERE label='{name}'"))?;
    match result
        .result
        .records
        .first()
        .and_then(|record| record.get("rid"))
    {
        Some(Value::UnsignedInteger(value)) => Ok(*value),
        Some(Value::Integer(value)) => Ok(u64::try_from(*value)?),
        other => Err(format!("expected node RID for {name}, got {other:?}").into()),
    }
}

#[test]
fn document_graph_vector_policy_corpus() -> Result<(), Box<dyn std::error::Error>> {
    let directory = tempfile::tempdir()?;
    let rt = RedDBRuntime::with_options(RedDBOptions::persistent(directory.path().join("db.rdb")))?;
    for query in [
        "CREATE VECTOR embeddings DIM 2 METRIC cosine",
        "INSERT INTO embeddings VECTOR (dense,content) VALUES ([1.0,0.0],'bob') WITH METADATA (owner='bob')",
        "INSERT INTO embeddings VECTOR (dense,content) VALUES ([0.8,0.6],'alice') WITH METADATA (owner='alice')",
        "CREATE DOCUMENT papers",
        "INSERT INTO papers DOCUMENT VALUES ({name:'alice',owner:'alice'})",
        "INSERT INTO papers DOCUMENT VALUES ({name:'bob',owner:'bob'})",
        "CREATE DOCUMENT public_papers",
        "INSERT INTO public_papers DOCUMENT VALUES ({name:'alice'})",
        "INSERT INTO public_papers DOCUMENT VALUES ({name:'bob'})",
        "CREATE GRAPH links",
        "INSERT INTO links NODE (label,node_type,owner) VALUES ('alice','Paper','alice')",
        "INSERT INTO links NODE (label,node_type,owner) VALUES ('bob','Paper','bob')",
        "CREATE POLICY own_docs ON DOCUMENTS OF papers USING (owner=CURRENT_USER())",
        "CREATE POLICY own_nodes ON NODES OF links USING (properties.owner=CURRENT_USER())",
        "CREATE POLICY visible_edges ON EDGES OF links USING (properties.visible=true)",
    ] { rt.execute_query(query).map_err(|error| format!("{query}: {error}"))?; }
    let alice_rid = node_rid(&rt, "alice")?;
    let bob_rid = node_rid(&rt, "bob")?;
    for (from, to) in [
        (alice_rid, alice_rid),
        (bob_rid, bob_rid),
        (alice_rid, bob_rid),
    ] {
        rt.execute_query(&format!("INSERT INTO links EDGE (label,from,to,visible) VALUES ('provided_by',{from},{to},true)"))?;
    }
    let traversal = "MATCH (p:Paper)-[:provided_by]->(n:Paper) RETURN n.label";
    let mut cases = vec![
        capture(
            &rt,
            "document_fixture_without_rls",
            "SELECT name FROM papers",
            "name",
            &["alice", "bob"],
        ),
        capture(
            &rt,
            "graph_fixture_without_rls",
            "MATCH (n:Paper) RETURN n.label",
            "n.label",
            &["alice", "bob"],
        ),
        capture(
            &rt,
            "vector_fixture_without_rls",
            "VECTOR SEARCH embeddings SIMILAR TO [1.0,0.0] LIMIT 10",
            "content",
            &["alice", "bob"],
        ),
        capture(
            &rt,
            "graph_traversal_without_rls",
            traversal,
            "n.label",
            &["alice", "bob", "bob"],
        ),
    ];
    rt.execute_query("ALTER TABLE papers ENABLE ROW LEVEL SECURITY")?;
    rt.execute_query("ALTER TABLE links ENABLE ROW LEVEL SECURITY")?;
    let syntax =
        "CREATE POLICY own_vectors ON VECTORS OF embeddings USING (metadata.owner=CURRENT_USER())";
    let result = rt.execute_query(syntax);
    let mut record = Map::new();
    record.insert(
        "name".into(),
        Json::String("vector_policy_documented_syntax".into()),
    );
    record.insert("sql".into(), Json::String(syntax.into()));
    record.insert("valid".into(), Json::Bool(result.is_ok()));
    if let Err(error) = result {
        record.insert("error".into(), Json::String(error.to_string()));
        // The documented legacy TABLE policy also applies to vector entities.
        // Use it to test enforcement independently of the syntax failure.
        rt.execute_query(
            "CREATE POLICY own_vectors ON embeddings USING (metadata.owner=CURRENT_USER())",
        )?;
    }
    rt.execute_query("ALTER TABLE embeddings ENABLE ROW LEVEL SECURITY")?;
    cases.push(Json::Object(record));
    let search = "VECTOR SEARCH embeddings SIMILAR TO [1.0,0.0] LIMIT 1";
    for (iteration, user) in ["alice", "bob", "alice", "nobody"].into_iter().enumerate() {
        set_current_auth_identity(user.to_string(), Role::Read);
        let expected: &[&str] = if user == "nobody" {
            &[]
        } else {
            std::slice::from_ref(&user)
        };
        cases.push(capture(
            &rt,
            &format!("identity_{user}_{iteration}"),
            "SELECT CURRENT_USER() AS principal",
            "principal",
            &[user],
        ));
        cases.push(capture(
            &rt,
            &format!("vector_rls_{user}_{}", cases.len()),
            search,
            "content",
            expected,
        ));
        cases.push(capture(
            &rt,
            &format!("document_rls_{user}_{iteration}"),
            "SELECT name FROM papers",
            "name",
            expected,
        ));
        cases.push(capture(
            &rt,
            &format!("graph_rls_{user}_{iteration}"),
            "MATCH (n:Paper) RETURN n.label",
            "n.label",
            expected,
        ));
        cases.push(capture(
            &rt,
            &format!("graph_traversal_rls_{user}_{iteration}"),
            traversal,
            "n.label",
            expected,
        ));
        cases.push(capture(&rt, &format!("join_rls_{user}_{iteration}"),
            "SELECT d.name FROM papers d JOIN VECTOR SEARCH embeddings SIMILAR TO [1.0,0.0] LIMIT 10 AS v ON d.name=v.content",
            "d.name", expected));
        cases.push(capture(&rt, &format!("vector_leaf_join_rls_{user}_{iteration}"),
            "SELECT v.content FROM public_papers d JOIN VECTOR SEARCH embeddings SIMILAR TO [1.0,0.0] LIMIT 10 AS v ON d.name=v.content",
            "v.content", expected));
        clear_current_auth_identity();
    }
    rt.execute_query("DROP POLICY own_vectors ON embeddings")?;
    set_current_auth_identity("alice".into(), Role::Read);
    cases.push(capture(
        &rt,
        "vector_no_matching_policy",
        search,
        "content",
        &[],
    ));
    clear_current_auth_identity();
    rt.execute_query("DROP POLICY own_docs ON papers")?;
    rt.execute_query("CREATE POLICY legacy_docs ON papers USING (owner=CURRENT_USER())")?;
    set_current_auth_identity("alice".into(), Role::Read);
    cases.push(capture(
        &rt,
        "document_legacy_policy_alice",
        "SELECT name FROM papers",
        "name",
        &["alice"],
    ));
    clear_current_auth_identity();
    rt.execute_query("DROP POLICY visible_edges ON links")?;
    rt.execute_query("CREATE POLICY all_edges ON EDGES OF links USING (true)")?;
    set_current_auth_identity("alice".into(), Role::Read);
    cases.push(capture(
        &rt,
        "graph_traversal_unconditional_edge_policy_alice",
        traversal,
        "n.label",
        &["alice"],
    ));
    clear_current_auth_identity();
    let failures: Vec<_> = cases
        .iter()
        .filter(|case| case.get("valid") != Some(&Json::Bool(true)))
        .collect();
    assert!(failures.is_empty(), "policy corpus failures: {failures:?}");
    Ok(())
}

#[test]
fn graph_policy_changes_invalidate_cached_paths() {
    let directory = tempfile::tempdir().expect("persistent fixture");
    for options in [
        RedDBOptions::in_memory(),
        RedDBOptions::persistent(directory.path().join("policy.rdb")),
    ] {
        let rt = RedDBRuntime::with_options(options).expect("runtime");
        for sql in [
            "CREATE GRAPH links",
            "INSERT INTO links NODE (label,node_type) VALUES ('alice','Paper')",
        ] {
            rt.execute_query(sql).expect("fixture");
        }
        let id = node_rid(&rt, "alice").expect("node rid");
        rt.execute_query(&format!(
            "INSERT INTO links EDGE (label,from,to,visible) VALUES ('provided_by',{id},{id},true)"
        ))
        .expect("edge");
        for sql in [
            "CREATE POLICY nodes ON NODES OF links USING (true)",
            "CREATE POLICY edges ON EDGES OF links USING (true)",
            "ALTER TABLE links ENABLE ROW LEVEL SECURITY",
        ] {
            rt.execute_query(sql).expect("policies");
        }
        let query = "MATCH (p:Paper)-[:provided_by]->(n:Paper) RETURN n.label";
        for _ in 0..2 {
            assert_eq!(
                rt.execute_query(query)
                    .expect("allowed and cached")
                    .result
                    .records
                    .len(),
                1
            );
        }
        rt.execute_query("DROP POLICY edges ON links")
            .expect("revoke");
        let revoked = rt.execute_query(query).expect("no edge policy");
        assert!(
            revoked.result.records.is_empty(),
            "revoked paths: {:?}, cache: {:?}",
            revoked.result.records,
            rt.result_cache_metrics()
        );
        rt.execute_query("CREATE POLICY edges ON EDGES OF links USING (false)")
            .expect("explicit deny");
        assert!(rt
            .execute_query(query)
            .expect("denied")
            .result
            .records
            .is_empty());
        rt.execute_query("DROP POLICY edges ON links")
            .expect("drop deny");
        rt.execute_query("CREATE POLICY edges ON EDGES OF links USING (true)")
            .expect("restore");
        assert_eq!(
            rt.execute_query(query)
                .expect("restored")
                .result
                .records
                .len(),
            1
        );
    }
}

#[test]
fn typed_policy_targets_preserve_kind_and_role_boundaries() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    for target in [
        "NODES",
        "EDGES",
        "VECTORS",
        "MESSAGES",
        "POINTS",
        "DOCUMENTS",
    ] {
        rt.execute_query(&format!(
            "CREATE POLICY typed ON {target} OF fixture FOR SELECT TO read USING (true)"
        ))
        .expect("documented typed policy syntax");
        rt.execute_query("DROP POLICY typed ON fixture")
            .expect("drop policy");
    }
    for sql in [
        "CREATE TABLE private_rows (name TEXT)",
        "INSERT INTO private_rows (name) VALUES ('private')",
        "CREATE POLICY docs_only ON DOCUMENTS OF private_rows USING (true)",
        "ALTER TABLE private_rows ENABLE ROW LEVEL SECURITY",
        "CREATE DOCUMENT docs",
        "INSERT INTO docs DOCUMENT VALUES ({name:'private'})",
        "CREATE POLICY docs_admin ON DOCUMENTS OF docs FOR SELECT TO admin USING (true)",
        "ALTER TABLE docs ENABLE ROW LEVEL SECURITY",
    ] {
        rt.execute_query(sql).expect("fixture");
    }
    set_current_auth_identity("alice".into(), Role::Read);
    let table = rt.execute_query("SELECT name FROM private_rows");
    let docs = rt.execute_query("SELECT name FROM docs");
    clear_current_auth_identity();
    assert!(table.expect("wrong model denied").result.records.is_empty());
    assert!(docs.expect("wrong role denied").result.records.is_empty());
}

#[test]
fn graph_property_namespace_matches_static_and_dynamic_predicates() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    for sql in [
        "CREATE GRAPH links",
        "INSERT INTO links NODE (label,node_type,owner,visible) VALUES ('alice','Paper','alice',true)",
        "INSERT INTO links NODE (label,node_type,owner,visible) VALUES ('bob','Paper','bob',false)",
        "CREATE POLICY nodes ON NODES OF links USING (properties.visible=true)",
    ] {
        rt.execute_query(sql).expect("fixture");
    }
    let id = node_rid(&rt, "alice").expect("visible endpoint");
    rt.execute_query("ALTER TABLE links ENABLE ROW LEVEL SECURITY")
        .expect("enable RLS");
    for visible in [true, false] {
        rt.execute_query(&format!(
            "INSERT INTO links EDGE (label,from,to,visible) VALUES ('provided_by',{id},{id},{visible})"
        )).expect("edge fixture");
    }
    rt.execute_query("CREATE POLICY edges ON EDGES OF links USING (properties.visible=true)")
        .expect("edge policy");
    let result = capture(
        &rt,
        "hidden edge",
        "MATCH (p:Paper)-[:provided_by]->(n:Paper) RETURN n.label",
        "n.label",
        &["alice"],
    );
    assert_eq!(result.get("valid"), Some(&Json::Bool(true)), "{result:?}");
    let result = capture(
        &rt,
        "static node namespace",
        "MATCH (n:Paper) RETURN n.label",
        "n.label",
        &["alice"],
    );
    assert_eq!(result.get("valid"), Some(&Json::Bool(true)), "{result:?}");
    rt.execute_query("DROP POLICY nodes ON links")
        .expect("drop");
    rt.execute_query(
        "CREATE POLICY nodes ON NODES OF links USING (properties.owner=CURRENT_USER())",
    )
    .expect("dynamic policy");
    set_current_auth_identity("bob".into(), Role::Read);
    let result = capture(
        &rt,
        "dynamic node namespace",
        "MATCH (n:Paper) RETURN n.label",
        "n.label",
        &["bob"],
    );
    clear_current_auth_identity();
    assert_eq!(result.get("valid"), Some(&Json::Bool(true)), "{result:?}");
}
