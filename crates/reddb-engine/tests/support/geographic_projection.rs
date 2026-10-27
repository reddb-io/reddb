//! Legacy geographic dispatch uses the same fixture host as other projections.
use super::*;
use reddb_engine::query::runtime::geographic::resolve_geo_arg;

fn column(name: &str) -> Projection {
    Projection::Column(name.into())
}
fn close(value: Option<Value>, expected: f64) {
    let Some(Value::Float(actual)) = value else {
        panic!("expected geographic float, got {value:?}")
    };
    assert!((actual - expected).abs() < 1e-5, "{actual} != {expected}");
}

pub(super) fn aliases_and_operations() {
    for name in ["GEO_DISTANCE:arc", "HAVERSINE"] {
        close(
            scalar_values(
                name,
                vec![
                    Value::Float(0.0),
                    Value::Integer(0),
                    Value::Integer(0),
                    Value::text("1"),
                ],
            ),
            111.19492664455873,
        );
    }
    for name in ["VINCENTY", "GEO_DISTANCE_VINCENTY:arc"] {
        close(
            scalar_values(
                name,
                vec![
                    Value::Integer(0),
                    Value::Integer(0),
                    Value::Integer(0),
                    Value::Integer(1),
                ],
            ),
            111.3194907932264,
        );
    }
    close(
        scalar_values(
            "GEO_BEARING",
            vec![
                Value::Integer(0),
                Value::Integer(0),
                Value::Integer(0),
                Value::Integer(1),
            ],
        ),
        90.0,
    );
    assert_eq!(
        scalar_values(
            "GEO_MIDPOINT",
            vec![
                Value::Integer(0),
                Value::Integer(0),
                Value::Integer(0),
                Value::Integer(2)
            ]
        ),
        Some(Value::GeoPoint(0, 1_000_000))
    );
}

pub(super) fn argument_forms() {
    let host = Host::new(None);
    let mut source = UnifiedRecord::new();
    source.set("point", Value::GeoPoint(0, 0));
    source.set(
        "other",
        Value::Json(br#"{"type":"Point","coordinates":[1,0]}"#.to_vec()),
    );
    source.set(
        "body",
        Value::Json(br#"{"location":{"lat":0,"lon":1}}"#.to_vec()),
    );
    for arguments in [
        vec![column("point"), column("other")],
        vec![column("point"), column("body.location")],
        vec![column("point"), column("POINT:0:1")],
        vec![column("point"), column("LIT:0"), column("LIT:1")],
    ] {
        close(
            evaluate_scalar_function_with_host(&host, "GEO_DISTANCE", &arguments, &source),
            111.19492664455873,
        );
    }
    source.set("lat", Value::Float(0.0));
    source.set("longitude", Value::Integer(1));
    source.set("lng", Value::Integer(2));
    let resolve = |argument: &Projection| eval_projection_value_with_host(&host, argument, &source);
    assert_eq!(
        resolve_geo_arg(&column("lat"), &source, &resolve),
        Some((0.0, 1.0)),
        "retain legacy longitude alias priority"
    );
    assert_eq!(
        resolve_geo_arg(&column("longitude"), &source, &resolve),
        None,
        "only latitude siblings form a point in the native column path"
    );
    assert_eq!(
        resolve_geo_arg(&column("POINT:91:0"), &source, &resolve),
        Some((91.0, 0.0)),
        "sentinel literals retain native parsing rather than stored-value validation"
    );
    for name in [
        "GEO_DISTANCE",
        "HAVERSINE",
        "VINCENTY",
        "GEO_DISTANCE_VINCENTY",
        "GEO_BEARING",
        "GEO_MIDPOINT",
    ] {
        for arguments in [
            vec![],
            vec![column("point")],
            vec![column("missing"), column("point")],
            vec![column("POINT:bad:0"), column("point")],
        ] {
            assert_eq!(
                evaluate_scalar_function_with_host(&host, name, &arguments, &source),
                None,
                "{name}"
            );
        }
    }
}

pub(super) fn input_policy() {
    let host = Host {
        deny_input: true,
        ..Host::new(None)
    };
    let mut source = UnifiedRecord::new();
    source.set("point", Value::GeoPoint(0, 0));
    source.set("angle", Value::Integer(1));
    source.set("protected", Value::Secret(vec![1, 2, 3]));
    let resolve = |argument: &Projection| eval_projection_value_with_host(&host, argument, &source);
    assert_eq!(
        resolve_geo_arg(&column("point"), &source, &resolve),
        Some((0.0, 0.0)),
        "native direct-column lookup stays outside argument admission"
    );
    assert_eq!(
        resolve_geo_arg(&Projection::Field(field("point"), None), &source, &resolve),
        None,
        "field projections retain input admission"
    );
    assert_eq!(
        resolve_geo_arg(&column("protected"), &source, &resolve),
        None,
        "a raw secret wrapper is never a point"
    );
    assert_eq!(
        evaluate_scalar_function_with_host(
            &host,
            "GEO_DISTANCE",
            &[
                column("LIT:0"),
                column("LIT:0"),
                column("LIT:0"),
                column("angle"),
            ],
            &source
        ),
        None,
        "numeric arguments retain host admission"
    );
}

pub(super) fn verify_recovered(host: &Host<'_>, records: &[UnifiedRecord], updated: bool) {
    let columns = projections("SELECT GEO_DISTANCE(0, 0, 0, quantity) AS arc, VINCENTY(0, 0, 0, quantity) AS ellipsoid, GEO_BEARING(0, 0, 0, quantity) AS bearing_value, GEO_MIDPOINT(0, 0, 0, quantity) AS center FROM orders");
    for (index, source) in records.iter().enumerate() {
        let record = project(host, source, &columns).expect("recovered geographic scalars");
        let quantities = if updated { [4, 3] } else { [3, 2] };
        close(
            record.get("arc").cloned(),
            f64::from(quantities[index]) * 111.19492664455873,
        );
        close(
            record.get("ellipsoid").cloned(),
            f64::from(quantities[index]) * 111.3194907932264,
        );
        close(record.get("bearing_value").cloned(), 90.0);
        assert_eq!(
            record.get("center"),
            Some(&Value::GeoPoint(0, quantities[index] * 500_000))
        );
    }
}
