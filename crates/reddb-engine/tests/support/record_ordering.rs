//! Native runtime ORDER BY semantics exercised unchanged in native and WASM.
use reddb_engine::catalog::CollectionContract;
use reddb_engine::query::{
    runtime::{
        expression::{evaluate_runtime_expr_with_host, RuntimeExpressionHost},
        field_resolution::resolve_runtime_field,
        ordering::{
            compare_runtime_optional_values, compare_runtime_order_with_host,
            sort_or_top_k_records_with_host, sort_records_by_order_by_with_host,
            top_k_records_by_order_by_with_host, try_compare_runtime_order_with_host,
            try_sort_or_top_k_records_with_host, try_sort_records_by_order_by_with_host,
            try_top_k_records_by_order_by_with_host, FallibleOrderingHost, OrderingHost,
        },
        value_functions::{evaluate_value_function_with_host, ValueFunctionHost},
    },
    unified::UnifiedRecord,
};
use reddb_rql::ast::{Expr, FieldRef, OrderByClause, TableQuery};
use reddb_types::Value;
use std::{cell::Cell, cmp::Ordering};

pub const CASES: &[(&str, fn())] = &[
    ("record_order_exact_integer_keys", exact_integer_keys),
    ("record_order_nullable_keys", nullable_keys),
    (
        "record_order_text_prefixes_and_stable_ties",
        text_prefixes_and_stable_ties,
    ),
    ("record_order_expression_keys_once", expression_keys_once),
    (
        "record_order_topk_matches_stable_sort",
        topk_matches_stable_sort,
    ),
    ("record_order_limit_offset_width", limit_offset_width),
    (
        "record_order_identity_and_secret_callbacks",
        identity_and_secret_callbacks,
    ),
    (
        "record_order_unordered_and_nan_edges",
        unordered_and_nan_edges,
    ),
    (
        "record_order_fallible_keys_and_retry",
        fallible_keys_and_retry,
    ),
];

struct Host<'a> {
    query: &'a TableQuery,
    evaluations: Cell<usize>,
    identities: Cell<usize>,
    topk_disabled: bool,
    failure_at: Cell<Option<usize>>,
}

impl<'a> Host<'a> {
    fn new(query: &'a TableQuery, _contract: Option<&'a CollectionContract>) -> Self {
        Self {
            query,
            evaluations: Cell::new(0),
            identities: Cell::new(0),
            topk_disabled: false,
            failure_at: Cell::new(None),
        }
    }
}

impl OrderingHost for Host<'_> {
    fn evaluate_expression(&self, expression: &Expr, record: &UnifiedRecord) -> Option<Value> {
        self.evaluations.set(self.evaluations.get() + 1);
        evaluate_runtime_expr_with_host(
            &RecordExpressionHost {
                query: self.query,
                record,
            },
            expression,
        )
    }
    fn secret_query_input(&self, value: Value) -> Option<Value> {
        super::record_filters::plaintext(value)
    }
    fn record_identity_key(&self, record: &UnifiedRecord) -> String {
        self.identities.set(self.identities.get() + 1);
        // A supplied test identity; native identity construction stays in its host.
        record
            .get("rid")
            .expect("fixture identity")
            .display_string()
    }
    fn topk_disabled(&self) -> bool {
        self.topk_disabled
    }
}

impl FallibleOrderingHost for Host<'_> {
    type Error = &'static str;
    fn try_resolve_field(
        &self,
        record: &UnifiedRecord,
        field: &FieldRef,
        table_name: Option<&str>,
        table_alias: Option<&str>,
    ) -> Result<Option<Value>, Self::Error> {
        self.evaluations.set(self.evaluations.get() + 1);
        if self.failure_at.get() == Some(self.evaluations.get()) {
            return Err("sort key read failed");
        }
        Ok(resolve_runtime_field(
            record,
            field,
            table_name,
            table_alias,
        ))
    }
    fn try_evaluate_expression(
        &self,
        expression: &Expr,
        record: &UnifiedRecord,
    ) -> Result<Option<Value>, Self::Error> {
        if self.failure_at.get() == Some(self.evaluations.get() + 1) {
            self.evaluations.set(self.evaluations.get() + 1);
            return Err("sort key read failed");
        }
        Ok(self.evaluate_expression(expression, record))
    }
}

