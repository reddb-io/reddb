//! Proptest strategies that emit syntactically valid probabilistic
//! data-structure statements (issue #105).
//!
//! Mirrors the layout of `queue_grammar.rs` (#103),
//! `geo_grammar.rs` (#104), and `vector_search_grammar.rs` (#100):
//! each strategy returns a `String` that, when fed back through the
//! main `parser::parse` entry point, must not panic. Valid-shape
//! strategies must additionally succeed.
//!
//! Surface covered (see
//! `crates/reddb-server/src/storage/query/parser/probabilistic_commands.rs`
//! for the read-only grammar reference):
//!
//! HyperLogLog:
//!   - `CREATE HLL [IF NOT EXISTS] name`
//!   - `HLL ADD name 'el1' 'el2' …`
//!   - `HLL COUNT name [name2 …]`
//!   - `HLL MERGE dest src1 src2 …`
//!   - `HLL INFO name`
//!   - `DROP HLL [IF EXISTS] name`
//!
//! Count-Min Sketch:
//!   - `CREATE SKETCH [IF NOT EXISTS] name [WIDTH n] [DEPTH n]`
//!   - `SKETCH ADD name 'element' [count]`
//!   - `SKETCH COUNT name 'element'`
//!   - `SKETCH MERGE dest src1 src2 …`
//!   - `SKETCH INFO name`
//!   - `DROP SKETCH [IF EXISTS] name`
//!
//! Cuckoo Filter:
//!   - `CREATE FILTER [IF NOT EXISTS] name [CAPACITY n]`
//!   - `FILTER ADD name 'element'`
//!   - `FILTER CHECK name 'element'`
//!   - `FILTER DELETE name 'element'`
//!   - `FILTER COUNT name`
//!   - `FILTER INFO name`
//!   - `DROP FILTER [IF EXISTS] name`
//!
//! Generators are intentionally restricted to *valid* shapes so
//! happy-path proptests succeed. Adversarial / malformed cases live
//! in `corpus::probabilistic_adversarial_inputs()` and the
//! `arbitrary_suffix` strategy below.
//!
//! Caveat — `DEPTH` is a reserved lexer token (`Token::Depth`), not
//! a plain identifier. The parser's `consume_ident_ci("DEPTH")` only
//! matches `Token::Ident`, so any sketch shape that includes `DEPTH`
//! today fails to parse at the top-level "trailing-tokens" check.
//! The valid-shape SKETCH strategy therefore emits `WIDTH` only and
//! the `DEPTH` regression is captured by a FIXME pin in
//! `tests/probabilistic_parser.rs`.

use proptest::prelude::*;

/// Identifier suitable for HLL/SKETCH/FILTER names. Stays under the
/// default `max_identifier_chars` cap and avoids reserved keywords
/// by carrying a short alphabetic prefix.
pub fn ident() -> impl Strategy<Value = String> {
    "p_[a-z0-9_]{0,12}".prop_map(|s| s)
}

/// Single-quoted string element (no embedded quotes) suitable for
/// HLL ADD / SKETCH ADD / FILTER ADD payloads.
pub fn string_element() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 _.@-]{1,16}".prop_map(|s| format!("'{}'", s))
}

/// Optional `IF NOT EXISTS` modifier prefix.
pub fn opt_if_not_exists() -> impl Strategy<Value = &'static str> {
    prop_oneof![Just(""), Just(" IF NOT EXISTS")]
}

/// Optional `IF EXISTS` modifier prefix used by DROP statements.
pub fn opt_if_exists() -> impl Strategy<Value = &'static str> {
    prop_oneof![Just(""), Just(" IF EXISTS")]
}

// ---------------------------------------------------------------
// Strategy 1 of 6: CREATE / DROP envelopes for all three structures
// ---------------------------------------------------------------

