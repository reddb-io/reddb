//! Pinned vector-search parse-error snapshots and happy-path
//! regressions (issue #100).
//!
//! Mirrors `parser_snapshots.rs` (#87) and
//! `migration_parser_snapshots.rs` (#88) for the vector-search
//! grammar surface:
//!   - `SEARCH SIMILAR [floats…]`
//!   - `SEARCH SIMILAR TEXT 'q'`
//!   - `SEARCH HYBRID …`
//!   - `VECTOR SEARCH … SIMILAR TO …`
//!   - `HYBRID FROM … VECTOR SEARCH … FUSION …`
//!   - `INSERT … WITH AUTO EMBED (…) [USING …] [MODEL '…']`
//!
//! Every snapshot test installs the shared secret-redactor (#98)
//! before calling `insta::assert_snapshot!`, so provider names and
//! model identifiers — which are sometimes secret-shaped — never
//! reach a `*.snap` file unmasked.
//!
//! Workflow:
//!   - First run: `cargo insta accept` records the new outputs.
//!   - Reviewing changes: `cargo insta review`.
//!   - CI: snapshots must match exactly.

mod support {
    pub mod parser_hardening;
}

use reddb_server::storage::query::ast::{FusionStrategy, QueryExpr, SearchCommand, VectorSource};
use reddb_server::storage::query::parser;
use support::parser_hardening::secret_redactor;

/// Parse `input` and format the resulting error for snapshotting.
/// Successful parses render as `UNEXPECTED OK` so a missing error
/// path is visible in the diff.
fn fmt_parse_error(input: &str) -> String {
    match parser::parse(input) {
        Ok(_) => format!("UNEXPECTED OK\ninput: {:?}\n", input),
        Err(e) => format!("input: {:?}\nkind:  {:?}\nerror: {}\n", input, e.kind, e),
    }
}

/// Wrapper that pins both the install_redactions guard and the
/// snapshot name. Every snapshot test below uses this macro so the
/// redaction guard can never be accidentally omitted (#98 + #100).
macro_rules! snap_redacted {
    ($name:ident, $input:expr) => {
        #[test]
        fn $name() {
            let _g = secret_redactor::install_redactions();
            insta::assert_snapshot!(stringify!($name), fmt_parse_error($input));
        }
    };
}

// ============================================================
// SEARCH SIMILAR [vector] error scenarios
// ============================================================

snap_redacted!(search_similar_eof_after_keyword, "SEARCH SIMILAR");
snap_redacted!(
    search_similar_unterminated_literal,
    "SEARCH SIMILAR [0.1, 0.2 COLLECTION c"
);
snap_redacted!(
    search_similar_dangling_comma_in_literal,
    "SEARCH SIMILAR [0.1, 0.2,] COLLECTION c"
);
snap_redacted!(
    search_similar_no_collection_keyword,
    "SEARCH SIMILAR [0.1, 0.2]"
);
snap_redacted!(
    search_similar_eof_after_collection,
    "SEARCH SIMILAR [0.1, 0.2] COLLECTION"
);
snap_redacted!(
    search_similar_garbage_in_literal,
    "SEARCH SIMILAR [0.1, @#$, 0.3] COLLECTION c"
);
snap_redacted!(
    search_similar_nan_in_literal,
    "SEARCH SIMILAR [0.1, NaN, 0.3] COLLECTION c"
);
snap_redacted!(
    search_similar_inf_in_literal,
    "SEARCH SIMILAR [0.1, Infinity, 0.3] COLLECTION c"
);
snap_redacted!(
    search_similar_min_score_no_value,
    "SEARCH SIMILAR [0.1, 0.2] COLLECTION c MIN_SCORE"
);
snap_redacted!(
    search_similar_using_no_provider,
    "SEARCH SIMILAR [0.1, 0.2] COLLECTION c USING"
);

// ============================================================
// SEARCH SIMILAR TEXT 'q' error scenarios
// ============================================================

snap_redacted!(
    search_similar_text_no_string,
    "SEARCH SIMILAR TEXT COLLECTION c"
);
snap_redacted!(
    search_similar_text_unterminated,
    "SEARCH SIMILAR TEXT 'unterminated COLLECTION c"
);

// ============================================================
// SEARCH HYBRID error scenarios
// ============================================================

snap_redacted!(
    search_hybrid_neither_set,
    "SEARCH HYBRID COLLECTION c LIMIT 10"
);
snap_redacted!(search_hybrid_eof, "SEARCH HYBRID");

// ============================================================
// VECTOR SEARCH … SIMILAR TO error scenarios
// ============================================================

