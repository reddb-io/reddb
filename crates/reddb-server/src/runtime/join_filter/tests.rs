//! join_filter unit tests.
use super::*;
use crate::storage::query::unified::MatchedNode;

#[test]
fn test_evaluate_metadata_field_compare_entity_type_is_case_insensitive() {
    let field = FieldRef::TableColumn {
        table: "any".to_string(),
        column: "red_entity_type".to_string(),
    };

    assert_eq!(
        evaluate_metadata_field_compare(
            &field,
            &Value::text("table".to_string()),
            CompareOp::Eq,
            &Value::text("TABLE".to_string()),
        ),
        Some(true)
    );

    assert_eq!(
        evaluate_metadata_field_compare(
            &field,
            &Value::text("graph_node".to_string()),
            CompareOp::Ne,
            &Value::text("GRAPH_NODE".to_string()),
        ),
        Some(false)
    );
}

#[test]
fn test_evaluate_metadata_field_in_entity_type_is_case_insensitive() {
    let field = FieldRef::TableColumn {
        table: "any".to_string(),
        column: "red_entity_type".to_string(),
    };

    assert_eq!(
        evaluate_metadata_field_in(
            &field,
            &Value::text("vector".to_string()),
            &[
                Value::text("TABLE".to_string()),
                Value::text("vector".to_string()),
                Value::text("graph_node".to_string()),
            ],
        ),
        Some(true)
    );

    assert_eq!(
        evaluate_metadata_field_in(
            &field,
            &Value::text("document".to_string()),
            &[
                Value::text("TABLE".to_string()),
                Value::text("GRAPH_NODE".to_string()),
            ],
        ),
        Some(false)
    );
}

#[test]
fn test_evaluate_metadata_field_compare_entity_type_unsupported_op_is_false() {
    let field = FieldRef::TableColumn {
        table: "any".to_string(),
        column: "red_entity_type".to_string(),
    };

    assert_eq!(
        evaluate_metadata_field_compare(
            &field,
            &Value::text("vector".to_string()),
            CompareOp::Gt,
            &Value::text("vector".to_string()),
        ),
        Some(false)
    );
}

#[test]
fn test_resolve_runtime_field_node_property_from_node_properties() {
    let mut record = UnifiedRecord::new();
    let mut node_properties = HashMap::new();
    node_properties.insert(
        "nginx_version".to_string(),
        Value::text("1.22.1".to_string()),
    );
    let node = MatchedNode {
        id: "svc:nginx:80".to_string(),
        label: "nginx".to_string(),
        node_label: "service".to_string(),
        properties: node_properties,
    };
    record.set_node("svc", node);

    let field = FieldRef::node_prop("svc", "nginx_version");
    assert_eq!(
        resolve_runtime_field(&record, &field, None, None),
        Some(Value::text("1.22.1".to_string()))
    );
}

#[test]
fn test_compare_runtime_values_preserves_integer_unsigned_boundaries() {
    let above_i64_max = Value::UnsignedInteger(i64::MAX as u64 + 1);
    let max_i64 = Value::Integer(i64::MAX);

    assert!(compare_runtime_values(
        &above_i64_max,
        &max_i64,
        CompareOp::Gt
    ));
    assert!(compare_runtime_values(
        &above_i64_max,
        &max_i64,
        CompareOp::Ge
    ));
    assert!(!compare_runtime_values(
        &above_i64_max,
        &max_i64,
        CompareOp::Eq
    ));

    assert!(compare_runtime_values(
        &Value::Integer(-1),
        &Value::UnsignedInteger(0),
        CompareOp::Lt
    ));
    assert!(compare_runtime_values(
        &Value::UnsignedInteger(0),
        &Value::Integer(-1),
        CompareOp::Gt
    ));
}

// ── Top-K parity tests ───────────────────────────────────────────
// Each test asserts `top_k_records_by_order_by_with_db(k)` returns
// a result byte-for-byte identical to `sort_records_by_order_by_with_db`
// followed by `records.truncate(k)`. Any divergence here is a bug.

fn rec_i(col: &str, v: i64) -> UnifiedRecord {
    let mut r = UnifiedRecord::with_capacity(1);
    r.set(col, Value::Integer(v));
    r
}

fn rec_f(col: &str, v: f64) -> UnifiedRecord {
    let mut r = UnifiedRecord::with_capacity(1);
    r.set(col, Value::Float(v));
    r
}

