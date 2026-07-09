//! Geographic computation module.
//!
//! Pure functions for distances, bearings, midpoints, and bounding boxes
//! on the WGS-84 ellipsoid and the unit sphere.

use std::f64::consts::PI;

use crate::storage::schema::Value;

pub mod h3;

const EARTH_RADIUS_KM: f64 = 6_371.0;
const EARTH_RADIUS_M: f64 = 6_371_000.0;

// WGS-84 ellipsoid parameters
const WGS84_A: f64 = 6_378_137.0; // semi-major axis (meters)
const WGS84_F: f64 = 1.0 / 298.257_223_563; // flattening
const WGS84_B: f64 = WGS84_A * (1.0 - WGS84_F); // semi-minor axis

pub const RECOGNIZED_GEO_SHAPES: &str = "GEO_POINT or {lat, lon} object";

// ── Helpers ──────────────────────────────────────────────────────────────────

#[inline]
fn to_rad(deg: f64) -> f64 {
    deg * PI / 180.0
}

#[inline]
fn to_deg(rad: f64) -> f64 {
    rad * 180.0 / PI
}

#[inline]
pub fn micro_to_deg(micro: i32) -> f64 {
    micro as f64 / 1_000_000.0
}

#[inline]
pub fn deg_to_micro(deg: f64) -> i32 {
    (deg * 1_000_000.0).round() as i32
}

/// Recognize a storage value as a geographic point in `(lat, lon)` degrees.
pub fn recognize_geo_value(value: &Value) -> Option<(f64, f64)> {
    match value {
        Value::GeoPoint(lat_micro, lon_micro) => {
            recognize_geo_degrees(micro_to_deg(*lat_micro), micro_to_deg(*lon_micro))
        }
        Value::Json(bytes) => recognize_geo_json(bytes),
        _ => None,
    }
}

/// Recognize an object-shaped value from row/node fields as `(lat, lon)` degrees.
pub fn recognize_geo_fields<'a>(field: impl Fn(&str) -> Option<&'a Value>) -> Option<(f64, f64)> {
    let lat = field("lat")
        .or_else(|| field("latitude"))
        .and_then(numeric_value_to_f64)?;
    let lon = field("lon")
        .or_else(|| field("lng"))
        .or_else(|| field("longitude"))
        .and_then(numeric_value_to_f64)?;
    recognize_geo_degrees(lat, lon)
}

fn recognize_geo_degrees(lat: f64, lon: f64) -> Option<(f64, f64)> {
    if lat.is_finite()
        && lon.is_finite()
        && (-90.0..=90.0).contains(&lat)
        && (-180.0..=180.0).contains(&lon)
    {
        Some((lat, lon))
    } else {
        None
    }
}

fn numeric_value_to_f64(value: &Value) -> Option<f64> {
    match value {
        Value::Float(value) => Some(*value),
        Value::Integer(value) => Some(*value as f64),
        Value::UnsignedInteger(value) => Some(*value as f64),
        _ => None,
    }
}

fn recognize_geo_json(bytes: &[u8]) -> Option<(f64, f64)> {
    let json = crate::json::from_slice::<crate::json::Value>(bytes).ok()?;
    let object = json.as_object()?;
    let lat = object
        .get("lat")
        .or_else(|| object.get("latitude"))
        .and_then(json_number_to_f64)?;
    let lon = object
        .get("lon")
        .or_else(|| object.get("lng"))
        .or_else(|| object.get("longitude"))
        .and_then(json_number_to_f64)?;
    recognize_geo_degrees(lat, lon)
}

fn json_number_to_f64(value: &crate::json::Value) -> Option<f64> {
    value.as_f64()
}

// ── Haversine (spherical model) ─────────────────────────────────────────────

