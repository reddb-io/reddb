//! Native runtime ORDER BY semantics exercised unchanged in native and WASM.
use reddb_engine::catalog::CollectionContract;
use reddb_engine::query::{
    evaluator,
    runtime::{
        filter::resolve_filter_row_value,
        ordering::{
            compare_runtime_optional_values, compare_runtime_order_with_host,
            sort_or_top_k_records_with_host, sort_records_by_order_by_with_host,
            top_k_records_by_order_by_with_host, OrderingHost,
        },
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
];

struct Host<'a> {
    query: &'a TableQuery,
    contract: Option<&'a CollectionContract>,
    evaluations: Cell<usize>,
    identities: Cell<usize>,
}

impl<'a> Host<'a> {
    fn new(query: &'a TableQuery, contract: Option<&'a CollectionContract>) -> Self {
        Self {
            query,
            contract,
            evaluations: Cell::new(0),
            identities: Cell::new(0),
        }
    }
}

impl OrderingHost for Host<'_> {
    fn evaluate_expression(&self, expression: &Expr, record: &UnifiedRecord) -> Option<Value> {
        self.evaluations.set(self.evaluations.get() + 1);
        evaluator::evaluate(expression, &|field: &FieldRef| {
            resolve_filter_row_value(
                record,
                field,
                Some(&self.query.table),
                self.query.alias.as_deref(),
                &super::record_filters::plaintext,
                |collection, name| {
                    self.contract.is_some_and(|contract| {
                        contract.name == collection
                            && contract
                                .declared_columns
                                .iter()
                                .any(|column| column.name == name && !column.not_null)
                    })
                },
            )
        })
        .ok()
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
}

fn row(id: u64, value: Value) -> UnifiedRecord {
    let mut row = UnifiedRecord::new();
    row.set("rid", Value::UnsignedInteger(id));
    row.set("key", value);
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

/// Test-only composition over supplied/admitted rows, not a public SQL dispatcher.
pub fn ordered_records(
    records: &[UnifiedRecord],
    query: &TableQuery,
    contract: Option<&CollectionContract>,
    disable_topk: bool,
) -> Vec<UnifiedRecord> {
    let host = Host::new(query, contract);
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
        disable_topk,
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
            &query("ORDER BY o.key ASC"),
            None,
            false
        )),
        [1, 4, 3, 0, 2]
    );
    assert_eq!(
        ids(&ordered_records(
            &rows,
            &query("ORDER BY o.key DESC LIMIT 2"),
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
                &query(&format!("ORDER BY o.key {suffix}")),
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
            &query("ORDER BY key ASC"),
            None,
            false
        )),
        [4, 6, 2, 3, 0, 5, 1]
    );
    let mut selected = rows.clone();
    let query = query("ORDER BY key ASC");
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
    let query = query("ORDER BY o.key * 2 DESC, o.key ASC");
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
    for suffix in ["ORDER BY key ASC", "ORDER BY key DESC, rid ASC"] {
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
                &query("ORDER BY key ASC, rid ASC LIMIT 3 OFFSET 2"),
                None,
                disabled
            )),
            {
                let sorted =
                    ordered_records(&rows, &query("ORDER BY key ASC, rid ASC"), None, true);
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
    let query = query("ORDER BY key ASC");
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
            false,
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
    assert!(ordered_records(&rows, &self::query("ORDER BY key LIMIT 0"), None, false).is_empty());
    assert!(ordered_records(
        &rows,
        &self::query("ORDER BY key LIMIT 1 OFFSET 4294967296"),
        None,
        false
    )
    .is_empty());
}

fn identity_and_secret_callbacks() {
    let query = query("ORDER BY key ASC");
    let host = Host::new(&query, None);
    let rows = [row(2, Value::Integer(1)), row(1, Value::Integer(1))];
    let clause = OrderByClause::asc(FieldRef::TableColumn {
        table: String::new(),
        column: "key".into(),
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
    let query = query("ORDER BY key");
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