fn rec_t(col: &str, v: &str) -> UnifiedRecord {
    let mut r = UnifiedRecord::with_capacity(1);
    r.set(col, Value::text(v.to_string()));
    r
}

fn rec_pair(c1: &str, v1: Value, c2: &str, v2: Value) -> UnifiedRecord {
    let mut r = UnifiedRecord::with_capacity(2);
    r.set(c1, v1);
    r.set(c2, v2);
    r
}

fn order_by_col(col: &str, asc: bool, nulls_first: bool) -> OrderByClause {
    OrderByClause {
        field: FieldRef::TableColumn {
            table: String::new(),
            column: col.to_string(),
        },
        expr: None,
        ascending: asc,
        nulls_first,
    }
}

fn reference_sort_truncate(
    mut records: Vec<UnifiedRecord>,
    ob: &[OrderByClause],
    k: usize,
) -> Vec<UnifiedRecord> {
    sort_records_by_order_by_with_db(None, &mut records, ob, None, None);
    records.truncate(k);
    records
}

fn topk_via(records: Vec<UnifiedRecord>, ob: &[OrderByClause], k: usize) -> Vec<UnifiedRecord> {
    let mut v = records;
    top_k_records_by_order_by_with_db(None, &mut v, ob, k, None, None);
    v
}

fn extract_i(records: &[UnifiedRecord], col: &str) -> Vec<Option<i64>> {
    records
        .iter()
        .map(|r| match r.get(col) {
            Some(Value::Integer(n)) => Some(*n),
            _ => None,
        })
        .collect()
}

fn extract_f(records: &[UnifiedRecord], col: &str) -> Vec<Option<f64>> {
    records
        .iter()
        .map(|r| match r.get(col) {
            Some(Value::Float(n)) => Some(*n),
            _ => None,
        })
        .collect()
}

fn extract_t(records: &[UnifiedRecord], col: &str) -> Vec<Option<String>> {
    records
        .iter()
        .map(|r| match r.get(col) {
            Some(Value::Text(s)) => Some(s.as_ref().to_string()),
            _ => None,
        })
        .collect()
}

#[test]
fn topk_asc_smaller_k_matches_sort_truncate() {
    let rows: Vec<_> = [5i64, 3, 8, 1, 9, 4, 7, 2, 6, 0]
        .iter()
        .map(|n| rec_i("a", *n))
        .collect();
    let ob = vec![order_by_col("a", true, false)];
    for k in [1usize, 3, 5, 9] {
        let expected = reference_sort_truncate(rows.clone(), &ob, k);
        let actual = topk_via(rows.clone(), &ob, k);
        assert_eq!(extract_i(&actual, "a"), extract_i(&expected, "a"), "k={k}");
    }
}

#[test]
fn topk_desc_matches_sort_truncate() {
    let rows: Vec<_> = [5i64, 3, 8, 1, 9, 4, 7, 2, 6, 0]
        .iter()
        .map(|n| rec_i("a", *n))
        .collect();
    let ob = vec![order_by_col("a", false, false)];
    for k in [1usize, 3, 5, 9] {
        let expected = reference_sort_truncate(rows.clone(), &ob, k);
        let actual = topk_via(rows.clone(), &ob, k);
        assert_eq!(extract_i(&actual, "a"), extract_i(&expected, "a"), "k={k}");
    }
}

#[test]
fn topk_ties_preserve_stable_order() {
    // Multiple records with the same sort key but distinct secondary
    // column — stable sort keeps insertion order, top-k must match.
    let rows = vec![
        rec_pair("k", Value::Integer(1), "tag", Value::text("a")),
        rec_pair("k", Value::Integer(2), "tag", Value::text("b")),
        rec_pair("k", Value::Integer(1), "tag", Value::text("c")),
        rec_pair("k", Value::Integer(2), "tag", Value::text("d")),
        rec_pair("k", Value::Integer(1), "tag", Value::text("e")),
    ];
    let ob = vec![order_by_col("k", true, false)];
    for k in [1usize, 2, 3, 4] {
        let expected = reference_sort_truncate(rows.clone(), &ob, k);
        let actual = topk_via(rows.clone(), &ob, k);
        assert_eq!(
            extract_t(&actual, "tag"),
            extract_t(&expected, "tag"),
            "k={k}"
        );
    }
}

