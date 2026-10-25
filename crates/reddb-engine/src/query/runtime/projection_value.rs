//! Native scalar argument resolution over an admitted record.
use super::field_resolution::resolve_runtime_field;
use crate::query::unified::UnifiedRecord;
use reddb_rql::ast::{Expr, Filter, Projection};
use reddb_types::Value;

/// Unlike top-level projection, optional scalar arguments retain legacy fallback
/// on *any* typed evaluation error. Hosts retain effects and secret input policy;
/// the native adapter evaluates without database/table/alias context.
pub trait ProjectionValueHost {
    fn try_expression(&self, expression: &Expr, source: &UnifiedRecord) -> Option<Value>;
    fn fallback_function(
        &self,
        name: &str,
        arguments: &[Projection],
        source: &UnifiedRecord,
    ) -> Option<Value>;
    fn fallback_filter(&self, filter: &Filter, source: &UnifiedRecord) -> bool;
    fn secret_query_input(&self, value: Value) -> Option<Value>;
}

pub fn eval_projection_value_with_host<H: ProjectionValueHost>(
    host: &H,
    projection: &Projection,
    source: &UnifiedRecord,
) -> Option<Value> {
    let value = match projection {
        Projection::Column(column) => {
            // Literal input returns before secret admission, as in the native
            // dispatcher. Aliased literals still pass through the outer check.
            if column.starts_with("LIT:") {
                return decode_projection_literal(column);
            }
            source.get(column.as_str()).cloned()
        }
        Projection::Alias(column, _) => {
            eval_projection_value_with_host(host, &Projection::Column(column.clone()), source)
        }
        Projection::Field(field, _) => resolve_runtime_field(source, field, None, None),
        Projection::Function(name, arguments) => {
            reddb_rql::sql_lowering::projection_to_expr(projection)
                .and_then(|(expression, _)| host.try_expression(&expression, source))
                .or_else(|| host.fallback_function(name, arguments, source))
        }
        Projection::Expression(filter, _) => {
            reddb_rql::sql_lowering::projection_to_expr(projection)
                .and_then(|(expression, _)| host.try_expression(&expression, source))
                .or_else(|| Some(Value::Boolean(host.fallback_filter(filter, source))))
        }
        Projection::All => None,
        Projection::Window { name, alias, .. } => {
            let label = alias.as_ref().unwrap_or(name);
            source.get(label.as_str()).cloned()
        }
    };
    value.and_then(|value| host.secret_query_input(value))
}

/// Decode the native legacy projection sentinel, including its text fallback.
/// This is intentionally not a JSON parser; quoted comma/bracket quirks remain
/// compatible with the native dispatcher. Composite nesting is bounded by the
/// canonical RQL JSON depth cap (root at depth zero).
pub fn decode_projection_literal(column: &str) -> Option<Value> {
    let literal = column.strip_prefix("LIT:")?;
    if literal.is_empty() {
        return Some(Value::Null);
    }
    if let Some(payload) = literal.strip_prefix("@RL:") {
        if let Some(value) = parse_rl_literal(payload, 0) {
            return Some(value);
        }
    }
    if let Ok(number) = literal.parse::<i64>() {
        return Some(Value::Integer(number));
    }
    if let Ok(number) = literal.parse::<f64>() {
        return Some(Value::Float(number));
    }
    Some(Value::text(literal.to_string()))
}

/// Parse the `@RL:` sentinel payload written by
/// `sql_lowering::serialize_value_json`. Handles arrays `[...]`, vectors
/// `V[...]`, and scalar fallbacks. Not a full JSON parser — only
/// covers the shapes the serializer emits.
fn parse_rl_literal(payload: &str, depth: u32) -> Option<Value> {
    if depth > reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
        return None;
    }
    let trimmed = payload.trim();
    if let Some(inner) = trimmed.strip_prefix("V[").and_then(|s| s.strip_suffix(']')) {
        let mut out = Vec::new();
        if !inner.trim().is_empty() {
            for part in inner.split(',') {
                out.push(part.trim().parse::<f32>().ok()?);
            }
        }
        return Some(Value::Vector(out));
    }
    if let Some(inner) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        let mut out = Vec::new();
        if !inner.trim().is_empty() {
            for part in split_top_level(inner) {
                out.push(parse_rl_atom(part.trim(), depth + 1)?);
            }
        }
        return Some(Value::Array(out));
    }
    parse_rl_atom(trimmed, depth)
}

fn parse_rl_atom(s: &str, depth: u32) -> Option<Value> {
    if depth > reddb_rql::limits::JSON_LITERAL_MAX_DEPTH {
        return None;
    }
    if s == "null" {
        return Some(Value::Null);
    }
    if s == "true" {
        return Some(Value::Boolean(true));
    }
    if s == "false" {
        return Some(Value::Boolean(false));
    }
    if let Some(inner) = s.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
        return Some(Value::text(
            inner.replace("\\\"", "\"").replace("\\\\", "\\"),
        ));
    }
    if s.starts_with('[') || s.starts_with("V[") {
        return parse_rl_literal(s, depth);
    }
    if let Ok(n) = s.parse::<i64>() {
        return Some(Value::Integer(n));
    }
    if let Ok(f) = s.parse::<f64>() {
        return Some(Value::Float(f));
    }
    None
}

fn split_top_level(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    let bytes = s.as_bytes();
    for (i, &b) in bytes.iter().enumerate() {
        match b {
            b'[' => depth += 1,
            b']' => depth -= 1,
            b',' if depth == 0 => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if start <= s.len() {
        out.push(&s[start..]);
    }
    out
}
