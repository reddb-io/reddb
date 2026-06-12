//! Rendering of engine values into sqllogictest comparison cells.
//!
//! The sqllogictest format pins each query result column to a *type hint* in
//! the `query <types>` header — `T` for text, `I` for integer, `R` for real
//! (floating point). The comparator then matches the harness's rendered cell
//! string against the expected block. Both sides must therefore agree on
//! exactly how a logical value becomes text. SQLite's reference harness fixes
//! that rendering; we reproduce the same rules here so a corpus slice authored
//! against SQLite compares faithfully against the RedDB engine.
//!
//! This rendering is the one piece of conformance machinery that is genuinely
//! storage-agnostic — it consumes a [`reddb_types::Value`] and a [`CellType`]
//! and needs nothing from the executor — so it lives in the library rather
//! than the integration-test harness.

use reddb_types::Value;

/// The per-column type hint declared by a sqllogictest `query <types>` header.
///
/// Mirrors SQLite's three coercion classes. A column's hint controls how each
/// cell value in that column is rendered for comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellType {
    /// `T` — render as text.
    Text,
    /// `I` — render as a 64-bit integer.
    Integer,
    /// `R` — render as a fixed 3-decimal real.
    Real,
}

impl CellType {
    /// Parse one type-hint character from a `query` header (`T`/`I`/`R`).
    ///
    /// Returns `None` for any other character so callers can surface a precise
    /// "unknown column type" diagnostic rather than silently mis-rendering.
    pub fn from_char(c: char) -> Option<Self> {
        match c {
            'T' => Some(CellType::Text),
            'I' => Some(CellType::Integer),
            'R' => Some(CellType::Real),
            _ => None,
        }
    }

    /// The header character this hint renders from.
    pub fn to_char(self) -> char {
        match self {
            CellType::Text => 'T',
            CellType::Integer => 'I',
            CellType::Real => 'R',
        }
    }
}

/// Render an engine [`Value`] into the textual cell sqllogictest compares.
///
/// Follows SQLite's reference rules:
/// * `NULL` renders as the literal `NULL` regardless of column type.
/// * Under [`CellType::Text`], an empty string renders as `(empty)`; any byte
///   below `0x20` or above `0x7e` is replaced with `@`, matching the reference
///   harness's non-printable handling.
/// * Under [`CellType::Integer`], the value is coerced to an `i64` and printed
///   in decimal (booleans map to `1`/`0`).
/// * Under [`CellType::Real`], the value is printed with exactly three decimal
///   places.
pub fn render_cell(value: &Value, ty: CellType) -> String {
    if matches!(value, Value::Null) {
        return "NULL".to_string();
    }
    match ty {
        CellType::Text => render_text(value),
        CellType::Integer => match coerce_integer(value) {
            Some(i) => i.to_string(),
            None => render_text(value),
        },
        CellType::Real => match coerce_real(value) {
            Some(f) => format!("{f:.3}"),
            None => render_text(value),
        },
    }
}

fn render_text(value: &Value) -> String {
    let raw = match value {
        Value::Text(s) => s.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::UnsignedInteger(u) => u.to_string(),
        Value::Boolean(b) => (if *b { 1 } else { 0 }).to_string(),
        Value::Float(f) => format!("{f:.3}"),
        other => format!("{other:?}"),
    };
    if raw.is_empty() {
        return "(empty)".to_string();
    }
    raw.chars()
        .map(|c| {
            if ('\u{20}'..='\u{7e}').contains(&c) {
                c
            } else {
                '@'
            }
        })
        .collect()
}

fn coerce_integer(value: &Value) -> Option<i64> {
    match value {
        Value::Integer(i) => Some(*i),
        Value::UnsignedInteger(u) => Some(*u as i64),
        Value::Boolean(b) => Some(if *b { 1 } else { 0 }),
        Value::Float(f) => Some(*f as i64),
        _ => None,
    }
}

fn coerce_real(value: &Value) -> Option<f64> {
    match value {
        Value::Float(f) => Some(*f),
        Value::Integer(i) => Some(*i as f64),
        Value::UnsignedInteger(u) => Some(*u as f64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn null_renders_as_keyword_for_every_type() {
        for ty in [CellType::Text, CellType::Integer, CellType::Real] {
            assert_eq!(render_cell(&Value::Null, ty), "NULL");
        }
    }

    #[test]
    fn text_empty_string_is_marked() {
        assert_eq!(
            render_cell(&Value::Text("".into()), CellType::Text),
            "(empty)"
        );
    }

    #[test]
    fn text_passes_printable_through_and_scrubs_the_rest() {
        assert_eq!(
            render_cell(&Value::Text("Alice".into()), CellType::Text),
            "Alice"
        );
        assert_eq!(
            render_cell(&Value::Text("a\tb".into()), CellType::Text),
            "a@b"
        );
    }

    #[test]
    fn integer_coerces_and_prints_decimal() {
        assert_eq!(render_cell(&Value::Integer(42), CellType::Integer), "42");
        assert_eq!(
            render_cell(&Value::UnsignedInteger(7), CellType::Integer),
            "7"
        );
        assert_eq!(render_cell(&Value::Boolean(true), CellType::Integer), "1");
    }

    #[test]
    fn real_prints_three_decimals() {
        assert_eq!(render_cell(&Value::Float(1.5), CellType::Real), "1.500");
        assert_eq!(render_cell(&Value::Integer(2), CellType::Real), "2.000");
    }

    #[test]
    fn cell_type_round_trips_through_char() {
        for ty in [CellType::Text, CellType::Integer, CellType::Real] {
            assert_eq!(CellType::from_char(ty.to_char()), Some(ty));
        }
        assert_eq!(CellType::from_char('X'), None);
    }
}
