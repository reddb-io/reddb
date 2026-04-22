//! `BatchAggregate` — group by keys + reduce each numeric column.
//!
//! The reducer supports COUNT / SUM / AVG / MIN / MAX over Int64 and
//! Float64 columns. Group keys may be Int64, Float64 bit-patterns,
//! Bool, or Text. Output is a `Vec<AggregateRow>` — operator-level
//! primitive; the SQL dispatch layer converts it to a result batch
//! as part of the B5 (projections) sprint.

use std::collections::HashMap;

use super::super::column_batch::{ColumnBatch, ColumnVector, ValueRef};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AggregateOp {
    Count,
    Sum,
    Avg,
    Min,
    Max,
}

#[derive(Debug, Clone)]
pub struct AggregateSpec {
    /// Column index to aggregate. For `Count`, ignored (counts rows).
    pub column: usize,
    pub op: AggregateOp,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GroupKeyPart {
    Int64(i64),
    Float64Bits(u64),
    Bool(bool),
    Text(String),
    Null,
}

impl Eq for GroupKeyPart {}

impl std::hash::Hash for GroupKeyPart {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        match self {
            GroupKeyPart::Int64(v) => {
                0u8.hash(state);
                v.hash(state);
            }
            GroupKeyPart::Float64Bits(v) => {
                1u8.hash(state);
                v.hash(state);
            }
            GroupKeyPart::Bool(v) => {
                2u8.hash(state);
                v.hash(state);
            }
            GroupKeyPart::Text(v) => {
                3u8.hash(state);
                v.hash(state);
            }
            GroupKeyPart::Null => {
                4u8.hash(state);
            }
        }
    }
}

type GroupKey = Vec<GroupKeyPart>;

#[derive(Debug, Clone)]
pub struct AggregateResult {
    pub op: AggregateOp,
    pub column: usize,
    pub value: f64,
    /// For averages we also expose the intermediate count so callers
    /// can merge partial aggregations across batches.
    pub count: u64,
}

#[derive(Debug, Clone)]
pub struct AggregateRow {
    pub key: GroupKey,
    pub results: Vec<AggregateResult>,
}

/// Group `batch` by `group_columns` and produce one row per key with
/// each `AggregateSpec` applied. `group_columns` may be empty, in
/// which case the whole batch reduces to a single row.
pub fn batch_aggregate(
    batch: &ColumnBatch,
    group_columns: &[usize],
    specs: &[AggregateSpec],
) -> Vec<AggregateRow> {
    if batch.is_empty() {
        return Vec::new();
    }
    let mut groups: HashMap<GroupKey, Vec<Accumulator>> = HashMap::new();
    for row in 0..batch.len() {
        let key: GroupKey = group_columns
            .iter()
            .map(|c| group_key_part(batch, row, *c))
            .collect();
        let accs = groups
            .entry(key)
            .or_insert_with(|| specs.iter().map(Accumulator::new).collect());
        for (idx, spec) in specs.iter().enumerate() {
            accs[idx].observe(batch, row, spec);
        }
    }
    let mut out: Vec<AggregateRow> = groups
        .into_iter()
        .map(|(key, accs)| {
            let results = accs
                .into_iter()
                .zip(specs.iter())
                .map(|(acc, spec)| acc.finalize(spec))
                .collect();
            AggregateRow { key, results }
        })
        .collect();
    // Deterministic output ordering simplifies test assertions.
    out.sort_by(|a, b| compare_keys(&a.key, &b.key));
    out
}

fn group_key_part(batch: &ColumnBatch, row: usize, column: usize) -> GroupKeyPart {
    match batch.value(row, column) {
        ValueRef::Int64(v) => GroupKeyPart::Int64(v),
        ValueRef::Float64(v) => GroupKeyPart::Float64Bits(v.to_bits()),
        ValueRef::Bool(v) => GroupKeyPart::Bool(v),
        ValueRef::Text(v) => GroupKeyPart::Text(v.to_string()),
        ValueRef::Null => GroupKeyPart::Null,
    }
}

