//! Runtime filter evaluation and LIKE/metadata helpers.
use super::*;

pub(in crate::runtime) fn evaluate_runtime_filter(
    record: &UnifiedRecord,
    filter: &Filter,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> bool {
    evaluate_runtime_filter_with_db(None, record, filter, table_name, table_alias)
}

/// Row adapter for the typed scalar evaluator — bridges `UnifiedRecord`
/// to `crate::storage::query::evaluator::Row` so that `evaluator::evaluate`
/// can look up columns via the same `resolve_runtime_field` path used by
/// the rest of the runtime filter evaluation.
pub(in crate::runtime::join_filter) struct RecordRow<'a> {
    pub(in crate::runtime::join_filter) record: &'a UnifiedRecord,
    pub(in crate::runtime::join_filter) table_name: Option<&'a str>,
    pub(in crate::runtime::join_filter) table_alias: Option<&'a str>,
}

impl crate::storage::query::evaluator::Row for RecordRow<'_> {
    fn get(&self, field: &FieldRef) -> Option<Value> {
        resolve_runtime_field(self.record, field, self.table_name, self.table_alias)
    }
}

pub(in crate::runtime) fn evaluate_runtime_filter_with_db(
    db: Option<&RedDB>,
    record: &UnifiedRecord,
    filter: &Filter,
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> bool {
    match filter {
        Filter::Compare { field, op, value } => {
            resolve_runtime_field(record, field, table_name, table_alias)
                .as_ref()
                .and_then(|candidate| evaluate_metadata_field_compare(field, candidate, *op, value))
                .or_else(|| {
                    resolve_runtime_field(record, field, table_name, table_alias)
                        .as_ref()
                        .map(|candidate| compare_runtime_values(candidate, value, *op))
                })
                .unwrap_or(false)
        }
        Filter::CompareFields { left, op, right } => {
            let left_value = resolve_runtime_field(record, left, table_name, table_alias);
            let right_value = resolve_runtime_field(record, right, table_name, table_alias);
            match (left_value, right_value) {
                (Some(l), Some(r)) => compare_runtime_values(&l, &r, *op),
                _ => false,
            }
        }
        Filter::CompareExpr { lhs, op, rhs } => {
            // Route through the typed evaluator (catalog-resolved
            // operator / cast / function dispatch). Falls back to the
            // untyped expr_eval walker for CONFIG / KV / ML_* and any
            // other shape the evaluator does not cover yet.
            let row = RecordRow {
                record,
                table_name,
                table_alias,
            };
            let eval_side = |expr| {
                crate::storage::query::evaluator::evaluate(expr, &row)
                    .ok()
                    .or_else(|| {
                        super::expr_eval::evaluate_runtime_expr_with_db(
                            db,
                            expr,
                            record,
                            table_name,
                            table_alias,
                        )
                    })
            };
            match (eval_side(lhs), eval_side(rhs)) {
                (Some(lv), Some(rv)) => compare_runtime_values(&lv, &rv, *op),
                _ => false,
            }
        }
        Filter::And(left, right) => {
            evaluate_runtime_filter_with_db(db, record, left, table_name, table_alias)
                && evaluate_runtime_filter_with_db(db, record, right, table_name, table_alias)
        }
        Filter::Or(left, right) => {
            evaluate_runtime_filter_with_db(db, record, left, table_name, table_alias)
                || evaluate_runtime_filter_with_db(db, record, right, table_name, table_alias)
        }
        Filter::Not(inner) => {
            !evaluate_runtime_filter_with_db(db, record, inner, table_name, table_alias)
        }
        Filter::IsNull(field) => resolve_runtime_field(record, field, table_name, table_alias)
            .map(|value| value == Value::Null)
            .unwrap_or(true),
        Filter::IsNotNull(field) => resolve_runtime_field(record, field, table_name, table_alias)
            .map(|value| value != Value::Null)
            .unwrap_or(false),
        Filter::In { field, values } => {
            resolve_runtime_field(record, field, table_name, table_alias)
                .as_ref()
                .is_some_and(|candidate| {
                    evaluate_metadata_field_in(field, candidate, values).unwrap_or_else(|| {
                        values
                            .iter()
                            .any(|value| compare_runtime_values(candidate, value, CompareOp::Eq))
                    })
                })
        }
        Filter::Between { field, low, high } => {
            resolve_runtime_field(record, field, table_name, table_alias)
                .as_ref()
                .is_some_and(|candidate| {
                    compare_runtime_values(candidate, low, CompareOp::Ge)
                        && compare_runtime_values(candidate, high, CompareOp::Le)
                })
        }
        Filter::Like { field, pattern } => {
            resolve_runtime_field(record, field, table_name, table_alias)
                .as_ref()
                .and_then(runtime_value_text)
                .is_some_and(|value| like_matches(&value, pattern))
        }
        Filter::StartsWith { field, prefix } => {
            resolve_runtime_field(record, field, table_name, table_alias)
                .as_ref()
                .and_then(runtime_value_text)
                .is_some_and(|value| value.starts_with(prefix))
        }
        Filter::EndsWith { field, suffix } => {
            resolve_runtime_field(record, field, table_name, table_alias)
                .as_ref()
                .and_then(runtime_value_text)
                .is_some_and(|value| value.ends_with(suffix))
        }
        Filter::Contains { field, substring } => {
            resolve_runtime_field(record, field, table_name, table_alias)
                .as_ref()
                .is_some_and(|value| runtime_value_contains(value, substring))
        }
    }
}

fn runtime_value_contains(value: &Value, needle: &str) -> bool {
    match value {
        Value::Array(values) => values
            .iter()
            .any(|value| runtime_value_contains(value, needle)),
        Value::Json(bytes) => {
            crate::serde_json::from_slice::<JsonValue>(bytes)
                .ok()
                .is_some_and(|json| json_value_contains(&json, needle))
                || String::from_utf8_lossy(bytes).contains(needle)
        }
        other => runtime_value_text(other).is_some_and(|value| value.contains(needle)),
    }
}

fn json_value_contains(value: &JsonValue, needle: &str) -> bool {
    match value {
        JsonValue::Array(values) => values
            .iter()
            .any(|value| json_value_contains(value, needle)),
        JsonValue::String(value) => value == needle,
        JsonValue::Number(value) => value.to_string() == needle,
        JsonValue::Bool(value) => value.to_string() == needle,
        JsonValue::Null | JsonValue::Object(_) => false,
    }
}

/// Map a legacy public-identity column name to its canonical rid-envelope
/// field. The rid-envelope refactor exposes identity under `rid` /
/// `collection` / `kind`, but WHERE/ORDER predicates written against the
/// older `entity_id` / `red_collection` / `red_kind` names must still
/// resolve. We only consult this alias when the literal column is absent
/// from the materialized record, so it never shadows a real user column and
/// never adds these names to `SELECT *` output (that stays envelope-clean).
pub(in crate::runtime) fn evaluate_metadata_field_compare(
    field: &FieldRef,
    candidate: &Value,
    op: CompareOp,
    value: &Value,
) -> Option<bool> {
    let column = table_column_name(field)?;
    if !column.eq_ignore_ascii_case("red_capabilities") {
        if column.eq_ignore_ascii_case("red_entity_type") {
            let candidate = runtime_value_text(candidate).map(|item| item.to_ascii_lowercase())?;
            let value = runtime_value_text(value).map(|item| item.to_ascii_lowercase())?;
            return Some(match op {
                CompareOp::Eq => candidate == value,
                CompareOp::Ne => candidate != value,
                _ => false,
            });
        }

        return None;
    }

    let capability = runtime_value_text(value)?;
    let capabilities = runtime_value_text(candidate)?;
    let capabilities = capabilities
        .split(',')
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    let target = capability.trim().to_ascii_lowercase();

    match op {
        CompareOp::Eq => Some(capabilities.iter().any(|value| value == &target)),
        CompareOp::Ne => Some(!capabilities.iter().any(|value| value == &target)),
        _ => None,
    }
}

pub(in crate::runtime) fn evaluate_metadata_field_in(
    field: &FieldRef,
    candidate: &Value,
    values: &[Value],
) -> Option<bool> {
    let column = table_column_name(field)?;
    if !column.eq_ignore_ascii_case("red_capabilities") {
        if !column.eq_ignore_ascii_case("red_entity_type") {
            return None;
        }

        let candidate = runtime_value_text(candidate).map(|item| item.to_ascii_lowercase())?;

        for value in values {
            let Some(value) = runtime_value_text(value) else {
                continue;
            };
            if value.to_ascii_lowercase() == candidate {
                return Some(true);
            }
        }

        return Some(false);
    }

    let capabilities = runtime_value_text(candidate)?
        .split(',')
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();

    if capabilities.is_empty() {
        return Some(false);
    }

    for value in values {
        let Some(value) = runtime_value_text(value) else {
            continue;
        };
        let value = value.trim().to_ascii_lowercase();
        if capabilities.iter().any(|candidate| candidate == &value) {
            return Some(true);
        }
    }
    Some(false)
}

pub(in crate::runtime) fn like_matches(value: &str, pattern: &str) -> bool {
    like_matches_bytes(value.as_bytes(), pattern.as_bytes())
}

/// O(m × n) iterative LIKE matching — mirrors the Wildcards/Leetcode-44 DP
/// approach but without heap allocation. Replaces the recursive version which
/// was exponential on patterns with many `%` wildcards.
///
/// `%` matches any sequence of zero or more characters.
/// `_` matches exactly one character.
/// All other bytes are literal.
pub(in crate::runtime) fn like_matches_bytes(value: &[u8], pattern: &[u8]) -> bool {
    let (mut vi, mut pi) = (0usize, 0usize);
    // `star_vi` / `star_pi`: position after the last `%` wildcard seen.
    let (mut star_vi, mut star_pi) = (usize::MAX, usize::MAX);

    while vi < value.len() {
        if pi < pattern.len() && (pattern[pi] == b'_' || pattern[pi] == value[vi]) {
            vi += 1;
            pi += 1;
        } else if pi < pattern.len() && pattern[pi] == b'%' {
            // Record position right after `%`; the `%` matches empty for now.
            star_vi = vi;
            star_pi = pi;
            pi += 1;
        } else if star_pi != usize::MAX {
            // Backtrack: the `%` consumes one more value character.
            star_vi += 1;
            vi = star_vi;
            pi = star_pi + 1;
        } else {
            return false;
        }
    }

    // Consume trailing `%` wildcards in pattern.
    while pi < pattern.len() && pattern[pi] == b'%' {
        pi += 1;
    }

    pi == pattern.len()
}
