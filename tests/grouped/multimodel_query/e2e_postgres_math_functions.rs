use reddb::runtime::{RedDBRuntime, RuntimeQueryResult};
use reddb::storage::query::unified::UnifiedRecord;
use reddb::storage::schema::Value;

fn runtime() -> RedDBRuntime {
    RedDBRuntime::in_memory().expect("in-memory runtime")
}

fn exec(rt: &RedDBRuntime, sql: &str) -> RuntimeQueryResult {
    rt.execute_query(sql)
        .unwrap_or_else(|err| panic!("query failed: {sql}\n{err:?}"))
}

fn only_record(result: &RuntimeQueryResult) -> &UnifiedRecord {
    assert_eq!(
        result.result.records.len(),
        1,
        "expected one row for query `{}`",
        result.query
    );
    &result.result.records[0]
}

fn float_value(row: &UnifiedRecord, column: &str) -> f64 {
    match row.get(column) {
        Some(Value::Float(value)) => *value,
        other => panic!("expected float column {column}, got {other:?}"),
    }
}

fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-12,
        "expected {expected}, got {actual}"
    );
}

fn assert_query_error_contains(rt: &RedDBRuntime, sql: &str, expected: &str) {
    let err = match rt.execute_query(sql) {
        Ok(_) => panic!("query should fail: {sql}"),
        Err(err) => err,
    };
    let message = err.to_string();
    assert!(
        message.contains(expected),
        "expected error containing `{expected}` for `{sql}`, got `{message}`"
    );
}

#[test]
fn postgres_math_canonical_functions_execute_through_sql_expressions() {
    let rt = runtime();

    let source_free = exec(
        &rt,
        "SELECT SQRT(9) AS root, POWER(2, 3) AS power_value, EXP(1) AS exp_value, \
         LN(EXP(1)) AS ln_value, LOG(100) AS log_value, LOG(2, 8) AS log_base, \
         LOG10(1000) AS log10_value, SIN(0) AS sin_value, COS(0) AS cos_value, \
         TAN(0) AS tan_value, ASIN(0) AS asin_value, ACOS(1) AS acos_value, \
         ATAN(1) AS atan_value, ATAN2(1, 1) AS atan2_value, COT(RADIANS(45)) AS cot_value, \
         DEGREES(PI()) AS degrees_value, RADIANS(180) AS radians_value, PI() AS pi_value",
    );
    let row = only_record(&source_free);
    assert_close(float_value(row, "root"), 3.0);
    assert_close(float_value(row, "power_value"), 8.0);
    assert_close(float_value(row, "exp_value"), std::f64::consts::E);
    assert_close(float_value(row, "ln_value"), 1.0);
    assert_close(float_value(row, "log_value"), 2.0);
    assert_close(float_value(row, "log_base"), 3.0);
    assert_close(float_value(row, "log10_value"), 3.0);
    assert_close(float_value(row, "sin_value"), 0.0);
    assert_close(float_value(row, "cos_value"), 1.0);
    assert_close(float_value(row, "tan_value"), 0.0);
    assert_close(float_value(row, "asin_value"), 0.0);
    assert_close(float_value(row, "acos_value"), 0.0);
    assert_close(float_value(row, "atan_value"), std::f64::consts::FRAC_PI_4);
    assert_close(float_value(row, "atan2_value"), std::f64::consts::FRAC_PI_4);
    assert_close(float_value(row, "cot_value"), 1.0);
    assert_close(float_value(row, "degrees_value"), 180.0);
    assert_close(float_value(row, "radians_value"), std::f64::consts::PI);
    assert_close(float_value(row, "pi_value"), std::f64::consts::PI);

    exec(&rt, "CREATE TABLE math_inputs (id INT, x FLOAT)");
    exec(&rt, "INSERT INTO math_inputs (id, x) VALUES (1, 16)");
    let table_result = exec(&rt, "SELECT SQRT(x) AS root FROM math_inputs");
    assert_close(float_value(only_record(&table_result), "root"), 4.0);
}

#[test]
fn postgres_math_aliases_return_float_values() {
    let rt = runtime();

    let result = exec(
        &rt,
        "SELECT POW(2, 4) AS pow_value, ARCSIN(0) AS arcsin_value, \
         ARCCOS(1) AS arccos_value, ARCTAN(1) AS arctan_value",
    );
    let row = only_record(&result);

    assert_close(float_value(row, "pow_value"), 16.0);
    assert_close(float_value(row, "arcsin_value"), 0.0);
    assert_close(float_value(row, "arccos_value"), 0.0);
    assert_close(
        float_value(row, "arctan_value"),
        std::f64::consts::FRAC_PI_4,
    );
}

#[test]
fn postgres_math_invalid_numeric_results_surface_as_errors() {
    let rt = runtime();

    assert_query_error_contains(&rt, "SELECT 1 / 0 AS bad", "division by zero");
    assert_query_error_contains(&rt, "SELECT 1 % 0 AS bad", "division by zero");
    assert_query_error_contains(
        &rt,
        "SELECT SQRT(-1) AS bad",
        "greater than or equal to zero",
    );
    assert_query_error_contains(&rt, "SELECT LN(0) AS bad", "greater than zero");
    assert_query_error_contains(&rt, "SELECT LOG(1, 8) AS bad", "base must not equal one");
    assert_query_error_contains(&rt, "SELECT ASIN(2) AS bad", "between -1 and 1");
    assert_query_error_contains(&rt, "SELECT EXP(10000) AS bad", "NaN or infinite");
}
