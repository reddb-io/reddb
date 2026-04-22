//! `BatchFilter` — predicate evaluation on a columnar batch.
//!
//! Produces a bitmap of surviving rows, then hands it to
//! [`ColumnBatch::take`]. The predicate surface intentionally
//! handles the common shapes (compare column-to-literal, boolean
//! combinators) without dragging the full SQL AST into the batch
//! module.

use super::super::column_batch::{ColumnBatch, ColumnVector};

#[derive(Debug, Clone)]
pub enum Predicate {
    Eq { column: usize, value: Literal },
    NotEq { column: usize, value: Literal },
    Lt { column: usize, value: Literal },
    LtEq { column: usize, value: Literal },
    Gt { column: usize, value: Literal },
    GtEq { column: usize, value: Literal },
    And(Vec<Predicate>),
    Or(Vec<Predicate>),
    Not(Box<Predicate>),
    All,
}

#[derive(Debug, Clone)]
pub enum Literal {
    Int64(i64),
    Float64(f64),
    Bool(bool),
    Text(String),
}

pub fn batch_filter(batch: &ColumnBatch, predicate: &Predicate) -> ColumnBatch {
    let mask = evaluate(batch, predicate);
    let indices: Vec<usize> = mask
        .iter()
        .enumerate()
        .filter_map(|(i, keep)| if *keep { Some(i) } else { None })
        .collect();
    batch.take(&indices)
}

fn evaluate(batch: &ColumnBatch, predicate: &Predicate) -> Vec<bool> {
    let n = batch.len();
    match predicate {
        Predicate::All => vec![true; n],
        Predicate::Eq { column, value } => cmp_vec(batch, *column, value, |a, b| a == b),
        Predicate::NotEq { column, value } => cmp_vec(batch, *column, value, |a, b| a != b),
        Predicate::Lt { column, value } => cmp_vec(batch, *column, value, |a, b| a < b),
        Predicate::LtEq { column, value } => cmp_vec(batch, *column, value, |a, b| a <= b),
        Predicate::Gt { column, value } => cmp_vec(batch, *column, value, |a, b| a > b),
        Predicate::GtEq { column, value } => cmp_vec(batch, *column, value, |a, b| a >= b),
        Predicate::And(children) => {
            let mut acc = vec![true; n];
            for child in children {
                let m = evaluate(batch, child);
                for i in 0..n {
                    acc[i] = acc[i] && m.get(i).copied().unwrap_or(false);
                }
            }
            acc
        }
        Predicate::Or(children) => {
            let mut acc = vec![false; n];
            for child in children {
                let m = evaluate(batch, child);
                for i in 0..n {
                    acc[i] = acc[i] || m.get(i).copied().unwrap_or(false);
                }
            }
            acc
        }
        Predicate::Not(inner) => {
            let m = evaluate(batch, inner);
            m.iter().map(|b| !b).collect()
        }
    }
}

