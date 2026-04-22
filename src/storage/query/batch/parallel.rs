//! Parallel reductions over columnar batches.
//!
//! Uses the existing `rayon` dependency so we reuse the work-stealing
//! thread pool the rest of the storage layer already warms up (vector
//! search, bulk indexing). Two entry points:
//!
//! * `parallel_sum_f64` — split the slice into N chunks, reduce each
//!   chunk with the SIMD path from [`super::simd`], then combine.
//! * `parallel_aggregate` — run [`super::operators::batch_aggregate`]
//!   over a list of batches concurrently, merging group-by state.
//!
//! This module is deliberately conservative: the parallelism
//! threshold is tuned so short scans skip rayon entirely (overhead
//! dominates). The threshold is tweakable via `min_parallel_len`.

use std::collections::HashMap;

use rayon::prelude::*;

use super::column_batch::ColumnBatch;
use super::operators::aggregate::{
    batch_aggregate, AggregateOp, AggregateResult, AggregateRow, AggregateSpec, GroupKeyPart,
};
use super::simd::{sum_f64, sum_f64_scalar};

const DEFAULT_MIN_PARALLEL_LEN: usize = 4096;

/// Parallel reduction for `sum_f64`. Falls back to single-threaded
/// SIMD under `min_parallel_len`.
pub fn parallel_sum_f64(data: &[f64]) -> f64 {
    parallel_sum_f64_with(data, DEFAULT_MIN_PARALLEL_LEN)
}

pub fn parallel_sum_f64_with(data: &[f64], min_parallel_len: usize) -> f64 {
    if data.len() < min_parallel_len {
        return sum_f64(data);
    }
    let chunk_size = (data.len() / rayon::current_num_threads().max(1)).max(1024);
    data.par_chunks(chunk_size)
        .map(|chunk| sum_f64(chunk))
        .sum()
}

/// Parallel aggregate — splits input batches across threads, runs
/// `batch_aggregate` on each, then merges the partial group-by state.
/// Supports the same set of [`AggregateOp`] as the single-threaded
/// path. Thread-safety comes from each thread producing its own
/// `HashMap` which is merged sequentially at the end.
pub fn parallel_aggregate(
    batches: &[ColumnBatch],
    group_columns: &[usize],
    specs: &[AggregateSpec],
) -> Vec<AggregateRow> {
    if batches.is_empty() {
        return Vec::new();
    }
    let partials: Vec<Vec<AggregateRow>> = batches
        .par_iter()
        .map(|batch| batch_aggregate(batch, group_columns, specs))
        .collect();
    merge_partials(partials, specs)
}

fn merge_partials(partials: Vec<Vec<AggregateRow>>, specs: &[AggregateSpec]) -> Vec<AggregateRow> {
    let mut combined: HashMap<Vec<GroupKeyPart>, Vec<MergedState>> = HashMap::new();
    for rows in partials {
        for row in rows {
            let entry = combined
                .entry(row.key)
                .or_insert_with(|| specs.iter().map(MergedState::new).collect());
            for (idx, result) in row.results.into_iter().enumerate() {
                entry[idx].absorb(&result);
            }
        }
    }
    let mut out: Vec<AggregateRow> = combined
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
    out.sort_by(|a, b| compare_keys(&a.key, &b.key));
    out
}

#[derive(Debug, Clone)]
struct MergedState {
    count: u64,
    sum: f64,
    min: f64,
    max: f64,
    any_observed: bool,
}

impl MergedState {
    fn new(_spec: &AggregateSpec) -> Self {
        Self {
            count: 0,
            sum: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            any_observed: false,
        }
    }

