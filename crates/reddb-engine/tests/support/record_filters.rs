//! Canonical record access and WHERE semantics, native and browser WASM.
use reddb_engine::catalog::CollectionContract;
use reddb_engine::query::{
    evaluator::{self, EvalError},
    runtime::{field_resolution::*, filter::*, is_universal_entity_source, values::*},
    unified::{MatchedEdge, MatchedNode, UnifiedRecord},
};
use reddb_rql::ast::{CompareOp, Expr, FieldRef, Filter, TableQuery};
use reddb_types::Value;
use std::{cell::Cell, collections::HashMap};

pub const CASES: &[(&str, fn())] = &[
    ("record_fields_aliases_and_identity", aliases_and_identity),
    ("record_fields_json_and_binary_paths", json_and_binary_paths),
    ("record_fields_graph_bindings", graph_bindings),
    (
        "record_filter_nullable_column_lookup",
        nullable_column_lookup,
    ),
    (
        "record_filter_exact_values_and_metadata",
        exact_values_and_metadata,
    ),
    ("record_filter_predicates_and_errors", predicates_and_errors),
    (
        "record_filter_short_circuit_and_host_errors",
        short_circuit_and_host_errors,
    ),
    ("record_filter_secret_admission", secret_admission),
];

pub fn plaintext(value: Value) -> Option<Value> {
    if matches!(value, Value::Secret(_)) {
        None
    } else {
        Some(value)
    }
}

pub fn matches_where(
    record: &UnifiedRecord,
    query: &TableQuery,
    contract: Option<&CollectionContract>,
) -> Result<bool, EvalError> {
    let Some(filter) = reddb_rql::sql_lowering::effective_table_filter(query) else {
        return Ok(true);
    };
    let host = ReadHost {
        record,
        table_name: &query.table,
        table_alias: query.alias.as_deref().unwrap_or(&query.table),
        contract,
    };
    evaluate_runtime_filter_result_with_host(
        &host,
        record,
        &filter,
        Some(host.table_name),
        Some(host.table_alias),
    )
}

