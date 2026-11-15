//! Public-SQL contracts for every function in the native typed catalog.
//!
//! Pure functions use compact table-driven cases. Stateful and contextual
//! functions keep scenario assertions so the test checks behaviour, not just
//! that a function name parses. The catalog gate at the bottom makes adding a
//! function without adding a public SQL journey a test failure.

use std::collections::BTreeSet;
use std::time::{SystemTime, UNIX_EPOCH};

use reddb::auth::Role;
use reddb::runtime::mvcc::{
    clear_current_auth_identity, clear_current_tenant, set_current_auth_identity,
};
use reddb::RedDBRuntime;
use reddb_types::function_catalog::FUNCTION_CATALOG;
use reddb_types::Value;

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> reddb::runtime::RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("query failed: {sql}\n{err:?}"))
}

fn scalar(rt: &RedDBRuntime, sql: &str) -> Value {
    let result = exec(rt, sql);
    assert_eq!(result.result.records.len(), 1, "{sql}");
    result.result.records[0]
        .get("value")
        .unwrap_or_else(|| panic!("missing value column for {sql}: {result:?}"))
        .clone()
}

fn scalar_params(rt: &RedDBRuntime, sql: &str, params: &[Value]) -> Value {
    let result = rt
        .execute_query_with_params(sql, params)
        .unwrap_or_else(|err| panic!("query failed: {sql}\n{err:?}"));
    assert_eq!(result.result.records.len(), 1, "{sql}");
    result.result.records[0]
        .get("value")
        .unwrap_or_else(|| panic!("missing value column for {sql}: {result:?}"))
        .clone()
}

fn iso_fingerprint() -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"FMR\0 20\0");
    bytes.extend_from_slice(&48_u32.to_be_bytes());
    bytes.extend_from_slice(&0_u16.to_be_bytes());
    bytes.extend_from_slice(&500_u16.to_be_bytes());
    bytes.extend_from_slice(&500_u16.to_be_bytes());
    bytes.extend_from_slice(&197_u16.to_be_bytes());
    bytes.extend_from_slice(&197_u16.to_be_bytes());
    bytes.extend_from_slice(&[1, 0]);
    bytes.extend_from_slice(&[1, 0, 100, 3]);
    for (x, y, angle) in [(100_u16, 120_u16, 10_u8), (180, 160, 40), (130, 230, 90)] {
        bytes.extend_from_slice(&(x | 0x4000).to_be_bytes());
        bytes.extend_from_slice(&y.to_be_bytes());
        bytes.extend_from_slice(&[angle, 90]);
    }
    bytes.extend_from_slice(&[0, 0]);
    bytes
}

fn iso_face_landmarks() -> Vec<u8> {
    let points = [
        (0xc1_u8, 10_u16, 20_u16),
        (0xc2, 30, 20),
        (0xc3, 14, 30),
        (0xc4, 26, 30),
        (0x9f, 20, 36),
    ];
    let block_length = 20 + points.len() * 8 + 12;
    let mut bytes = vec![0; 14 + block_length];
    let total_length = bytes.len() as u32;
    bytes[..8].copy_from_slice(b"FAC\0010\0");
    bytes[8..12].copy_from_slice(&total_length.to_be_bytes());
    bytes[12..14].copy_from_slice(&1_u16.to_be_bytes());
    bytes[14..18].copy_from_slice(&(block_length as u32).to_be_bytes());
    bytes[18..20].copy_from_slice(&(points.len() as u16).to_be_bytes());
    for (index, (code, x, y)) in points.iter().enumerate() {
        let offset = 34 + index * 8;
        bytes[offset] = 1;
        bytes[offset + 1] = *code;
        bytes[offset + 2..offset + 4].copy_from_slice(&x.to_be_bytes());
        bytes[offset + 4..offset + 6].copy_from_slice(&y.to_be_bytes());
    }
    let image = 34 + points.len() * 8;
    bytes[image + 2..image + 4].copy_from_slice(&1_u16.to_be_bytes());
    bytes[image + 4..image + 6].copy_from_slice(&1_u16.to_be_bytes());
    bytes
}

