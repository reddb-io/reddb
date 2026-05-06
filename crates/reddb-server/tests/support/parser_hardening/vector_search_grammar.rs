//! Proptest strategies that emit syntactically valid vector-search
//! statements (issue #100).
//!
//! Mirrors the layout of `sql_grammar.rs` (#87) and
//! `migration_grammar.rs` (#88): each strategy returns a `String`
//! that, when fed through `parser::parse`, must not panic; valid-shape
//! strategies must additionally succeed.
//!
//! Surface covered:
//!   - `SEARCH SIMILAR [floats…] COLLECTION col [LIMIT n] [MIN_SCORE f] [USING provider]`
//!   - `SEARCH SIMILAR TEXT 'query' COLLECTION col [LIMIT n] [MIN_SCORE f] [USING provider]`
//!   - `SEARCH HYBRID [SIMILAR [...]] [TEXT '...'] COLLECTION col [LIMIT n]`
//!   - `VECTOR SEARCH collection SIMILAR TO ([...] | 'text') [WHERE …] [METRIC …] [LIMIT k]`
//!   - `INSERT INTO t (...) VALUES (...) WITH AUTO EMBED (col[, col]) [USING provider] [MODEL '...']`
//!   - `HYBRID FROM table VECTOR SEARCH col SIMILAR TO [...] FUSION strategy [LIMIT n]`
//!
//! Provider names + model identifiers are kept generic (`p_…`,
//! `m_…`) so the snapshot secret-redactor (`secret_redactor.rs`,
//! #98) never has to worry about a real OpenAI / Anthropic name
//! getting pinned to disk.

use proptest::prelude::*;

/// A simple identifier: starts with `id_` to dodge SQL reserved
/// words. Stays short to keep generated input well under
/// `max_identifier_chars`.
pub fn ident() -> impl Strategy<Value = String> {
    "id_[a-z0-9_]{0,10}".prop_map(|s| s)
}

/// A short collection name. Vector search collections are routinely
/// tagged things like `embeddings`, `docs_v2`, `cve_vec`; the lexer
/// treats these as `Token::Ident`, so the generic `ident()` shape is
/// sufficient.
pub fn collection_name() -> impl Strategy<Value = String> {
    "col_[a-z0-9_]{0,10}".prop_map(|s| s)
}

/// Generic "provider" identifier. Real providers are `openai`,
/// `anthropic`, `ollama`, …; generated names dodge the secret-redactor
/// by using a `p_` prefix.
pub fn provider_name() -> impl Strategy<Value = String> {
    "p_[a-z0-9]{1,8}".prop_map(|s| s)
}

/// Generic "model" identifier — same redactor-safe shape as
/// `provider_name`. Real model names like
/// `text-embedding-3-small` aren't secrets, but we keep generated
/// inputs synthetic so a future stricter redactor doesn't trip the
/// snapshot lint.
pub fn model_name() -> impl Strategy<Value = String> {
    "m_[a-z0-9]{1,10}".prop_map(|s| s)
}

/// A finite f32 rendered as a decimal, covering both positive and
/// negative values now that `Parser::parse_float` accepts a leading
/// unary `-` prefix (#107). The full `[-1000.0, 1000.0]` range
/// exercises the minus-prefix path inside vector literals,
/// `THRESHOLD`, `MIN_SCORE`, `RERANK(w)`, and `UNION(sw, vw)`.
///
/// Adversarial NaN / Infinity / oversized literals live in the
/// corpus and the snapshot suite — *not* here, where every emitted
/// string must parse cleanly.
pub fn finite_float_lit() -> impl Strategy<Value = String> {
    (-1000.0_f32..1000.0_f32).prop_map(|f| {
        // Force a decimal point so the lexer always picks the
        // `Token::Float` branch (some integers would otherwise
        // tokenise as `Token::Integer`, which the vector-literal
        // parser also accepts via `parse_float`).
        format!("{:.4}", f)
    })
}

/// A vector literal `[f1, f2, …, fn]` of `dim` floats. `dim` is
/// constrained at the call site so each strategy gets a specific
/// dimensional sweep (e.g. small-dim, mid-dim, dim-1).
pub fn vector_literal(dim_range: std::ops::RangeInclusive<usize>) -> impl Strategy<Value = String> {
    proptest::collection::vec(finite_float_lit(), dim_range).prop_map(|floats| {
        format!("[{}]", floats.join(", "))
    })
}

/// Quoted text query body. Escaping is sidestepped by restricting to
/// safe ASCII alphanumerics + space so the lexer's string scanner
/// never has to decide on a backslash.
pub fn text_query_lit() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 ]{1,32}".prop_map(|s| format!("'{}'", s))
}