fn row(id: u64, value: Value) -> UnifiedRecord {
    let mut row = UnifiedRecord::new();
    row.set("rid", Value::UnsignedInteger(id));
    row.set("quantity", value);
    row
}

fn ids(records: &[UnifiedRecord]) -> Vec<u64> {
    records
        .iter()
        .map(|record| match record.get("rid") {
            Some(Value::UnsignedInteger(id)) => *id,
            _ => panic!("fixture identity"),
        })
        .collect()
}

fn query(suffix: &str) -> TableQuery {
    super::record_filters::table_query(&format!("SELECT * FROM orders AS o {suffix}"))
}

pub fn sort_records(
    records: &mut Vec<UnifiedRecord>,
    query: &TableQuery,
    contract: Option<&CollectionContract>,
) {
    let host = Host::new(query, contract);
    sort_records_by_order_by_with_host(
        &host,
        records,
        &query.order_by,
        Some(&query.table),
        query.alias.as_deref(),
    );
}

/// Test-only composition over supplied/admitted rows, not a public SQL dispatcher.
pub fn ordered_records(
    records: &[UnifiedRecord],
    query: &TableQuery,
    contract: Option<&CollectionContract>,
    disable_topk: bool,
) -> Vec<UnifiedRecord> {
    let host = Host {
        topk_disabled: disable_topk,
        ..Host::new(query, contract)
    };
    let mut records: Vec<_> = records
        .iter()
        .filter(|record| {
            super::record_filters::matches_where(record, query, contract).expect("WHERE verdict")
        })
        .cloned()
        .collect();
    sort_or_top_k_records_with_host(
        &host,
        &mut records,
        &query.order_by,
        query.offset,
        query.limit,
        Some(&query.table),
        query.alias.as_deref(),
    );
    let offset = usize::try_from(query.offset.unwrap_or(0)).unwrap_or(usize::MAX);
    let limit = usize::try_from(query.limit.unwrap_or(u64::MAX)).unwrap_or(usize::MAX);
    records.into_iter().skip(offset).take(limit).collect()
}

fn exact_integer_keys() {
    let rows = vec![
        row(0, Value::UnsignedInteger(9_007_199_254_740_993)),
        row(1, Value::Integer(-1)),
        row(2, Value::UnsignedInteger(u64::MAX)),
        row(3, Value::Integer(9_007_199_254_740_992)),
        row(4, Value::UnsignedInteger(0)),
    ];
    assert_eq!(
        ids(&ordered_records(
            &rows,
            &query("ORDER BY o.quantity ASC"),
            None,
            false
        )),
        [1, 4, 3, 0, 2]
    );
    assert_eq!(
        ids(&ordered_records(
            &rows,
            &query("ORDER BY o.quantity DESC LIMIT 2"),
            None,
            false
        )),
        [2, 0]
    );
}

fn nullable_keys() {
    let mut missing = UnifiedRecord::new();
    missing.set("rid", Value::UnsignedInteger(1));
    let rows = vec![
        row(0, Value::Integer(2)),
        missing,
        row(2, Value::Null),
        row(3, Value::Integer(1)),
    ];
    for (suffix, expected) in [
        ("ASC NULLS FIRST", vec![1, 2, 3, 0]),
        ("ASC NULLS LAST", vec![3, 0, 2, 1]),
        // Preserve the native direction reversal after optional-key comparison.
        ("DESC NULLS FIRST", vec![0, 3, 2, 1]),
        ("DESC NULLS LAST", vec![1, 2, 0, 3]),
    ] {
        assert_eq!(
            ids(&ordered_records(
                &rows,
                &query(&format!("ORDER BY o.quantity {suffix}")),
                None,
                false
            )),
            expected
        );
    }
    assert_eq!(
        compare_runtime_optional_values(
            &super::record_filters::plaintext,
            None,
            Some(&Value::Null),
            true
        ),
        Ordering::Less
    );
}