fn iso_iris() -> Vec<u8> {
    let mut bytes = vec![0; 68];
    bytes[..8].copy_from_slice(b"IIR\0020\0");
    bytes[8..12].copy_from_slice(&68_u32.to_be_bytes());
    bytes[12..14].copy_from_slice(&1_u16.to_be_bytes());
    bytes[15] = 1;
    bytes[16..20].copy_from_slice(&52_u32.to_be_bytes());
    bytes[41..43].copy_from_slice(&1_u16.to_be_bytes());
    bytes[43..45].copy_from_slice(&1_u16.to_be_bytes());
    bytes[45] = 8;
    bytes
}

fn assert_float(actual: &Value, expected: f64, sql: &str) {
    let Value::Float(actual) = actual else {
        panic!("expected float {expected} for {sql}, got {actual:?}");
    };
    assert!(
        (actual - expected).abs() < 1e-12,
        "expected {expected} for {sql}, got {actual}"
    );
}

#[rustfmt::skip]
macro_rules! exact_function_cases {
    ($case:ident) => {
        $case!("UPPER", "SELECT UPPER('RedDB') AS value", Value::text("REDDB"));
        $case!("LOWER", "SELECT LOWER('RedDB') AS value", Value::text("reddb"));
        $case!("LENGTH", "SELECT LENGTH('é') AS value", Value::Integer(1));
        $case!("CHAR_LENGTH", "SELECT CHAR_LENGTH('é') AS value", Value::Integer(1));
        $case!("CHARACTER_LENGTH", "SELECT CHARACTER_LENGTH('é') AS value", Value::Integer(1));
        $case!("OCTET_LENGTH", "SELECT OCTET_LENGTH('é') AS value", Value::Integer(2));
        $case!("BIT_LENGTH", "SELECT BIT_LENGTH('é') AS value", Value::Integer(16));
        $case!("SUBSTRING", "SELECT SUBSTRING('abcdef', 2, 3) AS value", Value::text("bcd"));
        $case!("SUBSTR", "SELECT SUBSTR('abcdef', 2, 3) AS value", Value::text("bcd"));
        $case!("POSITION", "SELECT POSITION('db', 'reddb') AS value", Value::Integer(4));
        $case!("TRIM", "SELECT TRIM('  red  ') AS value", Value::text("red"));
        $case!("LTRIM", "SELECT LTRIM('xxred', 'x') AS value", Value::text("red"));
        $case!("RTRIM", "SELECT RTRIM('redxx', 'x') AS value", Value::text("red"));
        $case!("BTRIM", "SELECT BTRIM('xxredxx', 'x') AS value", Value::text("red"));
        $case!("CONCAT", "SELECT CONCAT('red', NULL, 'db') AS value", Value::text("reddb"));
        $case!("CONCAT_WS", "SELECT CONCAT_WS('-', 'red', NULL, 'db') AS value", Value::text("red-db"));
        $case!("REVERSE", "SELECT REVERSE('RedDB') AS value", Value::text("BDdeR"));
        $case!("LEFT", "SELECT LEFT('reddb', 3) AS value", Value::text("red"));
        $case!("RIGHT", "SELECT RIGHT('reddb', 2) AS value", Value::text("db"));
        $case!("QUOTE_LITERAL", "SELECT QUOTE_LITERAL('O''Reilly') AS value", Value::text("'O''Reilly'"));
        $case!("COALESCE", "SELECT COALESCE(NULL, 'fallback') AS value", Value::text("fallback"));
        $case!("ABS", "SELECT ABS(-7) AS value", Value::Integer(7));
        $case!("TIME_BUCKET", "SELECT TIME_BUCKET('5s', 7000000000) AS value", Value::UnsignedInteger(5_000_000_000));
        $case!("MONEY", "SELECT MONEY('USD 12.34') AS value", Value::Money { asset_code: "USD".to_string(), minor_units: 1_234, scale: 2 });
        $case!("MONEY_ASSET", "SELECT MONEY_ASSET(MONEY('USD 12.34')) AS value", Value::AssetCode("USD".to_string()));
        $case!("MONEY_MINOR", "SELECT MONEY_MINOR(MONEY('USD 12.34')) AS value", Value::BigInt(1_234));
        $case!("MONEY_SCALE", "SELECT MONEY_SCALE(MONEY('USD 12.34')) AS value", Value::Integer(2));
        $case!("TEMPERATURE", "SELECT TEMPERATURE('20 C') AS value", Value::Temperature(reddb_types::Temperature::new(20, 0, reddb_types::TemperatureUnit::Celsius).expect("valid temperature")));
        $case!("TEMPERATURE_UNIT", "SELECT TEMPERATURE_UNIT(TEMPERATURE('20 C')) AS value", Value::text("C"));
        $case!("JSON_PARSE", "SELECT JSON_PARSE('{\"a\":1}') AS value", Value::Json(br#"{"a":1}"#.to_vec()));
        $case!("JSON_EXTRACT", "SELECT JSON_EXTRACT('{\"a\":\"x\"}', '$.a') AS value", Value::text("\"x\""));
        $case!("JSON_EXTRACT_TEXT", "SELECT JSON_EXTRACT_TEXT('{\"a\":\"x\"}', '$.a') AS value", Value::text("x"));
        $case!("CONTAINS", "SELECT CONTAINS('{\"tags\":[\"red\",\"db\"]}', 'db') AS value", Value::Boolean(true));
        $case!("JSON_SET", "SELECT JSON_SET('{\"a\":1}', '$.b', 'x') AS value", Value::Json(br#"{"a":1,"b":"x"}"#.to_vec()));
        $case!("JSON_ARRAY_LENGTH", "SELECT JSON_ARRAY_LENGTH('[1,2,3]') AS value", Value::Integer(3));
        $case!("JSON_TYPEOF", "SELECT JSON_TYPEOF('{\"a\":1}') AS value", Value::text("object"));
        $case!("JSON_VALID", "SELECT JSON_VALID('{\"a\":1}') AS value", Value::Boolean(true));
        $case!("JSON_ARRAY", "SELECT JSON_ARRAY_LENGTH(JSON_ARRAY(1, 'x', true)) AS value", Value::Integer(3));
        $case!("JSON_OBJECT", "SELECT JSON_EXTRACT_TEXT(JSON_OBJECT('k', 'v'), '$.k') AS value", Value::text("v"));
    };
}

#[rustfmt::skip]
macro_rules! float_function_cases {
    ($case:ident) => {
        $case!("ROUND", "SELECT ROUND(1.6) AS value", 2.0);
        $case!("FLOOR", "SELECT FLOOR(1.6) AS value", 1.0);
        $case!("CEIL", "SELECT CEIL(1.1) AS value", 2.0);
        $case!("SQRT", "SELECT SQRT(9) AS value", 3.0);
        $case!("POWER", "SELECT POWER(2, 3) AS value", 8.0);
        $case!("POW", "SELECT POW(2, 4) AS value", 16.0);
        $case!("EXP", "SELECT EXP(0) AS value", 1.0);
        $case!("LN", "SELECT LN(EXP(1)) AS value", 1.0);
        $case!("LOG", "SELECT LOG(100) AS value", 2.0);
        $case!("LOG10", "SELECT LOG10(1000) AS value", 3.0);
        $case!("SIN", "SELECT SIN(0) AS value", 0.0);
        $case!("COS", "SELECT COS(0) AS value", 1.0);
        $case!("TAN", "SELECT TAN(0) AS value", 0.0);
        $case!("ASIN", "SELECT ASIN(0) AS value", 0.0);
        $case!("ARCSIN", "SELECT ARCSIN(0) AS value", 0.0);
        $case!("ACOS", "SELECT ACOS(1) AS value", 0.0);
        $case!("ARCCOS", "SELECT ARCCOS(1) AS value", 0.0);
        $case!("ATAN", "SELECT ATAN(1) AS value", std::f64::consts::FRAC_PI_4);
        $case!("ARCTAN", "SELECT ARCTAN(1) AS value", std::f64::consts::FRAC_PI_4);
        $case!("ATAN2", "SELECT ATAN2(1, 1) AS value", std::f64::consts::FRAC_PI_4);
        $case!("COT", "SELECT COT(RADIANS(45)) AS value", 1.0);
        $case!("DEGREES", "SELECT DEGREES(PI()) AS value", 180.0);
        $case!("RADIANS", "SELECT RADIANS(180) AS value", std::f64::consts::PI);
        $case!("PI", "SELECT PI() AS value", std::f64::consts::PI);
        $case!("TEMPERATURE_VALUE", "SELECT TEMPERATURE_VALUE(TEMPERATURE('20 C'), 'F') AS value", 68.0);
    };
}

#[test]
fn pure_native_functions_return_their_documented_values() {
    let rt = runtime();
    macro_rules! exact_case {
        ($name:expr, $sql:expr, $expected:expr) => {
            assert_eq!(scalar(&rt, $sql), $expected, "{}", $name)
        };
    }
    macro_rules! float_case {
        ($name:expr, $sql:expr, $expected:expr) => {
            assert_float(&scalar(&rt, $sql), $expected, $sql)
        };
    }
    exact_function_cases!(exact_case);
    float_function_cases!(float_case);
}

#[test]
fn biometric_and_landmark_functions_have_public_sql_contracts() {
    let rt = runtime();
    let space = Value::Uuid([0x51; 16]);

    let fingerprint = scalar_params(
        &rt,
        "SELECT FINGERPRINT($1) AS value",
        &[Value::Blob(iso_fingerprint())],
    );
    let face = scalar_params(
        &rt,
        "SELECT FACE($1) AS value",
        &[Value::Blob(iso_face_landmarks())],
    );
    let iris = scalar_params(&rt, "SELECT IRIS($1) AS value", &[Value::Blob(iso_iris())]);
    assert_eq!(
        scalar_params(&rt, "SELECT BIOMETRIC_FORMAT($1) AS value", &[face.clone()]),
        Value::text("ISO/IEC 19794-5")
    );
    assert!(matches!(
        scalar_params(&rt, "SELECT BIOMETRIC_INFO($1) AS value", &[iris]),
        Value::Json(_)
    ));

    let feature = scalar_params(
        &rt,
        "SELECT FEATURE_VECTOR($1, $2) AS value",
        &[space.clone(), Value::Vector(vec![1.0, 0.0])],
    );
    assert_eq!(
        scalar_params(
            &rt,
            "SELECT FEATURE_COMPARE($1, $2) AS value",
            &[feature.clone(), feature.clone()],
        ),
        Value::Float(1.0)
    );
    assert_eq!(
        scalar_params(
            &rt,
            "SELECT FEATURE_MATCH($1, $2, $3) AS value",
            &[feature.clone(), feature, Value::Float(1.0)],
        ),
        Value::Boolean(true)
    );

    let bits = scalar_params(
        &rt,
        "SELECT BIT_SIGNATURE($1, $2, $3) AS value",
        &[
            space.clone(),
            Value::Blob(vec![0xaa]),
            Value::Blob(vec![0xff]),
        ],
    );
    assert_eq!(
        scalar_params(
            &rt,
            "SELECT BIT_SIGNATURE_COMPARE($1, $2) AS value",
            &[bits.clone(), bits.clone()],
        ),
        Value::Float(1.0)
    );
    assert_eq!(
        scalar_params(
            &rt,
            "SELECT BIT_SIGNATURE_MATCH($1, $2, $3) AS value",
            &[bits.clone(), bits, Value::Float(1.0)],
        ),
        Value::Boolean(true)
    );
    assert_eq!(
        scalar_params(
            &rt,
            "SELECT FINGERPRINT_COMPARE($1, $2) AS value",
            &[fingerprint.clone(), fingerprint.clone()],
        ),
        Value::Float(1.0)
    );
    assert_eq!(
        scalar_params(
            &rt,
            "SELECT FINGERPRINT_MATCH($1, $2, $3) AS value",
            &[fingerprint.clone(), fingerprint, Value::Float(1.0)],
        ),
        Value::Boolean(true)
    );

    let geometry = br#"{"dimensions":2,"pairs":[{"left":[-1,1],"right":[1,1]},{"left":[-2,-1],"right":[2,-1]}],"midline":[[0,2]]}"#;
    let landmarks = scalar_params(
        &rt,
        "SELECT LANDMARK_SET($1, $2) AS value",
        &[space, Value::Json(geometry.to_vec())],
    );
    assert_eq!(
        scalar_params(
            &rt,
            "SELECT LANDMARK_SYMMETRY($1) AS value",
            std::slice::from_ref(&landmarks),
        ),
        Value::Float(1.0)
    );
    assert_eq!(
        scalar_params(
            &rt,
            "SELECT LANDMARK_IS_SYMMETRIC($1, $2) AS value",
            &[landmarks, Value::Integer(1)],
        ),
        Value::Boolean(true)
    );
    let extracted = scalar_params(&rt, "SELECT FACE_LANDMARKS($1) AS value", &[face]);
    assert!(matches!(extracted, Value::LandmarkSet(_)));
}

#[test]
fn aggregate_functions_fold_real_rows_and_ignore_nulls() {
    let rt = runtime();
    exec(
        &rt,
        "CREATE TABLE samples (id INTEGER, n FLOAT, label TEXT)",
    );
    exec(
        &rt,
        "INSERT INTO samples (id, n, label) VALUES \
         (1, 1.0, 'a'), (2, 2.0, 'b'), (3, 3.0, 'c'), (4, NULL, NULL)",
    );

    let result = exec(
        &rt,
        "SELECT COUNT(*) AS row_count, COUNT(n) AS present, SUM(n) AS total, AVG(n) AS mean, \
         MIN(n) AS least, MAX(label) AS greatest, VARIANCE(n) AS variance, \
         STDDEV(n) AS stddev, GROUP_CONCAT(label, '|') AS grouped, \
         STRING_AGG(label, '-') AS joined FROM samples",
    );
    let row = &result.result.records[0];
    assert_eq!(row.get("row_count"), Some(&Value::Integer(4)));
    assert_eq!(row.get("present"), Some(&Value::Integer(3)));
    assert_eq!(row.get("total"), Some(&Value::Integer(6)));
    assert_eq!(row.get("mean"), Some(&Value::Float(2.0)));
    assert_eq!(row.get("least"), Some(&Value::Float(1.0)));
    assert_eq!(row.get("greatest"), Some(&Value::text("c")));
    assert_float(
        row.get("variance").expect("variance"),
        2.0 / 3.0,
        "VARIANCE(n)",
    );
    assert_float(
        row.get("stddev").expect("stddev"),
        (2.0_f64 / 3.0).sqrt(),
        "STDDEV(n)",
    );
    assert_eq!(row.get("grouped"), Some(&Value::text("a|b|c")));
    assert_eq!(row.get("joined"), Some(&Value::text("a-b-c")));
}

#[test]
fn geographic_and_h3_functions_preserve_spatial_relationships() {
    let rt = runtime();
    exec(&rt, "CREATE TABLE places (id INTEGER, loc GEOPOINT)");
    exec(
        &rt,
        "INSERT INTO places (id, loc) VALUES (1, '38.76,-77.15')",
    );

    let cell = reddb::geo::h3::lat_lng_to_cell(38.76, -77.15, 9);
    assert_eq!(
        scalar(&rt, "SELECT H3_INDEX(38.76, -77.15, 9) AS value"),
        Value::UnsignedInteger(cell)
    );
    assert_eq!(
        scalar(&rt, "SELECT H3_CELL(loc, 9) AS value FROM places"),
        Value::UnsignedInteger(cell)
    );

    let parent = reddb::geo::h3::cell_to_parent(cell, 4);
    assert_eq!(
        scalar(
            &rt,
            "SELECT H3_PARENT(H3_CELL(loc, 9), 4) AS value FROM places",
        ),
        Value::UnsignedInteger(parent)
    );

    let centre = scalar(&rt, &format!("SELECT H3_TO_LATLNG({cell}) AS value"));
    let Value::Array(centre) = centre else {
        panic!("H3_TO_LATLNG must return [lat, lon]");
    };
    assert_eq!(centre.len(), 2);
    let Value::Float(lat) = &centre[0] else {
        panic!("latitude")
    };
    let Value::Float(lon) = &centre[1] else {
        panic!("longitude")
    };
    assert!((lat - 38.76).abs() < 0.01, "cell latitude {lat}");
    assert!((lon + 77.15).abs() < 0.01, "cell longitude {lon}");

    let ring = scalar(&rt, &format!("SELECT H3_RING({cell}, 1) AS value"));
    let Value::Array(ring) = ring else {
        panic!("H3_RING must return cells");
    };
    assert_eq!(ring.len(), 7);
    assert!(ring.contains(&Value::UnsignedInteger(cell)));

    let inside = exec(
        &rt,
        "SELECT id FROM places WHERE GEO_WITHIN(loc, \
         POLYGON((38.70 -77.20), (38.80 -77.20), (38.80 -77.05), (38.70 -77.05)))",
    );
    assert_eq!(inside.result.records.len(), 1);

    let distance = scalar(
        &rt,
        "SELECT GEO_DISTANCE(48.8566, 2.3522, 51.5074, -0.1278) AS value",
    );
    let haversine = scalar(
        &rt,
        "SELECT HAVERSINE(48.8566, 2.3522, 51.5074, -0.1278) AS value",
    );
    assert_eq!(distance, haversine);
    let Value::Float(distance) = distance else {
        panic!("distance")
    };
    assert!(
        (340.0..350.0).contains(&distance),
        "Paris-London: {distance}"
    );

    let vincenty = scalar(
        &rt,
        "SELECT VINCENTY(48.8566, 2.3522, 51.5074, -0.1278) AS value",
    );
    let Value::Float(vincenty) = vincenty else {
        panic!("vincenty")
    };
    assert!(
        (340.0..350.0).contains(&vincenty),
        "Paris-London: {vincenty}"
    );

    let bearing = scalar(&rt, "SELECT GEO_BEARING(0.0, 0.0, 1.0, 0.0) AS value");
    assert_float(&bearing, 0.0, "GEO_BEARING north");
}

struct ContextGuard;

impl Drop for ContextGuard {
    fn drop(&mut self) {
        clear_current_tenant();
        clear_current_auth_identity();
    }
}

#[test]
fn volatile_session_and_password_functions_use_runtime_context() {
    let _guard = ContextGuard;
    clear_current_tenant();
    clear_current_auth_identity();
    let rt = runtime();

    let before_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_millis() as i64;
    for name in ["NOW", "CURRENT_TIMESTAMP"] {
        let Value::TimestampMs(value) = scalar(&rt, &format!("SELECT {name}() AS value")) else {
            panic!("{name} must return TimestampMs");
        };
        let after_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock")
            .as_millis() as i64;
        assert!((before_ms..=after_ms).contains(&value), "{name}: {value}");
    }
    assert!(matches!(
        scalar(&rt, "SELECT CURRENT_DATE() AS value"),
        Value::Date(_)
    ));

    exec(&rt, "SET TENANT 'native-functions'");
    exec(
        &rt,
        "CREATE TABLE accounts (username TEXT, password PASSWORD)",
    );
    exec(
        &rt,
        "INSERT INTO accounts (username, password) VALUES ('alice', PASSWORD('correct horse'))",
    );
    set_current_auth_identity("alice".to_string(), Role::Read);
    assert_eq!(
        scalar(&rt, "SELECT CURRENT_TENANT() AS value"),
        Value::text("native-functions")
    );
    assert_eq!(
        scalar(&rt, "SELECT CURRENT_USER() AS value"),
        Value::text("alice")
    );
    assert_eq!(
        scalar(&rt, "SELECT SESSION_USER() AS value"),
        Value::text("alice")
    );
    assert_eq!(
        scalar(&rt, "SELECT CURRENT_ROLE() AS value"),
        Value::text("read")
    );

    assert_eq!(
        scalar(
            &rt,
            "SELECT VERIFY_PASSWORD(password, 'correct horse') AS value FROM accounts",
        ),
        Value::Boolean(true)
    );
    assert_eq!(
        scalar(
            &rt,
            "SELECT VERIFY_PASSWORD(password, 'wrong') AS value FROM accounts",
        ),
        Value::Boolean(false)
    );
}

const AGGREGATE_FUNCTIONS: &[&str] = &[
    "AVG",
    "COUNT",
    "GROUP_CONCAT",
    "MAX",
    "MIN",
    "STDDEV",
    "STRING_AGG",
    "SUM",
    "VARIANCE",
];

const GEOGRAPHIC_FUNCTIONS: &[&str] = &[
    "GEO_BEARING",
    "GEO_DISTANCE",
    "GEO_WITHIN",
    "H3_CELL",
    "H3_INDEX",
    "H3_PARENT",
    "H3_RING",
    "H3_TO_LATLNG",
    "HAVERSINE",
    "POLYGON",
    "VINCENTY",
];

const CONTEXT_FUNCTIONS: &[&str] = &[
    "CURRENT_DATE",
    "CURRENT_ROLE",
    "CURRENT_TENANT",
    "CURRENT_TIMESTAMP",
    "CURRENT_USER",
    "NOW",
    "SESSION_USER",
    "VERIFY_PASSWORD",
];

const BIOMETRIC_FUNCTIONS: &[&str] = &[
    "ANSI_NIST_RECORD",
    "BIOMETRIC_FORMAT",
    "BIOMETRIC_INFO",
    "BIT_SIGNATURE",
    "BIT_SIGNATURE_COMPARE",
    "BIT_SIGNATURE_MATCH",
    "FACE",
    "FACE_LANDMARKS",
    "FEATURE_COMPARE",
    "FEATURE_MATCH",
    "FEATURE_VECTOR",
    "FINGERPRINT",
    "FINGERPRINT_COMPARE",
    "FINGERPRINT_MATCH",
    "IRIS",
    "LANDMARK_IS_SYMMETRIC",
    "LANDMARK_SET",
    "LANDMARK_SYMMETRY",
];

#[test]
fn every_catalog_function_has_a_public_sql_contract() {
    let catalog = FUNCTION_CATALOG
        .iter()
        .map(|entry| entry.name)
        .collect::<BTreeSet<_>>();
    let mut covered = BTreeSet::new();
    macro_rules! cover_case {
        ($name:expr, $sql:expr, $expected:expr) => {
            covered.insert($name)
        };
    }
    exact_function_cases!(cover_case);
    float_function_cases!(cover_case);
    covered.extend(AGGREGATE_FUNCTIONS);
    covered.extend(GEOGRAPHIC_FUNCTIONS);
    covered.extend(CONTEXT_FUNCTIONS);
    covered.extend(BIOMETRIC_FUNCTIONS);

    assert_eq!(
        covered, catalog,
        "update the public-SQL journey whenever the native catalog changes"
    );
}
