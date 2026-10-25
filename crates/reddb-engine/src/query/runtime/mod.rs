//! Canonical record-level runtime semantics. Hosts retain read admission and secret policy.
pub mod field_resolution;
pub mod filter;
pub mod numeric;
pub mod ordering;
pub mod projection;
pub mod values;

pub fn is_universal_entity_source(table: &str) -> bool {
    table.eq_ignore_ascii_case("any")
        || table.eq_ignore_ascii_case("_any")
        || table.eq_ignore_ascii_case("all")
        || table.eq_ignore_ascii_case("entity")
        || table.eq_ignore_ascii_case("universal")
        || table.eq_ignore_ascii_case("mixed")
        || table == "*"
}