fn text_prefixes_and_stable_ties() {
    let rows = [
        "abcdefghz",
        "é",
        "abcdefgha",
        "abcdefgha",
        "",
        "alpha",
        "abcdefgh",
    ]
    .into_iter()
    .enumerate()
    .map(|(id, text)| row(id as u64, Value::text(text)))
    .collect::<Vec<_>>();
    assert_eq!(
        ids(&ordered_records(
            &rows,
            &query("ORDER BY quantity ASC"),
            None,
            false
        )),
        [4, 6, 2, 3, 0, 5, 1]
    );
    let mut selected = rows.clone();
    let query = query("ORDER BY quantity ASC");
    let host = Host::new(&query, None);
    top_k_records_by_order_by_with_host(
        &host,
        &mut selected,
        &query.order_by,
        4,
        Some("orders"),
        Some("o"),
    );
    assert_eq!(ids(&selected), [4, 6, 2, 3]);
    assert_eq!(
        host.identities.get(),
        0,
        "stable ties use input index, not identity"
    );
}

fn expression_keys_once() {
    let rows = (0..20)
        .rev()
        .map(|id| row(id, Value::Integer(id as i64)))
        .collect::<Vec<_>>();
    let query = query("ORDER BY o.quantity * 2 DESC, o.quantity ASC");
    let host = Host::new(&query, None);
    let mut sorted = rows.clone();
    sort_records_by_order_by_with_host(
        &host,
        &mut sorted,
        &query.order_by,
        Some("orders"),
        Some("o"),
    );
    assert_eq!(ids(&sorted), (0..20).rev().collect::<Vec<_>>());
    let expression_count = query
        .order_by
        .iter()
        .filter(|clause| clause.expr.is_some())
        .count();
    assert!(expression_count > 0);
    assert_eq!(host.evaluations.get(), rows.len() * expression_count);
    host.evaluations.set(0);
    let mut selected = rows;
    top_k_records_by_order_by_with_host(
        &host,
        &mut selected,
        &query.order_by,
        3,
        Some("orders"),
        Some("o"),
    );
    assert_eq!(ids(&selected), [19, 18, 17]);
    assert_eq!(host.evaluations.get(), 20 * expression_count);
}

fn topk_matches_stable_sort() {
    let mut state = 0x9E3779B97F4A7C15u64;
    let rows = (0..80)
        .map(|id| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            row(id, Value::Integer((state % 11) as i64))
        })
        .collect::<Vec<_>>();
    for suffix in ["ORDER BY quantity ASC", "ORDER BY quantity DESC, rid ASC"] {
        let query = query(suffix);
        let host = Host::new(&query, None);
        let mut sorted = rows.clone();
        sort_records_by_order_by_with_host(
            &host,
            &mut sorted,
            &query.order_by,
            Some("orders"),
            Some("o"),
        );
        for count in [0, 1, 3, 20, 79, 80, 100] {
            let mut selected = rows.clone();
            top_k_records_by_order_by_with_host(
                &host,
                &mut selected,
                &query.order_by,
                count,
                Some("orders"),
                Some("o"),
            );
            assert_eq!(ids(&selected), ids(&sorted[..count.min(sorted.len())]));
        }
    }
    for disabled in [false, true] {
        assert_eq!(
            ids(&ordered_records(
                &rows,
                &query("ORDER BY quantity ASC, rid ASC LIMIT 3 OFFSET 2"),
                None,
                disabled
            )),
            {
                let sorted =
                    ordered_records(&rows, &query("ORDER BY quantity ASC, rid ASC"), None, true);
                ids(&sorted[2..5])
            }
        );
    }
}

fn limit_offset_width() {
    let rows = (0..10)
        .rev()
        .map(|id| row(id, Value::UnsignedInteger(id)))
        .collect::<Vec<_>>();
    let query = query("ORDER BY quantity ASC");
    let host = Host::new(&query, None);
    for (offset, limit) in [
        (None, None),
        (None, Some(u64::MAX)),
        (Some(1u64 << 32), Some(1)),
        (Some(u64::MAX), Some(u64::MAX)),
    ] {
        let mut sorted = rows.clone();
        sort_or_top_k_records_with_host(
            &host,
            &mut sorted,
            &query.order_by,
            offset,
            limit,
            Some("orders"),
            Some("o"),
        );
        assert_eq!(
            ids(&sorted),
            (0..10).collect::<Vec<_>>(),
            "wide bounds must not truncate to a small top-K on wasm32"
        );
    }
    assert!(ordered_records(
        &rows,
        &self::query("ORDER BY quantity LIMIT 0"),
        None,
        false
    )
    .is_empty());
    assert!(ordered_records(
        &rows,
        &self::query("ORDER BY quantity LIMIT 1 OFFSET 4294967296"),
        None,
        false
    )
    .is_empty());
}

