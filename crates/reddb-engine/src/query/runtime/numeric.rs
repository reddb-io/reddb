//! Native projection scalar arithmetic and coercion, separate from typed evaluation.
use reddb_types::Value;

/// Resolve a single scalar argument from a function's arg list.
/// Evaluate an arithmetic binary operator on two values. Promotes
/// heterogeneous numeric operands to Float when either side is Float;
/// preserves Integer when both sides are Integer. Non-numeric operands
/// and zero divisors collapse to `Value::Null` so queries keep running
/// — SQL-style "erroring on bad arithmetic" is the job of the type
/// system v2 (Fase 3), not Fase 1.3.
pub fn arith_binop(op: &str, a: Value, b: Value) -> Value {
    if let Some(value) = timestamp_ms_arith(op, &a, &b) {
        return value;
    }

    let (lhs, rhs) = match (value_as_number(&a), value_as_number(&b)) {
        (Some(l), Some(r)) => (l, r),
        _ => return Value::Null,
    };
    // Integer fast path when both operands are integers and the op
    // doesn't force a float (division always floats for predictability
    // — avoids surprising truncation).
    let force_float = matches!(op, "DIV") || lhs.is_float || rhs.is_float;
    let out = match op {
        "ADD" => lhs.as_f64() + rhs.as_f64(),
        "SUB" => lhs.as_f64() - rhs.as_f64(),
        "MUL" => lhs.as_f64() * rhs.as_f64(),
        "DIV" => {
            if rhs.as_f64() == 0.0 {
                return Value::Null;
            }
            lhs.as_f64() / rhs.as_f64()
        }
        "MOD" => {
            if rhs.as_f64() == 0.0 {
                return Value::Null;
            }
            lhs.as_f64() % rhs.as_f64()
        }
        _ => return Value::Null,
    };
    if force_float {
        Value::Float(out)
    } else {
        Value::Integer(out as i64)
    }
}

fn timestamp_ms_arith(op: &str, a: &Value, b: &Value) -> Option<Value> {
    match (op, a, b) {
        ("ADD", Value::TimestampMs(ts), rhs) => Some(Value::TimestampMs(
            ts.checked_add(duration_ms_operand(rhs)?)?,
        )),
        ("ADD", lhs, Value::TimestampMs(ts)) => Some(Value::TimestampMs(
            ts.checked_add(duration_ms_operand(lhs)?)?,
        )),
        ("SUB", Value::TimestampMs(ts), rhs) => Some(Value::TimestampMs(
            ts.checked_sub(duration_ms_operand(rhs)?)?,
        )),
        _ => None,
    }
}

fn duration_ms_operand(value: &Value) -> Option<i64> {
    match value {
        Value::Integer(value) | Value::BigInt(value) | Value::Duration(value) => Some(*value),
        Value::UnsignedInteger(value) => i64::try_from(*value).ok(),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct NumOperand {
    pub int_val: i64,
    pub float_val: f64,
    pub is_float: bool,
}

impl NumOperand {
    pub fn as_f64(self) -> f64 {
        if self.is_float {
            self.float_val
        } else {
            self.int_val as f64
        }
    }
}

pub fn value_as_number(v: &Value) -> Option<NumOperand> {
    match v {
        Value::Integer(n) | Value::BigInt(n) => Some(NumOperand {
            int_val: *n,
            float_val: *n as f64,
            is_float: false,
        }),
        Value::UnsignedInteger(n) => Some(NumOperand {
            int_val: *n as i64,
            float_val: *n as f64,
            is_float: false,
        }),
        Value::Float(f) => Some(NumOperand {
            int_val: *f as i64,
            float_val: *f,
            is_float: true,
        }),
        Value::Decimal(d) => Some(NumOperand {
            int_val: (*d / 10_000),
            float_val: *d as f64 / 10_000.0,
            is_float: true,
        }),
        Value::Text(s) => {
            if let Ok(n) = s.parse::<i64>() {
                Some(NumOperand {
                    int_val: n,
                    float_val: n as f64,
                    is_float: false,
                })
            } else if let Ok(f) = s.parse::<f64>() {
                Some(NumOperand {
                    int_val: f as i64,
                    float_val: f,
                    is_float: true,
                })
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Convert a `Value` to a new `Value` of the requested `DataType`. Used
/// by the CAST scalar function. Covers the common numeric/text/boolean
/// paths directly (so `CAST(123 AS TEXT)` doesn't round-trip through the
/// schema coercion layer) and falls back to `schema::coerce::coerce`
/// on the value's `display_string()` for everything else — that reuses
/// the battle-tested input validators we already have for INSERT.
pub fn cast_value_to(src: &Value, target: reddb_types::DataType) -> Value {
    use reddb_types::types::DataType as DT;
    match (src, target) {
        (v, DT::Text) => Value::text(v.display_string()),
        (Value::Integer(n), DT::Float) => Value::Float(*n as f64),
        (Value::Integer(n), DT::BigInt) => Value::BigInt(*n),
        (Value::Integer(n), DT::UnsignedInteger) if *n >= 0 => Value::UnsignedInteger(*n as u64),
        (Value::UnsignedInteger(n), DT::Integer) if *n <= i64::MAX as u64 => {
            Value::Integer(*n as i64)
        }
        (Value::UnsignedInteger(n), DT::Float) => Value::Float(*n as f64),
        (Value::Float(f), DT::Integer) => Value::Integer(*f as i64),
        (Value::Float(f), DT::UnsignedInteger) if *f >= 0.0 => Value::UnsignedInteger(*f as u64),
        (Value::Boolean(b), DT::Integer) => Value::Integer(if *b { 1 } else { 0 }),
        (Value::Integer(n), DT::Boolean) => Value::Boolean(*n != 0),
        (Value::Text(s), target) => match reddb_types::coerce::coerce(s, target, None) {
            Ok(v) => v,
            Err(_) => Value::Null,
        },
        (v, target) => match reddb_types::coerce::coerce(&v.display_string(), target, None) {
            Ok(v) => v,
            Err(_) => Value::Null,
        },
    }
}