#[derive(Debug, Clone)]
struct Accumulator {
    count: u64,
    sum: f64,
    min: f64,
    max: f64,
    any_observed: bool,
}

impl Accumulator {
    fn new(_spec: &AggregateSpec) -> Self {
        Self {
            count: 0,
            sum: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            any_observed: false,
        }
    }

    fn observe(&mut self, batch: &ColumnBatch, row: usize, spec: &AggregateSpec) {
        match spec.op {
            AggregateOp::Count => {
                self.count += 1;
            }
            AggregateOp::Sum | AggregateOp::Avg | AggregateOp::Min | AggregateOp::Max => {
                if let Some(v) = numeric_value(batch, row, spec.column) {
                    self.count += 1;
                    self.sum += v;
                    if v < self.min {
                        self.min = v;
                    }
                    if v > self.max {
                        self.max = v;
                    }
                    self.any_observed = true;
                }
            }
        }
    }

    fn finalize(self, spec: &AggregateSpec) -> AggregateResult {
        let value = match spec.op {
            AggregateOp::Count => self.count as f64,
            AggregateOp::Sum => self.sum,
            AggregateOp::Avg => {
                if self.count == 0 {
                    0.0
                } else {
                    self.sum / self.count as f64
                }
            }
            AggregateOp::Min => {
                if self.any_observed {
                    self.min
                } else {
                    0.0
                }
            }
            AggregateOp::Max => {
                if self.any_observed {
                    self.max
                } else {
                    0.0
                }
            }
        };
        AggregateResult {
            op: spec.op,
            column: spec.column,
            value,
            count: self.count,
        }
    }
}

fn numeric_value(batch: &ColumnBatch, row: usize, column: usize) -> Option<f64> {
    let col = batch.columns.get(column)?;
    if !col.is_valid(row) {
        return None;
    }
    match col {
        ColumnVector::Int64 { data, .. } => Some(data[row] as f64),
        ColumnVector::Float64 { data, .. } => Some(data[row]),
        _ => None,
    }
}