    fn absorb(&mut self, partial: &AggregateResult) {
        match partial.op {
            AggregateOp::Count => {
                self.count += partial.value as u64;
            }
            AggregateOp::Sum => {
                self.sum += partial.value;
                self.count += partial.count;
                self.any_observed |= partial.count > 0;
            }
            AggregateOp::Avg => {
                // partial.value = sum/count; reconstruct sum.
                self.sum += partial.value * partial.count as f64;
                self.count += partial.count;
                self.any_observed |= partial.count > 0;
            }
            AggregateOp::Min => {
                if partial.count > 0 && partial.value < self.min {
                    self.min = partial.value;
                }
                self.count += partial.count;
                self.any_observed |= partial.count > 0;
            }
            AggregateOp::Max => {
                if partial.count > 0 && partial.value > self.max {
                    self.max = partial.value;
                }
                self.count += partial.count;
                self.any_observed |= partial.count > 0;
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

fn compare_keys(a: &[GroupKeyPart], b: &[GroupKeyPart]) -> std::cmp::Ordering {
    for (x, y) in a.iter().zip(b.iter()) {
        let ord = match (x, y) {
            (GroupKeyPart::Int64(x), GroupKeyPart::Int64(y)) => x.cmp(y),
            (GroupKeyPart::Text(x), GroupKeyPart::Text(y)) => x.cmp(y),
            (GroupKeyPart::Bool(x), GroupKeyPart::Bool(y)) => x.cmp(y),
            (GroupKeyPart::Float64Bits(x), GroupKeyPart::Float64Bits(y)) => f64::from_bits(*x)
                .partial_cmp(&f64::from_bits(*y))
                .unwrap_or(std::cmp::Ordering::Equal),
            (GroupKeyPart::Null, GroupKeyPart::Null) => std::cmp::Ordering::Equal,
            (GroupKeyPart::Null, _) => std::cmp::Ordering::Less,
            (_, GroupKeyPart::Null) => std::cmp::Ordering::Greater,
            _ => std::cmp::Ordering::Equal,
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    a.len().cmp(&b.len())
}

#[cfg(test)]
mod tests {
    use super::super::column_batch::{ColumnKind, ColumnVector, Field, Schema};
    use super::*;
    use std::sync::Arc;

    fn synthetic_sum_sample() -> Vec<f64> {
        (0..10_000).map(|i| i as f64 * 0.25).collect()
    }

    #[test]
    fn parallel_sum_matches_scalar_on_large_input() {
        let data = synthetic_sum_sample();
        let expected = sum_f64_scalar(&data);
        let actual = parallel_sum_f64(&data);
        assert!((expected - actual).abs() < 1e-4);
    }

    #[test]
    fn parallel_sum_falls_back_under_threshold() {
        let data = vec![1.0; 100];
        let actual = parallel_sum_f64(&data);
        assert!((actual - 100.0).abs() < 1e-9);
    }

    fn batch_with_regions() -> ColumnBatch {
        let schema = Arc::new(Schema::new(vec![
            Field {
                name: "region".into(),
                kind: ColumnKind::Text,
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
                ColumnVector::Text {
                    data: vec!["a".into(), "a".into(), "b".into(), "b".into(), "a".into()],
                    validity: None,
                },
                ColumnVector::Float64 {
                    data: vec![1.0, 2.0, 10.0, 20.0, 3.0],
                    validity: None,
                },
            ],
        )
    }

    #[test]
    fn parallel_aggregate_merges_partials_correctly() {
        let b1 = batch_with_regions();
        let b2 = batch_with_regions();
        let out = parallel_aggregate(
            &[b1, b2],
            &[0],
            &[AggregateSpec {
                column: 1,
                op: AggregateOp::Sum,
            }],
        );
        assert_eq!(out.len(), 2);
        // Two identical batches: each group's sum doubles.
        let a = out
            .iter()
            .find(|r| r.key[0] == GroupKeyPart::Text("a".into()))
            .unwrap();
        let b = out
            .iter()
            .find(|r| r.key[0] == GroupKeyPart::Text("b".into()))
            .unwrap();
        assert!((a.results[0].value - 12.0).abs() < 1e-9);
        assert!((b.results[0].value - 60.0).abs() < 1e-9);
    }

    #[test]
    fn parallel_aggregate_handles_avg_via_partial_reconstruction() {
        let b1 = batch_with_regions();
        let b2 = batch_with_regions();
        let out = parallel_aggregate(
            &[b1, b2],
            &[0],
            &[AggregateSpec {
                column: 1,
                op: AggregateOp::Avg,
            }],
        );
        let a = out
            .iter()
            .find(|r| r.key[0] == GroupKeyPart::Text("a".into()))
            .unwrap();
        assert!((a.results[0].value - 2.0).abs() < 1e-9);
    }

    #[test]
    fn parallel_aggregate_count_across_batches() {
        let batches: Vec<ColumnBatch> = (0..4).map(|_| batch_with_regions()).collect();
        let out = parallel_aggregate(
            &batches,
            &[],
            &[AggregateSpec {
                column: 0,
                op: AggregateOp::Count,
            }],
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].results[0].value, 20.0);
    }

    #[test]
    fn parallel_aggregate_empty_input_is_empty_output() {
        let out = parallel_aggregate(
            &[],
            &[],
            &[AggregateSpec {
                column: 0,
                op: AggregateOp::Count,
            }],
        );
        assert!(out.is_empty());
    }
}
