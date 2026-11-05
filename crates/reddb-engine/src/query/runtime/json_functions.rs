//! Native JSON scalar helpers used by value-argument function dispatch.
use reddb_rql::limits::JSON_LITERAL_MAX_DEPTH;
use reddb_types::Value;

// New padding across one JSON_SET call may consume at most the canonical
// JSON literal byte budget: each additional null slot serializes as ",null".
const JSON_SET_ARRAY_GROWTH_MAX: usize = reddb_rql::lexer::JSON_LITERAL_MAX_BYTES / 5;

// ─────────────────────────────────────────────────────────────────────
// JSON scalar helpers (Phase 1.4 PG parity)
// ─────────────────────────────────────────────────────────────────────

/// Parse a scalar argument into a JSON value. `Value::Json` bytes are
/// decoded; `Value::Text` is parsed. Anything else returns None so
/// callers propagate Null.
pub(super) fn value_to_json(value: &Value) -> Option<crate::json::Value> {
    match value {
        Value::Null => Some(crate::json::Value::Null),
        // A document body may be the native binary container (PRD-1398); decode
        // it to JSON so json builtins see the same shape as a plain JSON body.
        Value::Json(bytes) => {
            if let Some(body) = reddb_types::document_body_codec::decode_container_to_json(bytes) {
                return Some(body);
            }
            let text = String::from_utf8_lossy(bytes);
            crate::json::from_str(&text).ok()
        }
        Value::Text(s) => crate::json::from_str(s).ok(),
        _ => None,
    }
}

pub(super) fn json_parse_impl(input: &Value) -> Option<Value> {
    let Value::Text(text) = input else {
        return Some(Value::Null);
    };
    crate::json::from_str::<crate::json::Value>(text)
        .ok()
        .map(|json| Value::Json(json.to_string_compact().into_bytes()))
        .or(Some(Value::Null))
}

pub(super) fn value_contains(value: &Value, needle: &str) -> bool {
    value_contains_at_depth(value, needle, 0)
}

fn value_contains_at_depth(value: &Value, needle: &str, depth: u32) -> bool {
    if depth > JSON_LITERAL_MAX_DEPTH {
        return false;
    }
    match value {
        Value::Array(values) => values
            .iter()
            .any(|value| value_contains_at_depth(value, needle, depth + 1)),
        Value::Json(_) => value_to_json(value)
            .as_ref()
            .is_some_and(|json| json_value_contains(json, needle, depth)),
        Value::Text(text) => text.contains(needle),
        other => other.display_string().contains(needle),
    }
}

fn json_value_contains(value: &crate::json::Value, needle: &str, depth: u32) -> bool {
    if depth > JSON_LITERAL_MAX_DEPTH {
        return false;
    }
    match value {
        crate::json::Value::Array(values) => values
            .iter()
            .any(|value| json_value_contains(value, needle, depth + 1)),
        // Recurse into object *values* so a needle nested inside a JSON
        // object matches — e.g. `CONTAINS(vision_detections, 'person')`
        // over a derived `[{"label":"person",...}]` array (#1275). Keys
        // are deliberately not matched: containment is about the data, not
        // the field names. Mirrors the same rule in the scalar evaluator.
        crate::json::Value::Object(map) => map
            .values()
            .any(|value| json_value_contains(value, needle, depth + 1)),
        crate::json::Value::String(value) => value == needle,
        crate::json::Value::Integer(value) => value.to_string() == needle,
        crate::json::Value::Number(value) => value.to_string() == needle,
        crate::json::Value::Decimal(value) => value == needle,
        crate::json::Value::Bool(value) => value.to_string() == needle,
        crate::json::Value::Null => false,
    }
}

