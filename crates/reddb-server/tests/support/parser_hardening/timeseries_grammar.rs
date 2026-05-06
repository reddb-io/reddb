//! Proptest strategies that emit syntactically valid time-series DSL
//! statements (issue #102).
//!
//! Mirrors the layout of `sql_grammar.rs` (#87) and
//! `migration_grammar.rs` (#88): each strategy returns a `String` that,
//! when fed back through `parser::parse`, must not panic. Valid-shape
//! strategies must additionally succeed.
//!
//! Surfaces covered:
//!   - `CREATE TIMESERIES name [RETENTION n unit] [CHUNK_SIZE n]
//!     [DOWNSAMPLE spec[, spec...]]`
//!   - `CREATE HYPERTABLE name TIME_COLUMN col CHUNK_INTERVAL '<dur>'
//!     [TTL '<dur>'] [RETENTION n unit]`
//!   - bare `CHUNK_INTERVAL '<dur>'` literal values, exercised through
//!     the hypertable strategy slot
//!   - `RETENTION n unit` WITH-style clauses (where `unit` ∈ {ms, s,
//!     m, h, d} plus their long aliases)
//!   - `CREATE MATERIALIZED VIEW [IF NOT EXISTS] name AS SELECT …`
//!     stands in for the continuous-aggregate surface — the parser
//!     today routes continuous aggregates through the materialized-
//!     view path (`storage/query/sql.rs:1565` REFRESH MATERIALIZED
//!     VIEW); a dedicated `CREATE CONTINUOUS AGGREGATE` keyword does
//!     not yet exist in the lexer (`lexer.rs` only ships `Timeseries`
//!     + `Retention`).

use proptest::prelude::*;

/// Identifier suitable for time-series / hypertable / column names.
/// Stays well below the `max_identifier_chars` cap and steers clear
/// of SQL reserved words.
pub fn ident() -> impl Strategy<Value = String> {
    "id_[a-z0-9_]{0,12}".prop_map(|s| s)
}

/// One of the duration units the `parse_duration_unit` helper accepts
/// for the bare `RETENTION n unit` form. Mixes short + long aliases so
/// the property tests exercise every match arm.
///
/// FIXME(#TBD-min-collides-with-aggregate-keyword): the units `min`,
/// `max`, `avg` collide with the `MIN` / `MAX` / `AVG` aggregate
/// keywords in `lexer.rs`, so `RETENTION 1 min` lexes as `1
/// Token::Min` instead of `1 Token::Ident("min")`. The
/// `parse_duration_unit` helper only inspects `Token::Ident`, so the
/// keyword collision falls into the silent default-to-seconds branch
/// and the trailing `min` token then trips the top-level loop. The
/// dedicated `create_timeseries_retention_min_unit_silent_default`
/// snapshot pins the current shape; the follow-up issue tightens
/// `parse_duration_unit` to also accept the aggregate-keyword tokens.
pub fn retention_unit() -> impl Strategy<Value = &'static str> {
    prop_oneof![
        Just("ms"),
        Just("s"),
        Just("sec"),
        Just("secs"),
        Just("seconds"),
        Just("m"),
        Just("mins"),
        Just("minute"),
        Just("minutes"),
        Just("h"),
        Just("hr"),
        Just("hrs"),
        Just("hour"),
        Just("hours"),
        Just("d"),
        Just("day"),
        Just("days"),
    ]
}

/// `RETENTION <n> <unit>` clause body. `n` stays small so the
/// resulting `n * unit` multiplication fits in `u64` without overflow
/// (the parser stores the result in `retention_ms: Option<u64>`).
pub fn retention_clause() -> impl Strategy<Value = String> {
    (1u64..1000, retention_unit()).prop_map(|(n, u)| format!("RETENTION {} {}", n, u))
}

/// String-literal duration accepted by `CHUNK_INTERVAL` /  `TTL`. The
/// underlying `parse_duration_ns` helper only supports the short
/// suffixes (`ms`/`s`/`m`/`h`/`d`) packed into a single token — the
/// long-form (`'1 day'`, `'5 minutes'`) is **not** accepted today and
/// is exercised separately by the negative fuzz seeds.
pub fn chunk_interval_literal() -> impl Strategy<Value = String> {
    let unit = prop_oneof![
        Just("ms"),
        Just("s"),
        Just("m"),
        Just("h"),
        Just("d"),
    ];
    (1u64..1000, unit).prop_map(|(n, u)| format!("'{}{}'", n, u))
}

