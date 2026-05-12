//! `PgWireAskRowEncoder` — pure encoder that turns an
//! [`AskResult`](super::ask_response_envelope::AskResult) into the
//! single-row Postgres-wire result set that #408 exposes to psycopg /
//! pgx / JDBC.
//!
//! Deep module: no I/O, no transport, no clock. Mirrors the slice-1
//! pattern used by #395, #396, #398, #400, #401, #402, #403, #405,
//! #406, #409, #411 — the contract is pinned in tests so the wiring
//! slice (extended-query handler in `wire::postgres`) cannot drop or
//! rename a column.
//!
//! ## Why a separate module
//!
//! The acceptance criteria for #408 demand that ASK over PG-wire
//! returns a single-row result set with stable columns and stable
//! OIDs across every PG client. Three different drivers (psycopg, pgx,
//! JDBC) read columns by index after a `RowDescription`; renaming or
//! reordering a column silently breaks every integration test in this
//! issue's blocker chain. Building the row in a tested deep module
//! keeps the wiring slice focused on "where do I hand these bytes to
//! the PG codec" and the contract here on "are the bytes right".
//!
//! ## Shape pinned by tests
//!
//! Twelve columns, alphabetised — exact same order as the JSON keys
//! [`AskResponseEnvelope::build`](super::ask_response_envelope::build)
//! emits. Sharing the order means a bridge that takes the envelope
//! JSON object and projects it column-wise stays index-aligned without
//! re-shuffling.
//!
//! | # | Name              | OID                  | Format                                      |
//! |---|-------------------|----------------------|---------------------------------------------|
//! | 0 | `answer`          | `text` (25)          | UTF-8                                       |
//! | 1 | `cache_hit`       | `bool` (16)          | `"t"` / `"f"`                               |
//! | 2 | `citations`       | `jsonb` (3802)       | compact JSON, marker-ascending              |
//! | 3 | `completion_tokens` | `int8` (20)        | decimal                                     |
//! | 4 | `cost_usd`        | `numeric` (1700)     | decimal, `f64::to_string` form              |
//! | 5 | `mode`            | `text` (25)          | `"strict"` / `"lenient"`                    |
//! | 6 | `model`           | `text` (25)          | UTF-8                                       |
//! | 7 | `prompt_tokens`   | `int8` (20)          | decimal                                     |
//! | 8 | `provider`        | `text` (25)          | UTF-8                                       |
//! | 9 | `retry_count`     | `int8` (20)          | decimal (0 or 1 per #395)                   |
//! |10 | `sources_flat`    | `jsonb` (3802)       | compact JSON, RRF order preserved           |
//! |11 | `validation`      | `jsonb` (3802)       | compact JSON `{errors, ok, warnings}`       |
//!
//! ## Why text format
//!
//! Binary format is opt-in via `Bind`'s result-column format codes.
//! The existing PG-wire surface (`wire::postgres::types`) only emits
//! text — see `value_to_pg_wire_bytes`. ASK rides the same codec, so
//! every cell here is a UTF-8 byte buffer.
//!
//! ## Why numeric for cost_usd
//!
//! PG `numeric` is the only PG type with arbitrary precision and no
//! representation loss across the wire. `float8` would round
//! `0.0000001` to scientific form and trip JDBC `BigDecimal` parsing.
//! `f64::to_string()` produces the canonical Rust form which PG's
//! `numeric` parser accepts verbatim.
//!
//! ## Why jsonb (not json)
//!
//! psycopg ≥ 3 maps `jsonb` to native dicts; pgx exposes a
//! `pgtype.JSONB` decoder; JDBC's PG driver routes `jsonb` to
//! `PGobject` with `getType() == "jsonb"`. Returning `jsonb` lets
//! every supported driver decode the column without an explicit cast.

use crate::runtime::ai::ask_response_envelope::{
    self, AskResult, Citation, SourceRow, Validation, ValidationError, ValidationWarning,
};
use crate::serde_json::{Map, Value};
use crate::wire::postgres::types::PgOid;

/// One column in the ASK PG-wire result set.
///
/// `name` is always a `&'static str` — the column set is fixed at
/// compile time and no user-supplied string ever appears here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColumnDesc {
    pub name: &'static str,
    pub oid: PgOid,
}

/// The single-row ASK PG-wire result set. The wiring slice hands
/// `columns` to the `RowDescription` codec and `cells` to the
/// `DataRow` codec (one row, then `CommandComplete`).
#[derive(Debug, Clone)]
pub struct AskRow {
    pub columns: Vec<ColumnDesc>,
    pub cells: Vec<Option<Vec<u8>>>,
}

