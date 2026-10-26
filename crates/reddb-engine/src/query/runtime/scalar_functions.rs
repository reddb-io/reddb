//! Native legacy scalar dispatch shared by server and WASM hosts.
use super::geographic::resolve_two_geo_points;
use super::numeric::{arith_binop, cast_value_to};
use super::temporal::*;
use super::text::*;
use super::values::value_as_i64;
use crate::query::unified::UnifiedRecord;
use reddb_rql::ast::{Filter, Projection};
use reddb_types::Value;

/// Hosts supply Unix milliseconds, resolve arguments with their input policy, evaluate CASE conditions
/// without database context, and retain scalar effects not owned by this module.
/// Resolution stays lazy; None is distinct from an admitted SQL NULL.
pub trait ScalarFunctionHost {
    fn current_unix_ms(&self) -> i64;
    fn resolve_argument(&self, argument: &Projection, source: &UnifiedRecord) -> Option<Value>;
    fn evaluate_condition(&self, filter: &Filter, source: &UnifiedRecord) -> bool;
    fn fallback_function(
        &self,
        name: &str,
        arguments: &[Projection],
        source: &UnifiedRecord,
    ) -> Option<Value>;
}

pub fn evaluate_scalar_function_with_host<H: ScalarFunctionHost>(
    host: &H,
    name: &str,
    args: &[Projection],
    source: &UnifiedRecord,
) -> Option<Value> {
    // Preserve legacy case sensitivity and strip only the output alias suffix.
    let func_name = name.split(':').next().unwrap_or(name);
    let resolve_scalar_arg = |args: &[Projection], index: usize, source: &UnifiedRecord| {
        host.resolve_argument(args.get(index)?, source)
    };
    match func_name {
        "ADD" | "SUB" | "MUL" | "DIV" | "MOD" => {
            let a = resolve_scalar_arg(args, 0, source)?;
            let b = resolve_scalar_arg(args, 1, source)?;
            Some(arith_binop(func_name, a, b))
        }
        "CONCAT" => {
            let mut out = String::new();
            for idx in 0..args.len() {
                let value = resolve_scalar_arg(args, idx, source)?;
                if matches!(value, Value::Null) {
                    continue;
                }
                out.push_str(&value.plain_text());
            }
            Some(Value::text(out))
        }
        "CASE" => {
            // CASE WHEN cond THEN val ... ELSE val END is encoded as
            //   Function("CASE", [Expression(cond1), val1,
            //                      Expression(cond2), val2,
            //                      ..., else_val?])
            // Even-length args => no ELSE; odd-length => last arg is ELSE.
            // Walk WHEN/THEN pairs left-to-right, short-circuit on first
            // matching predicate. Fall through to ELSE (or Null) when no
            // branch matches.
            let mut i = 0;
            while i + 1 < args.len() {
                if let Projection::Expression(filter, _) = &args[i] {
                    let matched = host.evaluate_condition(filter, source);
                    if matched {
                        return resolve_scalar_arg(args, i + 1, source).or(Some(Value::Null));
                    }
                    i += 2;
                } else {
                    break;
                }
            }
            if args.len() % 2 == 1 {
                return resolve_scalar_arg(args, args.len() - 1, source).or(Some(Value::Null));
            }
            Some(Value::Null)
        }
        "CAST" => {
            // CAST(expr AS type) is parsed into Function("CAST", [inner, Column("TYPE:<name>")]).
            // Resolve the source value, look up the target type by SQL name,
            // and reuse the existing string→Value coerce path. On any
            // failure (unknown type, coerce error) we emit Null so queries
            // keep running — CAST is advisory, not a hard assertion.
            let src = resolve_scalar_arg(args, 0, source)?;
            let Some(Projection::Column(col)) = args.get(1) else {
                return Some(Value::Null);
            };
            let Some(type_name) = col.strip_prefix("TYPE:") else {
                return Some(Value::Null);
            };
            let Some(target) = reddb_types::DataType::from_sql_name(type_name) else {
                return Some(Value::Null);
            };
            Some(cast_value_to(&src, target))
        }
        "UPPER" => {
            let val = resolve_scalar_arg(args, 0, source)?;
            match val {
                Value::Text(s) => Some(Value::text(s.to_uppercase())),
                _ => Some(val),
            }
        }
        "LOWER" => {
            let val = resolve_scalar_arg(args, 0, source)?;
            match val {
                Value::Text(s) => Some(Value::text(s.to_lowercase())),
                _ => Some(val),
            }
        }
        "LENGTH" | "CHAR_LENGTH" | "CHARACTER_LENGTH" => {
            let val = resolve_scalar_arg(args, 0, source)?;
            match val {
                Value::Text(s) => Some(Value::Integer(s.chars().count() as i64)),
                Value::Blob(b) => Some(Value::Integer(b.len() as i64)),
                Value::Array(a) => Some(Value::Integer(a.len() as i64)),
                _ => Some(Value::Null),
            }
        }
        "OCTET_LENGTH" => {
            let val = resolve_scalar_arg(args, 0, source)?;
            match val {
                Value::Text(s) => Some(Value::Integer(s.len() as i64)),
                Value::Blob(b) => Some(Value::Integer(b.len() as i64)),
                _ => Some(Value::Null),
            }
        }
        "BIT_LENGTH" => {
            let val = resolve_scalar_arg(args, 0, source)?;
            match val {
                Value::Text(s) => Some(Value::Integer((s.len() * 8) as i64)),
                Value::Blob(b) => Some(Value::Integer((b.len() * 8) as i64)),
                _ => Some(Value::Null),
            }
        }
        "CONCAT_WS" => {
            let separator: String = match resolve_scalar_arg(args, 0, source)? {
                Value::Null => return Some(Value::Null),
                Value::Text(text) => text.to_string(),
                other => other.display_string(),
            };
            let mut parts = Vec::new();
            for idx in 1..args.len() {
                let value = resolve_scalar_arg(args, idx, source)?;
                if matches!(value, Value::Null) {
                    continue;
                }
                parts.push(value.display_string());
            }
            Some(Value::text(parts.join(&separator)))
        }
        "REVERSE" => {
            let text = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            Some(Value::text(text.chars().rev().collect::<String>()))
        }
        "ABS" => {
            let val = resolve_scalar_arg(args, 0, source)?;
            match val {
                Value::Float(f) => Some(Value::Float(f.abs())),
                Value::Integer(n) => Some(Value::Integer(n.abs())),
                _ => Some(Value::Null),
            }
        }
        "ROUND" => {
            let val = resolve_scalar_arg(args, 0, source)?;
            match val {
                Value::Float(f) => Some(Value::Float(f.round())),
                other => Some(other),
            }
        }
        "COALESCE" => {
            for (i, _) in args.iter().enumerate() {
                if let Some(val) = resolve_scalar_arg(args, i, source) {
                    if val != Value::Null {
                        return Some(val);
                    }
                }
            }
            Some(Value::Null)
        }
        "MONEY" => money_from_scalar_args(host, args, source),
        "MONEY_ASSET" => match resolve_scalar_arg(args, 0, source)? {
            Value::Money { asset_code, .. } => Some(Value::AssetCode(asset_code)),
            _ => Some(Value::Null),
        },
        "MONEY_MINOR" => match resolve_scalar_arg(args, 0, source)? {
            Value::Money { minor_units, .. } => Some(Value::BigInt(minor_units)),
            _ => Some(Value::Null),
        },
        "MONEY_SCALE" => match resolve_scalar_arg(args, 0, source)? {
            Value::Money { scale, .. } => Some(Value::Integer(i64::from(scale))),
            _ => Some(Value::Null),
        },
        "SUBSTRING" | "SUBSTR" => {
            let text = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            match resolve_scalar_arg(args, 1, source)? {
                Value::Text(pattern) if func_name == "SUBSTRING" && args.len() == 2 => {
                    Some(match substring_pattern_text(&text, &pattern) {
                        Some(matched) => Value::text(matched),
                        None => Value::Null,
                    })
                }
                start_value => {
                    let start = value_as_i64(&start_value)?;
                    let count = args.get(2).and_then(|_| {
                        resolve_scalar_arg(args, 2, source).and_then(|value| value_as_i64(&value))
                    });
                    Some(Value::text(substring_text(&text, start, count)?))
                }
            }
        }
        "POSITION" => {
            let needle = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let haystack = match resolve_scalar_arg(args, 1, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            Some(Value::Integer(position_text(&needle, &haystack)))
        }
        "TRIM" | "BTRIM" => {
            let text = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let chars = match args
                .get(1)
                .and_then(|_| resolve_scalar_arg(args, 1, source))
            {
                None => None,
                Some(Value::Text(chars)) => Some(chars),
                Some(_) => return Some(Value::Null),
            };
            Some(Value::text(trim_text(&text, chars.as_deref(), true, true)))
        }
        "LTRIM" => {
            let text = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let chars = match args
                .get(1)
                .and_then(|_| resolve_scalar_arg(args, 1, source))
            {
                None => None,
                Some(Value::Text(chars)) => Some(chars),
                Some(_) => return Some(Value::Null),
            };
            Some(Value::text(trim_text(&text, chars.as_deref(), true, false)))
        }
        "RTRIM" => {
            let text = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let chars = match args
                .get(1)
                .and_then(|_| resolve_scalar_arg(args, 1, source))
            {
                None => None,
                Some(Value::Text(chars)) => Some(chars),
                Some(_) => return Some(Value::Null),
            };
            Some(Value::text(trim_text(&text, chars.as_deref(), false, true)))
        }
        "LEFT" => {
            let text = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let count =
                resolve_scalar_arg(args, 1, source).and_then(|value| value_as_i64(&value))?;
            Some(Value::text(slice_left_text(&text, count)))
        }
        "RIGHT" => {
            let text = match resolve_scalar_arg(args, 0, source)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let count =
                resolve_scalar_arg(args, 1, source).and_then(|value| value_as_i64(&value))?;
            Some(Value::text(slice_right_text(&text, count)))
        }
        "QUOTE_LITERAL" => match resolve_scalar_arg(args, 0, source)? {
            Value::Null => Some(Value::Null),
            Value::Text(text) => Some(Value::text(quote_literal_text(&text))),
            other => Some(Value::text(quote_literal_text(&other.display_string()))),
        },
        "TIME_BUCKET" => {
            let bucket_ns = resolve_time_bucket_duration(args, 0)?;
            let timestamp_ns = resolve_time_bucket_timestamp(args, source, &|argument| {
                host.resolve_argument(argument, source)
            })?;
            Some(Value::UnsignedInteger(bucket_start_ns(
                bucket_ns,
                timestamp_ns,
            )))
        }
        "NOW" | "CURRENT_TIMESTAMP" => Some(Value::TimestampMs(host.current_unix_ms())),
        "CURRENT_DATE" => Some(Value::Date((host.current_unix_ms() / 86_400_000) as i32)),
        "CURRENT_TIME" => Some(Value::Time(time_ms_since_midnight(host.current_unix_ms()))),
        "GEO_DISTANCE" | "HAVERSINE" => {
            let (lat1, lon1, lat2, lon2) = resolve_two_geo_points(args, source, &|argument| {
                host.resolve_argument(argument, source)
            })?;
            Some(Value::Float(crate::geo::haversine_km(
                lat1, lon1, lat2, lon2,
            )))
        }
        "GEO_DISTANCE_VINCENTY" | "VINCENTY" => {
            let (lat1, lon1, lat2, lon2) = resolve_two_geo_points(args, source, &|argument| {
                host.resolve_argument(argument, source)
            })?;
            Some(Value::Float(crate::geo::vincenty_km(
                lat1, lon1, lat2, lon2,
            )))
        }
        "GEO_BEARING" => {
            let (lat1, lon1, lat2, lon2) = resolve_two_geo_points(args, source, &|argument| {
                host.resolve_argument(argument, source)
            })?;
            Some(Value::Float(crate::geo::bearing(lat1, lon1, lat2, lon2)))
        }
        "GEO_MIDPOINT" => {
            let (lat1, lon1, lat2, lon2) = resolve_two_geo_points(args, source, &|argument| {
                host.resolve_argument(argument, source)
            })?;
            let (lat, lon) = crate::geo::midpoint(lat1, lon1, lat2, lon2);
            Some(Value::GeoPoint(
                crate::geo::deg_to_micro(lat),
                crate::geo::deg_to_micro(lon),
            ))
        }
        _ => host.fallback_function(name, args, source),
    }
}

fn money_from_scalar_args<H: ScalarFunctionHost>(
    host: &H,
    args: &[Projection],
    source: &UnifiedRecord,
) -> Option<Value> {
    let resolve_scalar_arg = |args: &[Projection], index: usize, source: &UnifiedRecord| {
        host.resolve_argument(args.get(index)?, source)
    };
    let input = match args {
        [single] => money_arg_text(resolve_scalar_arg(std::slice::from_ref(single), 0, source)?)?,
        [_, _] => {
            let lhs = money_arg_text(resolve_scalar_arg(args, 0, source)?)?;
            let rhs = money_arg_text(resolve_scalar_arg(args, 1, source)?)?;
            format!("{} {}", lhs, rhs)
        }
        _ => return Some(Value::Null),
    };
    match reddb_types::coerce::coerce(&input, reddb_types::DataType::Money, None) {
        Ok(value) => Some(value),
        Err(_) if args.len() == 2 => {
            let lhs = money_arg_text(resolve_scalar_arg(args, 1, source)?)?;
            let rhs = money_arg_text(resolve_scalar_arg(args, 0, source)?)?;
            reddb_types::coerce::coerce(
                &format!("{} {}", lhs, rhs),
                reddb_types::DataType::Money,
                None,
            )
            .ok()
        }
        Err(_) => Some(Value::Null),
    }
}

fn money_arg_text(value: Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Text(text) => Some(text.to_string()),
        Value::AssetCode(code) => Some(code),
        Value::Currency(code) => Some(String::from_utf8_lossy(&code).to_string()),
        other => Some(other.display_string()),
    }
}