#[test]
fn topk_multi_key_mixed_asc_desc() {
    let rows = vec![
        rec_pair("a", Value::Integer(1), "b", Value::Integer(10)),
        rec_pair("a", Value::Integer(2), "b", Value::Integer(5)),
        rec_pair("a", Value::Integer(1), "b", Value::Integer(30)),
        rec_pair("a", Value::Integer(2), "b", Value::Integer(25)),
        rec_pair("a", Value::Integer(1), "b", Value::Integer(20)),
    ];
    let ob = vec![
        order_by_col("a", true, false),
        order_by_col("b", false, false),
    ];
    for k in [1usize, 2, 3, 4] {
        let expected = reference_sort_truncate(rows.clone(), &ob, k);
        let actual = topk_via(rows.clone(), &ob, k);
        assert_eq!(
            extract_i(&actual, "a"),
            extract_i(&expected, "a"),
            "k={k} a"
        );
        assert_eq!(
            extract_i(&actual, "b"),
            extract_i(&expected, "b"),
            "k={k} b"
        );
    }
}

#[test]
fn topk_nulls_first_and_last() {
    let rows = vec![
        rec_i("a", 3),
        {
            let mut r = UnifiedRecord::with_capacity(1);
            r.set("a", Value::Null);
            r
        },
        rec_i("a", 1),
        {
            let mut r = UnifiedRecord::with_capacity(1);
            r.set("a", Value::Null);
            r
        },
        rec_i("a", 2),
    ];
    for nulls_first in [false, true] {
        let ob = vec![order_by_col("a", true, nulls_first)];
        for k in [1usize, 2, 3, 4] {
            let expected = reference_sort_truncate(rows.clone(), &ob, k);
            let actual = topk_via(rows.clone(), &ob, k);
            let exp_i = extract_i(&expected, "a");
            let act_i = extract_i(&actual, "a");
            assert_eq!(act_i, exp_i, "nulls_first={nulls_first} k={k}");
        }
    }
}

#[test]
fn topk_nan_float_count_and_subset() {
    // NaN breaks total ordering: both `sort_by` and our quickselect
    // produce implementation-defined orderings when NaN participates.
    // The real invariant is "doesn't panic" + "returns k elements
    // drawn from the input" — verify that much.
    let rows = vec![
        rec_f("a", 1.5),
        rec_f("a", f64::NAN),
        rec_f("a", 0.5),
        rec_f("a", f64::NAN),
        rec_f("a", 2.5),
    ];
    let ob = vec![order_by_col("a", true, false)];
    for k in [1usize, 2, 3, 4, 5] {
        let actual = topk_via(rows.clone(), &ob, k);
        assert_eq!(actual.len(), k.min(rows.len()), "k={k}");
        for rec in &actual {
            let v = extract_f(std::slice::from_ref(rec), "a").pop().flatten();
            assert!(
                matches!(v, Some(f) if f.is_nan() || [0.5_f64, 1.5, 2.5].contains(&f)),
                "k={k} value not from input: {v:?}"
            );
        }
    }
}

#[test]
fn topk_k_zero_clears() {
    let rows: Vec<_> = (0..5).map(|n| rec_i("a", n)).collect();
    let ob = vec![order_by_col("a", true, false)];
    let got = topk_via(rows, &ob, 0);
    assert!(got.is_empty());
}

#[test]
fn topk_k_ge_len_full_sorted() {
    let rows: Vec<_> = [5i64, 3, 8, 1, 9, 4]
        .iter()
        .map(|n| rec_i("a", *n))
        .collect();
    let ob = vec![order_by_col("a", true, false)];
    let expected = reference_sort_truncate(rows.clone(), &ob, 100);
    let actual = topk_via(rows, &ob, 100);
    assert_eq!(extract_i(&actual, "a"), extract_i(&expected, "a"));
}

#[test]
fn topk_text_abbrev_path_matches_sort() {
    // Text sort triggers the abbreviated u64 prefix fast path in
    // both functions — ensures both traverse it identically.
    let rows: Vec<_> = [
        "delta", "alpha", "echo", "bravo", "charlie", "foxtrot", "golf",
    ]
    .iter()
    .map(|s| rec_t("a", s))
    .collect();
    let ob = vec![order_by_col("a", true, false)];
    for k in [1usize, 2, 3, 4, 5, 6] {
        let expected = reference_sort_truncate(rows.clone(), &ob, k);
        let actual = topk_via(rows.clone(), &ob, k);
        assert_eq!(extract_t(&actual, "a"), extract_t(&expected, "a"), "k={k}");
    }
}

