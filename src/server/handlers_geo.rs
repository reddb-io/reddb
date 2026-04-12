use super::transport::{json_response, HttpResponse};
use crate::json::{from_slice as json_from_slice, Map, Value as JsonValue};

pub(crate) fn handle_geo_distance(body: Vec<u8>) -> HttpResponse {
    let body: JsonValue = json_from_slice(&body).unwrap_or(JsonValue::Null);
    let (lat1, lon1, lat2, lon2) = match extract_two_points(&body) {
        Some(v) => v,
        None => return json_response(400, err_json("provide from.lat, from.lon, to.lat, to.lon")),
    };

    let method = match &body {
        JsonValue::Object(obj) => obj
            .get("method")
            .and_then(|v| match v {
                JsonValue::String(s) => Some(s.as_str()),
                _ => None,
            })
            .unwrap_or("haversine"),
        _ => "haversine",
    };

    let (km, m) = match method {
        "vincenty" => {
            let m = crate::geo::vincenty_m(lat1, lon1, lat2, lon2);
            (m / 1000.0, m)
        }
        _ => {
            let km = crate::geo::haversine_km(lat1, lon1, lat2, lon2);
            (km, km * 1000.0)
        }
    };

    let mut obj = Map::new();
    obj.insert("ok".to_string(), JsonValue::Bool(true));
    obj.insert("distance_km".to_string(), JsonValue::Number(km));
    obj.insert("distance_m".to_string(), JsonValue::Number(m));
    obj.insert("method".to_string(), JsonValue::String(method.to_string()));
    json_response(200, JsonValue::Object(obj))
}

pub(crate) fn handle_geo_bearing(body: Vec<u8>) -> HttpResponse {
    let body: JsonValue = json_from_slice(&body).unwrap_or(JsonValue::Null);
    let (lat1, lon1, lat2, lon2) = match extract_two_points(&body) {
        Some(v) => v,
        None => return json_response(400, err_json("provide from.lat, from.lon, to.lat, to.lon")),
    };

    let initial = crate::geo::bearing(lat1, lon1, lat2, lon2);
    let final_b = crate::geo::final_bearing(lat1, lon1, lat2, lon2);

    let mut obj = Map::new();
    obj.insert("ok".to_string(), JsonValue::Bool(true));
    obj.insert("initial_bearing".to_string(), JsonValue::Number(initial));
    obj.insert("final_bearing".to_string(), JsonValue::Number(final_b));
    json_response(200, JsonValue::Object(obj))
}

pub(crate) fn handle_geo_midpoint(body: Vec<u8>) -> HttpResponse {
    let body: JsonValue = json_from_slice(&body).unwrap_or(JsonValue::Null);
    let (lat1, lon1, lat2, lon2) = match extract_two_points(&body) {
        Some(v) => v,
        None => return json_response(400, err_json("provide from.lat, from.lon, to.lat, to.lon")),
    };

    let (lat, lon) = crate::geo::midpoint(lat1, lon1, lat2, lon2);

    let mut obj = Map::new();
    obj.insert("ok".to_string(), JsonValue::Bool(true));
    obj.insert("lat".to_string(), JsonValue::Number(lat));
    obj.insert("lon".to_string(), JsonValue::Number(lon));
    json_response(200, JsonValue::Object(obj))
}

pub(crate) fn handle_geo_destination(body: Vec<u8>) -> HttpResponse {
    let body: JsonValue = json_from_slice(&body).unwrap_or(JsonValue::Null);
    let obj = match &body {
        JsonValue::Object(o) => o,
        _ => return json_response(400, err_json("provide lat, lon, bearing, distance_km")),
    };

    let lat = num_field(obj, "lat").unwrap_or(0.0);
    let lon = num_field(obj, "lon").unwrap_or(0.0);
    let bearing_deg = num_field(obj, "bearing").unwrap_or(0.0);
    let distance_km = num_field(obj, "distance_km").unwrap_or(0.0);

    let (dest_lat, dest_lon) = crate::geo::destination(lat, lon, bearing_deg, distance_km);

    let mut out = Map::new();
    out.insert("ok".to_string(), JsonValue::Bool(true));
    out.insert("lat".to_string(), JsonValue::Number(dest_lat));
    out.insert("lon".to_string(), JsonValue::Number(dest_lon));
    json_response(200, JsonValue::Object(out))
}

pub(crate) fn handle_geo_bounding_box(body: Vec<u8>) -> HttpResponse {
    let body: JsonValue = json_from_slice(&body).unwrap_or(JsonValue::Null);
    let obj = match &body {
        JsonValue::Object(o) => o,
        _ => return json_response(400, err_json("provide lat, lon, radius_km")),
    };

    let lat = num_field(obj, "lat").unwrap_or(0.0);
    let lon = num_field(obj, "lon").unwrap_or(0.0);
    let radius_km = num_field(obj, "radius_km").unwrap_or(1.0);

    let (min_lat, min_lon, max_lat, max_lon) = crate::geo::bounding_box(lat, lon, radius_km);

    let mut out = Map::new();
    out.insert("ok".to_string(), JsonValue::Bool(true));
    out.insert("min_lat".to_string(), JsonValue::Number(min_lat));
    out.insert("min_lon".to_string(), JsonValue::Number(min_lon));
    out.insert("max_lat".to_string(), JsonValue::Number(max_lat));
    out.insert("max_lon".to_string(), JsonValue::Number(max_lon));
    json_response(200, JsonValue::Object(out))
}

fn extract_two_points(body: &JsonValue) -> Option<(f64, f64, f64, f64)> {
    let obj = match body {
        JsonValue::Object(o) => o,
        _ => return None,
    };

    let from = obj.get("from").and_then(|v| match v {
        JsonValue::Object(o) => Some(o),
        _ => None,
    })?;
    let to = obj.get("to").and_then(|v| match v {
        JsonValue::Object(o) => Some(o),
        _ => None,
    })?;

    Some((
        num_field(from, "lat")?,
        num_field(from, "lon")?,
        num_field(to, "lat")?,
        num_field(to, "lon")?,
    ))
}

fn num_field(obj: &Map<String, JsonValue>, key: &str) -> Option<f64> {
    obj.get(key).and_then(|v| match v {
        JsonValue::Number(n) => Some(*n),
        _ => None,
    })
}

fn err_json(msg: &str) -> JsonValue {
    let mut obj = Map::<String, JsonValue>::new();
    obj.insert("ok".to_string(), JsonValue::Bool(false));
    obj.insert("error".to_string(), JsonValue::String(msg.to_string()));
    JsonValue::Object(obj)
}