struct ReadHost<'a> {
    record: &'a UnifiedRecord,
    table_name: &'a str,
    table_alias: &'a str,
    contract: Option<&'a CollectionContract>,
}
impl FilterHost for ReadHost<'_> {
    type Error = EvalError;
    fn secret_query_input(&self, value: Value) -> Option<Value> {
        plaintext(value)
    }
    fn evaluate_expression(&self, expression: &Expr) -> Result<Option<Value>, Self::Error> {
        evaluator::evaluate(expression, &|field: &FieldRef| {
            resolve_filter_row_value(
                self.record,
                field,
                Some(self.table_name),
                Some(self.table_alias),
                &plaintext,
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
        .map(Some)
    }
}

fn field(table: &str, column: &str) -> FieldRef {
    FieldRef::TableColumn {
        table: table.into(),
        column: column.into(),
    }
}

fn aliases_and_identity() {
    let mut record = UnifiedRecord::new();
    record.set("rid", Value::UnsignedInteger(9_007_199_254_740_993));
    record.set("quantity", Value::Integer(3));
    let resolve = |field| resolve_runtime_field(&record, &field, Some("orders"), Some("o"));
    for qualifier in ["", "orders", "o"] {
        assert_eq!(
            resolve(field(qualifier, "quantity")),
            Some(Value::Integer(3))
        );
    }
    assert_eq!(
        resolve(field("foreign", "quantity")),
        Some(Value::Integer(3)),
        "preserve the native flattened document-path fallback"
    );
    assert_eq!(
        resolve(field("o", "entity_id")),
        Some(Value::UnsignedInteger(9_007_199_254_740_993))
    );
    record.set("entity_id", Value::text("user field"));
    assert_eq!(
        resolve_runtime_field(&record, &field("", "entity_id"), Some("orders"), None),
        Some(Value::text("user field")),
        "legacy aliases cannot shadow a real field"
    );
    record.set("foreign.quantity", Value::Integer(7));
    assert_eq!(
        resolve_runtime_field(&record, &field("foreign", "quantity"), Some("orders"), None),
        Some(Value::Integer(7)),
        "qualified materialized key wins"
    );
    for name in ["ANY", "_any", "all", "entity", "universal", "mixed", "*"] {
        assert!(is_universal_entity_source(name));
        assert!(runtime_table_context_matches(name, Some("any"), None));
    }
    assert!(!is_universal_entity_source("orders"));
    assert!(parse_canonical_field_ref("table:orders.quantity").is_ok());
    assert_eq!(
        parse_canonical_field_ref("table:broken").expect_err("malformed reference"),
        "invalid canonical table field ref table:broken"
    );
}

fn json_and_binary_paths() {
    let mut binary = vec![];
    let nested = Value::Json(br#"{"Items":[{"count":9007199254740993}],"flag":true}"#.to_vec());
    reddb_types::document_body_codec::encode(&[("nested", &nested)], &mut binary)
        .expect("binary document");
    for body in [
        Value::Json(binary.clone()),
        Value::Blob(binary),
        Value::Json(br#"{"nested":{"Items":[{"count":9007199254740993}],"flag":true}}"#.to_vec()),
        Value::text(r#" {"nested":{"Items":[{"count":9007199254740993}],"flag":true}}"#),
    ] {
        let mut record = UnifiedRecord::new();
        record.set("body", body);
        assert_eq!(
            resolve_runtime_document_path(&record, "body.nested.items[0].count"),
            Some(Value::Integer(9_007_199_254_740_993))
        );
        assert_eq!(
            resolve_runtime_field(
                &record,
                &field("o", "body.nested.flag"),
                Some("orders"),
                Some("o")
            ),
            Some(Value::Boolean(true))
        );
        assert!(resolve_runtime_document_path(&record, "body.nested.items[9].count").is_none());
        assert!(resolve_runtime_document_path(&record, "body.missing").is_none());
    }
}

fn graph_bindings() {
    let mut record = UnifiedRecord::new();
    record.nodes.insert(
        "n".into(),
        MatchedNode {
            id: "node-1".into(),
            label: "one".into(),
            node_label: "person".into(),
            properties: HashMap::from([("age".into(), Value::Integer(42))]),
        },
    );
    record.edges.insert(
        "e".into(),
        MatchedEdge::from_tuple("node-1", "knows", "node-2", 0.5),
    );
    assert_eq!(
        resolve_runtime_field(
            &record,
            &FieldRef::NodeProperty {
                alias: "n".into(),
                property: "age".into()
            },
            None,
            None
        ),
        Some(Value::Integer(42))
    );
    assert_eq!(
        resolve_runtime_field(&record, &FieldRef::NodeId { alias: "n".into() }, None, None),
        Some(Value::NodeRef("node-1".into()))
    );
    assert_eq!(
        resolve_runtime_field(
            &record,
            &FieldRef::EdgeProperty {
                alias: "e".into(),
                property: "weight".into()
            },
            None,
            None
        ),
        Some(Value::Float(0.5))
    );
    assert_eq!(
        resolve_runtime_field(
            &record,
            &FieldRef::EdgeProperty {
                alias: "e".into(),
                property: "target".into()
            },
            None,
            None
        ),
        Some(Value::NodeRef("node-2".into()))
    );
}

fn nullable_column_lookup() {
    let mut record = UnifiedRecord::new();
    record.set("collection", Value::text("orders"));
    record.set("quantity", Value::Integer(3));
    let lookups = Cell::new(0);
    let nullable = |collection: &str, column: &str| {
        lookups.set(lookups.get() + 1);
        collection == "orders" && column == "nickname"
    };
    assert_eq!(
        resolve_filter_row_value(
            &record,
            &field("o", "quantity"),
            Some("orders"),
            Some("o"),
            &plaintext,
            nullable
        ),
        Some(Value::Integer(3))
    );
    assert_eq!(
        lookups.get(),
        0,
        "hot field resolution does not consult the catalog"
    );
    assert_eq!(
        resolve_filter_row_value(
            &record,
            &field("", "nickname"),
            Some("any"),
            None,
            &plaintext,
            nullable
        ),
        Some(Value::Null),
        "universal records retain their source collection"
    );
    assert_eq!(
        resolve_filter_row_value(
            &record,
            &field("", "unknown"),
            Some("orders"),
            None,
            &plaintext,
            nullable
        ),
        None
    );
    let before = lookups.get();
    assert_eq!(
        resolve_filter_row_value(
            &record,
            &field("foreign", "nickname"),
            Some("orders"),
            Some("o"),
            &plaintext,
            nullable
        ),
        None
    );
    assert_eq!(
        lookups.get(),
        before,
        "foreign aliases do not trigger catalog reads"
    );
    record.set("kind", Value::text("document"));
    assert_eq!(
        resolve_filter_row_value(
            &record,
            &field("", "missing"),
            Some("orders"),
            None,
            &plaintext,
            nullable
        ),
        Some(Value::Null)
    );
    assert_eq!(
        lookups.get(),
        before,
        "sparse documents do not consult table nullability"
    );
}

fn exact_values_and_metadata() {
    assert!(compare_runtime_values(
        &Value::UnsignedInteger(9_007_199_254_740_993),
        &Value::Integer(9_007_199_254_740_992),
        CompareOp::Gt,
        &plaintext
    ));
    assert!(!compare_runtime_values(
        &Value::UnsignedInteger(u64::MAX),
        &Value::Integer(-1),
        CompareOp::Eq,
        &plaintext
    ));
    assert!(compare_runtime_values(
        &Value::Integer(-1),
        &Value::UnsignedInteger(0),
        CompareOp::Lt,
        &plaintext
    ));
    assert!(!compare_runtime_values(
        &Value::Float(f64::NAN),
        &Value::Integer(1),
        CompareOp::Lt,
        &plaintext
    ));
    let capability = field("any", "red_capabilities");
    assert_eq!(
        evaluate_metadata_field_compare(
            &capability,
            &Value::text("table, document"),
            CompareOp::Eq,
            &Value::text("DOCUMENT")
        ),
        Some(true)
    );
    assert_eq!(
        evaluate_metadata_field_in(
            &field("any", "red_entity_type"),
            &Value::text("graph_node"),
            &[Value::text("TABLE"), Value::text("GRAPH_NODE")]
        ),
        Some(true)
    );
    for (value, pattern, expected) in [
        ("abc", "a_c", true),
        ("alphabet", "%pha%", true),
        ("abc", "a%d", false),
        ("", "%", true),
        ("é", "__", true),
        ("é", "_", false),
    ] {
        assert_eq!(
            like_matches(value, pattern),
            expected,
            "existing byte-oriented LIKE semantics"
        );
    }
}

pub fn table_query(sql: &str) -> TableQuery {
    let query = reddb_rql::parse_frontend(sql)
        .expect("bounded RQL query")
        .into_query_expr();
    let reddb_rql::ast::QueryExpr::Table(query) = query else {
        panic!("fixture is a table query")
    };
    query
}

fn predicates_and_errors() {
    let mut record = UnifiedRecord::new();
    record.set("quantity", Value::Integer(3));
    record.set("label", Value::text("alpha"));
    record.set(
        "tags",
        Value::Array(vec![Value::text("red"), Value::text("blue")]),
    );
    record.set("meta", Value::text(r#"{"tenant":"red"}"#));
    for (predicate, expected) in [
        ("quantity = 3", true),
        ("quantity BETWEEN 2 AND 4", true),
        ("quantity IN (1, 3)", true),
        ("quantity > 9 OR label LIKE 'al%'", true),
        ("NOT (quantity = 3)", false),
        ("meta.tenant = 'red'", true),
        ("missing IS NULL", true),
        ("missing IS NOT NULL", false),
        ("quantity * 2 = 6", true),
    ] {
        let query = table_query(&format!("SELECT * FROM orders AS o WHERE {predicate}"));
        assert_eq!(
            matches_where(&record, &query, None).expect("WHERE verdict"),
            expected,
            "{predicate}"
        );
    }
    let query = table_query("SELECT * FROM orders WHERE quantity / 0 > 1");
    assert!(matches!(
        matches_where(&record, &query, None),
        Err(EvalError::DivisionByZero)
    ));
    let query = table_query("SELECT * FROM orders WHERE missing + 1 > 0");
    assert!(matches!(
        matches_where(&record, &query, None),
        Err(EvalError::UnknownColumn(_))
    ));
}

fn short_circuit_and_host_errors() {
    struct Host {
        calls: Cell<u32>,
    }
    impl FilterHost for Host {
        type Error = &'static str;
        fn secret_query_input(&self, value: Value) -> Option<Value> {
            plaintext(value)
        }
        fn evaluate_expression(&self, _: &Expr) -> Result<Option<Value>, Self::Error> {
            self.calls.set(self.calls.get() + 1);
            Err("host expression failure")
        }
    }
    let host = Host {
        calls: Cell::new(0),
    };
    let mut record = UnifiedRecord::new();
    record.set("quantity", Value::Integer(3));
    let expression = || {
        reddb_rql::schema_expression::SchemaExpression::parse("1 / 0".into())
            .expect("expression")
            .expression()
            .clone()
    };
    let fail = Filter::CompareExpr {
        lhs: expression(),
        op: CompareOp::Gt,
        rhs: expression(),
    };
    let no = Filter::Compare {
        field: field("", "quantity"),
        op: CompareOp::Eq,
        value: Value::Integer(0),
    };
    let yes = Filter::Not(Box::new(no.clone()));
    for (filter, expected) in [
        (Filter::And(Box::new(no), Box::new(fail.clone())), false),
        (Filter::Or(Box::new(yes), Box::new(fail.clone())), true),
    ] {
        assert_eq!(
            evaluate_runtime_filter_result_with_host(&host, &record, &filter, Some("orders"), None),
            Ok(expected)
        );
    }
    assert_eq!(
        host.calls.get(),
        0,
        "short circuit does not invoke expression effects"
    );
    assert_eq!(
        evaluate_runtime_filter_result_with_host(&host, &record, &fail, Some("orders"), None),
        Err("host expression failure")
    );
    assert_eq!(host.calls.get(), 1, "left error prevents right evaluation");
}

fn secret_admission() {
    let secret = Value::Secret(b"fixture ciphertext".to_vec());
    assert!(!compare_runtime_values(
        &secret,
        &secret,
        CompareOp::Eq,
        &plaintext
    ));
    assert!(runtime_partial_cmp(&secret, &Value::Integer(7), &plaintext).is_none());
    let admitted = |value| {
        Some(if matches!(value, Value::Secret(_)) {
            Value::Integer(7)
        } else {
            value
        })
    };
    assert!(compare_runtime_values(
        &secret,
        &Value::Integer(7),
        CompareOp::Eq,
        &admitted
    ));
    let mut record = UnifiedRecord::new();
    record.set("protected", secret);
    assert!(resolve_filter_field(
        &record,
        &field("", "protected"),
        Some("orders"),
        None,
        &plaintext
    )
    .is_none());
    assert_eq!(
        runtime_value_text(&Value::Password("fixture password".into())),
        Some("***".into())
    );
}