/// `CREATE HLL [IF NOT EXISTS] name`, `CREATE SKETCH [IF NOT EXISTS]
/// name [WIDTH n]`, `CREATE FILTER [IF NOT EXISTS] name [CAPACITY n]`,
/// plus the matching `DROP` shapes.
///
/// Pinned as the entry-point strategy: every other strategy assumes
/// the structure has been declared and the most common regression
/// is a tweak to the CREATE/DROP envelope.
///
/// The SKETCH variant intentionally never emits a `DEPTH` clause
/// because `Token::Depth` lexes as a reserved keyword; see the
/// FIXME pin `fixme_sketch_depth_clause_breaks_top_level_eof` in
/// `tests/probabilistic_parser.rs`.
pub fn create_drop_stmt() -> impl Strategy<Value = String> {
    prop_oneof![
        // CREATE HLL
        (opt_if_not_exists(), ident())
            .prop_map(|(ine, n)| format!("CREATE HLL{} {}", ine, n)),
        // CREATE SKETCH (bare)
        (opt_if_not_exists(), ident())
            .prop_map(|(ine, n)| format!("CREATE SKETCH{} {}", ine, n)),
        // CREATE SKETCH name WIDTH w
        (opt_if_not_exists(), ident(), 1u32..=10_000u32)
            .prop_map(|(ine, n, w)| format!("CREATE SKETCH{} {} WIDTH {}", ine, n, w)),
        // CREATE FILTER (bare)
        (opt_if_not_exists(), ident())
            .prop_map(|(ine, n)| format!("CREATE FILTER{} {}", ine, n)),
        // CREATE FILTER name CAPACITY c
        (opt_if_not_exists(), ident(), 1u32..=1_000_000u32)
            .prop_map(|(ine, n, c)| format!("CREATE FILTER{} {} CAPACITY {}", ine, n, c)),
        // DROP HLL / SKETCH / FILTER
        (
            prop_oneof![Just("HLL"), Just("SKETCH"), Just("FILTER")],
            opt_if_exists(),
            ident(),
        )
            .prop_map(|(kind, ie, n)| format!("DROP {}{} {}", kind, ie, n)),
    ]
}

// ---------------------------------------------------------------
// Strategy 2 of 6: HLL ADD / COUNT / MERGE / INFO
// ---------------------------------------------------------------

/// HLL operational surface: ADD with 1..=8 string elements, COUNT
/// over 1..=4 names, MERGE dest + 1..=4 sources, INFO.
///
/// Pinned independently from CREATE because the `HLL ADD` accumulator
/// loop and the `HLL COUNT` multi-name greedy loop are the two
/// distinct call-sites a regression would touch in
/// `parse_hll_command`.
pub fn hll_op_stmt() -> impl Strategy<Value = String> {
    let add = (
        ident(),
        prop::collection::vec(string_element(), 1..=8),
    )
        .prop_map(|(name, els)| format!("HLL ADD {} {}", name, els.join(" ")));
    let count = prop::collection::vec(ident(), 1..=4)
        .prop_map(|names| format!("HLL COUNT {}", names.join(" ")));
    let merge = (ident(), prop::collection::vec(ident(), 1..=4))
        .prop_map(|(dest, srcs)| format!("HLL MERGE {} {}", dest, srcs.join(" ")));
    let info = ident().prop_map(|n| format!("HLL INFO {}", n));
    prop_oneof![add, count, merge, info]
}

// ---------------------------------------------------------------
// Strategy 3 of 6: SKETCH ADD / COUNT / MERGE / INFO
// ---------------------------------------------------------------