fn identity_and_secret_callbacks() {
    let query = query("ORDER BY quantity ASC");
    let host = Host::new(&query, None);
    let rows = [row(2, Value::Integer(1)), row(1, Value::Integer(1))];
    let clause = OrderByClause::asc(FieldRef::TableColumn {
        table: String::new(),
        column: "quantity".into(),
    });
    assert_eq!(
        compare_runtime_order_with_host(
            &host,
            &rows[0],
            &rows[1],
            &[clause],
            Some("orders"),
            Some("o")
        ),
        Ordering::Greater
    );
    assert_eq!(host.identities.get(), 2);
    let secret = Value::Secret(b"fixture ciphertext".to_vec());
    assert_eq!(
        compare_runtime_optional_values(
            &super::record_filters::plaintext,
            Some(&secret),
            Some(&Value::Integer(1)),
            false
        ),
        Ordering::Equal
    );
    let admission = |value| {
        Some(if matches!(value, Value::Secret(_)) {
            Value::Integer(2)
        } else {
            value
        })
    };
    assert_eq!(
        compare_runtime_optional_values(&admission, Some(&secret), Some(&Value::Integer(1)), false),
        Ordering::Greater
    );
}

fn unordered_and_nan_edges() {
    let rows = vec![
        row(0, Value::Float(f64::NAN)),
        row(1, Value::Float(1.0)),
        row(2, Value::Float(2.0)),
    ];
    let query = query("ORDER BY quantity");
    let host = Host::new(&query, None);
    let mut selected = rows.clone();
    top_k_records_by_order_by_with_host(&host, &mut selected, &[], 2, None, None);
    assert_eq!(ids(&selected), [0, 1]);
    for count in 0..=3 {
        let mut selected = rows.clone();
        top_k_records_by_order_by_with_host(
            &host,
            &mut selected,
            &query.order_by,
            count,
            None,
            None,
        );
        assert_eq!(selected.len(), count);
        assert!(ids(&selected).iter().all(|id| *id < 3));
    }
}

pub fn verify_recovered(records: &[UnifiedRecord], contract: &CollectionContract, updated: bool) {
    let [first, second, third, fourth] = [9_007_199_254_740_993, 2, 3, 9_007_199_254_740_992];
    for (suffix, expected) in [
        (
            "WHERE total >= 10 ORDER BY o.total DESC LIMIT 2 OFFSET 1",
            if updated {
                vec![third, second]
            } else {
                vec![second, fourth]
            },
        ),
        ("ORDER BY o.rid ASC", vec![second, third, fourth, first]),
        ("ORDER BY payload.score DESC LIMIT 2", vec![third, second]),
        (
            "ORDER BY COALESCE(nickname, 'none') ASC, rid ASC",
            vec![second, fourth, third, first],
        ),
        (
            "ORDER BY LENGTH(COALESCE(nickname, 'none')) ASC, rid ASC",
            vec![second, third, first, fourth],
        ),
        (
            "ORDER BY CAST(JSON_EXTRACT_TEXT(payload, '$.score') AS INT) DESC, rid ASC LIMIT 2",
            vec![third, second],
        ),
        (
            "ORDER BY nickname ASC NULLS FIRST, rid DESC",
            vec![first, third, second, fourth],
        ),
        (
            "ORDER BY quantity * 5 DESC LIMIT 1",
            vec![if updated { first } else { third }],
        ),
    ] {
        let query = query(suffix);
        for disabled in [false, true] {
            assert_eq!(
                ids(&ordered_records(records, &query, Some(contract), disabled)),
                expected,
                "recovered {suffix}"
            );
        }
    }
}