/// Canonical column list, in wire order. Kept private so callers can't
/// pull out a stale copy and drift from `encode()`.
const COLUMNS: &[ColumnDesc] = &[
    ColumnDesc {
        name: "answer",
        oid: PgOid::Text,
    },
    ColumnDesc {
        name: "cache_hit",
        oid: PgOid::Bool,
    },
    ColumnDesc {
        name: "citations",
        oid: PgOid::Jsonb,
    },
    ColumnDesc {
        name: "completion_tokens",
        oid: PgOid::Int8,
    },
    ColumnDesc {
        name: "cost_usd",
        oid: PgOid::Numeric,
    },
    ColumnDesc {
        name: "mode",
        oid: PgOid::Text,
    },
    ColumnDesc {
        name: "model",
        oid: PgOid::Text,
    },
    ColumnDesc {
        name: "prompt_tokens",
        oid: PgOid::Int8,
    },
    ColumnDesc {
        name: "provider",
        oid: PgOid::Text,
    },
    ColumnDesc {
        name: "retry_count",
        oid: PgOid::Int8,
    },
    ColumnDesc {
        name: "sources_flat",
        oid: PgOid::Jsonb,
    },
    ColumnDesc {
        name: "validation",
        oid: PgOid::Jsonb,
    },
];

/// Encode an [`AskResult`] as the single-row PG-wire result set.
///
/// Deterministic: re-running on byte-equal input is byte-equal output
/// (pinned by `encode_is_deterministic_across_calls`). Required by the
/// ASK determinism contract (#400) and by the cache-hit path (#403)
/// where the cached PG row must equal the freshly-encoded one.
pub fn encode(result: &AskResult) -> AskRow {
    let cells = vec![
        Some(result.answer.as_bytes().to_vec()),
        Some(bool_cell(result.cache_hit)),
        Some(citations_jsonb(&result.citations)),
        Some(result.completion_tokens.to_string().into_bytes()),
        Some(numeric_cell(result.cost_usd)),
        Some(mode_cell(result.effective_mode)),
        Some(result.model.as_bytes().to_vec()),
        Some(result.prompt_tokens.to_string().into_bytes()),
        Some(result.provider.as_bytes().to_vec()),
        Some(result.retry_count.to_string().into_bytes()),
        Some(sources_jsonb(&result.sources_flat)),
        Some(validation_jsonb(&result.validation)),
    ];
    AskRow {
        columns: COLUMNS.to_vec(),
        cells,
    }
}

/// Column descriptors only — the wiring slice needs these before it
/// knows the result body (PG protocol: `RowDescription` is sent before
/// `Execute` finishes). Exposed so the `Parse`/`Describe` codepath can
/// answer without running the query.
pub fn columns() -> Vec<ColumnDesc> {
    COLUMNS.to_vec()
}

fn bool_cell(b: bool) -> Vec<u8> {
    if b {
        b"t".to_vec()
    } else {
        b"f".to_vec()
    }
}

fn mode_cell(m: ask_response_envelope::Mode) -> Vec<u8> {
    match m {
        ask_response_envelope::Mode::Strict => b"strict".to_vec(),
        ask_response_envelope::Mode::Lenient => b"lenient".to_vec(),
    }
}

fn numeric_cell(v: f64) -> Vec<u8> {
    // PG `numeric` text input accepts the same forms Rust's
    // `f64::to_string` emits, including `0`, `0.0000321`, and
    // scientific notation for very large values. Non-finite values
    // (`NaN`, `Inf`) cannot appear here — cost is always a
    // non-negative finite number per the cost-counter contract
    // (#401). If one does sneak through, PG will reject the row with
    // a parse error at the codec layer, which is the correct loud
    // failure mode.
    v.to_string().into_bytes()
}

fn citations_jsonb(cites: &[Citation]) -> Vec<u8> {
    // Marker-ascending order is the contract — same as
    // ask_response_envelope::citations_value. Re-implemented here
    // rather than reused so the JSON-shape tests in the envelope
    // module and the column-shape tests here can fail
    // independently: a wire shape regression points at one module,
    // not the seam between two.
    let mut sorted: Vec<Citation> = cites.to_vec();
    sorted.sort_by_key(|c| c.marker);
    let arr: Vec<Value> = sorted
        .iter()
        .map(|c| {
            let mut o = Map::new();
            o.insert("marker".into(), Value::Number(c.marker as f64));
            o.insert("urn".into(), Value::String(c.urn.clone()));
            Value::Object(o)
        })
        .collect();
    Value::Array(arr).to_string_compact().into_bytes()
}

