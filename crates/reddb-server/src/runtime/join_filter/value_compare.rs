//! Native comparison entry points retain ambient secret admission.
use super::*;
pub(in crate::runtime::join_filter) use reddb_engine::query::runtime::values::{
    runtime_exact_integer_cmp, value_as_i64, value_as_u64,
};
pub(in crate::runtime) use reddb_engine::query::runtime::values::{
    runtime_value_number, runtime_value_text, runtime_value_text_cow, runtime_value_text_str,
    runtime_values_equal, table_column_name,
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

/// Decode each encrypted layer under the read allowance, without recursion.
/// Missing keys and invalid ciphertext retain the existing optional policy.
pub(in crate::runtime) fn try_secret_runtime_input(
    mut value: Value,
) -> crate::RedDBResult<Option<Value>> {
    for _ in 0..reddb_types::utils::json::JSON_MAX_NESTING_DEPTH {
        if !matches!(value, Value::Secret(_)) {
            return Ok(Some(value));
        }
        let Some(decoded) = crate::runtime::execution_context::try_secret_query_input(value)?
        else {
            return Ok(None);
        };
        value = decoded;
    }
    if matches!(value, Value::Secret(_)) {
        return Err(crate::RedDBError::Query(
            "secret value nesting limit exceeded".into(),
        ));
    }
    Ok(Some(value))
}

pub(in crate::runtime) fn try_compare_runtime_values(
    left: &Value,
    right: &Value,
    op: CompareOp,
) -> crate::RedDBResult<bool> {
    use std::borrow::Cow;
    let decode = |value: &Value| -> crate::RedDBResult<Option<Value>> {
        try_secret_runtime_input(value.clone())
    };
    let left = if matches!(left, Value::Secret(_)) {
        let Some(value) = decode(left)? else {
            return Ok(false);
        };
        Cow::Owned(value)
    } else {
        Cow::Borrowed(left)
    };
    let right = if matches!(right, Value::Secret(_)) {
        let Some(value) = decode(right)? else {
            return Ok(false);
        };
        Cow::Owned(value)
    } else {
        Cow::Borrowed(right)
    };
    Ok(reddb_engine::query::runtime::values::compare_runtime_values(&left, &right, op, &Some))
}
pub(in crate::runtime) fn runtime_partial_cmp(left: &Value, right: &Value) -> Option<Ordering> {
    reddb_engine::query::runtime::values::runtime_partial_cmp(
        left,
        right,
        &crate::runtime::execution_context::secret_query_input,
    )
}
