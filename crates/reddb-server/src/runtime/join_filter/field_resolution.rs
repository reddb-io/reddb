//! Native aliases to canonical field and document-path resolution.
use super::*;
pub(in crate::runtime) use reddb_engine::query::runtime::field_resolution::*;

pub(in crate::runtime) fn parse_canonical_field_ref(value: &str) -> RedDBResult<FieldRef> {
    reddb_engine::query::runtime::field_resolution::parse_canonical_field_ref(value)
        .map_err(RedDBError::Query)
}