fn sources_jsonb(rows: &[SourceRow]) -> Vec<u8> {
    let arr: Vec<Value> = rows
        .iter()
        .map(|r| {
            let mut o = Map::new();
            o.insert("payload".into(), Value::String(r.payload.clone()));
            o.insert("urn".into(), Value::String(r.urn.clone()));
            Value::Object(o)
        })
        .collect();
    Value::Array(arr).to_string_compact().into_bytes()
}

fn validation_jsonb(v: &Validation) -> Vec<u8> {
    let mut o = Map::new();
    o.insert(
        "errors".into(),
        Value::Array(v.errors.iter().map(error_value).collect()),
    );
    o.insert("ok".into(), Value::Bool(v.ok));
    o.insert(
        "warnings".into(),
        Value::Array(v.warnings.iter().map(warning_value).collect()),
    );
    Value::Object(o).to_string_compact().into_bytes()
}

fn warning_value(w: &ValidationWarning) -> Value {
    let mut o = Map::new();
    o.insert("detail".into(), Value::String(w.detail.clone()));
    o.insert("kind".into(), Value::String(w.kind.clone()));
    Value::Object(o)
}

fn error_value(e: &ValidationError) -> Value {
    let mut o = Map::new();
    o.insert("detail".into(), Value::String(e.detail.clone()));
    o.insert("kind".into(), Value::String(e.kind.clone()));
    Value::Object(o)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::ai::ask_response_envelope::{
        AskResult, Citation, Mode, SourceRow, Validation, ValidationError, ValidationWarning,
    };

    fn fixture() -> AskResult {
        AskResult {
            answer: "X is 42 [^1].".into(),
            sources_flat: vec![SourceRow {
                urn: "urn:reddb:row:1".into(),
                payload: "{\"k\":\"v\"}".into(),
            }],
            citations: vec![Citation {
                marker: 1,
                urn: "urn:reddb:row:1".into(),
            }],
            validation: Validation {
                ok: true,
                warnings: vec![],
                errors: vec![],
            },
            cache_hit: false,
            provider: "openai".into(),
            model: "gpt-4o-mini".into(),
            prompt_tokens: 123,
            completion_tokens: 45,
            cost_usd: 0.000_321,
            effective_mode: Mode::Strict,
            retry_count: 0,
        }
    }

    fn cell_str(row: &AskRow, idx: usize) -> &str {
        std::str::from_utf8(row.cells[idx].as_ref().unwrap()).unwrap()
    }

    #[test]
    fn emits_exactly_twelve_columns() {
        let row = encode(&fixture());
        assert_eq!(row.columns.len(), 12);
        assert_eq!(row.cells.len(), 12);
    }

    #[test]
    fn column_order_matches_envelope_alphabetical_order() {
        let row = encode(&fixture());
        let names: Vec<&str> = row.columns.iter().map(|c| c.name).collect();
        assert_eq!(
            names,
            vec![
                "answer",
                "cache_hit",
                "citations",
                "completion_tokens",
                "cost_usd",
                "mode",
                "model",
                "prompt_tokens",
                "provider",
                "retry_count",
                "sources_flat",
                "validation",
            ]
        );
    }

    #[test]
    fn columns_helper_returns_same_descriptors_as_encode() {
        let row = encode(&fixture());
        assert_eq!(columns(), row.columns);
    }

    #[test]
    fn oids_match_pg_type_d_h_canonical_values() {
        let row = encode(&fixture());
        let by_name: std::collections::BTreeMap<&str, PgOid> =
            row.columns.iter().map(|c| (c.name, c.oid)).collect();
        assert_eq!(by_name["answer"], PgOid::Text);
        assert_eq!(by_name["cache_hit"], PgOid::Bool);
        assert_eq!(by_name["citations"], PgOid::Jsonb);
        assert_eq!(by_name["completion_tokens"], PgOid::Int8);
        assert_eq!(by_name["cost_usd"], PgOid::Numeric);
        assert_eq!(by_name["mode"], PgOid::Text);
        assert_eq!(by_name["model"], PgOid::Text);
        assert_eq!(by_name["prompt_tokens"], PgOid::Int8);
        assert_eq!(by_name["provider"], PgOid::Text);
        assert_eq!(by_name["retry_count"], PgOid::Int8);
        assert_eq!(by_name["sources_flat"], PgOid::Jsonb);
        assert_eq!(by_name["validation"], PgOid::Jsonb);
    }

    #[test]
    fn answer_text_preserved_with_inline_markers() {
        let row = encode(&fixture());
        assert_eq!(cell_str(&row, 0), "X is 42 [^1].");
    }

    #[test]
    fn cache_hit_serializes_as_pg_bool_text() {
        let mut r = fixture();
        r.cache_hit = false;
        assert_eq!(cell_str(&encode(&r), 1), "f");
        r.cache_hit = true;
        assert_eq!(cell_str(&encode(&r), 1), "t");
    }

    #[test]
    fn citations_jsonb_is_marker_ascending() {
        let mut r = fixture();
        r.citations = vec![
            Citation {
                marker: 3,
                urn: "urn:c".into(),
            },
            Citation {
                marker: 1,
                urn: "urn:a".into(),
            },
            Citation {
                marker: 2,
                urn: "urn:b".into(),
            },
        ];
        let row = encode(&r);
        let s = cell_str(&row, 2);
        // Markers appear in ascending order within the JSON array.
        let p1 = s.find("\"marker\":1").unwrap();
        let p2 = s.find("\"marker\":2").unwrap();
        let p3 = s.find("\"marker\":3").unwrap();
        assert!(p1 < p2 && p2 < p3, "order: {s}");
    }

    #[test]
    fn empty_citations_serialize_as_empty_array_not_null() {
        let mut r = fixture();
        r.citations = vec![];
        assert_eq!(cell_str(&encode(&r), 2), "[]");
    }

    #[test]
    fn completion_tokens_is_decimal_text() {
        let mut r = fixture();
        r.completion_tokens = 0;
        assert_eq!(cell_str(&encode(&r), 3), "0");
        r.completion_tokens = 1_000_000;
        assert_eq!(cell_str(&encode(&r), 3), "1000000");
    }

    #[test]
    fn cost_usd_uses_canonical_rust_f64_form() {
        let mut r = fixture();
        r.cost_usd = 0.0;
        assert_eq!(cell_str(&encode(&r), 4), "0");
        r.cost_usd = 0.000_321;
        // f64::to_string preserves a representation PG `numeric`
        // accepts directly — no precision loss across the wire.
        assert_eq!(cell_str(&encode(&r), 4), "0.000321");
        r.cost_usd = 1.5;
        assert_eq!(cell_str(&encode(&r), 4), "1.5");
    }

    #[test]
    fn mode_serialises_as_strict_or_lenient_text() {
        let mut r = fixture();
        r.effective_mode = Mode::Strict;
        assert_eq!(cell_str(&encode(&r), 5), "strict");
        r.effective_mode = Mode::Lenient;
        assert_eq!(cell_str(&encode(&r), 5), "lenient");
    }

    #[test]
    fn model_and_provider_are_utf8_text() {
        let mut r = fixture();
        r.model = "claude-sonnet-4-6".into();
        r.provider = "anthropic".into();
        let row = encode(&r);
        assert_eq!(cell_str(&row, 6), "claude-sonnet-4-6");
        assert_eq!(cell_str(&row, 8), "anthropic");
    }

    #[test]
    fn prompt_tokens_is_decimal_text() {
        let mut r = fixture();
        r.prompt_tokens = 4096;
        assert_eq!(cell_str(&encode(&r), 7), "4096");
    }

    #[test]
    fn retry_count_is_zero_or_one() {
        // #395 caps retry budget at one. We don't enforce here (the
        // validator does) but the column must round-trip both values.
        let mut r = fixture();
        r.retry_count = 0;
        assert_eq!(cell_str(&encode(&r), 9), "0");
        r.retry_count = 1;
        assert_eq!(cell_str(&encode(&r), 9), "1");
    }

    #[test]
    fn sources_flat_preserves_input_order() {
        // Post-RRF order is the contract — `[^N]` indexes into this
        // array, so reordering would silently break grounding.
        let mut r = fixture();
        r.sources_flat = vec![
            SourceRow {
                urn: "urn:z".into(),
                payload: "{}".into(),
            },
            SourceRow {
                urn: "urn:a".into(),
                payload: "{}".into(),
            },
        ];
        let row = encode(&r);
        let s = cell_str(&row, 10);
        let pz = s.find("urn:z").unwrap();
        let pa = s.find("urn:a").unwrap();
        assert!(pz < pa, "order: {s}");
    }

    #[test]
    fn empty_sources_flat_serializes_as_empty_array() {
        let mut r = fixture();
        r.sources_flat = vec![];
        assert_eq!(cell_str(&encode(&r), 10), "[]");
    }

    #[test]
    fn validation_jsonb_carries_ok_false_with_errors() {
        let mut r = fixture();
        r.validation = Validation {
            ok: false,
            warnings: vec![],
            errors: vec![ValidationError {
                kind: "out_of_range".into(),
                detail: "marker 5 > sources_count 2".into(),
            }],
        };
        let encoded = encode(&r);
        let s = cell_str(&encoded, 11);
        assert!(s.contains("\"ok\":false"), "got {s}");
        assert!(s.contains("\"kind\":\"out_of_range\""), "got {s}");
        assert!(s.contains("marker 5 > sources_count 2"), "got {s}");
    }

    #[test]
    fn validation_jsonb_with_warnings_only_keeps_ok_true() {
        let mut r = fixture();
        r.validation = Validation {
            ok: true,
            warnings: vec![ValidationWarning {
                kind: "mode_fallback".into(),
                detail: "provider does not support citations".into(),
            }],
            errors: vec![],
        };
        let encoded = encode(&r);
        let s = cell_str(&encoded, 11);
        assert!(s.contains("\"ok\":true"), "got {s}");
        assert!(s.contains("\"kind\":\"mode_fallback\""), "got {s}");
    }

    #[test]
    fn validation_empty_arrays_are_present_not_null() {
        let row = encode(&fixture());
        let s = cell_str(&row, 11);
        assert!(s.contains("\"errors\":[]"), "got {s}");
        assert!(s.contains("\"warnings\":[]"), "got {s}");
    }

    #[test]
    fn every_cell_is_some_no_nulls() {
        // PG-wire `DataRow` distinguishes NULL (length -1) from empty
        // string (length 0). The ASK row never emits NULL — empty
        // arrays serialize as `[]`, empty strings as `""`. The wiring
        // slice can rely on this invariant when streaming cells.
        let row = encode(&fixture());
        assert!(row.cells.iter().all(|c| c.is_some()));
    }

    #[test]
    fn encode_is_deterministic_across_calls() {
        let r = fixture();
        let a = encode(&r);
        let b = encode(&r);
        assert_eq!(a.columns, b.columns);
        assert_eq!(a.cells, b.cells);
    }

    #[test]
    fn cells_index_aligns_with_columns_index() {
        // The wiring slice will iterate `columns` and `cells` in
        // lock-step. Pin the invariant so reordering one without the
        // other trips here first.
        let row = encode(&fixture());
        for (i, col) in row.columns.iter().enumerate() {
            let cell = row.cells[i].as_ref().expect("no nulls");
            match col.name {
                "answer" => assert_eq!(cell.as_slice(), b"X is 42 [^1]."),
                "cache_hit" => assert_eq!(cell.as_slice(), b"f"),
                "citations" => assert!(cell.starts_with(b"[")),
                "completion_tokens" => assert_eq!(cell.as_slice(), b"45"),
                "cost_usd" => assert_eq!(cell.as_slice(), b"0.000321"),
                "mode" => assert_eq!(cell.as_slice(), b"strict"),
                "model" => assert_eq!(cell.as_slice(), b"gpt-4o-mini"),
                "prompt_tokens" => assert_eq!(cell.as_slice(), b"123"),
                "provider" => assert_eq!(cell.as_slice(), b"openai"),
                "retry_count" => assert_eq!(cell.as_slice(), b"0"),
                "sources_flat" => assert!(cell.starts_with(b"[")),
                "validation" => assert!(cell.starts_with(b"{")),
                other => panic!("unexpected column {other}"),
            }
        }
    }

    #[test]
    fn answer_with_multibyte_utf8_round_trips_byte_for_byte() {
        let mut r = fixture();
        r.answer = "Café — résumé 中文 [^1]".into();
        let row = encode(&r);
        assert_eq!(
            row.cells[0].as_ref().unwrap().as_slice(),
            "Café — résumé 中文 [^1]".as_bytes()
        );
    }

    #[test]
    fn jsonb_outputs_are_compact_not_pretty() {
        // PG drivers route `jsonb` through their own parser; whitespace
        // is irrelevant for correctness but matters for wire-size
        // budgets and audit-row equality (#402). Pin compact form.
        let row = encode(&fixture());
        for idx in [2usize, 10, 11] {
            let s = cell_str(&row, idx);
            assert!(!s.contains("\n"), "col {idx} not compact: {s}");
            assert!(!s.contains(": "), "col {idx} pretty-spaced: {s}");
        }
    }

    #[test]
    fn columns_helper_is_callable_before_query_runs() {
        // The PG `Describe` codepath needs column descriptors before
        // the query executes — pin that `columns()` does not require
        // an `AskResult` instance.
        let cols = columns();
        assert_eq!(cols.len(), 12);
        assert_eq!(cols[0].name, "answer");
    }
}