/// Test-only record/context adapter. Pure functions use the canonical native
/// value dispatcher; native effects are deliberately rejected by this fixture.
struct RecordExpressionHost<'a> {
    query: &'a TableQuery,
    record: &'a UnifiedRecord,
}
impl RuntimeExpressionHost for RecordExpressionHost<'_> {
    fn secret_query_input(&self, value: Value) -> Option<Value> {
        super::record_filters::plaintext(value)
    }
    fn resolve_field(&self, field: &FieldRef) -> Option<Value> {
        resolve_runtime_field(
            self.record,
            field,
            Some(&self.query.table),
            self.query.alias.as_deref(),
        )
    }
    fn evaluate_special_function(
        &self,
        _: &str,
        _: &[Expr],
        _: &dyn Fn(&Expr) -> Option<Value>,
    ) -> Option<Value> {
        panic!("fixture does not admit context effects")
    }
    fn evaluate_function(&self, name: &str, arguments: &[Value]) -> Option<Value> {
        evaluate_value_function_with_host(self, name, arguments)
    }
}
impl ValueFunctionHost for RecordExpressionHost<'_> {
    fn current_unix_ms(&self) -> i64 {
        reddb_engine::query::runtime::temporal::current_unix_ms()
    }
    fn fallback_function(&self, _: &str, _: &[Value]) -> Option<Value> {
        panic!("fixture does not admit native scalar effects")
    }
}

fn fallible_keys_and_retry() {
    for suffix in [
        "ORDER BY o.quantity + 1 LIMIT 2 OFFSET 1",
        "ORDER BY o.quantity LIMIT 2 OFFSET 1",
    ] {
        fallible_keys_and_retry_for_query(&query(suffix));
    }
}

fn fallible_keys_and_retry_for_query(query: &TableQuery) {
    let rows: Vec<_> = (0..12)
        .rev()
        .map(|id| row(id, Value::Integer(id as i64)))
        .collect();
    for strategy in 0..3 {
        for failure_at in [1, 3, 12] {
            let host = Host::new(query, None);
            host.failure_at.set(Some(failure_at));
            let mut selected = rows.clone();
            let run = |records: &mut Vec<UnifiedRecord>| match strategy {
                0 => try_sort_records_by_order_by_with_host(
                    &host,
                    records,
                    &query.order_by,
                    Some("orders"),
                    Some("o"),
                ),
                1 => try_top_k_records_by_order_by_with_host(
                    &host,
                    records,
                    &query.order_by,
                    2,
                    Some("orders"),
                    Some("o"),
                ),
                _ => try_sort_or_top_k_records_with_host(
                    &host,
                    records,
                    &query.order_by,
                    query.offset,
                    query.limit,
                    Some("orders"),
                    Some("o"),
                ),
            };
            assert_eq!(run(&mut selected), Err("sort key read failed"));
            assert_eq!(
                ids(&selected),
                ids(&rows),
                "failed keys must not reorder or truncate input"
            );
            assert_eq!(host.evaluations.get(), failure_at);
            host.failure_at.set(None);
            host.evaluations.set(0);
            run(&mut selected).expect("retry after key read recovery");
            let count = match strategy {
                0 => 12,
                1 => 2,
                _ => 3,
            };
            assert_eq!(ids(&selected), (0..count).collect::<Vec<_>>());
            assert_eq!(host.evaluations.get(), 12, "extract keys once per row");
        }
    }
    let host = Host::new(query, None);
    host.failure_at.set(Some(2));
    assert_eq!(
        try_compare_runtime_order_with_host(
            &host,
            &rows[0],
            &rows[1],
            &query.order_by,
            Some("orders"),
            Some("o")
        ),
        Err("sort key read failed")
    );
    assert_eq!(host.evaluations.get(), 2);
    host.evaluations.set(0);
    host.failure_at.set(Some(1));
    let mut selected = rows.clone();
    try_top_k_records_by_order_by_with_host(
        &host,
        &mut selected,
        &query.order_by,
        0,
        Some("orders"),
        Some("o"),
    )
    .expect("zero result needs no key reads");
    assert!(selected.is_empty());
    assert_eq!(host.evaluations.get(), 0);
}