snap_redacted!(vector_search_eof_after_keyword, "VECTOR SEARCH");
snap_redacted!(vector_search_no_similar_to, "VECTOR SEARCH e [0.1, 0.2]");
snap_redacted!(
    vector_search_bad_metric,
    "VECTOR SEARCH e SIMILAR TO [0.1] METRIC GLORP LIMIT 5"
);
snap_redacted!(
    vector_search_include_garbage,
    "VECTOR SEARCH e SIMILAR TO [0.1] INCLUDE GLORP"
);

// ============================================================
// HYBRID FROM … FUSION error scenarios
// ============================================================

snap_redacted!(
    hybrid_from_no_fusion,
    "HYBRID FROM hosts VECTOR SEARCH e SIMILAR TO [0.1] LIMIT 10"
);
snap_redacted!(
    hybrid_from_unknown_fusion,
    "HYBRID FROM hosts VECTOR SEARCH e SIMILAR TO [0.1] FUSION GLORP LIMIT 10"
);

// ============================================================
// INSERT … WITH AUTO EMBED error scenarios
// ============================================================

snap_redacted!(
    auto_embed_using_no_provider,
    "INSERT INTO t (a) VALUES ('x') WITH AUTO EMBED (a) USING"
);
snap_redacted!(
    auto_embed_model_no_string,
    "INSERT INTO t (a) VALUES ('x') WITH AUTO EMBED (a) USING openai MODEL"
);
snap_redacted!(
    auto_embed_dangling_field_comma,
    "INSERT INTO t (a, b) VALUES ('x', 'y') WITH AUTO EMBED (a, b,)"
);

// ============================================================
// Happy-path regression tests
// ============================================================
//
// These are not snapshots — they assert the AST shape of correct
// inputs so a parser change that silently breaks the vector-search
// surface trips a precise assertion message instead of a snapshot
// diff. Mirrors the post-#92 happy-path coverage in
// `migration_parser.rs`.

fn parse_query(input: &str) -> QueryExpr {
    parser::parse(input)
        .unwrap_or_else(|e| panic!("expected ok for {input:?}, got error: {e}"))
        .query
}

#[test]
fn happy_search_similar_vector_minimal() {
    let q = parse_query("SEARCH SIMILAR [0.1, 0.2, 0.3] COLLECTION embeddings");
    match q {
        QueryExpr::SearchCommand(SearchCommand::Similar {
            vector,
            text,
            collection,
            limit,
            min_score,
            provider,
            vector_param: _,
            limit_param: _,
            min_score_param: _,
        }) => {
            assert_eq!(vector.len(), 3);
            assert!(text.is_none());
            assert_eq!(collection, "embeddings");
            assert_eq!(limit, 10);
            assert_eq!(min_score, 0.0);
            assert!(provider.is_none());
        }
        other => panic!("expected SearchCommand::Similar, got {other:?}"),
    }
}

#[test]
fn happy_search_similar_vector_full_clauses() {
    // Kitchen-sink form: LIMIT + MIN_SCORE + USING. The USING
    // branch is the regression guard for bug #108 — provider must
    // round-trip via `Token::Using` rather than the keyword-vs-ident
    // consumer that previously dropped it silently.
    let q = parse_query(
        "SEARCH SIMILAR [0.1, 0.2] COLLECTION embeddings LIMIT 25 MIN_SCORE 0.75 USING openai",
    );
    match q {
        QueryExpr::SearchCommand(SearchCommand::Similar {
            collection,
            limit,
            min_score,
            provider,
            ..
        }) => {
            assert_eq!(collection, "embeddings");
            assert_eq!(limit, 25);
            assert!((min_score - 0.75).abs() < 1e-4);
            assert_eq!(provider.as_deref(), Some("openai"));
        }
        other => panic!("expected SearchCommand::Similar, got {other:?}"),
    }
}

#[test]
fn happy_search_similar_text_semantic() {
    let q = parse_query("SEARCH SIMILAR TEXT 'find vulnerabilities' COLLECTION cves LIMIT 5");
    match q {
        QueryExpr::SearchCommand(SearchCommand::Similar {
            vector,
            text,
            collection,
            limit,
            ..
        }) => {
            assert!(vector.is_empty());
            assert_eq!(text.as_deref(), Some("find vulnerabilities"));
            assert_eq!(collection, "cves");
            assert_eq!(limit, 5);
        }
        other => panic!("expected SearchCommand::Similar, got {other:?}"),
    }
}

#[test]
fn happy_search_hybrid_vector_and_text() {
    let q =
        parse_query("SEARCH HYBRID SIMILAR [0.1, 0.2] TEXT 'ssh exploit' COLLECTION svc LIMIT 30");
    match q {
        QueryExpr::SearchCommand(SearchCommand::Hybrid {
            vector,
            query,
            collection,
            limit,
        }) => {
            assert!(vector.is_some());
            assert_eq!(vector.as_ref().unwrap().len(), 2);
            assert_eq!(query.as_deref(), Some("ssh exploit"));
            assert_eq!(collection, "svc");
            assert_eq!(limit, 30);
        }
        other => panic!("expected SearchCommand::Hybrid, got {other:?}"),
    }
}

