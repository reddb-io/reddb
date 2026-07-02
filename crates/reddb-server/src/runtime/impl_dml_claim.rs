//! DML claim-telemetry + compound-assignment helpers extracted from
//! `impl_dml`.
//!
//! Behaviour-preserving move (issue #1633); `pub(super)` visibility keeps the
//! sibling `impl_dml` call sites unchanged.

use super::*;
use crate::storage::query::ast::BinOp;

pub(super) fn record_claim_outcome(
    telemetry: &crate::runtime::claim_telemetry::ClaimTelemetryCounters,
    claim_limit: Option<u64>,
    table: &str,
    model: &str,
    affected: u64,
) {
    if claim_limit.is_none() {
        return;
    }
    if affected == 0 {
        telemetry.record_miss(table, model);
        tracing::debug!(
            target: "reddb::claim",
            collection = table,
            model,
            "concurrent claim missed"
        );
    } else {
        telemetry.record_successful(table, model, affected);
        tracing::debug!(
            target: "reddb::claim",
            collection = table,
            model,
            successful = affected,
            "concurrent claim succeeded"
        );
    }
}

pub(super) fn evaluate_compound_update_assignment(
    column: &str,
    record: &UnifiedRecord,
    op: BinOp,
    rhs: Value,
) -> RedDBResult<Value> {
    let lhs = record.get(column).ok_or_else(|| {
        RedDBError::Query(format!(
            "compound assignment requires existing numeric field '{column}'"
        ))
    })?;
    if matches!(lhs, Value::Null) {
        return Err(RedDBError::Query(format!(
            "compound assignment requires non-null numeric field '{column}'"
        )));
    }
    apply_compound_numeric_op(column, op, lhs, &rhs)
}

pub(super) fn apply_compound_numeric_op(
    column: &str,
    op: BinOp,
    lhs: &Value,
    rhs: &Value,
) -> RedDBResult<Value> {
    let Some(lhs_number) = CompoundNumber::from_value(lhs) else {
        return Err(RedDBError::Query(format!(
            "compound assignment requires numeric field '{column}'"
        )));
    };
    let Some(rhs_number) = CompoundNumber::from_value(rhs) else {
        return Err(RedDBError::Query(format!(
            "compound assignment requires numeric right-hand value for field '{column}'"
        )));
    };

    if lhs_number.is_float() || rhs_number.is_float() || matches!(op, BinOp::Div) {
        let a = lhs_number.as_f64();
        let b = rhs_number.as_f64();
        let out = match op {
            BinOp::Add => a + b,
            BinOp::Sub => a - b,
            BinOp::Mul => a * b,
            BinOp::Div => {
                if b == 0.0 {
                    return Err(RedDBError::Query(format!(
                        "division by zero in compound assignment for field '{column}'"
                    )));
                }
                a / b
            }
            BinOp::Mod => {
                if b == 0.0 {
                    return Err(RedDBError::Query(format!(
                        "modulo by zero in compound assignment for field '{column}'"
                    )));
                }
                a % b
            }
            _ => {
                return Err(RedDBError::Query(format!(
                    "unsupported compound assignment operator for field '{column}'"
                )));
            }
        };
        if !out.is_finite() {
            return Err(RedDBError::Query(format!(
                "numeric overflow in compound assignment for field '{column}'"
            )));
        }
        return Ok(Value::Float(out));
    }

    let a = lhs_number.as_i128();
    let b = rhs_number.as_i128();
    let out = match op {
        BinOp::Add => a.checked_add(b),
        BinOp::Sub => a.checked_sub(b),
        BinOp::Mul => a.checked_mul(b),
        BinOp::Mod => {
            if b == 0 {
                return Err(RedDBError::Query(format!(
                    "modulo by zero in compound assignment for field '{column}'"
                )));
            }
            a.checked_rem(b)
        }
        BinOp::Div => unreachable!("integer division is handled by the float branch"),
        _ => None,
    }
    .ok_or_else(|| {
        RedDBError::Query(format!(
            "numeric overflow in compound assignment for field '{column}'"
        ))
    })?;

    if matches!(lhs, Value::UnsignedInteger(_)) {
        let value = u64::try_from(out).map_err(|_| {
            RedDBError::Query(format!(
                "numeric overflow in compound assignment for field '{column}'"
            ))
        })?;
        Ok(Value::UnsignedInteger(value))
    } else {
        let value = i64::try_from(out).map_err(|_| {
            RedDBError::Query(format!(
                "numeric overflow in compound assignment for field '{column}'"
            ))
        })?;
        Ok(Value::Integer(value))
    }
}

#[derive(Clone, Copy)]
enum CompoundNumber {
    Integer(i128),
    Float(f64),
}

impl CompoundNumber {
    fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::Integer(value) | Value::BigInt(value) => Some(Self::Integer(*value as i128)),
            Value::UnsignedInteger(value) => Some(Self::Integer(*value as i128)),
            Value::Float(value) => value.is_finite().then_some(Self::Float(*value)),
            Value::Decimal(value) => Some(Self::Float(*value as f64 / 10_000.0)),
            _ => None,
        }
    }

    fn is_float(self) -> bool {
        matches!(self, Self::Float(_))
    }

    fn as_f64(self) -> f64 {
        match self {
            Self::Integer(value) => value as f64,
            Self::Float(value) => value,
        }
    }

    fn as_i128(self) -> i128 {
        match self {
            Self::Integer(value) => value,
            Self::Float(_) => unreachable!("float compound number used as integer"),
        }
    }
}