// ============================================================
// Strategy 1: SEARCH SIMILAR [floats] (classic vector search)
// ============================================================

/// `SEARCH SIMILAR [v1, v2, …] COLLECTION col [LIMIT n] [MIN_SCORE f] [USING provider]`.
/// Dimensional sweep covers 1..32 — enough to catch single-element
/// degenerate cases, mid-sized embeddings, and the comma-loop edge
/// without bloating shrinking time.
pub fn search_similar_vector_stmt() -> impl Strategy<Value = String> {
    (
        vector_literal(1..=32),
        collection_name(),
        proptest::option::of(1u32..1000),
        proptest::option::of(finite_float_lit()),
        proptest::option::of(provider_name()),
    )
        .prop_map(|(vec_lit, coll, limit, min_score, provider)| {
            let mut s = format!("SEARCH SIMILAR {} COLLECTION {}", vec_lit, coll);
            if let Some(l) = limit {
                s.push_str(&format!(" LIMIT {}", l));
            }
            if let Some(ms) = min_score {
                s.push_str(&format!(" MIN_SCORE {}", ms));
            }
            if let Some(p) = provider {
                s.push_str(&format!(" USING {}", p));
            }
            s
        })
}

// ============================================================
// Strategy 2: SEARCH SIMILAR TEXT 'query' (semantic search)
// ============================================================

/// `SEARCH SIMILAR TEXT 'query' COLLECTION col [LIMIT n] [MIN_SCORE f] [USING provider]`.
pub fn search_similar_text_stmt() -> impl Strategy<Value = String> {
    (
        text_query_lit(),
        collection_name(),
        proptest::option::of(1u32..1000),
        proptest::option::of(finite_float_lit()),
        proptest::option::of(provider_name()),
    )
        .prop_map(|(text, coll, limit, min_score, provider)| {
            let mut s = format!("SEARCH SIMILAR TEXT {} COLLECTION {}", text, coll);
            if let Some(l) = limit {
                s.push_str(&format!(" LIMIT {}", l));
            }
            if let Some(ms) = min_score {
                s.push_str(&format!(" MIN_SCORE {}", ms));
            }
            if let Some(p) = provider {
                s.push_str(&format!(" USING {}", p));
            }
            s
        })
}

// ============================================================
// Strategy 3: INSERT … WITH AUTO EMBED USING <provider>
// ============================================================

/// `INSERT INTO t (col_0[, col_1]) VALUES ('a'[, 'b']) WITH AUTO EMBED (field…) [USING provider] [MODEL '…']`.
///
/// Generates 1..3 columns, matching values, 1..3 embed-target
/// fields drawn from the same column pool. The `USING` / `MODEL`
/// suffixes are independently optional so the proptest sweep
/// exercises each combination.
pub fn insert_auto_embed_stmt() -> impl Strategy<Value = String> {
    (
        ident(),
        1usize..=3,
        // Embed-target subset count; the resulting fields are the
        // first `n_embed` columns. Keeps the embed list always a
        // valid subset of declared columns.
        1usize..=3,
        proptest::option::of(provider_name()),
        proptest::option::of(model_name()),
    )
        .prop_map(|(table, n_cols, n_embed_raw, provider, model)| {
            let n_embed = n_embed_raw.min(n_cols);
            let cols: Vec<String> = (0..n_cols).map(|i| format!("col_{}", i)).collect();
            let vals: Vec<String> = (0..n_cols).map(|i| format!("'v_{}'", i)).collect();
            let embed_fields: Vec<String> = cols.iter().take(n_embed).cloned().collect();
            let mut s = format!(
                "INSERT INTO {} ({}) VALUES ({}) WITH AUTO EMBED ({})",
                table,
                cols.join(", "),
                vals.join(", "),
                embed_fields.join(", "),
            );
            if let Some(p) = provider {
                s.push_str(&format!(" USING {}", p));
            }
            if let Some(m) = model {
                s.push_str(&format!(" MODEL '{}'", m));
            }
            s
        })
}

// ============================================================
// Strategy 4: VECTOR SEARCH (full-form vector query)
// ============================================================

/// Distance metric token. Generated uppercase so the parser's
/// `Token::L2 / Cosine / InnerProduct` branches are exercised
/// directly (not the `Token::Ident` fallback).
pub fn metric_kw() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("L2".to_string()),
        Just("COSINE".to_string()),
        Just("INNER_PRODUCT".to_string()),
    ]
}