/// Convert any scalar to the JSON representation used by JSON_ARRAY /
/// JSON_OBJECT / JSON_SET. Non-JSON-native types fall back to their
/// display string.
pub(super) fn value_as_json(value: &Value) -> crate::json::Value {
    match value {
        Value::Null => crate::json::Value::Null,
        Value::Boolean(b) => crate::json::Value::Bool(*b),
        Value::Integer(n) => crate::json::Value::Number(*n as f64),
        Value::UnsignedInteger(n) => crate::json::Value::Number(*n as f64),
        Value::BigInt(n) => crate::json::Value::Number(*n as f64),
        Value::Float(n) => crate::json::Value::Number(*n),
        Value::Text(s) => crate::json::Value::String(s.to_string()),
        Value::Json(bytes) => {
            // Decode the native binary container (PRD-1398) if present.
            if let Some(body) = reddb_types::document_body_codec::decode_container_to_json(bytes) {
                return body;
            }
            let text = String::from_utf8_lossy(bytes);
            crate::json::from_str(&text)
                .unwrap_or_else(|_| crate::json::Value::String(text.into_owned()))
        }
        other => crate::json::Value::String(other.display_string()),
    }
}

/// Parse a `$.a.b[0]` path into a sequence of (field | index) steps.
/// Returns None on unrecognised syntax.
enum JsonPathStep<'a> {
    Field(&'a str),
    Index(u64),
}

fn parse_json_path(path: &str) -> Option<Vec<JsonPathStep<'_>>> {
    let path = path.trim();
    let rest = path.strip_prefix('$').unwrap_or(path);
    let mut steps = Vec::new();
    let bytes = rest.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if steps.len() >= JSON_LITERAL_MAX_DEPTH as usize {
            return None;
        }
        match bytes[i] {
            b'.' => {
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != b'.' && bytes[i] != b'[' {
                    i += 1;
                }
                if start == i {
                    return None;
                }
                let field = std::str::from_utf8(&bytes[start..i]).ok()?;
                steps.push(JsonPathStep::Field(field));
            }
            b'[' => {
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != b']' {
                    i += 1;
                }
                if i >= bytes.len() {
                    return None;
                }
                let idx: u64 = std::str::from_utf8(&bytes[start..i]).ok()?.parse().ok()?;
                i += 1; // skip ']'
                steps.push(JsonPathStep::Index(idx));
            }
            _ => return None,
        }
    }
    Some(steps)
}

/// Traverse a JSON value along the path. Returns None if any step misses.
fn json_path_get<'a>(
    root: &'a crate::json::Value,
    steps: &[JsonPathStep<'_>],
) -> Option<&'a crate::json::Value> {
    let mut cur = root;
    for step in steps {
        match (step, cur) {
            (JsonPathStep::Field(name), crate::json::Value::Object(map)) => {
                cur = map.get(*name)?;
            }
            (JsonPathStep::Index(idx), crate::json::Value::Array(arr)) => {
                cur = arr.get(usize::try_from(*idx).ok()?)?;
            }
            _ => return None,
        }
    }
    Some(cur)
}

pub(super) fn json_extract_impl(input: &Value, path: &Value, as_text: bool) -> Option<Value> {
    let path_str = match path {
        Value::Text(s) => s.clone(),
        _ => return Some(Value::Null),
    };
    let json = value_to_json(input)?;
    let steps = parse_json_path(&path_str)?;
    let Some(target) = json_path_get(&json, &steps) else {
        // Missing path → SQL NULL, not function failure.
        return Some(Value::Null);
    };
    if as_text {
        // Unquoted scalar text, JSON for containers.
        match target {
            crate::json::Value::String(s) => Some(Value::text(s.clone())),
            crate::json::Value::Null => Some(Value::Null),
            crate::json::Value::Bool(b) => Some(Value::text(b.to_string())),
            crate::json::Value::Integer(n) => Some(Value::text(n.to_string())),
            crate::json::Value::Number(n) => Some(Value::text(n.to_string())),
            other => Some(Value::text(other.to_string_compact())),
        }
    } else {
        // JSON-serialised representation (strings come back quoted).
        Some(Value::text(target.to_string_compact()))
    }
}

