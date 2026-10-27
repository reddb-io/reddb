//! Native value-argument scalar dispatch shared by server and WASM hosts.
use super::json_functions::*;
use super::temporal::{
    bucket_start_ns, time_bucket_duration, time_ms_since_midnight, value_to_bucket_timestamp_ns,
};
use super::text::{
    position_text, quote_literal_text, slice_left_text, slice_right_text, substring_pattern_text,
    substring_text, trim_text,
};
use super::values::value_as_i64;
use reddb_types::Value;

/// Arguments are already resolved/admitted by the caller. The host supplies
/// time and retains identity, advisory locks, H3/password and unknown functions.
pub trait ValueFunctionHost {
    fn current_unix_ms(&self) -> i64;
    fn fallback_function(&self, name: &str, arguments: &[Value]) -> Option<Value>;
}

pub fn evaluate_value_function_with_host<H: ValueFunctionHost>(
    host: &H,
    name: &str,
    args: &[Value],
) -> Option<Value> {
    match name {
        "UPPER" => match args.first()? {
            Value::Text(s) => Some(Value::text(s.to_uppercase())),
            other => Some(other.clone()),
        },
        "LOWER" => match args.first()? {
            Value::Text(s) => Some(Value::text(s.to_lowercase())),
            other => Some(other.clone()),
        },
        "LENGTH" | "CHAR_LENGTH" | "CHARACTER_LENGTH" => match args.first()? {
            Value::Text(s) => Some(Value::Integer(s.chars().count() as i64)),
            Value::Blob(b) => Some(Value::Integer(b.len() as i64)),
            Value::Array(a) => Some(Value::Integer(a.len() as i64)),
            _ => Some(Value::Null),
        },
        "CONTAINS" => {
            let haystack = args.first()?;
            let needle = match args.get(1)? {
                Value::Text(text) => text.as_ref(),
                _ => return Some(Value::Null),
            };
            Some(Value::Boolean(value_contains(haystack, needle)))
        }
        "OCTET_LENGTH" => match args.first()? {
            Value::Text(s) => Some(Value::Integer(s.len() as i64)),
            Value::Blob(b) => Some(Value::Integer(b.len() as i64)),
            _ => Some(Value::Null),
        },
        "BIT_LENGTH" => match args.first()? {
            Value::Text(s) => i64::try_from(s.len())
                .ok()?
                .checked_mul(8)
                .map(Value::Integer),
            Value::Blob(b) => i64::try_from(b.len())
                .ok()?
                .checked_mul(8)
                .map(Value::Integer),
            _ => Some(Value::Null),
        },
        "SUBSTRING" | "SUBSTR" => {
            let text = match args.first()? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            match args.get(1)? {
                Value::Text(pattern) if name == "SUBSTRING" && args.len() == 2 => {
                    Some(match substring_pattern_text(text, pattern) {
                        Some(matched) => Value::text(matched),
                        None => Value::Null,
                    })
                }
                start_value => {
                    let start = value_as_i64(start_value)?;
                    let count = args.get(2).and_then(value_as_i64);
                    Some(Value::text(substring_text(text, start, count)?))
                }
            }
        }
        "POSITION" => {
            let needle = match args.first()? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let haystack = match args.get(1)? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            Some(Value::Integer(position_text(needle, haystack)))
        }
        "TRIM" | "BTRIM" => {
            let text = match args.first()? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let chars = match args.get(1) {
                None => None,
                Some(Value::Text(chars)) => Some(chars.as_ref()),
                Some(_) => return Some(Value::Null),
            };
            Some(Value::text(trim_text(text, chars, true, true)))
        }
        "LTRIM" => {
            let text = match args.first()? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let chars = match args.get(1) {
                None => None,
                Some(Value::Text(chars)) => Some(chars.as_ref()),
                Some(_) => return Some(Value::Null),
            };
            Some(Value::text(trim_text(text, chars, true, false)))
        }
        "RTRIM" => {
            let text = match args.first()? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let chars = match args.get(1) {
                None => None,
                Some(Value::Text(chars)) => Some(chars.as_ref()),
                Some(_) => return Some(Value::Null),
            };
            Some(Value::text(trim_text(text, chars, false, true)))
        }
        "CONCAT" => Some(Value::text(
            args.iter()
                .filter(|value| !matches!(value, Value::Null))
                .map(Value::plain_text)
                .collect::<String>(),
        )),
        "CONCAT_WS" => {
            let separator = match args.first()? {
                Value::Null => return Some(Value::Null),
                Value::Text(text) => text.as_ref(),
                other => {
                    return Some(Value::text(
                        args.iter()
                            .skip(1)
                            .filter(|value| !matches!(value, Value::Null))
                            .map(Value::display_string)
                            .collect::<Vec<_>>()
                            .join(&other.display_string()),
                    ))
                }
            };
            Some(Value::text(
                args.iter()
                    .skip(1)
                    .filter(|value| !matches!(value, Value::Null))
                    .map(Value::display_string)
                    .collect::<Vec<_>>()
                    .join(separator),
            ))
        }
        "REVERSE" => match args.first()? {
            Value::Text(text) => Some(Value::text(text.chars().rev().collect::<String>())),
            _ => Some(Value::Null),
        },
        "LEFT" => {
            let text = match args.first()? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let count = value_as_i64(args.get(1)?)?;
            Some(Value::text(slice_left_text(text, count)))
        }
        "RIGHT" => {
            let text = match args.first()? {
                Value::Text(text) => text,
                _ => return Some(Value::Null),
            };
            let count = value_as_i64(args.get(1)?)?;
            Some(Value::text(slice_right_text(text, count)))
        }
        "QUOTE_LITERAL" => match args.first()? {
            Value::Null => Some(Value::Null),
            Value::Text(text) => Some(Value::text(quote_literal_text(text))),
            other => Some(Value::text(quote_literal_text(&other.display_string()))),
        },
        "ABS" => match args.first()? {
            Value::Integer(n) => n.checked_abs().map(Value::Integer),
            Value::Float(f) => Some(Value::Float(f.abs())),
            _ => Some(Value::Null),
        },
        "ROUND" => match args.first()? {
            Value::Float(f) => Some(Value::Float(f.round())),
            other => Some(other.clone()),
        },
        "FLOOR" => match args.first()? {
            Value::Float(f) => Some(Value::Float(f.floor())),
            other => Some(other.clone()),
        },
        "CEIL" => match args.first()? {
            Value::Float(f) => Some(Value::Float(f.ceil())),
            other => Some(other.clone()),
        },
        "COALESCE" => args
            .iter()
            .find(|v| !matches!(v, Value::Null))
            .cloned()
            .or(Some(Value::Null)),
        "NOW" | "CURRENT_TIMESTAMP" => Some(Value::TimestampMs(host.current_unix_ms())),
        "CURRENT_DATE" => Some(Value::Date((host.current_unix_ms() / 86_400_000) as i32)),
        "CURRENT_TIME" => Some(Value::Time(time_ms_since_midnight(host.current_unix_ms()))),
        "TIME_BUCKET" => {
            let bucket_ns = time_bucket_duration(args.first()?)?;
            let timestamp_ns = args.get(1).and_then(value_to_bucket_timestamp_ns)?;
            Some(Value::UnsignedInteger(bucket_start_ns(
                bucket_ns,
                timestamp_ns,
            )))
        }
        "GEO_DISTANCE" | "HAVERSINE" => {
            let (lat1, lon1, lat2, lon2) = geo_args(args)?;
            Some(Value::Float(crate::geo::haversine_km(
                lat1, lon1, lat2, lon2,
            )))
        }
        "VINCENTY" => {
            let (lat1, lon1, lat2, lon2) = geo_args(args)?;
            Some(Value::Float(crate::geo::vincenty_km(
                lat1, lon1, lat2, lon2,
            )))
        }
        "GEO_BEARING" => {
            let (lat1, lon1, lat2, lon2) = geo_args(args)?;
            Some(Value::Float(crate::geo::bearing(lat1, lon1, lat2, lon2)))
        }
        // `POLYGON((lat lon), …)` — a flat `[lat0, lon0, lat1, lon1, …]`
        // array. The parser guarantees an even argument count of at least
        // three pairs; a coordinate that does not evaluate to a number
        // collapses the whole polygon to NULL.
        "POLYGON" => {
            let coords = args
                .iter()
                .map(|value| value_as_f64(value).map(Value::Float))
                .collect::<Option<Vec<_>>>()?;
            Some(Value::Array(coords))
        }
        // `GEO_WITHIN(col, POLYGON(...))` — the composable form of the
        // `SEARCH SPATIAL WITHIN POLYGON` verb, sharing its exact even-odd
        // inside-test. A row whose column holds no recognizable geo value
        // is outside every polygon, exactly as the verb skips it.
        "GEO_WITHIN" => {
            let vertices = polygon_vertices(args.get(1)?)?;
            let Some((lat, lon)) = geo_point(args.first()?) else {
                return Some(Value::Boolean(false));
            };
            Some(Value::Boolean(crate::geo::point_in_polygon_even_odd(
                lat, lon, &vertices,
            )))
        }
        "POWER" => {
            let base = value_as_f64(args.first()?)?;
            let exp = value_as_f64(args.get(1)?)?;
            Some(Value::Float(base.powf(exp)))
        }
        "MONEY" => money_from_args(args),
        "MONEY_ASSET" => match args.first()? {
            Value::Money { asset_code, .. } => Some(Value::AssetCode(asset_code.clone())),
            _ => Some(Value::Null),
        },
        "MONEY_MINOR" => match args.first()? {
            Value::Money { minor_units, .. } => Some(Value::BigInt(*minor_units)),
            _ => Some(Value::Null),
        },
        "MONEY_SCALE" => match args.first()? {
            Value::Money { scale, .. } => Some(Value::Integer(i64::from(*scale))),
            _ => Some(Value::Null),
        },
        // ─────────────────────────────────────────────────────────────
        // JSON functions (Phase 1.4 PG parity).
        //
        // Accepts Value::Json, Value::Text (parsed as JSON), or returns
        // Value::Null when the input is not valid JSON.
        // ─────────────────────────────────────────────────────────────
        "JSON_EXTRACT" => json_extract_impl(args.first()?, args.get(1)?, /*as_text=*/ false),
        "JSON_EXTRACT_TEXT" => {
            json_extract_impl(args.first()?, args.get(1)?, /*as_text=*/ true)
        }
        "JSON_PARSE" => json_parse_impl(args.first()?),
        "JSON_SET" => json_set_impl(args.first()?, args.get(1)?, args.get(2)?),
        "JSON_ARRAY_LENGTH" => {
            let v = value_to_json(args.first()?)?;
            match v {
                crate::json::Value::Array(a) => Some(Value::Integer(a.len() as i64)),
                _ => Some(Value::Null),
            }
        }
        "JSON_TYPEOF" => {
            let v = value_to_json(args.first()?)?;
            let name = match v {
                crate::json::Value::Null => "null",
                crate::json::Value::Bool(_) => "boolean",
                crate::json::Value::Integer(_) => "number",
                crate::json::Value::Number(_) => "number",
                crate::json::Value::Decimal(_) => "number",
                crate::json::Value::String(_) => "string",
                crate::json::Value::Array(_) => "array",
                crate::json::Value::Object(_) => "object",
            };
            Some(Value::text(name.to_string()))
        }
        "JSON_VALID" => {
            let text: String = match args.first()? {
                Value::Text(s) => s.to_string(),
                Value::Json(b) => String::from_utf8_lossy(b).to_string(),
                _ => return Some(Value::Boolean(false)),
            };
            Some(Value::Boolean(
                crate::json::from_str::<crate::json::Value>(&text).is_ok(),
            ))
        }
        "JSON_ARRAY" => {
            let arr: Vec<crate::json::Value> = args.iter().map(value_as_json).collect();
            let json = crate::json::Value::Array(arr);
            Some(Value::Json(json.to_string_compact().into_bytes()))
        }
        "JSON_OBJECT" => {
            // Args come as interleaved (key, value, key, value, ...).
            if !args.len().is_multiple_of(2) {
                return Some(Value::Null);
            }
            let mut map = crate::json::Map::new();
            for pair in args.chunks_exact(2) {
                let key: String = match &pair[0] {
                    Value::Text(s) => s.to_string(),
                    other => other.display_string(),
                };
                map.insert(key, value_as_json(&pair[1]));
            }
            let json = crate::json::Value::Object(map);
            Some(Value::Json(json.to_string_compact().into_bytes()))
        }
        _ => host.fallback_function(name, args),
    }
}