fn cmp_vec<F>(batch: &ColumnBatch, column: usize, value: &Literal, op: F) -> Vec<bool>
where
    F: Fn(std::cmp::Ordering, std::cmp::Ordering) -> bool,
{
    let n = batch.len();
    let Some(col) = batch.columns.get(column) else {
        return vec![false; n];
    };
    (0..n)
        .map(|i| {
            if !col.is_valid(i) {
                return false;
            }
            match (col, value) {
                (ColumnVector::Int64 { data, .. }, Literal::Int64(v)) => {
                    op(data[i].cmp(v), std::cmp::Ordering::Equal)
                }
                (ColumnVector::Float64 { data, .. }, Literal::Float64(v)) => data[i]
                    .partial_cmp(v)
                    .map(|ord| op(ord, std::cmp::Ordering::Equal))
                    .unwrap_or(false),
                (ColumnVector::Float64 { data, .. }, Literal::Int64(v)) => data[i]
                    .partial_cmp(&(*v as f64))
                    .map(|ord| op(ord, std::cmp::Ordering::Equal))
                    .unwrap_or(false),
                (ColumnVector::Int64 { data, .. }, Literal::Float64(v)) => (data[i] as f64)
                    .partial_cmp(v)
                    .map(|ord| op(ord, std::cmp::Ordering::Equal))
                    .unwrap_or(false),
                (ColumnVector::Bool { data, .. }, Literal::Bool(v)) => {
                    op(data[i].cmp(v), std::cmp::Ordering::Equal)
                }
                (ColumnVector::Text { data, .. }, Literal::Text(v)) => {
                    op(data[i].as_str().cmp(v.as_str()), std::cmp::Ordering::Equal)
                }
                _ => false,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::super::super::column_batch::{ColumnKind, Field, Schema, ValueRef};
    use super::*;
    use std::sync::Arc;

    fn batch() -> ColumnBatch {
        let schema = Arc::new(Schema::new(vec![
            Field {
                name: "id".into(),
                kind: ColumnKind::Int64,
                nullable: false,
            },
            Field {
                name: "v".into(),
                kind: ColumnKind::Float64,
                nullable: false,
            },
        ]));
        ColumnBatch::new(
            schema,
            vec![
                ColumnVector::Int64 {
                    data: (0..10).collect(),
                    validity: None,
                },
                ColumnVector::Float64 {
                    data: (0..10).map(|i| i as f64).collect(),
                    validity: None,
                },
            ],
        )
    }

    #[test]
    fn filter_eq_keeps_matching_row() {
        let b = batch();
        let out = batch_filter(
            &b,
            &Predicate::Eq {
                column: 0,
                value: Literal::Int64(5),
            },
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out.value(0, 0), ValueRef::Int64(5));
    }

    #[test]
    fn filter_range_via_and() {
        let b = batch();
        let out = batch_filter(
            &b,
            &Predicate::And(vec![
                Predicate::GtEq {
                    column: 0,
                    value: Literal::Int64(3),
                },
                Predicate::Lt {
                    column: 0,
                    value: Literal::Int64(7),
                },
            ]),
        );
        assert_eq!(out.len(), 4);
        for i in 0..out.len() {
            let v = out.value(i, 0).as_i64().unwrap();
            assert!(v >= 3 && v < 7);
        }
    }

    #[test]
    fn filter_or_unions_conditions() {
        let b = batch();
        let out = batch_filter(
            &b,
            &Predicate::Or(vec![
                Predicate::Eq {
                    column: 0,
                    value: Literal::Int64(0),
                },
                Predicate::Eq {
                    column: 0,
                    value: Literal::Int64(9),
                },
            ]),
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out.value(0, 0), ValueRef::Int64(0));
        assert_eq!(out.value(1, 0), ValueRef::Int64(9));
    }

    #[test]
    fn filter_not_inverts_mask() {
        let b = batch();
        let out = batch_filter(
            &b,
            &Predicate::Not(Box::new(Predicate::GtEq {
                column: 0,
                value: Literal::Int64(5),
            })),
        );
        assert_eq!(out.len(), 5);
        for i in 0..out.len() {
            assert!(out.value(i, 0).as_i64().unwrap() < 5);
        }
    }

    #[test]
    fn filter_all_keeps_every_row() {
        let b = batch();
        let out = batch_filter(&b, &Predicate::All);
        assert_eq!(out.len(), b.len());
    }

    #[test]
    fn filter_type_mismatch_returns_empty() {
        let b = batch();
        let out = batch_filter(
            &b,
            &Predicate::Eq {
                column: 0,
                value: Literal::Text("nope".into()),
            },
        );
        assert_eq!(out.len(), 0);
    }

    #[test]
    fn filter_float_comparisons_handle_mixed_int_literal() {
        let b = batch();
        let out = batch_filter(
            &b,
            &Predicate::Gt {
                column: 1,
                value: Literal::Int64(4),
            },
        );
        assert_eq!(out.len(), 5);
        for i in 0..out.len() {
            assert!(out.value(i, 1).as_f64().unwrap() > 4.0);
        }
    }
}