/// Great-circle distance between two points in kilometers (spherical Earth).
///
/// Accuracy: ~0.3% error (up to ~20 km over 6000 km). Fast and sufficient for
/// most applications.
pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let dlat = to_rad(lat2 - lat1);
    let dlon = to_rad(lon2 - lon1);
    let lat1_r = to_rad(lat1);
    let lat2_r = to_rad(lat2);

    let a = (dlat / 2.0).sin().powi(2) + lat1_r.cos() * lat2_r.cos() * (dlon / 2.0).sin().powi(2);
    EARTH_RADIUS_KM * 2.0 * a.sqrt().asin()
}

/// Great-circle distance in meters (spherical Earth).
pub fn haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    haversine_km(lat1, lon1, lat2, lon2) * 1000.0
}

// ── Vincenty (ellipsoidal model, WGS-84) ────────────────────────────────────

/// Geodesic distance between two points in meters using the Vincenty inverse
/// formula on the WGS-84 ellipsoid.
///
/// Accuracy: sub-millimeter. Iterative — converges in 3-8 iterations for most
/// point pairs. Falls back to haversine for antipodal points where Vincenty
/// does not converge.
pub fn vincenty_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let u1 = ((1.0 - WGS84_F) * to_rad(lat1).tan()).atan();
    let u2 = ((1.0 - WGS84_F) * to_rad(lat2).tan()).atan();
    let l = to_rad(lon2 - lon1);

    let sin_u1 = u1.sin();
    let cos_u1 = u1.cos();
    let sin_u2 = u2.sin();
    let cos_u2 = u2.cos();

    let mut lambda = l;
    let mut prev_lambda;
    let mut sin_sigma;
    let mut cos_sigma;
    let mut sigma;
    let mut sin_alpha;
    let mut cos2_alpha;
    let mut cos_2sigma_m;

    for _ in 0..100 {
        let sin_lambda = lambda.sin();
        let cos_lambda = lambda.cos();

        sin_sigma = ((cos_u2 * sin_lambda).powi(2)
            + (cos_u1 * sin_u2 - sin_u1 * cos_u2 * cos_lambda).powi(2))
        .sqrt();

        if sin_sigma == 0.0 {
            return 0.0; // coincident points
        }

        cos_sigma = sin_u1 * sin_u2 + cos_u1 * cos_u2 * cos_lambda;
        sigma = sin_sigma.atan2(cos_sigma);
        sin_alpha = cos_u1 * cos_u2 * sin_lambda / sin_sigma;
        cos2_alpha = 1.0 - sin_alpha.powi(2);

        cos_2sigma_m = if cos2_alpha != 0.0 {
            cos_sigma - 2.0 * sin_u1 * sin_u2 / cos2_alpha
        } else {
            0.0
        };

        let c = WGS84_F / 16.0 * cos2_alpha * (4.0 + WGS84_F * (4.0 - 3.0 * cos2_alpha));

        prev_lambda = lambda;
        lambda = l
            + (1.0 - c)
                * WGS84_F
                * sin_alpha
                * (sigma
                    + c * sin_sigma
                        * (cos_2sigma_m + c * cos_sigma * (-1.0 + 2.0 * cos_2sigma_m.powi(2))));

        if (lambda - prev_lambda).abs() < 1e-12 {
            // Converged — compute distance
            let u_sq = cos2_alpha * (WGS84_A.powi(2) - WGS84_B.powi(2)) / WGS84_B.powi(2);
            let a_coeff =
                1.0 + u_sq / 16384.0 * (4096.0 + u_sq * (-768.0 + u_sq * (320.0 - 175.0 * u_sq)));
            let b_coeff = u_sq / 1024.0 * (256.0 + u_sq * (-128.0 + u_sq * (74.0 - 47.0 * u_sq)));

            let delta_sigma = b_coeff
                * sin_sigma
                * (cos_2sigma_m
                    + b_coeff / 4.0
                        * (cos_sigma * (-1.0 + 2.0 * cos_2sigma_m.powi(2))
                            - b_coeff / 6.0
                                * cos_2sigma_m
                                * (-3.0 + 4.0 * sin_sigma.powi(2))
                                * (-3.0 + 4.0 * cos_2sigma_m.powi(2))));

            return WGS84_B * a_coeff * (sigma - delta_sigma);
        }
    }

    // Vincenty did not converge (near-antipodal points) — fall back to haversine
    haversine_m(lat1, lon1, lat2, lon2)
}

