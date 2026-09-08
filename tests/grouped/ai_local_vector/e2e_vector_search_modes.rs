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
            let filter = if filtered {
                "WHERE metadata.eligible=true"
            } else {
                ""
            };
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
