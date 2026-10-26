//! Canonical record-level runtime semantics. Hosts retain read admission and secret policy.
pub mod expression;
pub mod field_resolution;
pub mod filter;
pub mod geographic;
mod json_functions;
pub mod numeric;
pub mod ordering;
pub mod projection;
pub mod projection_value;
pub mod scalar_functions;
pub mod table_plan;
pub mod temporal;
pub mod text;
pub mod value_functions;
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
pub mod window_phase;
