//! Native legacy geographic argument resolution over admitted records.
//! POINT sentinels and column/document reads retain their direct lookup behavior;
//! other projections and numeric arguments use the host input/evaluation policy.
use super::{
    field_resolution::resolve_runtime_document_path,
    numeric::{value_as_number, NumOperand},
};
use crate::query::unified::UnifiedRecord;
use reddb_rql::ast::Projection;
use reddb_types::Value;

/// Resolve two geographic points from function arguments.
/// Supports: (column, POINT(lat, lon)) or (col1, col2)
pub fn resolve_two_geo_points(
    args: &[Projection],
    source: &UnifiedRecord,
    resolve: &impl Fn(&Projection) -> Option<Value>,
) -> Option<(f64, f64, f64, f64)> {
    match args {
        [left, right] => {
            let (lat1, lon1) = resolve_geo_arg(left, source, resolve)?;
            let (lat2, lon2) = resolve_geo_arg(right, source, resolve)?;
            Some((lat1, lon1, lat2, lon2))
        }
        [left, lat, lon] => {
            let (lat1, lon1) = resolve_geo_arg(left, source, resolve)?;
            let lat2 = resolve_geo_number(lat, resolve)?;
            let lon2 = resolve_geo_number(lon, resolve)?;
            Some((lat1, lon1, lat2, lon2))
        }
        [lat1, lon1, lat2, lon2] => Some((
            resolve_geo_number(lat1, resolve)?,
            resolve_geo_number(lon1, resolve)?,
            resolve_geo_number(lat2, resolve)?,
            resolve_geo_number(lon2, resolve)?,
        )),
        _ => None,
    }
}

/// Resolve a single geo argument — either a column (GeoPoint/Latitude/Longitude) or POINT literal.
pub fn resolve_geo_arg(
    arg: &Projection,
    source: &UnifiedRecord,
    resolve: &impl Fn(&Projection) -> Option<Value>,
) -> Option<(f64, f64)> {
    match arg {
        Projection::Column(col) => {
            // POINT:lat:lon literal
            if let Some(rest) = col.strip_prefix("POINT:") {
                let parts: Vec<&str> = rest.splitn(2, ':').collect();
                if parts.len() == 2 {
                    let lat: f64 = parts[0].parse().ok()?;
                    let lon: f64 = parts[1].parse().ok()?;
                    return Some((lat, lon));
                }
            }
            // Column reference → look up in record values
            let val = source
                .get(col.as_str())
                .cloned()
                .or_else(|| resolve_runtime_document_path(source, col))?;
            match &val {
                value if crate::geo::recognize_geo_value(value).is_some() => {
                    crate::geo::recognize_geo_value(value)
                }
                Value::Float(f) => {
                    // Could be a lat or lon — check for "lat"/"lon" sibling columns
                    let lat_keys = ["lat", "latitude"];
                    let lon_keys = ["lon", "longitude", "lng"];
                    if lat_keys.contains(&col.as_str()) {
                        let lon =
                            lon_keys
                                .iter()
                                .find_map(|k| source.get(k))
                                .and_then(|v| match v {
                                    Value::Float(f) => Some(*f),
                                    Value::Integer(n) => Some(*n as f64),
                                    _ => None,
                                })?;
                        Some((*f, lon))
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }
        _ => {
            let value = resolve(arg)?;
            crate::geo::recognize_geo_value(&value)
        }
    }
}

fn resolve_geo_number(
    arg: &Projection,
    resolve: &impl Fn(&Projection) -> Option<Value>,
) -> Option<f64> {
    let value = resolve(arg)?;
    value_as_number(&value).map(NumOperand::as_f64)
}
