//! Shared native projection traversal, including host policy boundaries.
use reddb_engine::catalog::CollectionContract;
use reddb_engine::query::{
    evaluator::{self, EvalError},
    runtime::{filter::resolve_filter_row_value, projection::*, projection_value::*},
    unified::{MatchedEdge, MatchedNode, UnifiedRecord, VectorSearchResult},
};
use reddb_rql::ast::{CompareOp, Expr, FieldRef, Filter, Projection};
use reddb_types::Value;
use std::{cell::Cell, collections::HashMap, sync::Arc};

pub const CASES: &[(&str, fn())] = &[
    ("projection_value_legacy_literals", legacy_literals),
    (
        "projection_value_resolution_and_fallback",
        value_resolution_and_fallback,
    ),
    ("projection_value_secret_input_order", secret_input_order),
    (
        "record_projection_fields_aliases_and_literals",
        fields_aliases_and_literals,
    ),
    (
        "record_projection_star_and_column_layout",
        star_and_column_layout,
    ),
    ("record_projection_document_paths", document_paths),
    (
        "record_projection_bindings_and_window_output",
        bindings_and_window_output,
    ),
    (
        "record_projection_typed_values_and_labels",
        typed_values_and_labels,
    ),
    (
        "record_projection_fallback_and_data_errors",
        fallback_and_data_errors,
    ),
    ("record_projection_secret_sensitivity", secret_sensitivity),
    (
        "record_projection_output_policy_and_redaction",
        output_policy_and_redaction,
    ),
];

struct Host<'a> {
    contract: Option<&'a CollectionContract>,
    forced_error: Option<EvalError>,
    fallback_calls: Cell<usize>,
    output_calls: Cell<usize>,
    deny_output: bool,
    input_calls: Cell<usize>,
    deny_input: bool,
}

impl<'a> Host<'a> {
    fn new(contract: Option<&'a CollectionContract>) -> Self {
        Self {
            contract,
            forced_error: None,
            fallback_calls: Cell::new(0),
            output_calls: Cell::new(0),
            deny_output: false,
            input_calls: Cell::new(0),
            deny_input: false,
        }
    }
}