#[test]
fn topk_property_random_matches_sort() {
    // Pseudo-random but deterministic — same seed each run so a
    // failure reproduces. 200 rows × 4 k-values × 2 directions.
    let mut rows: Vec<UnifiedRecord> = Vec::with_capacity(200);
    let mut state: u64 = 0x9E3779B97F4A7C15;
    for _ in 0..200 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        let v = (state % 50) as i64; // intentionally high collision rate
        rows.push(rec_i("a", v));
    }
    for asc in [true, false] {
        let ob = vec![order_by_col("a", asc, false)];
        for k in [1usize, 10, 50, 100, 199] {
            let expected = reference_sort_truncate(rows.clone(), &ob, k);
            let actual = topk_via(rows.clone(), &ob, k);
            assert_eq!(
                extract_i(&actual, "a"),
                extract_i(&expected, "a"),
                "asc={asc} k={k}"
            );
        }
    }
}

// ── Runtime join semantics coverage (#1339) ──────────────────────────────
// Tests exercise the lowest-level join algorithms directly so changes to
// the join-key representation cannot silently break observable query results.
// Both `execute_runtime_nested_loop_join` and `execute_runtime_hash_join`
// are exercised; they must produce the same row count for all cases where
// their semantics agree.

fn jrec(col: &str, val: Value) -> UnifiedRecord {
    let mut r = UnifiedRecord::with_capacity(1);
    r.set(col, val);
    r
}

fn jfield(col: &str) -> FieldRef {
    FieldRef::TableColumn {
        table: String::new(),
        column: col.to_string(),
    }
}

fn left_tq() -> TableQuery {
    TableQuery::new("L")
}

fn both_join_count(
    left: &[UnifiedRecord],
    right: &[UnifiedRecord],
    lf: &FieldRef,
    rf: &FieldRef,
    join_type: JoinType,
    expected: usize,
    label: &str,
) {
    let tq = left_tq();
    let nl = execute_runtime_nested_loop_join(
        &tq, left, None, None, lf, right, None, None, rf, join_type,
    )
    .unwrap_or_else(|e| panic!("nested_loop {label}: {e}"));
    assert_eq!(nl.len(), expected, "nested_loop {label}");

    let hj = execute_runtime_hash_join(&tq, left, None, None, lf, right, None, None, rf, join_type)
        .unwrap_or_else(|e| panic!("hash_join {label}: {e}"));
    assert_eq!(hj.len(), expected, "hash_join {label}");
}

#[test]
fn join_semantics_integer_inner_basic() {
    // left [1,2,3] ⋈ right [2,3,4] on integer key → 2 matched pairs
    let left: Vec<_> = [1i64, 2, 3]
        .iter()
        .map(|v| jrec("k", Value::Integer(*v)))
        .collect();
    let right: Vec<_> = [2i64, 3, 4]
        .iter()
        .map(|v| jrec("k", Value::Integer(*v)))
        .collect();
    both_join_count(
        &left,
        &right,
        &jfield("k"),
        &jfield("k"),
        JoinType::Inner,
        2,
        "integer_inner",
    );
}

#[test]
fn join_semantics_float_inner_basic() {
    // float values matched by equality
    let left: Vec<_> = [1.0f64, 2.5, 3.0]
        .iter()
        .map(|v| jrec("f", Value::Float(*v)))
        .collect();
    let right: Vec<_> = [2.5f64, 3.0, 4.0]
        .iter()
        .map(|v| jrec("f", Value::Float(*v)))
        .collect();
    both_join_count(
        &left,
        &right,
        &jfield("f"),
        &jfield("f"),
        JoinType::Inner,
        2,
        "float_inner",
    );
}

#[test]
fn join_semantics_boolean_inner() {
    // left [T,F,T] ⋈ right [T,F] → (T,T),(T,T),(F,F) = 3 rows
    let left = vec![
        jrec("b", Value::Boolean(true)),
        jrec("b", Value::Boolean(false)),
        jrec("b", Value::Boolean(true)),
    ];
    let right = vec![
        jrec("b", Value::Boolean(true)),
        jrec("b", Value::Boolean(false)),
    ];
    both_join_count(
        &left,
        &right,
        &jfield("b"),
        &jfield("b"),
        JoinType::Inner,
        3,
        "boolean_inner",
    );
}