/// Geodesic distance in kilometers (WGS-84 ellipsoid).
pub fn vincenty_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    vincenty_m(lat1, lon1, lat2, lon2) / 1000.0
}

// ── Bearing / Azimuth ───────────────────────────────────────────────────────

/// Initial bearing (forward azimuth) from point 1 to point 2 in degrees [0, 360).
///
/// This is the compass direction you would face at point 1 looking toward point 2.
/// North = 0°, East = 90°, South = 180°, West = 270°.
pub fn bearing(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let lat1_r = to_rad(lat1);
    let lat2_r = to_rad(lat2);
    let dlon = to_rad(lon2 - lon1);

    let y = dlon.sin() * lat2_r.cos();
    let x = lat1_r.cos() * lat2_r.sin() - lat1_r.sin() * lat2_r.cos() * dlon.cos();

    (to_deg(y.atan2(x)) + 360.0) % 360.0
}

/// Final bearing (reverse azimuth) at point 2 arriving from point 1.
pub fn final_bearing(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    (bearing(lat2, lon2, lat1, lon1) + 180.0) % 360.0
}

// ── Midpoint ────────────────────────────────────────────────────────────────

/// Geographic midpoint between two points (great-circle arc).
/// Returns (latitude, longitude) in degrees.
pub fn midpoint(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> (f64, f64) {
    let lat1_r = to_rad(lat1);
    let lat2_r = to_rad(lat2);
    let dlon = to_rad(lon2 - lon1);

    let bx = lat2_r.cos() * dlon.cos();
    let by = lat2_r.cos() * dlon.sin();

    let lat =
        (lat1_r.sin() + lat2_r.sin()).atan2(((lat1_r.cos() + bx).powi(2) + by.powi(2)).sqrt());
    let lon = to_rad(lon1) + by.atan2(lat1_r.cos() + bx);

    (to_deg(lat), to_deg(lon))
}

// ── Destination point ───────────────────────────────────────────────────────

/// Point at a given distance and bearing from a start point.
/// Returns (latitude, longitude) in degrees.
pub fn destination(lat: f64, lon: f64, bearing_deg: f64, distance_km: f64) -> (f64, f64) {
    let lat_r = to_rad(lat);
    let brng_r = to_rad(bearing_deg);
    let d = distance_km / EARTH_RADIUS_KM;

    let lat2 = (lat_r.sin() * d.cos() + lat_r.cos() * d.sin() * brng_r.cos()).asin();
    let lon2 = to_rad(lon)
        + (brng_r.sin() * d.sin() * lat_r.cos()).atan2(d.cos() - lat_r.sin() * lat2.sin());

    (to_deg(lat2), to_deg(lon2))
}

// ── Bounding box ────────────────────────────────────────────────────────────

/// Compute a bounding box around a center point with a given radius in km.
/// Returns (min_lat, min_lon, max_lat, max_lon) in degrees.
///
/// Uses a conservative approximation that works correctly near the poles
/// and across the antimeridian.
pub fn bounding_box(lat: f64, lon: f64, radius_km: f64) -> (f64, f64, f64, f64) {
    let lat_delta = radius_km / 111.32;
    let lon_delta = radius_km / (111.32 * to_rad(lat).cos().max(0.0001));

    let min_lat = (lat - lat_delta).max(-90.0);
    let max_lat = (lat + lat_delta).min(90.0);
    let min_lon = lon - lon_delta;
    let max_lon = lon + lon_delta;

    (min_lat, min_lon, max_lat, max_lon)
}

// ── Area ────────────────────────────────────────────────────────────────────

/// Approximate area of a spherical polygon defined by vertices, in square kilometers.
/// Uses the spherical excess formula. Vertices must be in order (CW or CCW).
pub fn polygon_area_km2(vertices: &[(f64, f64)]) -> f64 {
    let n = vertices.len();
    if n < 3 {
        return 0.0;
    }

    let mut total = 0.0f64;
    for i in 0..n {
        let (lat1, lon1) = vertices[i];
        let (lat2, lon2) = vertices[(i + 1) % n];
        total += to_rad(lon2 - lon1) * (2.0 + to_rad(lat1).sin() + to_rad(lat2).sin());
    }

    (total.abs() / 2.0) * EARTH_RADIUS_KM * EARTH_RADIUS_KM
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use std::collections::HashMap;

    fn json_value(json: &str) -> Value {
        Value::Json(json.as_bytes().to_vec())
    }

    fn fields(values: &[(&str, Value)]) -> HashMap<String, Value> {
        values
            .iter()
            .map(|(key, value)| ((*key).to_string(), value.clone()))
            .collect()
    }

    #[test]
    fn test_haversine_paris_london() {
        let d = haversine_km(48.8566, 2.3522, 51.5074, -0.1278);
        assert!((d - 344.0).abs() < 5.0, "Paris-London: {d} km");
    }

    #[test]
    fn test_haversine_zero_distance() {
        let d = haversine_km(0.0, 0.0, 0.0, 0.0);
        assert!(d.abs() < 0.001, "same point: {d} km");
    }

    #[test]
    fn test_haversine_antipodal() {
        let d = haversine_km(0.0, 0.0, 0.0, 180.0);
        assert!((d - 20015.0).abs() < 100.0, "antipodal: {d} km");
    }

    #[test]
    fn test_vincenty_paris_london() {
        let d = vincenty_km(48.8566, 2.3522, 51.5074, -0.1278);
        assert!((d - 343.5).abs() < 2.0, "Vincenty Paris-London: {d} km");
    }

    #[test]
    fn test_vincenty_coincident() {
        let d = vincenty_m(48.8566, 2.3522, 48.8566, 2.3522);
        assert!(d.abs() < 0.001, "coincident: {d} m");
    }

    #[test]
    fn test_vincenty_new_york_tokyo() {
        let d = vincenty_km(40.7128, -74.0060, 35.6762, 139.6503);
        assert!((d - 10838.0).abs() < 50.0, "NY-Tokyo: {d} km");
    }

    #[test]
    fn test_bearing_north() {
        let b = bearing(0.0, 0.0, 1.0, 0.0);
        assert!((b - 0.0).abs() < 1.0, "north bearing: {b}°");
    }

    #[test]
    fn test_bearing_east() {
        let b = bearing(0.0, 0.0, 0.0, 1.0);
        assert!((b - 90.0).abs() < 1.0, "east bearing: {b}°");
    }

    #[test]
    fn test_midpoint_equator() {
        let (lat, lon) = midpoint(0.0, 0.0, 0.0, 10.0);
        assert!((lat - 0.0).abs() < 0.01, "midpoint lat: {lat}");
        assert!((lon - 5.0).abs() < 0.01, "midpoint lon: {lon}");
    }

    #[test]
    fn test_destination() {
        let (lat, lon) = destination(0.0, 0.0, 0.0, 111.32);
        assert!((lat - 1.0).abs() < 0.1, "destination lat: {lat}");
        assert!(lon.abs() < 0.1, "destination lon: {lon}");
    }

    #[test]
    fn test_bounding_box() {
        let (min_lat, min_lon, max_lat, max_lon) = bounding_box(0.0, 0.0, 111.32);
        assert!((min_lat - (-1.0)).abs() < 0.1);
        assert!((max_lat - 1.0).abs() < 0.1);
        assert!(min_lon < 0.0);
        assert!(max_lon > 0.0);
    }

    #[test]
    fn test_micro_conversion() {
        let lat = -23.550520;
        let micro = deg_to_micro(lat);
        let back = micro_to_deg(micro);
        assert!((lat - back).abs() < 0.000001);
    }

    #[test]
    fn recognize_geo_value_accepts_geopoint_and_json_aliases() {
        assert_eq!(
            recognize_geo_value(&Value::GeoPoint(38_760_000, -77_150_000)),
            Some((38.76, -77.15))
        );
        assert_eq!(
            recognize_geo_value(&json_value(r#"{"lat":38.76,"lon":-77.15}"#)),
            Some((38.76, -77.15))
        );
        assert_eq!(
            recognize_geo_value(&json_value(r#"{"latitude":38,"lng":-77}"#)),
            Some((38.0, -77.0))
        );
        assert_eq!(
            recognize_geo_value(&json_value(r#"{"latitude":38.76,"longitude":-77.15}"#)),
            Some((38.76, -77.15))
        );
    }

    #[test]
    fn recognize_geo_fields_accepts_numeric_aliases() {
        let row = fields(&[
            ("latitude", Value::Integer(38)),
            ("longitude", Value::Float(-77.15)),
        ]);
        assert_eq!(
            recognize_geo_fields(|key| row.get(key)),
            Some((38.0, -77.15))
        );

        let node = fields(&[("lat", Value::Float(38.76)), ("lng", Value::Integer(-77))]);
        assert_eq!(
            recognize_geo_fields(|key| node.get(key)),
            Some((38.76, -77.0))
        );
    }

    #[test]
    fn recognize_geo_rejects_non_geo_shapes() {
        for value in [
            json_value(r#"{"lat":"38.76","lon":"-77.15"}"#),
            json_value(r#"{"type":"Point","coordinates":[-77.15,38.76]}"#),
            json_value(r#"{"lat":38.76}"#),
            json_value(r#"{"lat":91.0,"lon":0.0}"#),
            json_value(r#"{"lat":0.0,"lon":181.0}"#),
            json_value(r#"{"lat":null,"lon":0.0}"#),
            json_value(r#"not json"#),
            Value::text("38.76,-77.15".to_string()),
        ] {
            assert_eq!(recognize_geo_value(&value), None, "{value:?}");
        }

        let string_fields = fields(&[
            ("lat", Value::text("38.76".to_string())),
            ("lon", Value::Float(-77.15)),
        ]);
        assert_eq!(recognize_geo_fields(|key| string_fields.get(key)), None);

        let missing_fields = fields(&[("lat", Value::Float(38.76))]);
        assert_eq!(recognize_geo_fields(|key| missing_fields.get(key)), None);

        let non_finite_fields = fields(&[
            ("lat", Value::Float(f64::NAN)),
            ("lon", Value::Float(-77.15)),
        ]);
        assert_eq!(recognize_geo_fields(|key| non_finite_fields.get(key)), None);

        let out_of_range_fields =
            fields(&[("lat", Value::Float(38.76)), ("lon", Value::Float(-181.0))]);
        assert_eq!(
            recognize_geo_fields(|key| out_of_range_fields.get(key)),
            None
        );
    }

    proptest! {
        #[test]
        fn recognize_geo_json_and_field_maps_do_not_drift(
            lat in -90.0f64..=90.0,
            lon in -180.0f64..=180.0,
        ) {
            prop_assume!(lat.is_finite());
            prop_assume!(lon.is_finite());
            let json = json_value(&format!(r#"{{"lat":{lat},"lon":{lon}}}"#));
            let fields = fields(&[("lat", Value::Float(lat)), ("lon", Value::Float(lon))]);
            prop_assert_eq!(
                recognize_geo_value(&json),
                recognize_geo_fields(|key| fields.get(key))
            );
        }

        #[test]
        fn recognize_geo_rejects_generated_out_of_range_values(
            lat in prop_oneof![-1000.0f64..-90.000_001, 90.000_001f64..1000.0],
            lon in -180.0f64..=180.0,
        ) {
            let json = json_value(&format!(r#"{{"lat":{lat},"lon":{lon}}}"#));
            let fields = fields(&[("lat", Value::Float(lat)), ("lon", Value::Float(lon))]);
            prop_assert_eq!(recognize_geo_value(&json), None);
            prop_assert_eq!(recognize_geo_fields(|key| fields.get(key)), None);
        }
    }
}
