//! Proptest strategies that emit syntactically valid ASK / AI-extension
//! statements (issue #101).
//!
//! Mirrors the layout of `sql_grammar.rs` and `migration_grammar.rs`:
//! each strategy returns a `String` that, when fed back through
//! `parser::parse`, must not panic. Valid-shape strategies must
//! additionally succeed.
//!
//! The ASK grammar covers two top-level RAG / AI surfaces:
//!   - `ASK 'question' [USING provider] [MODEL 'model']
//!      [DEPTH n] [LIMIT n] [COLLECTION col]`  (any-order clauses)
//!   - `SEARCH CONTEXT 'query' [FIELD field] [COLLECTION col]
//!      [LIMIT n] [DEPTH n]`
//!
//! The string-literal slot for the question is a particularly sharp
//! edge: the tokenizer rejects single quotes inside the body, and an
//! adversarial caller can stuff provider names + model names into
//! error-rendering paths. Generators stay narrow enough to round-trip
//! cleanly so the property suite catches grammar holes without
//! drowning in false positives. Adversarial edges live in
//! `corpus::ask_adversarial_inputs`.

use proptest::prelude::*;

/// Identifier suitable for a provider / collection name. Stays well
/// below the `max_identifier_chars` cap.
pub fn ident() -> impl Strategy<Value = String> {
    "id_[a-z0-9_]{0,12}".prop_map(|s| s)
}

/// A small string literal (single-quoted, no embedded single quotes).
/// Used for both `ASK '<question>'` and `MODEL '<name>'` slots.
pub fn str_lit() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 ?.!,_-]{1,32}".prop_map(|s| format!("'{}'", s))
}

/// Provider name strategy. The grammar accepts any bare identifier
/// for `USING`, but in practice the runtime dispatches against a
/// known set; pinning a representative pool keeps generated inputs
/// realistic without constraining the parser surface.
pub fn provider_name() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("openai".to_string()),
        Just("groq".to_string()),
        Just("anthropic".to_string()),
        Just("ollama".to_string()),
        Just("bedrock".to_string()),
        ident(),
    ]
}

/// Model name strategy. The parser slot is a string literal (not an
/// identifier), so generated inputs always render quoted. Mixes the
/// most common production model identifiers with random short
/// strings so error paths formatting the model name flow through the
/// secret redactor under proptest.
pub fn model_name() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("'gpt-4o-mini'".to_string()),
        Just("'gpt-4o'".to_string()),
        Just("'claude-3-5-sonnet'".to_string()),
        Just("'llama-3.1-70b-versatile'".to_string()),
        Just("'mixtral-8x7b'".to_string()),
        str_lit(),
    ]
}

/// Small positive integer for DEPTH / LIMIT clauses.
pub fn small_uint() -> impl Strategy<Value = u32> {
    1u32..200
}

/// `ASK 'question' [MODEL 'model'] [DEPTH n] [LIMIT n]
/// [COLLECTION col]`.
///
/// All optional clauses are independently generated. The order
/// follows the documented RAG syntax. **`USING` is intentionally
/// omitted from this strategy** — see `ask_using_provider_stmt`
/// below for the FIXME(#101) note: `USING` lexes to
/// `Token::Using`, but `parse_ask_query` calls
/// `consume_ident_ci("USING")` which only matches `Token::Ident`,
/// so the clause currently fails to parse. Generators here emit
/// only the shapes the parser accepts today.
pub fn ask_stmt() -> impl Strategy<Value = String> {
    (
        str_lit(),
        proptest::option::of(model_name()),
        proptest::option::of(small_uint()),
        proptest::option::of(small_uint()),
        proptest::option::of(ident()),
    )
        .prop_map(|(question, model, depth, limit, collection)| {
            let mut s = format!("ASK {}", question);
            if let Some(m) = model {
                s.push_str(&format!(" MODEL {}", m));
            }
            if let Some(d) = depth {
                s.push_str(&format!(" DEPTH {}", d));
            }
            if let Some(l) = limit {
                s.push_str(&format!(" LIMIT {}", l));
            }
            if let Some(c) = collection {
                s.push_str(&format!(" COLLECTION {}", c));
            }
            s
        })
}

/// `ASK 'question' USING <provider>`. Because of FIXME(#101), this
/// shape currently fails to parse — `USING` is `Token::Using`, but
/// the optional-clause loop in `parse_ask_query` (parser/dml.rs:402)
/// uses `consume_ident_ci("USING")` which only matches
/// `Token::Ident`. The generator is exposed so the property suite
/// can pin `roundtrip_property` (no panic) on the broken shape until
/// the parser is fixed in a follow-up issue. See
/// `proptest_ask_using_provider_no_panic` in `ask_parser.rs`.
pub fn ask_using_provider_stmt() -> impl Strategy<Value = String> {
    (str_lit(), provider_name())
        .prop_map(|(q, p)| format!("ASK {} USING {}", q, p))
}

/// `ASK 'question' MODEL '...'` — pins the string-literal slot for
/// `MODEL`. Production models are dotted / dashed identifiers that
/// would not parse as bare `Token::Ident`, so the parser requires
/// quoting; this strategy enforces that contract.
pub fn ask_model_ident_stmt() -> impl Strategy<Value = String> {
    (str_lit(), model_name())
        .prop_map(|(q, m)| format!("ASK {} MODEL {}", q, m))
}

/// `SEARCH CONTEXT '<query>' [FIELD field] [COLLECTION col]
/// [LIMIT n] [DEPTH n]`.
///
/// FIELD is generated via a bare identifier (the parser uses
/// `consume_search_ident("FIELD")` which only matches an `Ident`
/// token). COLLECTION resolves to `Token::Collection` and so is
/// reserved.
pub fn search_context_stmt() -> impl Strategy<Value = String> {
    (
        str_lit(),
        proptest::option::of(ident()),
        proptest::option::of(ident()),
        proptest::option::of(small_uint()),
        proptest::option::of(small_uint()),
    )
        .prop_map(|(q, field, collection, limit, depth)| {
            let mut s = format!("SEARCH CONTEXT {}", q);
            if let Some(f) = field {
                s.push_str(&format!(" FIELD {}", f));
            }
            if let Some(c) = collection {
                s.push_str(&format!(" COLLECTION {}", c));
            }
            if let Some(l) = limit {
                s.push_str(&format!(" LIMIT {}", l));
            }
            if let Some(d) = depth {
                s.push_str(&format!(" DEPTH {}", d));
            }
            s
        })
}

/// Strategy that focuses on the depth / limit numeric range. The
/// parser accepts `parse_integer` (i64-domain, signed) for both
/// slots; emitting wide values exercises the integer parsing path
/// alongside the keyword-dispatch loop.
pub fn ask_depth_scope_stmt() -> impl Strategy<Value = String> {
    (str_lit(), 0u32..10_000, 0u32..10_000)
        .prop_map(|(q, d, l)| format!("ASK {} DEPTH {} LIMIT {}", q, d, l))
}

/// Top-level union: any of the ASK / SEARCH CONTEXT shapes.
pub fn any_ask_stmt() -> impl Strategy<Value = String> {
    prop_oneof![
        ask_stmt(),
        ask_using_provider_stmt(),
        ask_model_ident_stmt(),
        search_context_stmt(),
        ask_depth_scope_stmt(),
    ]
}