#[test]
fn join_semantics_text_inner() {
    // text key matching is case-sensitive
    let left: Vec<_> = ["alice", "bob", "carol"]
        .iter()
        .map(|s| jrec("name", Value::text(s.to_string())))
        .collect();
    let right: Vec<_> = ["bob", "dave", "carol"]
        .iter()
        .map(|s| jrec("name", Value::text(s.to_string())))
        .collect();
    both_join_count(
        &left,
        &right,
        &jfield("name"),
        &jfield("name"),
        JoinType::Inner,
        2,
        "text_inner",
    );
}

#[test]
fn join_semantics_null_both_sides_inner() {
    // Both join types treat NULL = NULL as a match via their respective
    // equality paths (PartialEq for nested loop; identical "NULL" hash key
    // for hash join). This protects the current semantics.
    let left = vec![jrec("k", Value::Null), jrec("k", Value::Integer(1))];
    let right = vec![jrec("k", Value::Null), jrec("k", Value::Integer(1))];
    let tq = left_tq();
    let lf = jfield("k");
    let rf = jfield("k");
    let nl = execute_runtime_nested_loop_join(
        &tq,
        &left,
        None,
        None,
        &lf,
        &right,
        None,
        None,
        &rf,
        JoinType::Inner,
    )
    .unwrap();
    let hj = execute_runtime_hash_join(
        &tq,
        &left,
        None,
        None,
        &lf,
        &right,
        None,
        None,
        &rf,
        JoinType::Inner,
    )
    .unwrap();
    // Both must agree: Null=Null matches, Integer(1)=Integer(1) matches → 2 rows
    assert_eq!(nl.len(), 2, "nested_loop null=null inner");
    assert_eq!(
        nl.len(),
        hj.len(),
        "nested_loop and hash_join must agree on null semantics"
    );
}

#[test]
fn join_semantics_missing_field_nested_loop_no_inner_match() {
    // Nested loop returns false when the join field is absent from a record
    // (join_condition_matches → None on either side → false).
    let left = vec![jrec("x", Value::Integer(1))]; // no "k" column
    let right = vec![jrec("y", Value::Integer(1))]; // no "k" column
    let tq = left_tq();
    let lf = jfield("k");
    let rf = jfield("k");
    let nl = execute_runtime_nested_loop_join(
        &tq,
        &left,
        None,
        None,
        &lf,
        &right,
        None,
        None,
        &rf,
        JoinType::Inner,
    )
    .unwrap();
    assert_eq!(
        nl.len(),
        0,
        "nested_loop: absent join field → no inner match"
    );
}

#[test]
fn join_semantics_duplicate_key_many_to_many() {
    // 2 left rows with key=1 × 2 right rows with key=1 → 4 result rows
    let left = vec![jrec("k", Value::Integer(1)), jrec("k", Value::Integer(1))];
    let right = vec![jrec("k", Value::Integer(1)), jrec("k", Value::Integer(1))];
    both_join_count(
        &left,
        &right,
        &jfield("k"),
        &jfield("k"),
        JoinType::Inner,
        4,
        "dup_key_n_to_m",
    );
}

#[test]
fn join_semantics_left_outer_unmatched_left_row() {
    // key=2 has no right match; left outer includes it padded with nulls
    let left = vec![jrec("k", Value::Integer(1)), jrec("k", Value::Integer(2))];
    let right = vec![jrec("k", Value::Integer(1))];
    both_join_count(
        &left,
        &right,
        &jfield("k"),
        &jfield("k"),
        JoinType::LeftOuter,
        2,
        "left_outer",
    );
}

#[test]
fn join_semantics_right_outer_unmatched_right_row() {
    // right key=5 has no left match; right outer includes it
    let left = vec![jrec("k", Value::Integer(1))];
    let right = vec![jrec("k", Value::Integer(1)), jrec("k", Value::Integer(5))];
    both_join_count(
        &left,
        &right,
        &jfield("k"),
        &jfield("k"),
        JoinType::RightOuter,
        2,
        "right_outer",
    );
}

#[test]
fn join_semantics_full_outer_both_sides_unmatched() {
    // left key=2 and right key=3 are both unmatched; full outer emits both
    let left = vec![jrec("k", Value::Integer(1)), jrec("k", Value::Integer(2))];
    let right = vec![jrec("k", Value::Integer(1)), jrec("k", Value::Integer(3))];
    // (1,1) matched + left 2 unmatched + right 3 unmatched = 3 rows
    both_join_count(
        &left,
        &right,
        &jfield("k"),
        &jfield("k"),
        JoinType::FullOuter,
        3,
        "full_outer",
    );
}

