//! Native comparison entry points retain ambient secret admission.
use super::*;
pub(in crate::runtime::join_filter) use reddb_engine::query::runtime::values::{
    runtime_exact_integer_cmp, value_as_i64, value_as_u64,
};
pub(in crate::runtime) use reddb_engine::query::runtime::values::{
    runtime_value_number, runtime_value_text, runtime_value_text_cow, runtime_value_text_str,
    runtime_values_equal, table_column_name, text_abbrev_key,
};

pub(in crate::runtime) fn compare_runtime_values(
    left: &Value,
    right: &Value,
    op: CompareOp,
) -> bool {
    reddb_engine::query::runtime::values::compare_runtime_values(
        left,
        right,
        op,
        &crate::runtime::execution_context::secret_query_input,
    )
}
pub(in crate::runtime) fn runtime_partial_cmp(left: &Value, right: &Value) -> Option<Ordering> {
    reddb_engine::query::runtime::values::runtime_partial_cmp(
        left,
        right,
        &crate::runtime::execution_context::secret_query_input,
    )
}