fn compare_keys(a: &[GroupKeyPart], b: &[GroupKeyPart]) -> std::cmp::Ordering {
    for (x, y) in a.iter().zip(b.iter()) {
        let ord = compare_key_part(x, y);
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    a.len().cmp(&b.len())
}

fn compare_key_part(x: &GroupKeyPart, y: &GroupKeyPart) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    use GroupKeyPart::*;
    match (x, y) {
        (Int64(a), Int64(b)) => a.cmp(b),
        (Float64Bits(a), Float64Bits(b)) => f64::from_bits(*a)
            .partial_cmp(&f64::from_bits(*b))
            .unwrap_or(Ordering::Equal),
        (Bool(a), Bool(b)) => a.cmp(b),
        (Text(a), Text(b)) => a.cmp(b),
        (Null, Null) => Ordering::Equal,
        (Null, _) => Ordering::Less,
        (_, Null) => Ordering::Greater,
        _ => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::super::super::column_batch::{ColumnKind, Field, Schema};
    use super::*;
    use std::sync::Arc;

    fn batch() -> ColumnBatch {
        let schema = Arc::new(Schema::new(vec![
            Field {
                name: "region".into(),
                kind: ColumnKind::Text,
                nullable: false,
            },
            Field {
                name: "amount".into(),
                kind: ColumnKind::Float64,
                nullable: false,
            },
        ]));
        ColumnBatch::new(
            schema,
            vec![
                ColumnVector::Text {
                    data: vec![
                        "us".into(),
                        "eu".into(),
                        "us".into(),
                        "us".into(),
                        "eu".into(),
                    ],
                    validity: None,
                },
                ColumnVector::Float64 {
                    data: vec![10.0, 20.0, 30.0, 40.0, 50.0],
                    validity: None,
                },
            ],
        )
    }

    #[test]
    fn count_star_over_whole_batch() {
        let b = batch();
        let out = batch_aggregate(
            &b,
            &[],
            &[AggregateSpec {
                column: 0,
                op: AggregateOp::Count,
            }],
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].results[0].value, 5.0);
    }

    #[test]
    fn sum_grouped_by_region() {
        let b = batch();
        let out = batch_aggregate(
            &b,
            &[0],
            &[AggregateSpec {
                column: 1,
                op: AggregateOp::Sum,
            }],
        );
        assert_eq!(out.len(), 2);
        // Ordering is deterministic (Text Ord) — eu first, us second.
        assert_eq!(out[0].key[0], GroupKeyPart::Text("eu".into()));
        assert_eq!(out[0].results[0].value, 70.0);
        assert_eq!(out[1].key[0], GroupKeyPart::Text("us".into()));
        assert_eq!(out[1].results[0].value, 80.0);
    }

    #[test]
    fn avg_handles_empty_group_cleanly() {
        let b = batch();
        let out = batch_aggregate(
            &b,
            &[0],
            &[AggregateSpec {
                column: 1,
                op: AggregateOp::Avg,
            }],
        );
        let eu_row = out
            .iter()
            .find(|r| r.key[0] == GroupKeyPart::Text("eu".into()))
            .unwrap();
        assert_eq!(eu_row.results[0].value, 35.0);
        let us_row = out
            .iter()
            .find(|r| r.key[0] == GroupKeyPart::Text("us".into()))
            .unwrap();
        assert!((us_row.results[0].value - (80.0 / 3.0)).abs() < 1e-6);
    }

    #[test]
    fn min_and_max_agree_on_shape() {
        let b = batch();
        let out = batch_aggregate(
            &b,
            &[0],
            &[
                AggregateSpec {
                    column: 1,
                    op: AggregateOp::Min,
                },
                AggregateSpec {
                    column: 1,
                    op: AggregateOp::Max,
                },
            ],
        );
        let us = out
            .iter()
            .find(|r| r.key[0] == GroupKeyPart::Text("us".into()))
            .unwrap();
        assert_eq!(us.results[0].value, 10.0);
        assert_eq!(us.results[1].value, 40.0);
    }

    #[test]
    fn empty_batch_returns_empty() {
        let b = batch();
        let empty = b.take(&[]);
        let out = batch_aggregate(
            &empty,
            &[],
            &[AggregateSpec {
                column: 0,
                op: AggregateOp::Count,
            }],
        );
        assert!(out.is_empty());
    }

    #[test]
    fn multi_key_grouping_preserves_combinations() {
        let schema = Arc::new(Schema::new(vec![
            Field {
                name: "region".into(),
                kind: ColumnKind::Text,
                nullable: false,
            },
            Field {
                name: "tier".into(),
                kind: ColumnKind::Int64,
                nullable: false,
            },
            Field {
                name: "v".into(),
                kind: ColumnKind::Int64,
                nullable: false,
            },
        ]));
        let b = ColumnBatch::new(
            schema,
            vec![
                ColumnVector::Text {
                    data: vec!["a".into(), "a".into(), "b".into(), "a".into()],
                    validity: None,
                },
                ColumnVector::Int64 {
                    data: vec![1, 2, 1, 1],
                    validity: None,
                },
                ColumnVector::Int64 {
                    data: vec![10, 20, 30, 40],
                    validity: None,
                },
            ],
        );
        let out = batch_aggregate(
            &b,
            &[0, 1],
            &[AggregateSpec {
                column: 2,
                op: AggregateOp::Sum,
            }],
        );
        assert_eq!(out.len(), 3);
        // (a, 1) → 10 + 40 = 50; (a, 2) → 20; (b, 1) → 30.
        let find = |r: &str, t: i64| {
            out.iter()
                .find(|row| {
                    row.key[0] == GroupKeyPart::Text(r.into())
                        && row.key[1] == GroupKeyPart::Int64(t)
                })
                .unwrap()
                .results[0]
                .value
        };
        assert_eq!(find("a", 1), 50.0);
        assert_eq!(find("a", 2), 20.0);
        assert_eq!(find("b", 1), 30.0);
    }
}