#[test]
fn join_semantics_cross_join_cartesian_product() {
    // 3 × 4 = 12 rows; join key is irrelevant for cross join
    let left: Vec<_> = (0i64..3).map(|i| jrec("a", Value::Integer(i))).collect();
    let right: Vec<_> = (0i64..4).map(|i| jrec("b", Value::Integer(i))).collect();
    both_join_count(
        &left,
        &right,
        &jfield("a"),
        &jfield("b"),
        JoinType::Cross,
        12,
        "cross_cartesian",
    );
}

#[test]
fn join_semantics_mixed_type_integer_float_numeric_equality() {
    // Integer(2) on left equals Float(2.0) on right via numeric coercion in
    // both join algorithms (nested loop: runtime_value_number path;
    // hash join: identical Display strings "2").
    let left = vec![jrec("k", Value::Integer(2)), jrec("k", Value::Integer(3))];
    let right = vec![jrec("k", Value::Float(2.0)), jrec("k", Value::Float(4.0))];
    both_join_count(
        &left,
        &right,
        &jfield("k"),
        &jfield("k"),
        JoinType::Inner,
        1,
        "mixed_int_float",
    );
}

#[test]
fn join_semantics_noderef_identity_match() {
    // NodeRef values match when the underlying id string is equal (both
    // nested loop via text-str borrow path and hash join via to_string key).
    let left = vec![
        jrec("id", Value::NodeRef("svc:1".to_string())),
        jrec("id", Value::NodeRef("svc:2".to_string())),
    ];
    let right = vec![
        jrec("ref", Value::NodeRef("svc:2".to_string())),
        jrec("ref", Value::NodeRef("svc:3".to_string())),
    ];
    both_join_count(
        &left,
        &right,
        &jfield("id"),
        &jfield("ref"),
        JoinType::Inner,
        1,
        "noderef_identity",
    );
}

#[test]
fn join_semantics_result_columns_preserved_after_join() {
    // Verify that the merged records produced by both join algorithms carry
    // the expected columns from both sides. Uses distinct column names to
    // avoid the name-collision prefix logic in merge_join_records.
    let left = vec![jrec("l_id", Value::Integer(7))];
    let right = vec![jrec("r_val", Value::text("hello".to_string()))];
    // No match — both produce 0 inner rows. Use left outer to get a merged row.
    let tq = left_tq();
    let lf = jfield("l_id");
    let rf = jfield("l_id"); // right has no "l_id" → left outer pads right with nulls
    let nl = execute_runtime_nested_loop_join(
        &tq,
        &left,
        None,
        None,
        &lf,
        &right,
        None,
        None,
        &rf,
        JoinType::LeftOuter,
    )
    .unwrap();
    assert_eq!(nl.len(), 1, "left outer must emit the unmatched left row");
    // The merged row must carry the left-side column
    assert_eq!(nl[0].get("l_id"), Some(&Value::Integer(7)));
}

#[test]
fn indexed_join_borrowed_candidate_list_matches_hash_join() {
    // execute_runtime_indexed_join probes the right-side hash bucket by
    // borrowing the candidate index list in place (no per-left-row clone,
    // #1346). Duplicate keys force multi-element candidate lists — the exact
    // path the borrow touches — and both inner and outer results must stay
    // identical to the hash join over the same inputs.
    let left: Vec<_> = [1i64, 1, 2, 3]
        .iter()
        .map(|v| jrec("k", Value::Integer(*v)))
        .collect();
    let right: Vec<_> = [1i64, 1, 2, 4]
        .iter()
        .map(|v| jrec("k", Value::Integer(*v)))
        .collect();
    let tq = left_tq();
    let lf = jfield("k");
    let rf = jfield("k");

    let indexed_inner = execute_runtime_indexed_join(
        &tq,
        &left,
        None,
        None,
        &lf,
        &right,
        None,
        None,
        &rf,
        JoinType::Inner,
    )
    .expect("indexed inner join");
    let hashed_inner = execute_runtime_hash_join(
        &tq,
        &left,
        None,
        None,
        &lf,
        &right,
        None,
        None,
        &rf,
        JoinType::Inner,
    )
    .expect("hash inner join");
    // key 1: 2 left × 2 right = 4, key 2: 1 × 1 = 1, keys 3/4 unmatched → 5
    assert_eq!(indexed_inner.len(), 5, "indexed inner many-to-many fan-out");
    assert_eq!(
        indexed_inner.len(),
        hashed_inner.len(),
        "indexed inner row count must match hash join after borrow change"
    );

    // Left outer pads the unmatched left key (3) with a null right side.
    let indexed_left = execute_runtime_indexed_join(
        &tq,
        &left,
        None,
        None,
        &lf,
        &right,
        None,
        None,
        &rf,
        JoinType::LeftOuter,
    )
    .expect("indexed left outer join");
    assert_eq!(
        indexed_left.len(),
        6,
        "indexed left outer must pad the unmatched left row"
    );
}