/// Walk along a JSON path, mutably creating missing container nodes. The
/// final step writes `new_value` into its parent (object field or array
/// slot). Arrays grow with nulls when the index exceeds the current length.
pub(super) fn json_set_impl(input: &Value, path: &Value, new_value: &Value) -> Option<Value> {
    let path_str = match path {
        Value::Text(s) => s.clone(),
        _ => return Some(Value::Null),
    };
    let mut json = value_to_json(input).unwrap_or(crate::json::Value::Null);
    let steps = parse_json_path(&path_str)?;
    if steps.is_empty() {
        // Root replacement.
        let replaced = value_as_json(new_value);
        return Some(Value::Json(replaced.to_string_compact().into_bytes()));
    }
    // Walk + insert; use a simple recursive helper so we can own the mutation path.
    fn walk(
        node: &mut crate::json::Value,
        steps: &[JsonPathStep<'_>],
        idx: usize,
        new_value: &crate::json::Value,
        array_growth_remaining: &mut usize,
    ) -> bool {
        if idx > JSON_LITERAL_MAX_DEPTH as usize {
            return false;
        }
        if idx == steps.len() {
            *node = new_value.clone();
            return true;
        }
        match (&steps[idx], node) {
            (JsonPathStep::Field(name), crate::json::Value::Object(map)) => {
                let entry = map
                    .entry(name.to_string())
                    .or_insert(crate::json::Value::Null);
                walk(entry, steps, idx + 1, new_value, array_growth_remaining)
            }
            (JsonPathStep::Field(name), other) => {
                // Coerce non-object into object to keep the path alive.
                let mut new_map = crate::json::Map::new();
                new_map.insert(name.to_string(), crate::json::Value::Null);
                *other = crate::json::Value::Object(new_map);
                if let crate::json::Value::Object(map) = other {
                    let entry = map.get_mut(*name).expect("path field was just inserted");
                    walk(entry, steps, idx + 1, new_value, array_growth_remaining)
                } else {
                    false
                }
            }
            (JsonPathStep::Index(i), crate::json::Value::Array(arr)) => {
                let Some(index) = grow_json_array(arr, *i, array_growth_remaining) else {
                    return false;
                };
                walk(
                    &mut arr[index],
                    steps,
                    idx + 1,
                    new_value,
                    array_growth_remaining,
                )
            }
            (JsonPathStep::Index(i), other) => {
                let mut arr = Vec::new();
                let Some(index) = grow_json_array(&mut arr, *i, array_growth_remaining) else {
                    return false;
                };
                *other = crate::json::Value::Array(arr);
                if let crate::json::Value::Array(arr) = other {
                    walk(
                        &mut arr[index],
                        steps,
                        idx + 1,
                        new_value,
                        array_growth_remaining,
                    )
                } else {
                    false
                }
            }
        }
    }
    let new_json = value_as_json(new_value);
    let mut array_growth_remaining = JSON_SET_ARRAY_GROWTH_MAX;
    if !walk(&mut json, &steps, 0, &new_json, &mut array_growth_remaining) {
        return Some(Value::Null);
    }
    Some(Value::Json(json.to_string_compact().into_bytes()))
}

fn grow_json_array(
    array: &mut Vec<crate::json::Value>,
    index: u64,
    growth_remaining: &mut usize,
) -> Option<usize> {
    let index = usize::try_from(index).ok()?;
    let len = index.checked_add(1)?;
    let growth = len.saturating_sub(array.len());
    if growth > *growth_remaining {
        return None;
    }
    array.try_reserve_exact(growth).ok()?;
    *growth_remaining -= growth;
    if growth != 0 {
        array.resize(len, crate::json::Value::Null);
    }
    Some(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_growth_budget_is_shared_and_rejection_keeps_arrays_intact() {
        let mut remaining = 3;
        let mut first = vec![];
        assert_eq!(grow_json_array(&mut first, 1, &mut remaining), Some(1));
        assert_eq!(remaining, 1);
        let mut second = vec![];
        assert_eq!(grow_json_array(&mut second, 1, &mut remaining), None);
        assert!(second.is_empty());
        assert_eq!(remaining, 1);
        assert_eq!(grow_json_array(&mut first, u64::MAX, &mut remaining), None);
        assert_eq!(first.len(), 2);
        assert_eq!(grow_json_array(&mut first, 0, &mut remaining), Some(0));
        assert_eq!(remaining, 1, "an existing slot consumes no growth budget");
    }
}
