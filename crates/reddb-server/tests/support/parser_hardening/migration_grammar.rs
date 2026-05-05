//! Proptest strategies that emit syntactically valid migration DSL
//! statements (issue #88).
//!
//! Mirrors the layout of `sql_grammar.rs`: each strategy returns a
//! `String` that, when fed back through `parser::parse`, must not
//! panic. Valid-shape strategies must additionally succeed.
//!
//! The migration grammar covers the four top-level forms:
//!   - `CREATE MIGRATION name [DEPENDS ON ...] [BATCH n ROWS]
//!      [NO ROLLBACK] [AS] body`
//!   - `APPLY MIGRATION (name | *) [FOR TENANT id]`
//!   - `ROLLBACK MIGRATION name`
//!   - `EXPLAIN MIGRATION name`
//!
//! The migration body is captured as raw SQL — generators emit a
//! handful of safe DDL/DML shapes (CREATE TABLE / ALTER TABLE /
//! CREATE INDEX / DROP INDEX / SELECT) so the grammar exercises the
//! body slot without dragging in vendor-specific surface area.

use proptest::prelude::*;

/// Identifier suitable for migration / table / column names. Stays
/// well below the `max_identifier_chars` cap.
pub fn ident() -> impl Strategy<Value = String> {
    "id_[a-z0-9_]{0,12}".prop_map(|s| s)
}

/// SQL column type drawn from the small set the parser accepts in
/// CREATE TABLE bodies.
pub fn col_type() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("INTEGER".to_string()),
        Just("BIGINT".to_string()),
        Just("TEXT".to_string()),
        Just("BOOLEAN".to_string()),
        Just("FLOAT".to_string()),
    ]
}

/// `<col> <type>` column definition.
pub fn col_def() -> impl Strategy<Value = String> {
    (ident(), col_type()).prop_map(|(c, t)| format!("{} {}", c, t))
}

/// `CREATE TABLE name (col1 type1, col2 type2, ...)` body.
pub fn create_table_stmt() -> impl Strategy<Value = String> {
    (ident(), proptest::collection::vec(col_def(), 1..4)).prop_map(|(table, cols)| {
        let cols_s = cols.join(", ");
        format!("CREATE TABLE {} ({})", table, cols_s)
    })
}

/// `ALTER TABLE name ADD COLUMN col type`.
pub fn alter_table_add_stmt() -> impl Strategy<Value = String> {
    (ident(), col_def())
        .prop_map(|(table, def)| format!("ALTER TABLE {} ADD COLUMN {}", table, def))
}

/// `ALTER TABLE name DROP COLUMN col`.
pub fn alter_table_drop_stmt() -> impl Strategy<Value = String> {
    (ident(), ident()).prop_map(|(table, col)| format!("ALTER TABLE {} DROP COLUMN {}", table, col))
}

/// `CREATE [UNIQUE] INDEX name ON table (col1, col2, ...)`.
pub fn create_index_stmt() -> impl Strategy<Value = String> {
    (
        any::<bool>(),
        ident(),
        ident(),
        proptest::collection::vec(ident(), 1..3),
    )
        .prop_map(|(unique, idx, table, cols)| {
            let kw = if unique { "CREATE UNIQUE INDEX" } else { "CREATE INDEX" };
            format!("{} {} ON {} ({})", kw, idx, table, cols.join(", "))
        })
}

/// `DROP INDEX name`.
pub fn drop_index_stmt() -> impl Strategy<Value = String> {
    ident().prop_map(|name| format!("DROP INDEX {}", name))
}

/// Body shapes a migration can hold. Single-statement bodies only —
/// the migration body is consumed verbatim and re-parsed at apply
/// time, so we generate things the body executor can actually run.
pub fn migration_body_stmt() -> impl Strategy<Value = String> {
    prop_oneof![
        create_table_stmt(),
        alter_table_add_stmt(),
        alter_table_drop_stmt(),
        create_index_stmt(),
        drop_index_stmt(),
    ]
}

/// `CREATE MIGRATION name [BATCH n ROWS] [NO ROLLBACK] AS body`.
///
/// Note: the `DEPENDS ON ...` clause and `FOR TENANT ...` apply
/// suffix are intentionally *not* generated here. Both clauses use
/// reserved tokens (`Token::On`, `Token::For`) that the migration
/// parser tries to consume via `consume_ident_ci`, which only
/// matches `Token::Ident`. Those broken edges are pinned via the
/// snapshot suite + adversarial corpus instead — extending the
/// happy-path generator there would produce inputs that the parser
/// rejects today, and the property suite would flag every shrink
/// as a "did not parse" regression.
pub fn create_migration_stmt() -> impl Strategy<Value = String> {
    (
        ident(),
        proptest::option::of(1u64..1000),
        any::<bool>(),
        migration_body_stmt(),
    )
        .prop_map(|(name, batch, no_rb, body)| {
            let mut s = format!("CREATE MIGRATION {}", name);
            if let Some(b) = batch {
                s.push_str(&format!(" BATCH {} ROWS", b));
            }
            if no_rb {
                s.push_str(" NO ROLLBACK");
            }
            s.push_str(" AS ");
            s.push_str(&body);
            s
        })
}

/// `APPLY MIGRATION (name | *)`. The `FOR TENANT ...` suffix is
/// not generated — `Token::For` is reserved and the migration
/// parser's `consume_ident_ci("FOR")` never matches it. Pinned via
/// snapshot suite + adversarial corpus.
pub fn apply_migration_stmt() -> impl Strategy<Value = String> {
    prop_oneof![ident(), Just("*".to_string())].prop_map(|target| format!("APPLY MIGRATION {}", target))
}

/// `ROLLBACK MIGRATION name`.
pub fn rollback_migration_stmt() -> impl Strategy<Value = String> {
    ident().prop_map(|name| format!("ROLLBACK MIGRATION {}", name))
}

/// `EXPLAIN MIGRATION name`.
pub fn explain_migration_stmt() -> impl Strategy<Value = String> {
    ident().prop_map(|name| format!("EXPLAIN MIGRATION {}", name))
}

/// Top-level union: any of the four migration shapes.
pub fn any_migration_stmt() -> impl Strategy<Value = String> {
    prop_oneof![
        create_migration_stmt(),
        apply_migration_stmt(),
        rollback_migration_stmt(),
        explain_migration_stmt(),
    ]
}