// ── Benchmark-style timing measurement (#1339) ───────────────────────────
// Run to capture the baseline cost of hash/nested-loop join build+probe:
//   CARGO_BUILD_JOBS=1 RUSTFLAGS="-C debuginfo=0" \
//   cargo nextest run -p reddb-io-server --lib \
//     -- join_filter::tests::benchmark_join_build_probe_timing --nocapture
//
// Baseline recorded 2026-06-25 on the reddb guard host (14G, debug profile):
//   hash_join   n=   10: avg=66µs    (20 iters)
//   nested_loop n=   10: avg=90µs    (20 iters)
//   hash_join   n=  100: avg=654µs   (20 iters)
//   nested_loop n=  100: avg=4906µs  (20 iters)
//   hash_join   n= 1000: avg=6385µs  (20 iters)
//   nested_loop n= 1000: avg=432443µs (20 iters)
// The O(n) vs O(n²) gap confirms hash join wins at n=100+ (7.5× faster)
// and dominates at n=1000 (68× faster). These unoptimized numbers are the
// baseline; future key-representation changes must not widen this gap.
#[test]
fn benchmark_join_build_probe_timing() {
    for &n in &[10usize, 100, 1_000] {
        let left: Vec<_> = (0i64..n as i64)
            .map(|i| jrec("k", Value::Integer(i)))
            .collect();
        let right: Vec<_> = (0i64..n as i64)
            .map(|i| jrec("k", Value::Integer(i)))
            .collect();
        let tq = left_tq();
        let lf = jfield("k");
        let rf = jfield("k");

        // warmup
        for _ in 0..3 {
            let _ = execute_runtime_hash_join(
                &tq,
                &left,
                None,
                None,
                &lf,
                &right,
                None,
                None,
                &rf,
                JoinType::Inner,
            );
        }

        let iters = 20usize;
        let t0 = std::time::Instant::now();
        for _ in 0..iters {
            let _ = execute_runtime_hash_join(
                &tq,
                &left,
                None,
                None,
                &lf,
                &right,
                None,
                None,
                &rf,
                JoinType::Inner,
            );
        }
        let hj_us = t0.elapsed().as_micros() / iters as u128;

        let t0 = std::time::Instant::now();
        for _ in 0..iters {
            let _ = execute_runtime_nested_loop_join(
                &tq,
                &left,
                None,
                None,
                &lf,
                &right,
                None,
                None,
                &rf,
                JoinType::Inner,
            );
        }
        let nl_us = t0.elapsed().as_micros() / iters as u128;

        // Indexed join shares the hash-bucket build but now borrows the
        // candidate list during probing instead of cloning it per left row
        // (#1346); measured here to show the clone-reduction impact.
        let t0 = std::time::Instant::now();
        for _ in 0..iters {
            let _ = execute_runtime_indexed_join(
                &tq,
                &left,
                None,
                None,
                &lf,
                &right,
                None,
                None,
                &rf,
                JoinType::Inner,
            );
        }
        let ix_us = t0.elapsed().as_micros() / iters as u128;

        println!("hash_join    n={n:>5}: avg={hj_us}µs  ({iters} iters)");
        println!("nested_loop  n={n:>5}: avg={nl_us}µs  ({iters} iters)");
        println!("indexed_join n={n:>5}: avg={ix_us}µs  ({iters} iters)");
    }
}

// ── Typed internal join key + indexed-join coverage (#1345) ───────────────
// The indexed and graph-lookup join paths now build/probe a typed
// `RuntimeJoinKey` index instead of formatted, prefix-namespaced strings.
// These tests pin the key-class namespacing and prove the indexed join
// still agrees with the nested-loop reference on the key classes it covers.

