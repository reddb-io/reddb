//! Proptest strategies that emit syntactically valid SQL strings.
//!
//! These are *generators*, not parsers — each strategy returns a
//! `String` that, when fed back through the SQL parser, must not
//! panic and (for the valid-shape strategies) must succeed.
//!
//! The generators stay deliberately small: enough variation to
//! exercise grammar corners, not so much that shrinking explodes.

use proptest::prelude::*;

/// A simple identifier: starts with the prefix `id_` to avoid
/// colliding with SQL reserved words (AS, IS, IN, BY, OR, NULL,
/// ...), then ASCII alphanumerics + underscore, max 16 chars.
/// Stays well below the `max_identifier_chars` cap so legitimate
/// inputs never trip the DoS guard.
pub fn ident() -> impl Strategy<Value = String> {
    "id_[a-z0-9_]{0,12}".prop_map(|s| s)
}

/// A small non-negative integer literal as a string. The grammar
/// uses unary `-` as a prefix operator, not a literal modifier;
/// so emitting raw `-714` would be parsed as `Neg(714)` and only
/// works as an Expr-position value, not in WHERE-RHS positions
/// that this generator pairs with comparison operators.
pub fn int_lit() -> impl Strategy<Value = String> {
    (0u64..1000u64).prop_map(|n| n.to_string())
}

/// A small string literal (single-quoted, no embedded quotes).
pub fn str_lit() -> impl Strategy<Value = String> {
    "[a-zA-Z0-9 ]{0,12}".prop_map(|s| format!("'{}'", s))
}

/// A literal value (int, string, true/false, null).
pub fn literal() -> impl Strategy<Value = String> {
    prop_oneof![
        int_lit(),
        str_lit(),
        Just("TRUE".to_string()),
        Just("FALSE".to_string()),
        Just("NULL".to_string()),
    ]
}

/// A simple comparison predicate `<col> <op> <literal>`.
pub fn predicate() -> impl Strategy<Value = String> {
    let op = prop_oneof![
        Just("="),
        Just("!="),
        Just("<"),
        Just(">"),
        Just("<="),
        Just(">=")
    ];
    (ident(), op, literal()).prop_map(|(c, o, v)| format!("{} {} {}", c, o, v))
}

/// A WHERE-clause body: optional AND/OR chain of predicates.
pub fn where_clause() -> impl Strategy<Value = String> {
    proptest::collection::vec(predicate(), 1..4).prop_map(|preds| preds.join(" AND "))
}

/// SELECT projection list (1..4 columns or `*`).
pub fn projection() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("*".to_string()),
        proptest::collection::vec(ident(), 1..4).prop_map(|cols| cols.join(", ")),
    ]
}

/// SELECT statement covering the core surface (cols, FROM,
/// optional WHERE, optional ORDER BY, optional LIMIT).
pub fn select_stmt() -> impl Strategy<Value = String> {
    (
        projection(),
        ident(),
        proptest::option::of(where_clause()),
        proptest::option::of(ident()),
        proptest::option::of(0u32..1000),
    )
        .prop_map(|(proj, table, wh, order, limit)| {
            let mut s = format!("SELECT {} FROM {}", proj, table);
            if let Some(w) = wh {
                s.push_str(" WHERE ");
                s.push_str(&w);
            }
            if let Some(o) = order {
                s.push_str(" ORDER BY ");
                s.push_str(&o);
            }
            if let Some(l) = limit {
                s.push_str(&format!(" LIMIT {}", l));
            }
            s
        })
}

/// INSERT statement with 1..3 columns and 1..3 row tuples. Uses a
/// deterministic column-name pool so the column list never has
/// duplicates (the parser rejects dup column names) and so the
/// row-arity invariant is mechanical instead of probabilistic.
pub fn insert_stmt() -> impl Strategy<Value = String> {
    (
        ident(),
        1usize..=3,
        // Generate all rows as the maximum arity, then truncate
        // each row to the chosen `n_cols`. Avoids the
        // `prop_filter_map` rejection rate that the
        // generate-and-filter approach hits.
        proptest::collection::vec(proptest::collection::vec(literal(), 3), 1..3),
    )
        .prop_map(|(table, n_cols, rows)| {
            let cols: Vec<String> = (0..n_cols).map(|i| format!("col_{}", i)).collect();
            let cols_s = cols.join(", ");
            let rows_s = rows
                .into_iter()
                .map(|r| {
                    let truncated: Vec<String> = r.into_iter().take(n_cols).collect();
                    format!("({})", truncated.join(", "))
                })
                .collect::<Vec<_>>()
                .join(", ");
            format!("INSERT INTO {} ({}) VALUES {}", table, cols_s, rows_s)
        })
}

/// UPDATE statement with 1..3 SET assignments and an optional
/// WHERE. SET targets use a unique numbered pool so generated
/// statements never duplicate a column name (which the parser
/// rejects as a semantic error).
pub fn update_stmt() -> impl Strategy<Value = String> {
    (
        ident(),
        1usize..=3,
        proptest::collection::vec(literal(), 3),
        proptest::option::of(where_clause()),
    )
        .prop_map(|(table, n_sets, vals, wh)| {
            let sets_s = (0..n_sets)
                .map(|i| format!("col_{} = {}", i, vals[i]))
                .collect::<Vec<_>>()
                .join(", ");
            let mut s = format!("UPDATE {} SET {}", table, sets_s);
            if let Some(w) = wh {
                s.push_str(" WHERE ");
                s.push_str(&w);
            }
            s
        })
}

/// DELETE statement with optional WHERE.
pub fn delete_stmt() -> impl Strategy<Value = String> {
    (ident(), proptest::option::of(where_clause())).prop_map(|(table, wh)| {
        let mut s = format!("DELETE FROM {}", table);
        if let Some(w) = wh {
            s.push_str(" WHERE ");
            s.push_str(&w);
        }
        s
    })
}

/// Top-level: any of the four major DML shapes.
pub fn any_stmt() -> impl Strategy<Value = String> {
    prop_oneof![select_stmt(), insert_stmt(), update_stmt(), delete_stmt(),]
}
