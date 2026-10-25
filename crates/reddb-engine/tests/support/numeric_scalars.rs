//! Preserve native projection-scalar semantics across native and WASM.
use reddb_engine::query::runtime::numeric::{arith_binop, cast_value_to, value_as_number};
use reddb_types::{DataType, Value};

pub const CASES: &[(&str, fn())] = &[
    ("numeric_scalar_operand_coercion", operand_coercion),
    ("numeric_scalar_arithmetic_types", arithmetic_types),
    ("numeric_scalar_null_and_zero", null_and_zero),
    ("numeric_scalar_timestamp_ms", timestamp_ms),
    ("numeric_scalar_legacy_precision", legacy_precision),
    ("numeric_scalar_cast_fast_paths", cast_fast_paths),
    ("numeric_scalar_cast_validation", cast_validation),
];

fn operand_coercion() {
    for (value, expected, floating) in [
        (Value::Integer(7), 7.0, false),
        (Value::BigInt(-7), -7.0, false),
        (Value::UnsignedInteger(7), 7.0, false),
        (Value::Float(1.5), 1.5, true),
        (Value::Decimal(15_000), 1.5, true),
        (Value::text("7"), 7.0, false),
        (Value::text("1.5"), 1.5, true),
    ] {
        let operand = value_as_number(&value).expect("native numeric operand");
        assert_eq!(operand.as_f64(), expected);
        assert_eq!(operand.is_float, floating);
    }
    for value in [
        Value::Null,
        Value::Boolean(true),
        Value::text("not a number"),
        Value::Array(vec![]),
        Value::Secret(vec![1]),
    ] {
        assert!(value_as_number(&value).is_none());
    }
    assert!(value_as_number(&Value::text("NaN"))
        .expect("native float spelling")
        .as_f64()
        .is_nan());
}

fn arithmetic_types() {
    for (op, left, right, expected) in [
        (
            "ADD",
            Value::Integer(3),
            Value::Integer(4),
            Value::Integer(7),
        ),
        (
            "SUB",
            Value::Integer(3),
            Value::Integer(4),
            Value::Integer(-1),
        ),
        (
            "MUL",
            Value::Integer(3),
            Value::Integer(4),
            Value::Integer(12),
        ),
        (
            "DIV",
            Value::Integer(3),
            Value::Integer(2),
            Value::Float(1.5),
        ),
        (
            "MOD",
            Value::Integer(-7),
            Value::Integer(3),
            Value::Integer(-1),
        ),
        (
            "ADD",
            Value::text("4"),
            Value::Integer(2),
            Value::Integer(6),
        ),
        (
            "MUL",
            Value::Decimal(15_000),
            Value::Integer(2),
            Value::Float(3.0),
        ),
        (
            "ADD",
            Value::Integer(2),
            Value::Float(0.5),
            Value::Float(2.5),
        ),
    ] {
        assert_eq!(arith_binop(op, left, right), expected, "native {op}");
    }
}

fn null_and_zero() {
    for op in ["DIV", "MOD"] {
        assert_eq!(
            arith_binop(op, Value::Integer(7), Value::Integer(0)),
            Value::Null
        );
        assert_eq!(
            arith_binop(op, Value::Float(7.0), Value::Float(-0.0)),
            Value::Null
        );
    }
    assert_eq!(
        arith_binop("ADD", Value::Null, Value::Integer(1)),
        Value::Null
    );
    assert_eq!(
        arith_binop("MUL", Value::text("invalid"), Value::Integer(1)),
        Value::Null
    );
    assert_eq!(
        arith_binop("unknown", Value::Integer(1), Value::Integer(2)),
        Value::Null
    );
    // The legacy projection dispatcher returns NULL; typed expression evaluation
    // retains its distinct DivisionByZero error, tested in the shared fixtures.
}

fn timestamp_ms() {
    for (op, left, right, expected) in [
        (
            "ADD",
            Value::TimestampMs(1000),
            Value::Integer(5),
            Value::TimestampMs(1005),
        ),
        (
            "ADD",
            Value::Duration(5),
            Value::TimestampMs(1000),
            Value::TimestampMs(1005),
        ),
        (
            "SUB",
            Value::TimestampMs(1000),
            Value::UnsignedInteger(5),
            Value::TimestampMs(995),
        ),
        (
            "ADD",
            Value::TimestampMs(i64::MAX),
            Value::Integer(1),
            Value::Null,
        ),
        (
            "SUB",
            Value::TimestampMs(i64::MIN),
            Value::Integer(1),
            Value::Null,
        ),
        (
            "ADD",
            Value::TimestampMs(1000),
            Value::UnsignedInteger(u64::MAX),
            Value::Null,
        ),
        (
            "MUL",
            Value::TimestampMs(1000),
            Value::Integer(2),
            Value::Null,
        ),
    ] {
        assert_eq!(arith_binop(op, left, right), expected);
    }
}

fn legacy_precision() {
    // Characterization of existing projection coercion, not a new precision
    // guarantee: relocating this dispatcher must not change native results.
    let operand =
        value_as_number(&Value::UnsignedInteger(u64::MAX)).expect("native unsigned coercion");
    assert_eq!(operand.int_val, -1);
    assert_eq!(operand.as_f64(), -1.0);
    assert_eq!(
        arith_binop("ADD", Value::UnsignedInteger(u64::MAX), Value::Integer(1)),
        Value::Integer(0)
    );
    assert_eq!(
        arith_binop(
            "ADD",
            Value::Integer(9_007_199_254_740_993),
            Value::Integer(0)
        ),
        Value::Integer(9_007_199_254_740_992)
    );
    assert_eq!(
        arith_binop("ADD", Value::Integer(i64::MAX), Value::Integer(1)),
        Value::Integer(i64::MAX)
    );
}

fn cast_fast_paths() {
    for (input, target, expected) in [
        (Value::Integer(42), DataType::Text, Value::text("42")),
        (Value::Integer(42), DataType::BigInt, Value::BigInt(42)),
        (Value::Integer(42), DataType::Float, Value::Float(42.0)),
        (
            Value::Integer(42),
            DataType::UnsignedInteger,
            Value::UnsignedInteger(42),
        ),
        (
            Value::UnsignedInteger(42),
            DataType::Integer,
            Value::Integer(42),
        ),
        (Value::Float(2.9), DataType::Integer, Value::Integer(2)),
        (Value::Boolean(true), DataType::Integer, Value::Integer(1)),
        (Value::Integer(0), DataType::Boolean, Value::Boolean(false)),
    ] {
        assert_eq!(cast_value_to(&input, target), expected);
    }
}

fn cast_validation() {
    for (input, target, expected) in [
        (Value::text("42"), DataType::Integer, Value::Integer(42)),
        (Value::text("invalid"), DataType::Integer, Value::Null),
        (Value::Integer(-1), DataType::UnsignedInteger, Value::Null),
        (
            Value::UnsignedInteger(u64::MAX),
            DataType::Integer,
            Value::Null,
        ),
    ] {
        assert_eq!(cast_value_to(&input, target), expected);
    }
}