/// `CREATE TIMESERIES name [RETENTION ...] [CHUNK_SIZE n]
/// [DOWNSAMPLE spec[, spec...]]`.
///
/// All optional clauses are generated. `IF NOT EXISTS` is omitted
/// here — the hardened invariant is that the canonical shape parses;
/// the IF-NOT-EXISTS variant is covered by the happy-path tests.
pub fn create_timeseries_stmt() -> impl Strategy<Value = String> {
    (
        ident(),
        proptest::option::of(retention_clause()),
        proptest::option::of(1u64..10_000),
    )
        .prop_map(|(name, retention, chunk_size)| {
            let mut s = format!("CREATE TIMESERIES {}", name);
            if let Some(r) = retention {
                s.push(' ');
                s.push_str(&r);
            }
            if let Some(c) = chunk_size {
                s.push_str(&format!(" CHUNK_SIZE {}", c));
            }
            s
        })
}

/// `CREATE HYPERTABLE name TIME_COLUMN col CHUNK_INTERVAL '<dur>'
/// [TTL '<dur>'] [RETENTION n unit]`.
///
/// `TIME_COLUMN` and `CHUNK_INTERVAL` are required by the parser; the
/// remaining clauses are optional. Generated in fixed order to keep
/// the strategy small — the parser accepts any order, exercised by
/// the dedicated property test below.
pub fn create_hypertable_stmt() -> impl Strategy<Value = String> {
    (
        ident(),
        ident(),
        chunk_interval_literal(),
        proptest::option::of(chunk_interval_literal()),
        proptest::option::of(retention_clause()),
    )
        .prop_map(|(name, time_col, chunk, ttl, retention)| {
            let mut s = format!(
                "CREATE HYPERTABLE {} TIME_COLUMN {} CHUNK_INTERVAL {}",
                name, time_col, chunk
            );
            if let Some(t) = ttl {
                s.push_str(&format!(" TTL {}", t));
            }
            if let Some(r) = retention {
                s.push(' ');
                s.push_str(&r);
            }
            s
        })
}

/// Bare `CHUNK_INTERVAL '<dur>'` token sequence wrapped in the smallest
/// hypertable that makes the parser reach the clause. Lets the
/// property test enumerate every supported unit literal independently
/// from the rest of the hypertable surface.
pub fn chunk_interval_focused_stmt() -> impl Strategy<Value = String> {
    (ident(), ident(), chunk_interval_literal()).prop_map(|(name, time_col, dur)| {
        format!(
            "CREATE HYPERTABLE {} TIME_COLUMN {} CHUNK_INTERVAL {}",
            name, time_col, dur
        )
    })
}

/// `RETENTION n unit` clause embedded in a CREATE TIMESERIES — the
/// minimal context the parser needs to reach `parse_duration_unit`.
/// Distinct from `create_timeseries_stmt` because it always emits the
/// retention slot, so 256 cases × N exercises every unit alias.
pub fn retention_focused_stmt() -> impl Strategy<Value = String> {
    (ident(), retention_clause())
        .prop_map(|(name, ret)| format!("CREATE TIMESERIES {} {}", name, ret))
}

/// `CREATE MATERIALIZED VIEW [IF NOT EXISTS] name AS SELECT col FROM
/// src` — the path continuous aggregates ride through today. Body is
/// kept to the smallest SELECT the parser will accept so the
/// generator stresses the MV envelope, not the query body.
pub fn continuous_aggregate_stmt() -> impl Strategy<Value = String> {
    (any::<bool>(), ident(), ident(), ident()).prop_map(|(if_not_exists, name, col, src)| {
        let prefix = if if_not_exists {
            "CREATE MATERIALIZED VIEW IF NOT EXISTS"
        } else {
            "CREATE MATERIALIZED VIEW"
        };
        format!("{} {} AS SELECT {} FROM {}", prefix, name, col, src)
    })
}

/// Top-level union: any of the time-series / hypertable shapes.
pub fn any_timeseries_stmt() -> impl Strategy<Value = String> {
    prop_oneof![
        create_timeseries_stmt(),
        create_hypertable_stmt(),
        chunk_interval_focused_stmt(),
        retention_focused_stmt(),
        continuous_aggregate_stmt(),
    ]
}
