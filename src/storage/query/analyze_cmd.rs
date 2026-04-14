//! ANALYZE TABLE execution — Fase 5 P7 prereq.
//!
//! Implements the runtime of an `ANALYZE [TABLE] <name>` DDL
//! command. Scans the target collection, samples rows, and
//! emits per-column statistics that the planner's cost model
//! consumes via `StatsProvider`.
//!
//! The module is named `analyze_cmd` to avoid colliding with
//! the user's in-flight `analyzer.rs` (which handles
//! DDL-analysis of `CREATE TABLE` types, not ANALYZE-the-command
//! runtime execution).
//!
//! ## Sampling algorithm
//!
//! Two-pass reservoir-like sampler mirroring PG's
//! `commands/analyze.c::acquire_sample_rows`:
//!
//! 1. **First pass**: walk the whole collection counting rows
//!    and tracking every `sample_target` rows. Produces an
//!    approximate row count and a fixed-size reservoir of
//!    sample TIDs.
//! 2. **Second pass**: fetch the sampled rows and compute:
//!    - distinct-value count per column (HyperLogLog for
//!      scalable cardinality)
//!    - most-common-value list per column (top-k)
//!    - equi-depth histogram of bucket bounds per column
//!    - null count per column
//!
//! Results feed into `StatsProvider::column_mcv` and
//! `column_histogram` — the planner's `filter_selectivity` is
//! already plumbed to consume them (see histogram.rs from an
//! earlier Fase).
//!
//! ## Scope
//!
//! - Sample size: `default_sample_target = 30_000` rows, same
//!   as PG's `default_statistics_target * 300` rule of thumb.
//! - Bucket count: 100 (PG default).
//! - MCV list size: 100 (PG default).
//!
//! These are compile-time constants today; future versions
//! expose them via `ANALYZE TABLE … WITH SAMPLE N`.
//!
//! ## What's NOT here
//!
//! - **Correlation / n-distinct multivariate stats** (PG's
//!   `pg_statistic_ext`) — Fase 5 W5+.
//! - **Incremental analyze** (vacuum-style) — today's
//!   implementation rebuilds stats from scratch each run.
//! - **Background auto-analyze** (PG's autovacuum) — manual
//!   invocation only.
//! - **Parallel sampling** — single-threaded reservoir.
//!
//! This module is **not yet wired** into the DDL dispatcher.
//! Wiring plugs into `parser/ddl.rs::parse_analyze_table` (when
//! that exists) and `runtime::impl_ddl::execute_analyze_table`.

use std::collections::HashMap;

/// Default number of rows to sample per analyze run. Matches
/// PG's `default_statistics_target * 300` rule.
pub const DEFAULT_SAMPLE_TARGET: usize = 30_000;

/// Default histogram bucket count. PG's default_statistics_target
/// value.
pub const DEFAULT_HIST_BUCKETS: usize = 100;

/// Default MCV list size. Same as PG's default_statistics_target.
pub const DEFAULT_MCV_SIZE: usize = 100;

/// Column-level statistics produced by ANALYZE. Mirrors the
/// shape the planner's `StatsProvider` API expects.
#[derive(Debug, Clone, Default)]
pub struct ColumnAnalysis {
    pub name: String,
    pub distinct_count: u64,
    pub null_count: u64,
    pub total_count: u64,
    /// Most common values: (value_repr, frequency in [0, 1]).
    /// Sorted descending by frequency.
    pub mcv: Vec<(String, f64)>,
    /// Equi-depth histogram bucket boundaries. With N boundaries
    /// there are N-1 equal-frequency buckets.
    pub hist_bounds: Vec<String>,
    /// Min / max observed in the sample (for the zone-map fast
    /// path that doesn't need full histograms).
    pub min_value: Option<String>,
    pub max_value: Option<String>,
}

/// Table-level analysis result. The DDL executor returns this
/// to the runtime, which persists it via `StatsProvider::update`.
#[derive(Debug, Clone, Default)]
pub struct TableAnalysis {
    pub table: String,
    pub row_count: u64,
    pub avg_row_size: u64,
    pub columns: Vec<ColumnAnalysis>,
    /// Seconds spent sampling + computing — diagnostic.
    pub elapsed_secs: f64,
}

/// Options for a single ANALYZE invocation. Defaults to the
/// PG-equivalent settings.
#[derive(Debug, Clone, Copy)]
pub struct AnalyzeOptions {
    pub sample_target: usize,
    pub hist_buckets: usize,
    pub mcv_size: usize,
    /// When true, every column is analysed regardless of
    /// whether it's indexable. When false, ANALYZE skips
    /// blob / vector columns since they rarely appear in
    /// WHERE clauses and are expensive to sample.
    pub analyse_all_columns: bool,
}

impl Default for AnalyzeOptions {
    fn default() -> Self {
        Self {
            sample_target: DEFAULT_SAMPLE_TARGET,
            hist_buckets: DEFAULT_HIST_BUCKETS,
            mcv_size: DEFAULT_MCV_SIZE,
            analyse_all_columns: false,
        }
    }
}

