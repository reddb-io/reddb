//! Native policy adapter for the canonical window phase.
use crate::api::RedDBError;
use crate::storage::query::unified::UnifiedRecord;
use reddb_engine::query::runtime::window_phase::{apply_with_host, WindowHost};
use reddb_rql::ast::{Expr, Projection};
use reddb_types::Value;

pub(crate) fn apply(
    db: Option<&crate::RedDB>,
    records: &mut [UnifiedRecord],
    projections: &[Projection],
    table_name: Option<&str>,
    table_alias: Option<&str>,
) -> Result<(), RedDBError> {
    apply_with_host(
        &NativeWindowHost {
            db,
            table_name,
            table_alias,
        },
        records,
        projections,
        table_name,
        table_alias,
    )
}

struct NativeWindowHost<'a> {
    db: Option<&'a crate::RedDB>,
    table_name: Option<&'a str>,
    table_alias: Option<&'a str>,
}

impl WindowHost for NativeWindowHost<'_> {
    type Error = RedDBError;
    fn guard_materialization(&self, rows: usize) -> Result<(), RedDBError> {
        if let Some(db) = self.db {
            super::materialization_limit::guard(db, "window", rows)?;
        }
        Ok(())
    }
    fn evaluate_expression(&self, expression: &Expr, record: &UnifiedRecord) -> Option<Value> {
        self.try_evaluate_expression(expression, record)
            .ok()
            .flatten()
    }
    fn try_evaluate_expression(
        &self,
        expression: &Expr,
        record: &UnifiedRecord,
    ) -> Result<Option<Value>, RedDBError> {
        let row = |field: &reddb_rql::ast::FieldRef| {
            super::join_filter::resolve_runtime_field(
                record,
                field,
                self.table_name,
                self.table_alias,
            )
            .and_then(super::execution_context::secret_query_input)
        };
        // The pure typed evaluator rejects visited context functions. Keep its
        // existing fast path and propagate operating failures from the fallback.
        if let Ok(value) =
            super::expr_eval::evaluate_typed_expr_with_secret_literals(expression, &row)
        {
            return Ok(Some(value));
        }
        super::expr_eval::try_evaluate_runtime_expr_with_db(
            self.db,
            expression,
            record,
            self.table_name,
            self.table_alias,
        )
    }
    fn secret_query_output(&self, value: Value) -> Result<Value, RedDBError> {
        super::execution_context::secret_query_output(value)
    }
    fn query_error(&self, message: String) -> RedDBError {
        RedDBError::Query(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reddb_rql::ast::WindowSpec;
    use reddb_rql::ast::{Expr, FieldRef, Span, WindowOrderItem};
    use reddb_types::Value;
    use std::collections::HashMap;

    fn rec(id: i64, user: &str, ts: i64) -> UnifiedRecord {
        let mut r = UnifiedRecord::new();
        r.set("id", Value::Integer(id));
        r.set("user_id", Value::text(user.to_string()));
        r.set("ts", Value::Integer(ts));
        r
    }

    fn col_field(name: &str) -> FieldRef {
        FieldRef::TableColumn {
            table: String::new(),
            column: name.to_string(),
        }
    }

    fn col_expr(name: &str) -> Expr {
        Expr::Column {
            field: col_field(name),
            span: Span::synthetic(),
        }
    }

    fn window_proj(name: &str, args: Vec<Projection>, spec: WindowSpec, alias: &str) -> Projection {
        Projection::Window {
            name: name.to_string(),
            args,
            window: Box::new(spec),
            alias: Some(alias.to_string()),
        }
    }

    #[test]
    fn row_number_partitioned_by_user_ordered_by_ts() {
        let mut rows = vec![
            rec(1, "u1", 100),
            rec(2, "u1", 200),
            rec(3, "u2", 50),
            rec(4, "u1", 150),
            rec(5, "u2", 75),
        ];
        let spec = WindowSpec {
            partition_by: vec![col_expr("user_id")],
            order_by: vec![WindowOrderItem {
                expr: col_expr("ts"),
                ascending: true,
                nulls_first: false,
            }],
            frame: None,
        };
        apply(
            None,
            &mut rows,
            &[window_proj("ROW_NUMBER", vec![], spec, "rn")],
            None,
            None,
        )
        .expect("apply");

        let by_id: HashMap<i64, i64> = rows
            .iter()
            .map(|r| {
                let id = match r.get("id").unwrap() {
                    Value::Integer(v) => *v,
                    _ => panic!("id"),
                };
                let rn = match r.get("rn").unwrap() {
                    Value::Integer(v) => *v,
                    _ => panic!("rn"),
                };
                (id, rn)
            })
            .collect();
        // u1 by ts: id=1 (100) → 1, id=4 (150) → 2, id=2 (200) → 3
        // u2 by ts: id=3 (50) → 1, id=5 (75) → 2
        assert_eq!(by_id[&1], 1);
        assert_eq!(by_id[&4], 2);
        assert_eq!(by_id[&2], 3);
        assert_eq!(by_id[&3], 1);
        assert_eq!(by_id[&5], 2);
    }

    #[test]
    fn rank_and_dense_rank_treat_ties_differently() {
        let mut rows = vec![
            rec(1, "u1", 100),
            rec(2, "u1", 100), // tied with id=1
            rec(3, "u1", 200),
        ];
        let spec = || WindowSpec {
            partition_by: vec![col_expr("user_id")],
            order_by: vec![WindowOrderItem {
                expr: col_expr("ts"),
                ascending: true,
                nulls_first: false,
            }],
            frame: None,
        };
        apply(
            None,
            &mut rows,
            &[
                window_proj("RANK", vec![], spec(), "rk"),
                window_proj("DENSE_RANK", vec![], spec(), "drk"),
            ],
            None,
            None,
        )
        .expect("apply");

        let result: HashMap<i64, (i64, i64)> = rows
            .iter()
            .map(|r| {
                let id = match r.get("id").unwrap() {
                    Value::Integer(v) => *v,
                    _ => panic!(),
                };
                let rk = match r.get("rk").unwrap() {
                    Value::Integer(v) => *v,
                    _ => panic!(),
                };
                let drk = match r.get("drk").unwrap() {
                    Value::Integer(v) => *v,
                    _ => panic!(),
                };
                (id, (rk, drk))
            })
            .collect();
        // RANK: id=1 → 1, id=2 → 1 (tie), id=3 → 3 (gap)
        // DENSE_RANK: id=1 → 1, id=2 → 1, id=3 → 2 (no gap)
        assert_eq!(result[&1], (1, 1));
        assert_eq!(result[&2], (1, 1));
        assert_eq!(result[&3], (3, 2));
    }

    #[test]
    fn lag_returns_prior_value_or_null_on_first_row() {
        let mut rows = vec![rec(1, "u1", 100), rec(2, "u1", 200), rec(3, "u1", 300)];
        let spec = WindowSpec {
            partition_by: vec![col_expr("user_id")],
            order_by: vec![WindowOrderItem {
                expr: col_expr("ts"),
                ascending: true,
                nulls_first: false,
            }],
            frame: None,
        };
        apply(
            None,
            &mut rows,
            &[window_proj(
                "LAG",
                vec![Projection::Field(col_field("ts"), None)],
                spec,
                "prev_ts",
            )],
            None,
            None,
        )
        .expect("apply");

        let by_id: HashMap<i64, Value> = rows
            .iter()
            .map(|r| {
                let id = match r.get("id").unwrap() {
                    Value::Integer(v) => *v,
                    _ => panic!(),
                };
                (id, r.get("prev_ts").cloned().unwrap_or(Value::Null))
            })
            .collect();
        assert!(matches!(by_id[&1], Value::Null));
        assert_eq!(by_id[&2], Value::Integer(100));
        assert_eq!(by_id[&3], Value::Integer(200));
    }

    #[test]
    fn lead_returns_next_value_or_null_on_last_row() {
        let mut rows = vec![rec(1, "u1", 100), rec(2, "u1", 200), rec(3, "u1", 300)];
        let spec = WindowSpec {
            partition_by: vec![col_expr("user_id")],
            order_by: vec![WindowOrderItem {
                expr: col_expr("ts"),
                ascending: true,
                nulls_first: false,
            }],
            frame: None,
        };
        apply(
            None,
            &mut rows,
            &[window_proj(
                "LEAD",
                vec![Projection::Field(col_field("ts"), None)],
                spec,
                "next_ts",
            )],
            None,
            None,
        )
        .expect("apply");

        let by_id: HashMap<i64, Value> = rows
            .iter()
            .map(|r| {
                let id = match r.get("id").unwrap() {
                    Value::Integer(v) => *v,
                    _ => panic!(),
                };
                (id, r.get("next_ts").cloned().unwrap_or(Value::Null))
            })
            .collect();
        assert_eq!(by_id[&1], Value::Integer(200));
        assert_eq!(by_id[&2], Value::Integer(300));
        assert!(matches!(by_id[&3], Value::Null));
    }

    #[test]
    fn lag_with_offset_and_default() {
        let mut rows = vec![
            rec(1, "u1", 100),
            rec(2, "u1", 200),
            rec(3, "u1", 300),
            rec(4, "u1", 400),
        ];
        let spec = WindowSpec {
            partition_by: vec![],
            order_by: vec![WindowOrderItem {
                expr: col_expr("ts"),
                ascending: true,
                nulls_first: false,
            }],
            frame: None,
        };
        apply(
            None,
            &mut rows,
            &[window_proj(
                "LAG",
                vec![
                    Projection::Field(col_field("ts"), None),
                    Projection::Column("LIT:2".to_string()),
                    Projection::Column("LIT:-1".to_string()),
                ],
                spec,
                "lag2",
            )],
            None,
            None,
        )
        .expect("apply");

        let by_id: HashMap<i64, Value> = rows
            .iter()
            .map(|r| {
                let id = match r.get("id").unwrap() {
                    Value::Integer(v) => *v,
                    _ => panic!(),
                };
                (id, r.get("lag2").cloned().unwrap_or(Value::Null))
            })
            .collect();
        // offset=2: positions [1..]=100, [2..]=200 ... so lag2:
        // pos 0 → default -1
        // pos 1 → default -1
        // pos 2 → ts of pos 0 = 100
        // pos 3 → ts of pos 1 = 200
        assert_eq!(by_id[&1], Value::Integer(-1));
        assert_eq!(by_id[&2], Value::Integer(-1));
        assert_eq!(by_id[&3], Value::Integer(100));
        assert_eq!(by_id[&4], Value::Integer(200));
    }
}