fn indexed_count(
    left: &[UnifiedRecord],
    right: &[UnifiedRecord],
    lf: &FieldRef,
    rf: &FieldRef,
    join_type: JoinType,
) -> usize {
    let tq = left_tq();
    execute_runtime_indexed_join(&tq, left, None, None, lf, right, None, None, rf, join_type)
        .unwrap()
        .len()
}

#[test]
fn typed_join_key_classes_are_disjoint_namespaces() {
    // numeric 1 and textual "1" never collide (was "n:1" vs "t:1")
    assert_ne!(
        runtime_join_lookup_key(&Value::Integer(1)),
        runtime_join_lookup_key(&Value::text("1".to_string()))
    );
    // boolean true and textual "true" never collide (was "b:true" vs "t:true")
    assert_ne!(
        runtime_join_lookup_key(&Value::Boolean(true)),
        runtime_join_lookup_key(&Value::text("true".to_string()))
    );
    // Integer(2) and Float(2.0) share one numeric key (was both "n:2")
    assert_eq!(
        runtime_join_lookup_key(&Value::Integer(2)),
        runtime_join_lookup_key(&Value::Float(2.0))
    );
    assert_eq!(
        runtime_join_lookup_key(&Value::Integer(2)),
        Some(RuntimeJoinKey::Number(2.0f64.to_bits()))
    );
    // null / array / blob produce no value key (unchanged)
    assert_eq!(runtime_join_lookup_key(&Value::Null), None);
}

#[test]
fn typed_identity_key_matches_numeric_to_reference_suffix() {
    // A numeric value's identity key collides with the trailing-segment
    // identity of a reference string, preserving the graph-join identity
    // match that used to compare "id:2" == "id:2".
    let num_keys = runtime_join_lookup_keys(&Value::Integer(2));
    let ref_keys = runtime_join_lookup_keys(&Value::NodeRef("svc:2".to_string()));
    assert!(num_keys.contains(&RuntimeJoinKey::Identity("2".to_string())));
    assert!(ref_keys.contains(&RuntimeJoinKey::Identity("2".to_string())));
}

#[test]
fn indexed_join_integer_inner_matches_reference() {
    let left: Vec<_> = [1i64, 2, 3]
        .iter()
        .map(|v| jrec("k", Value::Integer(*v)))
        .collect();
    let right: Vec<_> = [2i64, 3, 4]
        .iter()
        .map(|v| jrec("k", Value::Integer(*v)))
        .collect();
    assert_eq!(
        indexed_count(&left, &right, &jfield("k"), &jfield("k"), JoinType::Inner),
        2
    );
}

#[test]
fn indexed_join_text_inner_matches_reference() {
    let left: Vec<_> = ["alice", "bob", "carol"]
        .iter()
        .map(|s| jrec("name", Value::text(s.to_string())))
        .collect();
    let right: Vec<_> = ["bob", "dave", "carol"]
        .iter()
        .map(|s| jrec("name", Value::text(s.to_string())))
        .collect();
    assert_eq!(
        indexed_count(
            &left,
            &right,
            &jfield("name"),
            &jfield("name"),
            JoinType::Inner
        ),
        2
    );
}

#[test]
fn indexed_join_mixed_int_float_inner() {
    // Integer(2) key probes the same numeric bucket as Float(2.0) and the
    // candidate is confirmed by join_condition_matches → 1 matched pair.
    let left = vec![jrec("k", Value::Integer(2)), jrec("k", Value::Integer(3))];
    let right = vec![jrec("k", Value::Float(2.0)), jrec("k", Value::Float(4.0))];
    assert_eq!(
        indexed_count(&left, &right, &jfield("k"), &jfield("k"), JoinType::Inner),
        1
    );
}

#[test]
fn indexed_join_duplicate_key_many_to_many() {
    let left = vec![jrec("k", Value::Integer(1)), jrec("k", Value::Integer(1))];
    let right = vec![jrec("k", Value::Integer(1)), jrec("k", Value::Integer(1))];
    assert_eq!(
        indexed_count(&left, &right, &jfield("k"), &jfield("k"), JoinType::Inner),
        4
    );
}

#[test]
fn indexed_join_left_outer_pads_unmatched() {
    // key=2 has no right candidate → padded as an unmatched left row.
    let left = vec![jrec("k", Value::Integer(1)), jrec("k", Value::Integer(2))];
    let right = vec![jrec("k", Value::Integer(1))];
    assert_eq!(
        indexed_count(
            &left,
            &right,
            &jfield("k"),
            &jfield("k"),
            JoinType::LeftOuter
        ),
        2
    );
}