impl ProjectionHost for Host<'_> {
    type Error = String;
    fn evaluate_expression(
        &self,
        expression: &Expr,
        source: &UnifiedRecord,
    ) -> Result<Value, EvalError> {
        if let Some(error) = &self.forced_error {
            return Err(error.clone());
        }
        evaluator::evaluate(expression, &|field: &FieldRef| {
            resolve_filter_row_value(
                source,
                field,
                Some("orders"),
                Some("o"),
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
    }
    fn evaluate_filter(&self, _: &Filter, _: &UnifiedRecord) -> bool {
        self.fallback_calls.set(self.fallback_calls.get() + 1);
        true
    }
    fn evaluate_function(
        &self,
        name: &str,
        arguments: &[Projection],
        source: &UnifiedRecord,
    ) -> Option<Value> {
        self.fallback_calls.set(self.fallback_calls.get() + 1);
        let operation = name.split(':').next().unwrap_or(name);
        match operation {
            "ADD" | "SUB" | "MUL" | "DIV" | "MOD" => {
                let resolve =
                    |argument: &Projection| eval_projection_value_with_host(self, argument, source);
                Some(reddb_engine::query::runtime::numeric::arith_binop(
                    operation,
                    resolve(arguments.first()?)?,
                    resolve(arguments.get(1)?)?,
                ))
            }
            _ => Some(Value::text("host fallback")),
        }
    }
    fn evaluate_literal(&self, column: &str, _: &UnifiedRecord) -> Option<Value> {
        decode_projection_literal(column)
    }
    fn evaluation_error(&self, error: EvalError, sensitive: bool) -> String {
        if sensitive {
            "secret expression evaluation failed".into()
        } else {
            error.to_string()
        }
    }
    fn secret_query_output(&self, value: Value) -> Result<Value, String> {
        self.output_calls.set(self.output_calls.get() + 1);
        if self.deny_output {
            Err("secret output denied".into())
        } else {
            Ok(value)
        }
    }
}

// Test-only scalar host: real shared argument traversal and arithmetic, with
// explicit fallback sentinels for effects not implemented by this fixture.
impl ProjectionValueHost for Host<'_> {
    fn try_expression(&self, expression: &Expr, source: &UnifiedRecord) -> Option<Value> {
        if self.forced_error.is_some() {
            return None;
        }
        evaluator::evaluate(expression, &|field: &FieldRef| {
            resolve_filter_row_value(
                source,
                field,
                None,
                None,
                &super::record_filters::plaintext,
                |_, _| false,
            )
        })
        .ok()
    }
    fn fallback_function(
        &self,
        name: &str,
        arguments: &[Projection],
        source: &UnifiedRecord,
    ) -> Option<Value> {
        self.evaluate_function(name, arguments, source)
    }
    fn fallback_filter(&self, filter: &Filter, source: &UnifiedRecord) -> bool {
        self.evaluate_filter(filter, source)
    }
    fn secret_query_input(&self, value: Value) -> Option<Value> {
        self.input_calls.set(self.input_calls.get() + 1);
        if self.deny_input {
            None
        } else {
            Some(value)
        }
    }
}

fn legacy_literals() {
    for (input, expected) in [
        ("LIT:", Value::Null),
        ("LIT:42", Value::Integer(42)),
        ("LIT:2.5", Value::Float(2.5)),
        ("LIT:hello", Value::text("hello")),
        ("LIT:null", Value::text("null")),
        ("LIT:@RL:null", Value::Null),
        ("LIT:@RL:[]", Value::Array(vec![])),
        ("LIT:@RL:V[]", Value::Vector(vec![])),
        ("LIT:@RL:V[1,2.5]", Value::Vector(vec![1.0, 2.5])),
        (
            "LIT:@RL:[true,false,null,[1],V[2,3],\"text\"]",
            Value::Array(vec![
                Value::Boolean(true),
                Value::Boolean(false),
                Value::Null,
                Value::Array(vec![Value::Integer(1)]),
                Value::Vector(vec![2.0, 3.0]),
                Value::text("text"),
            ]),
        ),
        ("LIT:@RL:V[bad]", Value::text("@RL:V[bad]")),
        ("LIT:@RL:[1,]", Value::text("@RL:[1,]")),
        // Preserve the legacy splitter's quoted-comma behavior.
        ("LIT:@RL:[\"a,b\"]", Value::text("@RL:[\"a,b\"]")),
        (r#"LIT:@RL:"a\"b\\c""#, Value::text("a\"b\\c")),
    ] {
        assert_eq!(decode_projection_literal(input), Some(expected), "{input}");
    }
    assert_eq!(decode_projection_literal("quantity"), None);
    let depth = reddb_rql::limits::JSON_LITERAL_MAX_DEPTH as usize;
    let payload = format!("{}0{}", "[".repeat(depth), "]".repeat(depth));
    let value = decode_projection_literal(&format!("LIT:@RL:{payload}"));
    assert!(matches!(value, Some(Value::Array(_))), "depth boundary");
    let payload = format!("[{payload}]");
    assert_eq!(
        decode_projection_literal(&format!("LIT:@RL:{payload}")),
        Some(Value::text(format!("@RL:{payload}"))),
        "over-depth retains text fallback"
    );
}

fn value_resolution_and_fallback() {
    let mut source = source();
    source.set("position", Value::Integer(8));
    let host = Host::new(None);
    for projection in [
        Projection::Column("quantity".into()),
        Projection::Alias("quantity".into(), "amount".into()),
        Projection::Field(field("quantity"), None),
    ] {
        assert_eq!(
            eval_projection_value_with_host(&host, &projection, &source),
            Some(Value::Integer(3))
        );
    }
    let window =
        projections("SELECT ROW_NUMBER() OVER (ORDER BY quantity) AS position FROM orders");
    assert_eq!(
        eval_projection_value_with_host(&host, &window[0], &source),
        Some(Value::Integer(8))
    );
    assert_eq!(
        eval_projection_value_with_host(&host, &Projection::All, &source),
        None
    );
    assert_eq!(
        eval_projection_value_with_host(&host, &Projection::Column("missing".into()), &source),
        None
    );
    let columns = projections("SELECT (quantity + 1) * 2 AS nested FROM orders");
    assert_eq!(
        eval_projection_value_with_host(&host, &columns[0], &source),
        Some(Value::Integer(8))
    );
    assert_eq!(
        host.fallback_calls.get(),
        2,
        "nested ADD and MUL use canonical legacy arguments"
    );
    let host = Host {
        forced_error: Some(EvalError::DivisionByZero),
        ..Host::new(None)
    };
    let columns =
        projections("SELECT UPPER(nickname) AS display, quantity > 1 AS accepted FROM orders");
    assert_eq!(
        eval_projection_value_with_host(&host, &columns[0], &source),
        Some(Value::text("host fallback"))
    );
    assert_eq!(
        eval_projection_value_with_host(&host, &columns[1], &source),
        Some(Value::Boolean(true))
    );
    assert_eq!(
        host.fallback_calls.get(),
        2,
        "optional arguments fall back even on typed data errors"
    );
}

fn secret_input_order() {
    let source = source();
    let host = Host::new(None);
    let plain = Projection::Column("quantity".into());
    let alias = Projection::Alias("quantity".into(), "amount".into());
    assert_eq!(
        eval_projection_value_with_host(&host, &alias, &source),
        Some(Value::Integer(3))
    );
    assert_eq!(
        host.input_calls.get(),
        2,
        "column and outer alias each admit input"
    );
    host.input_calls.set(0);
    let literal = Projection::Column("LIT:7".into());
    assert_eq!(
        eval_projection_value_with_host(&host, &literal, &source),
        Some(Value::Integer(7))
    );
    assert_eq!(
        host.input_calls.get(),
        0,
        "direct literal returns before admission"
    );
    let host = Host {
        deny_input: true,
        ..Host::new(None)
    };
    assert_eq!(
        eval_projection_value_with_host(&host, &plain, &source),
        None
    );
    assert_eq!(host.input_calls.get(), 1);
    host.input_calls.set(0);
    assert_eq!(
        eval_projection_value_with_host(&host, &alias, &source),
        None
    );
    assert_eq!(
        host.input_calls.get(),
        1,
        "denied inner value skips outer admission"
    );
    assert_eq!(
        eval_projection_value_with_host(&host, &literal, &source),
        Some(Value::Integer(7))
    );
    host.input_calls.set(0);
    let alias = Projection::Alias("LIT:7".into(), "literal".into());
    assert_eq!(
        eval_projection_value_with_host(&host, &alias, &source),
        None
    );
    assert_eq!(
        host.input_calls.get(),
        1,
        "aliased literal still requires admission"
    );
}

fn field(column: &str) -> FieldRef {
    FieldRef::TableColumn {
        table: "o".into(),
        column: column.into(),
    }
}

fn project(
    host: &Host<'_>,
    source: &UnifiedRecord,
    projections: &[Projection],
) -> Result<UnifiedRecord, String> {
    project_runtime_record_with_host(
        host,
        source,
        projections,
        Some("orders"),
        Some("o"),
        false,
        false,
    )
}

fn projections(sql: &str) -> Vec<Projection> {
    let query = super::record_filters::table_query(sql);
    reddb_rql::sql_lowering::effective_table_projections(&query)
}

fn expression(text: &str) -> Expr {
    reddb_rql::schema_expression::SchemaExpression::parse(text.into())
        .expect("bounded expression")
        .expression()
        .clone()
}

fn source() -> UnifiedRecord {
    let mut row = UnifiedRecord::new();
    row.set("rid", Value::UnsignedInteger(9_007_199_254_740_993));
    row.set("quantity", Value::Integer(3));
    row.set("nickname", Value::text("Bee"));
    row
}

fn fields_aliases_and_literals() {
    let source = source();
    let projections = vec![
        Projection::Alias("quantity".into(), "amount".into()),
        Projection::Field(field("rid"), Some("identity".into())),
        Projection::Column("entity_id".into()),
        Projection::Column("missing".into()),
        Projection::Alias("LIT:42".into(), "answer".into()),
    ];
    let row = project(&Host::new(None), &source, &projections).expect("projection");
    assert_eq!(row.get("amount"), Some(&Value::Integer(3)));
    assert_eq!(row.get("identity"), source.get("rid"));
    assert_eq!(row.get("entity_id"), source.get("rid"));
    assert_eq!(row.get("missing"), Some(&Value::Null));
    assert_eq!(row.get("answer"), Some(&Value::Integer(42)));
    assert_eq!(
        projected_columns(&[row], &projections),
        ["amount", "identity", "entity_id", "missing", "answer"]
    );
}

fn star_and_column_layout() {
    let mut row = UnifiedRecord::with_schema(
        Arc::new(vec![Arc::from("quantity"), Arc::from("_private")]),
        vec![Value::Integer(3), Value::text("internal")],
    );
    row.set("nickname", Value::text("Bee"));
    let projected = project(&Host::new(None), &row, &[Projection::All]).expect("star");
    assert_eq!(visible_value_keys(&projected), ["nickname", "quantity"]);
    assert!(projected.get("_private").is_none());
    assert_eq!(projected.get("quantity"), Some(&Value::Integer(3)));
    let mut sparse = UnifiedRecord::new();
    sparse.set("extra", Value::Integer(7));
    assert_eq!(
        collect_visible_columns(&[projected.clone(), sparse]),
        ["extra", "nickname", "quantity"]
    );
    assert_eq!(
        collect_visible_columns(&[projected.clone(), projected]),
        ["nickname", "quantity"]
    );
    let collision = [
        Projection::Alias("quantity".into(), "same".into()),
        Projection::Alias("nickname".into(), "same".into()),
    ];
    let projected = project(&Host::new(None), &row, &collision).expect("colliding labels");
    assert_eq!(
        projected.get("same"),
        Some(&Value::text("Bee")),
        "last projection retains the native overwrite rule"
    );
    assert_eq!(
        projected_columns(&[projected], &collision),
        ["same", "same"]
    );
}

fn document_paths() {
    let mut binary = vec![];
    reddb_types::document_body_codec::encode(&[("score", &Value::Integer(9))], &mut binary)
        .expect("binary document");
    let mut source = source();
    source.set("body", Value::Json(binary));
    let columns = [Projection::Alias("body.score".into(), "score".into())];
    let host = Host::new(None);
    assert_eq!(
        project(&host, &source, &columns)
            .expect("plain row")
            .get("score"),
        Some(&Value::Null)
    );
    for (document, entity) in [(true, false), (false, true)] {
        let row = project_runtime_record_with_host(
            &host,
            &source,
            &columns,
            Some("orders"),
            Some("o"),
            document,
            entity,
        )
        .expect("document path");
        assert_eq!(row.get("score"), Some(&Value::Integer(9)));
    }
    let columns = [Projection::Field(field("body.score"), Some("score".into()))];
    assert_eq!(
        project(&host, &source, &columns)
            .expect("explicit field")
            .get("score"),
        Some(&Value::Integer(9))
    );
}

fn bindings_and_window_output() {
    let mut source = source();
    source.nodes.insert(
        "n".into(),
        MatchedNode {
            id: "one".into(),
            label: "One".into(),
            node_label: "person".into(),
            properties: HashMap::new(),
        },
    );
    source.edges.insert(
        "e".into(),
        MatchedEdge::from_tuple("one", "knows", "two", 0.5),
    );
    source
        .vector_results
        .push(VectorSearchResult::new(7, "vectors", 0.25));
    source.set("position", Value::Integer(8));
    let columns =
        projections("SELECT ROW_NUMBER() OVER (ORDER BY quantity) AS position FROM orders");
    let row = project(&Host::new(None), &source, &columns).expect("materialized window");
    assert_eq!(row.get("position"), Some(&Value::Integer(8)));
    assert_eq!(row.nodes["n"].id, "one");
    assert_eq!(row.edges["e"].to, "two");
    assert_eq!(row.vector_results[0].id, 7);
}

fn typed_values_and_labels() {
    let columns = projections("SELECT o.rid AS identity, ABS(quantity) AS magnitude, UPPER(nickname) AS display, quantity > 1 AS accepted FROM orders AS o");
    let row = project(&Host::new(None), &source(), &columns).expect("typed projection");
    assert_eq!(
        row.get("identity"),
        Some(&Value::UnsignedInteger(9_007_199_254_740_993))
    );
    assert_eq!(row.get("magnitude"), Some(&Value::Integer(3)));
    assert_eq!(row.get("display"), Some(&Value::text("BEE")));
    assert_eq!(row.get("accepted"), Some(&Value::Boolean(true)));
    assert_eq!(
        projected_columns(&[row], &columns),
        ["identity", "magnitude", "display", "accepted"]
    );
}

fn fallback_and_data_errors() {
    let function =
        Projection::Function("UPPER".into(), vec![Projection::Column("nickname".into())]);
    let boolean = Projection::Expression(
        Box::new(Filter::Compare {
            field: field("quantity"),
            op: CompareOp::Gt,
            value: Value::Integer(1),
        }),
        Some("accepted".into()),
    );
    for error in [
        EvalError::UnknownColumn(field("missing")),
        EvalError::UnknownFunction {
            name: "host_fn".into(),
            args: vec![],
        },
    ] {
        let host = Host {
            forced_error: Some(error),
            ..Host::new(None)
        };
        let row =
            project(&host, &source(), &[function.clone(), boolean.clone()]).expect("host fallback");
        assert_eq!(
            row.get("UPPER(nickname)"),
            Some(&Value::text("host fallback"))
        );
        assert_eq!(row.get("accepted"), Some(&Value::Boolean(true)));
        assert_eq!(host.fallback_calls.get(), 2);
    }
    let host = Host {
        forced_error: Some(EvalError::DivisionByZero),
        ..Host::new(None)
    };
    assert_eq!(
        project(&host, &source(), &[function]).expect_err("data error"),
        EvalError::DivisionByZero.to_string()
    );
    assert_eq!(
        project(&host, &source(), &[boolean]).expect_err("data error"),
        EvalError::DivisionByZero.to_string()
    );
    assert_eq!(
        host.fallback_calls.get(),
        0,
        "data errors never invoke the legacy dispatcher"
    );
    let columns = projections("SELECT quantity / 0 > 1 AS invalid FROM orders");
    assert_eq!(
        project(&Host::new(None), &source(), &columns).expect_err("actual arithmetic error"),
        EvalError::DivisionByZero.to_string()
    );
}

fn secret_sensitivity() {
    let mut source = source();
    source.set("protected", Value::Secret(b"fixture ciphertext".to_vec()));
    let case = expression("CASE WHEN 1 = 1 THEN 7 ELSE protected END");
    assert!(expression_uses_secret(
        &case,
        &source,
        Some("orders"),
        Some("o")
    ));
    assert!(expression_uses_secret(
        &Expr::lit(Value::Secret(vec![1])),
        &source,
        None,
        None
    ));
    assert!(projection_uses_secret(
        &Projection::Function("__SECRET_REF".into(), vec![]),
        &source,
        None,
        None
    ));
    let filter = Filter::CompareFields {
        left: field("quantity"),
        op: CompareOp::Eq,
        right: field("protected"),
    };
    assert!(filter_uses_secret(
        &Filter::Not(Box::new(filter)),
        &source,
        Some("orders"),
        Some("o")
    ));
    assert!(!expression_uses_secret(
        &expression("quantity * 2"),
        &source,
        Some("orders"),
        Some("o")
    ));
    let projection = reddb_rql::sql_lowering::expr_to_projection(&case).expect("CASE lowering");
    let host = Host {
        deny_output: true,
        ..Host::new(None)
    };
    assert_eq!(
        project(&host, &source, &[projection])
            .expect_err("untaken secret branch remains sensitive"),
        "secret output denied"
    );
    assert_eq!(host.output_calls.get(), 1);
}

fn output_policy_and_redaction() {
    let mut source = source();
    source.set("protected", Value::Secret(b"fixture ciphertext".to_vec()));
    let host = Host {
        deny_output: true,
        ..Host::new(None)
    };
    let columns = [Projection::Field(field("protected"), Some("value".into()))];
    assert_eq!(
        project(&host, &source, &columns).expect_err("output policy rejection"),
        "secret output denied"
    );
    let host = Host {
        forced_error: Some(EvalError::JsonParseFailed {
            reason: "private payload".into(),
        }),
        ..Host::new(None)
    };
    let sensitive = Projection::Function(
        "UPPER".into(),
        vec![Projection::Field(field("protected"), None)],
    );
    assert_eq!(
        project(&host, &source, &[sensitive]).expect_err("redacted evaluation error"),
        "secret expression evaluation failed"
    );
    assert_eq!(host.fallback_calls.get(), 0);
    let host = Host::new(None);
    let row = project(&host, &source, &[Projection::All]).expect("star preserves wrappers");
    assert!(
        matches!(row.get("protected"), Some(Value::Secret(_))),
        "star must not decrypt or stringify a secret"
    );
    assert_eq!(
        host.output_calls.get(),
        0,
        "native star handling retains wrappers for downstream output policy"
    );
}

pub fn verify_recovered(records: &[UnifiedRecord], contract: &CollectionContract, updated: bool) {
    let sql = "SELECT o.rid AS identity, total AS amount, ABS(quantity) AS magnitude, COALESCE(nickname, 'none') AS display, payload.score AS score FROM orders AS o WHERE total >= 10 ORDER BY total DESC LIMIT 2 OFFSET 1";
    let query = super::record_filters::table_query(sql);
    let columns = reddb_rql::sql_lowering::effective_table_projections(&query);
    let selected = super::record_ordering::ordered_records(records, &query, Some(contract), false);
    let host = Host::new(Some(contract));
    let projected = selected
        .iter()
        .map(|source| project(&host, source, &columns).expect("recovered projection"))
        .collect::<Vec<_>>();
    let expected = if updated {
        vec![(3, 20, 4, "none", 20), (2, 15, 3, "Bee", 15)]
    } else {
        vec![
            (2, 15, 3, "Bee", 15),
            (9_007_199_254_740_992, 10, 2, "Other", 10),
        ]
    };
    assert_eq!(projected.len(), expected.len());
    for (record, (id, amount, magnitude, display, score)) in projected.iter().zip(expected) {
        assert_eq!(record.get("identity"), Some(&Value::UnsignedInteger(id)));
        assert_eq!(record.get("amount"), Some(&Value::Integer(amount)));
        assert_eq!(record.get("magnitude"), Some(&Value::Integer(magnitude)));
        assert_eq!(record.get("display"), Some(&Value::text(display)));
        assert_eq!(record.get("score"), Some(&Value::Integer(score)));
    }
    assert_eq!(
        projected_columns(&projected, &columns),
        ["identity", "amount", "magnitude", "display", "score"]
    );
    assert_eq!(
        host.fallback_calls.get(),
        0,
        "the recovered query executes through canonical typed evaluation"
    );
    let query = super::record_filters::table_query("SELECT o.rid AS identity, quantity * 2 AS doubled, (quantity + 1) * 2 AS nested, total / quantity AS unit_price, quantity / 0 AS zero_divisor FROM orders AS o WHERE total >= 10 ORDER BY total DESC LIMIT 2 OFFSET 1");
    let columns = reddb_rql::sql_lowering::effective_table_projections(&query);
    let selected = super::record_ordering::ordered_records(records, &query, Some(contract), false);
    for (index, source) in selected.iter().enumerate() {
        let projected = project(&host, source, &columns).expect("recovered legacy arithmetic");
        assert_eq!(projected.get("identity"), source.get("rid"));
        let doubled = if updated { [8, 6] } else { [6, 4] };
        assert_eq!(
            projected.get("doubled"),
            Some(&Value::Integer(doubled[index]))
        );
        assert_eq!(
            projected.get("nested"),
            Some(&Value::Integer(doubled[index] + 2))
        );
        assert_eq!(projected.get("unit_price"), Some(&Value::Float(5.0)));
        assert_eq!(projected.get("zero_divisor"), Some(&Value::Null));
    }
    assert_eq!(
        host.fallback_calls.get(),
        10,
        "five legacy operations including nested arguments per recovered row"
    );
}