#[test]
fn happy_vector_search_full_clauses() {
    // VECTOR SEARCH e SIMILAR TO [0.1, 0.2, 0.3] METRIC COSINE THRESHOLD 0.5
    //   INCLUDE VECTORS INCLUDE METADATA LIMIT 100
    //
    // This is the canonical "kitchen sink" vector query from the
    // parser tests. Pinning the AST shape here means a regression
    // in any one of the optional clauses surfaces as a focused
    // assertion failure.
    let q = parse_query(
        "VECTOR SEARCH e SIMILAR TO [0.1, 0.2, 0.3] METRIC COSINE THRESHOLD 0.5 \
         INCLUDE VECTORS INCLUDE METADATA LIMIT 100",
    );
    match q {
        QueryExpr::Vector(v) => {
            assert_eq!(v.collection, "e");
            assert_eq!(v.k, 100);
            assert!(v.threshold.is_some());
            assert!(v.metric.is_some());
            assert!(v.include_vectors);
            assert!(v.include_metadata);
            match v.query_vector {
                VectorSource::Literal(values) => assert_eq!(values.len(), 3),
                other => panic!("expected Literal vector source, got {other:?}"),
            }
        }
        other => panic!("expected QueryExpr::Vector, got {other:?}"),
    }
}

#[test]
fn happy_vector_search_text_source() {
    let q = parse_query("VECTOR SEARCH cves SIMILAR TO 'remote code execution' LIMIT 5");
    match q {
        QueryExpr::Vector(v) => {
            assert_eq!(v.collection, "cves");
            assert_eq!(v.k, 5);
            match v.query_vector {
                VectorSource::Text(t) => assert_eq!(t, "remote code execution"),
                other => panic!("expected Text source, got {other:?}"),
            }
        }
        other => panic!("expected QueryExpr::Vector, got {other:?}"),
    }
}

#[test]
fn happy_hybrid_from_fusion_rerank() {
    let q = parse_query(
        "HYBRID FROM hosts VECTOR SEARCH e SIMILAR TO [0.1, 0.2] FUSION RERANK LIMIT 10",
    );
    match q {
        QueryExpr::Hybrid(h) => {
            assert_eq!(h.limit, Some(10));
            assert!(matches!(h.fusion, FusionStrategy::Rerank { .. }));
        }
        other => panic!("expected QueryExpr::Hybrid, got {other:?}"),
    }
}

#[test]
fn happy_hybrid_from_fusion_rrf_with_k() {
    let q =
        parse_query("HYBRID FROM hosts VECTOR SEARCH e SIMILAR TO [0.1] FUSION RRF(30) LIMIT 10");
    match q {
        QueryExpr::Hybrid(h) => match h.fusion {
            FusionStrategy::RRF { k } => assert_eq!(k, 30),
            other => panic!("expected RRF fusion, got {other:?}"),
        },
        other => panic!("expected QueryExpr::Hybrid, got {other:?}"),
    }
}

#[test]
fn happy_insert_auto_embed_default_provider() {
    // Default provider is "openai" (per dml.rs L196). USING is
    // omitted on purpose to pin the default-provider path.
    let q = parse_query("INSERT INTO docs (id, body) VALUES (1, 'hello') WITH AUTO EMBED (body)");
    match q {
        QueryExpr::Insert(i) => {
            let cfg = i.auto_embed.as_ref().expect("auto_embed must be set");
            assert_eq!(cfg.fields, vec!["body".to_string()]);
            assert_eq!(cfg.provider, "openai");
            assert!(cfg.model.is_none());
        }
        other => panic!("expected QueryExpr::Insert, got {other:?}"),
    }
}

// Regression guard for #108: `parse_with_clauses` now matches
// `USING` via `Token::Using` (the typed-keyword consumer), so the
// optional `USING <provider> MODEL '<m>'` suffix on
// `WITH AUTO EMBED` parses end-to-end.
#[test]
fn happy_insert_auto_embed_with_provider_and_model() {
    let q = parse_query(
        "INSERT INTO docs (id, body) VALUES (1, 'hello') \
         WITH AUTO EMBED (body) USING ollama MODEL 'nomic-embed-text'",
    );
    match q {
        QueryExpr::Insert(i) => {
            let cfg = i.auto_embed.as_ref().expect("auto_embed must be set");
            assert_eq!(cfg.fields, vec!["body".to_string()]);
            assert_eq!(cfg.provider, "ollama");
            assert_eq!(cfg.model.as_deref(), Some("nomic-embed-text"));
        }
        other => panic!("expected QueryExpr::Insert, got {other:?}"),
    }
}