fn money_from_args(args: &[Value]) -> Option<Value> {
    let input = match args {
        [single] => money_arg_text(single)?,
        [left, right] => format!("{} {}", money_arg_text(left)?, money_arg_text(right)?),
        _ => return Some(Value::Null),
    };
    match reddb_types::coerce::coerce(&input, reddb_types::DataType::Money, None) {
        Ok(value) => Some(value),
        Err(_) if args.len() == 2 => {
            let reversed = format!(
                "{} {}",
                money_arg_text(&args[1])?,
                money_arg_text(&args[0])?
            );
            reddb_types::coerce::coerce(&reversed, reddb_types::DataType::Money, None).ok()
        }
        Err(_) => Some(Value::Null),
    }
}

fn money_arg_text(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Text(text) => Some(text.to_string()),
        Value::AssetCode(code) => Some(code.clone()),
        Value::Currency(code) => Some(String::from_utf8_lossy(code).to_string()),
        other => Some(other.display_string()),
    }
}

fn geo_args(args: &[Value]) -> Option<(f64, f64, f64, f64)> {
    match args {
        [left, right] => {
            let (lat1, lon1) = geo_point(left)?;
            let (lat2, lon2) = geo_point(right)?;
            Some((lat1, lon1, lat2, lon2))
        }
        [left, lat2, lon2] => {
            let (lat1, lon1) = geo_point(left)?;
            Some((lat1, lon1, value_as_f64(lat2)?, value_as_f64(lon2)?))
        }
        [lat1, lon1, lat2, lon2] => Some((
            value_as_f64(lat1)?,
            value_as_f64(lon1)?,
            value_as_f64(lat2)?,
            value_as_f64(lon2)?,
        )),
        _ => None,
    }
}

pub fn geo_point(value: &Value) -> Option<(f64, f64)> {
    crate::geo::recognize_geo_value(value)
}

fn polygon_vertices(value: &Value) -> Option<Vec<(f64, f64)>> {
    let Value::Array(coords) = value else {
        return None;
    };
    if coords.len() < 6 || coords.len() % 2 != 0 {
        return None;
    }
    coords
        .chunks_exact(2)
        .map(|pair| Some((value_as_f64(&pair[0])?, value_as_f64(&pair[1])?)))
        .collect()
}

pub fn value_as_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Float(value) => Some(*value),
        Value::Integer(value) => Some(*value as f64),
        Value::UnsignedInteger(value) => Some(*value as f64),
        Value::BigInt(value) => Some(*value as f64),
        _ => None,
    }
}