/// `VECTOR SEARCH col SIMILAR TO ([...] | 'text') [METRIC m] [THRESHOLD f] [LIMIT k]`.
///
/// Full-form vector query. Threshold + metric + limit + source kind
/// are independent dimensions so the case space hits every
/// combination over 256 cases. WHERE / INCLUDE clauses are covered
/// in the snapshot + happy-path tests rather than here, where we
/// want clean parses.
pub fn vector_search_stmt() -> impl Strategy<Value = String> {
    (
        collection_name(),
        // VectorSource: literal vec or text query
        prop_oneof![
            vector_literal(1..=16).prop_map(|v| v),
            text_query_lit(),
        ],
        proptest::option::of(metric_kw()),
        proptest::option::of(finite_float_lit()),
        proptest::option::of(1u32..1000),
    )
        .prop_map(|(coll, source, metric, threshold, limit)| {
            let mut s = format!("VECTOR SEARCH {} SIMILAR TO {}", coll, source);
            if let Some(m) = metric {
                s.push_str(&format!(" METRIC {}", m));
            }
            if let Some(t) = threshold {
                s.push_str(&format!(" THRESHOLD {}", t));
            }
            if let Some(l) = limit {
                s.push_str(&format!(" LIMIT {}", l));
            }
            s
        })
}

// ============================================================
// Strategy 5: SEARCH HYBRID combinations
// ============================================================

/// `SEARCH HYBRID [SIMILAR [v…]] [TEXT 'q'] COLLECTION col [LIMIT n]`.
///
/// At least one of SIMILAR / TEXT must be present (parser enforces
/// this as a runtime check). The strategy guarantees the constraint
/// by always emitting at least one of the two and letting the other
/// be optional.
pub fn search_hybrid_stmt() -> impl Strategy<Value = String> {
    (
        // 0=vector only, 1=text only, 2=both
        0u32..3,
        vector_literal(1..=16),
        text_query_lit(),
        collection_name(),
        proptest::option::of(1u32..1000),
    )
        .prop_map(|(mode, vec_lit, text, coll, limit)| {
            let mut s = "SEARCH HYBRID".to_string();
            match mode {
                0 => s.push_str(&format!(" SIMILAR {}", vec_lit)),
                1 => s.push_str(&format!(" TEXT {}", text)),
                _ => s.push_str(&format!(" SIMILAR {} TEXT {}", vec_lit, text)),
            }
            s.push_str(&format!(" COLLECTION {}", coll));
            if let Some(l) = limit {
                s.push_str(&format!(" LIMIT {}", l));
            }
            s
        })
}

// ============================================================
// Strategy 6: HYBRID FROM table VECTOR SEARCH … FUSION strategy
// ============================================================

/// Fusion strategy keyword. `RERANK` + `RRF` accept optional
/// parenthesised arguments; `INTERSECTION` is bare; `UNION` accepts
/// optional `(sw, vw)`. Generator covers every shape independently
/// so the proptest sweep visits each parser branch.
pub fn fusion_strategy() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("RERANK".to_string()),
        (0.0_f32..1.0).prop_map(|w| format!("RERANK({:.2})", w)),
        Just("RRF".to_string()),
        (1u32..200).prop_map(|k| format!("RRF({})", k)),
        Just("INTERSECTION".to_string()),
        Just("UNION".to_string()),
        (0.0_f32..1.0, 0.0_f32..1.0).prop_map(|(sw, vw)| format!("UNION({:.2}, {:.2})", sw, vw)),
        Just("FILTER_THEN_SEARCH".to_string()),
        Just("SEARCH_THEN_FILTER".to_string()),
    ]
}

/// `HYBRID FROM table VECTOR SEARCH col SIMILAR TO [...] FUSION strategy [LIMIT n]`.
pub fn hybrid_from_stmt() -> impl Strategy<Value = String> {
    (
        ident(),
        collection_name(),
        vector_literal(1..=8),
        fusion_strategy(),
        proptest::option::of(1u32..1000),
    )
        .prop_map(|(table, coll, vec_lit, fusion, limit)| {
            let mut s = format!(
                "HYBRID FROM {} VECTOR SEARCH {} SIMILAR TO {} FUSION {}",
                table, coll, vec_lit, fusion
            );
            if let Some(l) = limit {
                s.push_str(&format!(" LIMIT {}", l));
            }
            s
        })
}

// ============================================================
// Top-level union (mirrors `sql_grammar::any_stmt`)
// ============================================================

/// Any of the six vector-search shapes covered above. Useful for the
/// arbitrary-bytes / no-panic property that wants a uniform mix.
pub fn any_vector_stmt() -> impl Strategy<Value = String> {
    prop_oneof![
        search_similar_vector_stmt(),
        search_similar_text_stmt(),
        insert_auto_embed_stmt(),
        vector_search_stmt(),
        search_hybrid_stmt(),
        hybrid_from_stmt(),
    ]
}
