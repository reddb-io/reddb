//! Canonical native geographic shapes and arithmetic in native and browser WASM.
use reddb_engine::geo::*;
use reddb_types::Value;

pub const CASES: &[(&str, fn())] = &[
    (
        "geo_values_and_coordinate_forms",
        values_and_coordinate_forms,
    ),
    (
        "geo_spherical_and_ellipsoidal_math",
        spherical_and_ellipsoidal_math,
    ),
    ("geo_polygon_and_bounds", polygon_and_bounds),
];

fn values_and_coordinate_forms() {
    assert_eq!(
        recognize_geo_value(&Value::GeoPoint(38_760_000, -77_150_000)),
        Some((38.76, -77.15))
    );
    for json in [
        r#"{"lat":38.76,"lon":-77.15}"#,
        r#"{"latitude":38.76,"lng":-77.15}"#,
        r#"{"type":"Point","coordinates":[-77.15,38.76]}"#,
    ] {
        assert_eq!(
            recognize_geo_value(&Value::Json(json.as_bytes().to_vec())),
            Some((38.76, -77.15))
        );
    }
    let fields = [
        ("lat", Value::DecimalText("38.76000000".into())),
        ("lon", Value::DecimalText("-77.15000000".into())),
    ];
    assert_eq!(
        recognize_geo_fields(|key| fields
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| value)),
        Some((38.76, -77.15))
    );
    for json in [
        r#"{"type":"Point","coordinates":[-77,38,0]}"#,
        r#"{"type":"Point","coordinates":["-77",38]}"#,
        r#"{"lat":91,"lon":0}"#,
        r#"{"lat":0,"lon":181}"#,
        r#"{"lat":0}"#,
    ] {
        assert_eq!(
            recognize_geo_value(&Value::Json(json.as_bytes().to_vec())),
            None
        );
    }
    assert_eq!(recognize_geo_value(&Value::GeoPoint(91_000_000, 0)), None);
    assert_eq!(recognize_geo_value(&Value::text("38.76,-77.15")), None);
    assert_eq!(recognize_geo_value(&Value::Secret(vec![1, 2, 3])), None);
    let nested = "[".repeat(reddb_rql::limits::JSON_LITERAL_MAX_DEPTH as usize + 1);
    assert_eq!(recognize_geo_value(&Value::Json(nested.into_bytes())), None);
}

fn spherical_and_ellipsoidal_math() {
    assert!((haversine_km(0.0, 0.0, 0.0, 1.0) - 111.19492664455873).abs() < 1e-6);
    assert!((vincenty_km(0.0, 0.0, 0.0, 1.0) - 111.3194907932264).abs() < 1e-6);
    assert_eq!(haversine_km(0.0, 0.0, 0.0, 0.0), 0.0);
    assert_eq!(vincenty_m(0.0, 0.0, 0.0, 0.0), 0.0);
    assert!(
        (vincenty_km(0.0, 0.0, 0.0, 180.0) - 20015.086796020572).abs() < 0.01,
        "bounded Vincenty iteration retains antipodal fallback"
    );
    assert!((bearing(0.0, 0.0, 0.0, 1.0) - 90.0).abs() < 1e-9);
    assert!((final_bearing(0.0, 0.0, 0.0, 1.0) - 90.0).abs() < 1e-9);
    let (lat, lon) = midpoint(0.0, 0.0, 0.0, 10.0);
    assert!(lat.abs() < 1e-9 && (lon - 5.0).abs() < 1e-9);
    let (lat, lon) = destination(0.0, 0.0, 90.0, 111.19492664455873);
    assert!(lat.abs() < 1e-6 && (lon - 1.0).abs() < 1e-6);
    assert_eq!(deg_to_micro(-23.550520), -23_550_520);
    assert!((micro_to_deg(-23_550_520) + 23.550520).abs() < 1e-9);
}

fn polygon_and_bounds() {
    let vertices = [(0.0, 0.0), (0.0, 1.0), (1.0, 1.0), (1.0, 0.0)];
    assert!(point_in_polygon_even_odd(0.5, 0.5, &vertices));
    assert!(point_in_polygon_even_odd(0.0, 0.5, &vertices));
    assert!(point_in_polygon_even_odd(1.0, 1.0, &vertices));
    assert!(!point_in_polygon_even_odd(2.0, 2.0, &vertices));
    assert!(!point_in_polygon_even_odd(0.0, 0.0, &[]));
    assert!((polygon_area_km2(&vertices) - 12363.7).abs() < 2.0);
    let (min_lat, min_lon, max_lat, max_lon) = bounding_box(0.0, 179.9, 50.0);
    assert!(min_lat < 0.0 && max_lat > 0.0 && min_lon < 179.9 && max_lon > 180.0);
    let (min_lat, _, max_lat, _) = bounding_box(90.0, 0.0, 50.0);
    assert!(min_lat < 90.0 && max_lat == 90.0);
}
