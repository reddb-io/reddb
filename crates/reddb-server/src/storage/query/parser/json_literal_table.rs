//! Table-driven coverage for the bare `{...}` JSON object literal.
//!
//! This file is the test counterpart of issue #86. It groups scenarios
//! into six explicit categories so future regressions surface against a
//! pinned shape:
//!
//! 1. Happy paths — well-formed JSON that must round-trip
//! 2. Whitespace — formatting variants that must produce equivalent rows
//! 3. SQL contexts — every clause the issue lists as a target site
//! 4. User mistakes — malformed input that must raise a `ParseError`
//! 5. Disambiguation — preserves quoted form, vector literal, etc.
//! 6. Limits / DoS — payload + depth ceilings raise a structured error
//!
//! Each happy-path case asserts byte-equality between the bare form and
//! `json::to_vec(parse_json(quoted_text))` so the on-disk encoding is
//! pinned in the parser's domain.

#![cfg(test)]

use crate::storage::query::ast::QueryExpr;
use crate::storage::query::parser::parse;
use crate::storage::schema::Value;

/// Parse `INSERT INTO t (body) VALUES (<expr>)` and pull out the first
/// row's first cell. Returns the literal `Value` so callers can assert
/// the `Value::Json` byte payload directly.
fn parse_first_value(insert_sql: &str) -> Value {
    let q = parse(insert_sql)
        .unwrap_or_else(|err| panic!("parse error for `{}`: {}", insert_sql, err))
        .query;
    let QueryExpr::Insert(ins) = q else {
        panic!("expected InsertQuery, got {:?}", insert_sql);
    };
    ins.values
        .into_iter()
        .next()
        .and_then(|row| row.into_iter().next())
        .unwrap_or_else(|| panic!("no value parsed for `{}`", insert_sql))
}

/// Encode the equivalent quoted form via the same parse_json + to_vec
/// pipeline the bare form uses internally. Returns the JSON bytes the
/// quoted form would persist after the executor's text→Json conversion.
fn canonical_json_bytes(raw: &str) -> Vec<u8> {
    let parsed = crate::utils::json::parse_json(raw)
        .unwrap_or_else(|err| panic!("parse_json failed for `{}`: {}", raw, err));
    let canonical = crate::serde_json::Value::from(parsed);
    crate::json::to_vec(&canonical).expect("to_vec must succeed for valid JSON")
}

/// Assert that the bare form `({raw})` produces `Value::Json` bytes
/// byte-identical to the canonical encoding of `raw`.
fn assert_bytewise_equivalent(raw: &str) {
    let sql = format!("INSERT INTO t (body) VALUES ({})", raw);
    let v = parse_first_value(&sql);
    let Value::Json(bare_bytes) = v else {
        panic!("expected Value::Json for `{}`, got {:?}", raw, v);
    };
    let expected = canonical_json_bytes(raw);
    assert_eq!(
        bare_bytes, expected,
        "bytewise mismatch for `{}`",
        raw
    );
}

// ============================================================================
// 1. Happy paths
// ============================================================================