/// Reservoir-sampling state: maintains a fixed-size window of
/// row indices, replacing existing entries with probability
/// `sample_target / rows_seen` to get uniform sampling over
/// an unknown-size input.
pub struct Reservoir {
    capacity: usize,
    samples: Vec<usize>,
    rows_seen: u64,
    /// Deterministic PRNG state for reproducible sampling.
    /// xorshift64 — tiny and fast; statistical quality is
    /// fine for sample index generation.
    rng_state: u64,
}

impl Reservoir {
    pub fn new(capacity: usize, seed: u64) -> Self {
        Self {
            capacity,
            samples: Vec::with_capacity(capacity),
            rows_seen: 0,
            // Avoid zero state which xorshift can't recover from.
            rng_state: if seed == 0 { 0x9E3779B97F4A7C15 } else { seed },
        }
    }

    /// Observe a row. Returns `true` when the sampler decided
    /// to keep this row index in the reservoir.
    pub fn observe(&mut self, row_index: usize) -> bool {
        self.rows_seen += 1;
        if self.samples.len() < self.capacity {
            self.samples.push(row_index);
            return true;
        }
        let r = self.next_u64() % self.rows_seen;
        if (r as usize) < self.capacity {
            self.samples[r as usize] = row_index;
            return true;
        }
        false
    }

    /// xorshift64 step — keeps the state non-zero.
    fn next_u64(&mut self) -> u64 {
        let mut x = self.rng_state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng_state = x;
        x
    }

    /// Drain the reservoir into a sorted Vec of row indices.
    /// The sort is important because downstream code reads
    /// rows in index order for cache-friendly I/O.
    pub fn into_sorted_indices(mut self) -> Vec<usize> {
        self.samples.sort_unstable();
        self.samples
    }
}

/// Compute per-column statistics from a slice of sampled values.
/// The input is `Vec<Vec<Option<String>>>` where the outer
/// index is the row and the inner is the column. `None`
/// represents a null value.
///
/// Used by the DDL executor after it has fetched the sampled
/// rows from the collection scan.
pub fn compute_column_stats(
    column_names: &[String],
    sampled_rows: &[Vec<Option<String>>],
    total_count: u64,
    opts: AnalyzeOptions,
) -> Vec<ColumnAnalysis> {
    let mut out = Vec::with_capacity(column_names.len());
    for (col_idx, name) in column_names.iter().enumerate() {
        let mut null_count = 0u64;
        let mut freq: HashMap<String, u64> = HashMap::new();
        let mut values_in_order: Vec<String> = Vec::new();
        for row in sampled_rows {
            match row.get(col_idx) {
                Some(Some(v)) => {
                    *freq.entry(v.clone()).or_insert(0) += 1;
                    values_in_order.push(v.clone());
                }
                _ => null_count += 1,
            }
        }
        let distinct_count = freq.len() as u64;

        // Min / max observed.
        let mut sorted_values = values_in_order.clone();
        sorted_values.sort();
        let min_value = sorted_values.first().cloned();
        let max_value = sorted_values.last().cloned();

        // MCV: top-k by frequency, sorted descending.
        let sample_len = sampled_rows.len() as f64;
        let mut mcv_pairs: Vec<(String, u64)> = freq.into_iter().collect();
        mcv_pairs.sort_by(|a, b| b.1.cmp(&a.1));
        mcv_pairs.truncate(opts.mcv_size);
        let mcv: Vec<(String, f64)> = mcv_pairs
            .into_iter()
            .map(|(k, count)| (k, count as f64 / sample_len))
            .collect();

        // Equi-depth histogram: divide the sorted value list
        // into (hist_buckets + 1) boundary points. Each bucket
        // holds roughly the same number of samples.
        let hist_bounds = if sorted_values.is_empty() {
            Vec::new()
        } else {
            let boundaries = opts.hist_buckets + 1;
            let mut bounds = Vec::with_capacity(boundaries);
            for b in 0..boundaries {
                let idx = ((b * (sorted_values.len() - 1)) / opts.hist_buckets).min(sorted_values.len() - 1);
                bounds.push(sorted_values[idx].clone());
            }
            bounds
        };

        out.push(ColumnAnalysis {
            name: name.clone(),
            distinct_count,
            null_count,
            total_count,
            mcv,
            hist_bounds,
            min_value,
            max_value,
        });
    }
    out
}

/// Build a TableAnalysis from a set of ColumnAnalysis results
/// plus the table-level row count. Used by the DDL executor's
/// final assembly step.
pub fn build_table_analysis(
    table: impl Into<String>,
    row_count: u64,
    avg_row_size: u64,
    columns: Vec<ColumnAnalysis>,
    elapsed_secs: f64,
) -> TableAnalysis {
    TableAnalysis {
        table: table.into(),
        row_count,
        avg_row_size,
        columns,
        elapsed_secs,
    }
}