/// Count-Min Sketch operational surface. `SKETCH ADD` always emits a
/// trailing count integer (the parser accepts both with and without;
/// pinning *with* exercises the optional path). `SKETCH COUNT` emits
/// exactly one element. MERGE and INFO mirror the HLL shapes.
pub fn sketch_op_stmt() -> impl Strategy<Value = String> {
    let add_with_count = (ident(), string_element(), 1u64..=1_000_000u64)
        .prop_map(|(n, e, c)| format!("SKETCH ADD {} {} {}", n, e, c));
    let add_no_count = (ident(), string_element())
        .prop_map(|(n, e)| format!("SKETCH ADD {} {}", n, e));
    let count = (ident(), string_element())
        .prop_map(|(n, e)| format!("SKETCH COUNT {} {}", n, e));
    let merge = (ident(), prop::collection::vec(ident(), 1..=4))
        .prop_map(|(dest, srcs)| format!("SKETCH MERGE {} {}", dest, srcs.join(" ")));
    let info = ident().prop_map(|n| format!("SKETCH INFO {}", n));
    prop_oneof![add_with_count, add_no_count, count, merge, info]
}

// ---------------------------------------------------------------
// Strategy 4 of 6: FILTER ADD / CHECK / DELETE / COUNT / INFO
// ---------------------------------------------------------------

/// Cuckoo Filter operational surface. Pinned independently because
/// FILTER is the only one of the three structures that exposes a
/// `DELETE` operation, and the parser dispatch on `Token::Delete`
/// vs the `CHECK`/`INFO` ident-based branches is a frequent source
/// of grammar drift.
pub fn filter_op_stmt() -> impl Strategy<Value = String> {
    let add = (ident(), string_element())
        .prop_map(|(n, e)| format!("FILTER ADD {} {}", n, e));
    let check = (ident(), string_element())
        .prop_map(|(n, e)| format!("FILTER CHECK {} {}", n, e));
    let delete = (ident(), string_element())
        .prop_map(|(n, e)| format!("FILTER DELETE {} {}", n, e));
    let count = ident().prop_map(|n| format!("FILTER COUNT {}", n));
    let info = ident().prop_map(|n| format!("FILTER INFO {}", n));
    prop_oneof![add, check, delete, count, info]
}

// ---------------------------------------------------------------
// Strategy 5 of 6: capacity / width modifiers
// ---------------------------------------------------------------

/// Modifier-focused strategy: pinpoints the `CAPACITY` token after
/// `CREATE FILTER` and the `WIDTH` token after `CREATE SKETCH`. A
/// regression that breaks the modifier shrinks straight to the
/// modifier keyword rather than a fuzzy whole-statement diff.
///
/// `DEPTH` is intentionally absent; see module-level note.
pub fn modifier_stmt() -> impl Strategy<Value = String> {
    prop_oneof![
        // CREATE FILTER name CAPACITY c
        (ident(), 1u32..=1_000_000u32)
            .prop_map(|(n, c)| format!("CREATE FILTER {} CAPACITY {}", n, c)),
        // CREATE FILTER IF NOT EXISTS name CAPACITY c
        (ident(), 1u32..=1_000_000u32)
            .prop_map(|(n, c)| format!("CREATE FILTER IF NOT EXISTS {} CAPACITY {}", n, c)),
        // CREATE SKETCH name WIDTH w
        (ident(), 1u32..=10_000u32)
            .prop_map(|(n, w)| format!("CREATE SKETCH {} WIDTH {}", n, w)),
        // CREATE SKETCH IF NOT EXISTS name WIDTH w
        (ident(), 1u32..=10_000u32)
            .prop_map(|(n, w)| format!("CREATE SKETCH IF NOT EXISTS {} WIDTH {}", n, w)),
    ]
}

// ---------------------------------------------------------------
// Strategy 6 of 6: top-level union — any probabilistic statement
// ---------------------------------------------------------------

/// Top-level union: any of the probabilistic-grammar shapes covered
/// above. Used by the catch-all panic-safety proptest.
pub fn any_probabilistic_stmt() -> impl Strategy<Value = String> {
    prop_oneof![
        create_drop_stmt(),
        hll_op_stmt(),
        sketch_op_stmt(),
        filter_op_stmt(),
        modifier_stmt(),
    ]
}