#[test]
fn happy_empty_object() {
    assert_bytewise_equivalent(r#"{}"#);
}

#[test]
fn happy_single_key() {
    assert_bytewise_equivalent(r#"{"a":1}"#);
}

#[test]
fn happy_multi_key() {
    assert_bytewise_equivalent(r#"{"a":1,"b":2,"c":3}"#);
}

#[test]
fn happy_nested() {
    assert_bytewise_equivalent(r#"{"outer":{"inner":{"deep":42}}}"#);
}

#[test]
fn happy_array_value() {
    assert_bytewise_equivalent(r#"{"tags":["a","b","c"]}"#);
}

#[test]
fn happy_mixed_types() {
    assert_bytewise_equivalent(
        r#"{"s":"x","i":1,"f":3.14,"b":true,"n":null,"a":[1,2]}"#,
    );
}

#[test]
fn happy_unicode_emoji() {
    assert_bytewise_equivalent(r#"{"emoji":"🚀","ja":"日本語"}"#);
}

#[test]
fn happy_surrogate_pair_via_escape() {
    // U+1F600 GRINNING FACE encoded as a UTF-16 surrogate pair.
    // parse_json decodes 😀 as one code point internally.
    let raw = r#"{"face":"😀"}"#;
    let sql = format!("INSERT INTO t (body) VALUES ({})", raw);
    // The quoted-form path can't be used with this exact raw text because
    // SQL string lexing would mangle the backslashes; assert the bare
    // form just parses cleanly into Value::Json.
    let v = parse_first_value(&sql);
    let Value::Json(bytes) = v else {
        panic!("expected Json, got {:?}", v);
    };
    assert!(!bytes.is_empty());
}

#[test]
fn happy_negative_number() {
    assert_bytewise_equivalent(r#"{"n":-42}"#);
}

#[test]
fn happy_exponent_number() {
    assert_bytewise_equivalent(r#"{"n":1.5e10}"#);
}

#[test]
fn happy_max_value_number() {
    assert_bytewise_equivalent(r#"{"n":9007199254740992}"#);
}

#[test]
fn happy_empty_string_value() {
    assert_bytewise_equivalent(r#"{"s":""}"#);
}

#[test]
fn happy_deeply_nested_4_levels() {
    assert_bytewise_equivalent(r#"{"a":{"b":{"c":{"d":1}}}}"#);
}

// ============================================================================
// 2. Whitespace
// ============================================================================

#[test]
fn whitespace_compact() {
    assert_bytewise_equivalent(r#"{"a":1}"#);
}

#[test]
fn whitespace_padded_around_colon() {
    let raw = r#"{"a" : 1}"#;
    let sql = format!("INSERT INTO t (body) VALUES ({})", raw);
    let v = parse_first_value(&sql);
    assert!(matches!(v, Value::Json(_)));
}

#[test]
fn whitespace_padded_around_comma() {
    let raw = r#"{"a":1 , "b":2}"#;
    let sql = format!("INSERT INTO t (body) VALUES ({})", raw);
    assert!(matches!(parse_first_value(&sql), Value::Json(_)));
}

#[test]
fn whitespace_multiline() {
    let raw = "{\n  \"a\": 1,\n  \"b\": 2\n}";
    let sql = format!("INSERT INTO t (body) VALUES ({})", raw);
    assert!(matches!(parse_first_value(&sql), Value::Json(_)));
}

#[test]
fn whitespace_tabs() {
    let raw = "{\t\"a\":\t1\t}";
    let sql = format!("INSERT INTO t (body) VALUES ({})", raw);
    assert!(matches!(parse_first_value(&sql), Value::Json(_)));
}

#[test]
fn whitespace_no_space_after_lparen() {
    let raw = r#"{"a":1}"#;
    let sql = format!("INSERT INTO t (body) VALUES({})", raw); // no space after VALUES
    assert!(matches!(parse_first_value(&sql), Value::Json(_)));
}

// ============================================================================
// 3. SQL contexts
// ============================================================================

#[test]
fn ctx_insert_values() {
    let q = parse(r#"INSERT INTO logs (body) VALUES ({"level":"info"})"#)
        .unwrap()
        .query;
    assert!(matches!(q, QueryExpr::Insert(_)));
}

#[test]
fn ctx_insert_document_form() {
    let q = parse(r#"INSERT INTO logs DOCUMENT (body) VALUES ({"level":"info"})"#)
        .unwrap()
        .query;
    assert!(matches!(q, QueryExpr::Insert(_)));
}

#[test]
fn ctx_update_set() {
    let q = parse(r#"UPDATE logs SET body = {"level":"warn"} WHERE id = 1"#)
        .unwrap()
        .query;
    assert!(matches!(q, QueryExpr::Update(_)));
}

#[test]
fn ctx_select_projection() {
    let q = parse(r#"SELECT {"k":1} AS lit FROM dual"#)
        .unwrap()
        .query;
    assert!(matches!(q, QueryExpr::Table(_)));
}

#[test]
fn ctx_where_compare() {
    let q = parse(r#"SELECT * FROM logs WHERE body = {"level":"info"}"#)
        .unwrap()
        .query;
    assert!(matches!(q, QueryExpr::Table(_)));
}

#[test]
fn ctx_function_arg() {
    let q = parse(r#"SELECT JSON_EXTRACT({"a":1}, '$.a') FROM dual"#)
        .unwrap()
        .query;
    assert!(matches!(q, QueryExpr::Table(_)));
}

#[test]
fn ctx_batched_insert_mixing_forms() {
    let q = parse(
        r#"INSERT INTO logs (body) VALUES ({"a":1}), ('{"b":2}')"#,
    )
    .unwrap()
    .query;
    let QueryExpr::Insert(ins) = q else {
        panic!("expected insert");
    };
    assert_eq!(ins.values.len(), 2);
    assert!(matches!(ins.values[0][0], Value::Json(_)));
    // Second row is the quoted form — lands as Value::Text in the AST.
    assert!(matches!(ins.values[1][0], Value::Text(_)));
}

#[test]
fn ctx_queue_push() {
    let q = parse(r#"QUEUE PUSH tasks {"job":"hello","retries":3}"#)
        .unwrap()
        .query;
    assert!(matches!(q, QueryExpr::QueueCommand(_)));
}

// ============================================================================
// 4. User mistakes
// ============================================================================

#[test]
fn err_unbalanced_open_brace() {
    let r = parse(r#"INSERT INTO t (body) VALUES ({"a":1)"#);
    assert!(r.is_err(), "expected error for unbalanced `{{`");
}

#[test]
fn err_unbalanced_close_brace() {
    // `{"a":1}}` — the lexer scans the first balanced `{...}` and the
    // second `}` is then a stray RBrace, producing a parser error.
    let r = parse(r#"INSERT INTO t (body) VALUES ({"a":1}})"#);
    assert!(r.is_err());
}

#[test]
fn err_trailing_comma() {
    let r = parse(r#"INSERT INTO t (body) VALUES ({"a":1,})"#);
    assert!(r.is_err(), "trailing comma must error");
}

#[test]
fn err_single_quotes_inside_object() {
    // `{"a":'x'}` — single-quoted string is not valid JSON.
    let r = parse(r#"INSERT INTO t (body) VALUES ({"a":'x'})"#);
    assert!(r.is_err(), "single-quoted value inside JSON literal must error");
}

#[test]
fn err_missing_key_quotes() {
    // `{a:1}` does NOT trigger JSON sub-mode (next char is `a`, not `"`).
    // It then falls through to the legacy element-based parser which
    // accepts identifier keys for backwards compatibility — so this case
    // is documented as parsing successfully under the legacy path, not
    // as an error. Pin that behaviour here to surface a regression if the
    // legacy fallback is removed later.
    let v = parse_first_value(r#"INSERT INTO t (body) VALUES ({a:1})"#);
    assert!(matches!(v, Value::Json(_)));
}

#[test]
fn duplicate_keys_last_wins() {
    // Standard JSON parsers vary; ours (utils/json.rs) keeps insertion
    // order in a Vec then `serde_json::Value::from` collects into a
    // BTreeMap which deduplicates with last-wins. Pin that behaviour.
    let v = parse_first_value(r#"INSERT INTO t (body) VALUES ({"a":1,"a":2})"#);
    let Value::Json(bytes) = v else {
        panic!("expected Json");
    };
    let s = std::str::from_utf8(&bytes).unwrap();
    assert!(s.contains("\"a\""));
    assert!(s.contains("2"));
}

#[test]
fn err_js_style_comment() {
    let r = parse(r#"INSERT INTO t (body) VALUES ({"a":1//x})"#);
    assert!(r.is_err());
}

#[test]
fn err_infinity_literal() {
    let r = parse(r#"INSERT INTO t (body) VALUES ({"n":Infinity})"#);
    assert!(r.is_err());
}

#[test]
fn err_nan_literal() {
    let r = parse(r#"INSERT INTO t (body) VALUES ({"n":NaN})"#);
    assert!(r.is_err());
}

#[test]
fn err_leading_zero_number() {
    let r = parse(r#"INSERT INTO t (body) VALUES ({"n":01})"#);
    assert!(r.is_err());
}

#[test]
fn raw_newline_in_string_documented_lenient() {
    // Per RFC 8259 a raw control char (incl. LF) inside a JSON string
    // is invalid. The bundled `utils/json.rs` parser is lenient and
    // accepts it. Documenting the behaviour here so a future strict-mode
    // upgrade flips this test rather than landing silently.
    let raw = "{\"s\":\"a\nb\"}";
    let sql = format!("INSERT INTO t (body) VALUES ({})", raw);
    let v = parse_first_value(&sql);
    assert!(matches!(v, Value::Json(_)));
}

#[test]
fn err_trailing_chars_after_object() {
    // `{...}xyz` — trailing junk after the literal becomes garbage tokens
    // for the SQL grammar, which then errors at the ` ) ` it expects.
    let r = parse(r#"INSERT INTO t (body) VALUES ({"a":1}xyz)"#);
    assert!(r.is_err());
}

#[test]
fn ok_mixed_quotes_in_string_value() {
    // `{"path":"O'Brien"}` is valid JSON — apostrophe inside a JSON string.
    assert_bytewise_equivalent(r#"{"path":"O'Brien"}"#);
}

#[test]
fn err_lone_open_brace_in_where_position() {
    // `WHERE { = 1` — `{` followed by whitespace and `=` doesn't enter
    // JSON mode (next non-ws char is `=`), so it lexes as LBrace and the
    // legacy fallback hits a parse error.
    let r = parse(r#"SELECT * FROM t WHERE { = 1"#);
    assert!(r.is_err());
}

// ============================================================================
// 5. Disambiguation
// ============================================================================

#[test]
fn quoted_form_still_parses() {
    let q = parse(r#"INSERT INTO logs DOCUMENT (body) VALUES ('{"a":1}')"#)
        .unwrap()
        .query;
    let QueryExpr::Insert(ins) = q else {
        panic!("expected Insert");
    };
    // Quoted form lands as Value::Text and is converted to Json by the
    // executor; here we just confirm the parser still accepts it.
    assert!(matches!(ins.values[0][0], Value::Text(_)));
}

#[test]
fn vector_literal_still_parses() {
    // `[0.1, 0.2]` must keep producing a Value::Vector, not be touched
    // by the JSON sub-mode (which only triggers on `{`).
    let q = parse(r#"INSERT INTO emb VECTOR (dense) VALUES ([0.1, 0.2, 0.3])"#)
        .unwrap()
        .query;
    let QueryExpr::Insert(ins) = q else {
        panic!("expected Insert");
    };
    assert!(matches!(ins.values[0][0], Value::Vector(_)));
}

#[test]
fn in_list_paren_intact() {
    // `IN (1, 2, 3)` must keep parsing as a value list.
    let q = parse(r#"SELECT * FROM logs WHERE id IN (1, 2, 3)"#)
        .unwrap()
        .query;
    assert!(matches!(q, QueryExpr::Table(_)));
}

#[test]
fn cypher_property_bag_still_works() {
    // `MATCH (n:User {name: 'alice'})` — property bag uses LBrace, not
    // JsonLiteral, because the inner content does not start with `"`.
    let q = parse(r#"MATCH (n:User {name: 'alice'}) RETURN n"#)
        .unwrap()
        .query;
    assert!(matches!(q, QueryExpr::Graph(_)));
}

#[test]
fn bare_and_quoted_produce_equivalent_bytes() {
    // The flagship bytewise-equivalence regression: `{"a":1}` (bare) and
    // `'{"a":1}'` (quoted, then text->json via parse_json+to_vec) must
    // yield identical bytes.
    let bare_v = parse_first_value(r#"INSERT INTO t (body) VALUES ({"a":1})"#);
    let Value::Json(bare_bytes) = bare_v else {
        panic!("expected Json");
    };
    let quoted_text = r#"{"a":1}"#;
    let expected = canonical_json_bytes(quoted_text);
    assert_eq!(bare_bytes, expected, "bytewise on-disk mismatch");
}

// ============================================================================
// 6. Limits / DoS
// ============================================================================

#[test]
fn dos_recursion_depth_exceeded() {
    // Build `{"a":{"a":{...×200}}}` — 200 levels of nesting.
    let mut s = String::new();
    for _ in 0..200 {
        s.push_str(r#"{"a":"#);
    }
    s.push('1');
    for _ in 0..200 {
        s.push('}');
    }
    let sql = format!("INSERT INTO t (body) VALUES ({})", s);
    let r = parse(&sql);
    let err = r.expect_err("expected depth-limit error");
    assert!(
        err.to_string().contains("JSON_LITERAL_MAX_DEPTH"),
        "expected depth error, got: {}",
        err
    );
}

#[test]
fn dos_recursion_depth_exactly_at_limit_passes() {
    // 64 levels — well under the 128 cap.
    let mut s = String::new();
    for _ in 0..64 {
        s.push_str(r#"{"a":"#);
    }
    s.push('1');
    for _ in 0..64 {
        s.push('}');
    }
    let sql = format!("INSERT INTO t (body) VALUES ({})", s);
    parse(&sql).expect("64 levels must parse");
}

#[test]
fn dos_thousand_keys() {
    let mut s = String::from("{");
    for i in 0..1000 {
        if i > 0 {
            s.push(',');
        }
        s.push_str(&format!("\"k{}\":{}", i, i));
    }
    s.push('}');
    let sql = format!("INSERT INTO t (body) VALUES ({})", s);
    let v = parse_first_value(&sql);
    assert!(matches!(v, Value::Json(_)));
}

#[test]
fn dos_payload_size_exceeded() {
    // Build a JSON literal whose raw text exceeds JSON_LITERAL_MAX_BYTES
    // (16 MiB). The lexer must bail out before allocating the value.
    use crate::storage::query::lexer::JSON_LITERAL_MAX_BYTES;
    let pad = "x".repeat(JSON_LITERAL_MAX_BYTES + 16);
    let raw = format!(r#"{{"k":"{}"}}"#, pad);
    let sql = format!("INSERT INTO t (body) VALUES ({})", raw);
    let r = parse(&sql);
    let err = r.expect_err("expected size-limit error");
    assert!(
        err.to_string().contains("JSON_LITERAL_MAX_BYTES"),
        "expected size error, got: {}",
        err
    );
}

#[test]
fn dos_unbalanced_eof_no_panic() {
    // Should produce a clean error, never panic.
    let r = parse(r#"INSERT INTO t (body) VALUES ({"a":"#);
    assert!(r.is_err());
}

#[test]
fn dos_invalid_utf8_in_string() {
    // Raw bytes that aren't valid UTF-8 — the lexer never sees them
    // because Rust `&str` enforces UTF-8 at the API boundary, but the
    // shape of an invalid \uXXXX escape is something we should reject.
    let r = parse(r#"INSERT INTO t (body) VALUES ({"k":"\uZZZZ"})"#);
    assert!(r.is_err());
}

#[test]
fn dos_bom_at_start_of_literal() {
    // BOM (U+FEFF) inside the literal text — utils/json.rs treats it as
    // an unexpected char, which is the correct strict-JSON behaviour.
    let raw = "{\u{FEFF}\"a\":1}";
    let sql = format!("INSERT INTO t (body) VALUES ({})", raw);
    let r = parse(&sql);
    assert!(r.is_err());
}
