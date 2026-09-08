use reddb::{RedDBOptions, RedDBRuntime};
use reddb_types::Value;

#[test]
fn vector_exact_default_matches_full_precision_oracle_beyond_rerank_window() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    rt.execute_query("CREATE VECTOR embeddings DIM 2 METRIC cosine")
        .expect("collection");
    // More than the old k*32 candidates, with close directions and the best
    // vector inserted last. Metadata selectivity changes the eligible winner.
    let mut vectors = Vec::new();
    for i in 0..160 {
        let angle = (160 - i) as f32 * 0.0007;
        let vector = [angle.cos(), angle.sin()];
        let eligible = i % 3 == 0;
        rt.execute_query(&format!("INSERT INTO embeddings VECTOR (dense,content) VALUES ([{},{}],'v{i}') WITH METADATA (eligible={eligible})", vector[0], vector[1])).expect("vector");
        vectors.push((i, vector, eligible));
    }
    for (metric, clause) in [
        ("cosine", ""),
        ("l2", "METRIC L2"),
        ("inner_product", "METRIC INNER_PRODUCT"),
    ] {
        for filtered in [false, true] {
            let filter = if filtered { "WHERE eligible=true" } else { "" };
            let mut oracle = vectors
                .iter()
                .filter(|(_, _, eligible)| !filtered || *eligible)
                .map(|(i, v, _)| {
                    let score = match metric {
                        "l2" => -((1.0 - v[0]).powi(2) + v[1].powi(2)),
                        "inner_product" => v[0],
                        _ => v[0] / (v[0] * v[0] + v[1] * v[1]).sqrt(),
                    };
                    (*i, score)
                })
                .collect::<Vec<_>>();
            oracle.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
            let expected = oracle
                .iter()
                .take(3)
                .map(|(i, _)| format!("v{i}"))
                .collect::<Vec<_>>();
            for mode in ["", "MODE EXACT"] {
                let query = format!(
                    "VECTOR SEARCH embeddings SIMILAR TO [1,0] {filter} {clause} {mode} LIMIT 3"
                );
                let result = rt.execute_query(&query).expect("exact query");
                let contents = result
                    .result
                    .records
                    .iter()
                    .map(|record| match record.get("content") {
                        Some(Value::Text(text)) => text.to_string(),
                        other => panic!("content {other:?}"),
                    })
                    .collect::<Vec<_>>();
                assert_eq!(contents, expected, "{query}");
                let stats = result.result.stats.vector.expect("metrics");
                assert_eq!(stats.mode_executed, "exact");
                assert!(!stats.index_used);
                assert_eq!(stats.approximate_distance_evaluations, 0);
                assert_eq!(stats.exact_distance_evaluations, oracle.len() as u64);
                assert_eq!(stats.peak_topk_entries, 3);
            }
        }
    }
}

#[test]
fn vector_exact_ties_and_threshold_are_stable() {
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    rt.execute_query("CREATE VECTOR ties DIM 2 METRIC cosine")
        .expect("collection");
    for content in ["first", "second", "third"] {
        rt.execute_query(&format!(
            "INSERT INTO ties VECTOR (dense,content) VALUES ([1,0],'{content}')"
        ))
        .expect("vector");
    }
    let result = rt
        .execute_query("VECTOR SEARCH ties SIMILAR TO [1,0] THRESHOLD 1 LIMIT 2")
        .expect("ties");
    assert_eq!(
        result.result.records[0].get("content"),
        Some(&Value::text("first"))
    );
    assert_eq!(
        result.result.records[1].get("content"),
        Some(&Value::text("second"))
    );
    let result = rt
        .execute_query("VECTOR SEARCH ties SIMILAR TO [0,1] THRESHOLD 0.5 LIMIT 2")
        .expect("threshold");
    assert!(result.result.records.is_empty());
}

#[test]
fn vector_exact_stream_preserves_snapshot_across_sealed_and_growing_segments() {
    use reddb::runtime::mvcc::{clear_current_connection_id, set_current_connection_id};
    struct ConnectionGuard;
    impl Drop for ConnectionGuard {
        fn drop(&mut self) {
            clear_current_connection_id();
        }
    }
    let _connection = ConnectionGuard;
    let rt = RedDBRuntime::with_options(RedDBOptions::in_memory()).expect("runtime");
    set_current_connection_id(99810);
    rt.execute_query("CREATE VECTOR stream_snapshot DIM 2 METRIC cosine")
        .expect("collection");
    rt.execute_query("INSERT INTO stream_snapshot VECTOR (dense,content) VALUES ([0,1],'base')")
        .expect("base vector");
    rt.db()
        .store()
        .get_collection("stream_snapshot")
        .expect("manager")
        .force_seal()
        .expect("seal base segment");
    rt.execute_query("BEGIN ISOLATION LEVEL SNAPSHOT")
        .expect("begin reader");
    let query = "VECTOR SEARCH stream_snapshot SIMILAR TO [1,0] MODE EXACT LIMIT 3";
    assert_eq!(
        rt.execute_query(query)
            .expect("initial read")
            .result
            .records
            .len(),
        1
    );

    set_current_connection_id(99811);
    rt.execute_query("BEGIN").expect("begin writer");
    rt.execute_query("INSERT INTO stream_snapshot VECTOR (dense,content) VALUES ([1,0],'later')")
        .expect("new vector in growing segment");
    rt.execute_query("COMMIT").expect("commit writer");

    set_current_connection_id(99810);
    // A distinct cache key exercises the scan under the original snapshot.
    let result = rt
        .execute_query("VECTOR SEARCH stream_snapshot SIMILAR TO [1,0] MODE EXACT LIMIT 4")
        .expect("old snapshot read");
    assert_eq!(result.result.records.len(), 1);
    assert_eq!(
        result.result.records[0].get("content"),
        Some(&Value::text("base"))
    );
    assert_eq!(
        result
            .result
            .stats
            .vector
            .expect("stats")
            .exact_distance_evaluations,
        1
    );
    rt.execute_query("COMMIT").expect("close reader");
    let result = rt.execute_query(query).expect("fresh snapshot");
    assert_eq!(result.result.records.len(), 2);
    assert_eq!(
        result.result.records[0].get("content"),
        Some(&Value::text("later"))
    );
    assert_eq!(
        result
            .result
            .stats
            .vector
            .expect("stats")
            .exact_distance_evaluations,
        2
    );
}
