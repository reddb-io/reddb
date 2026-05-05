//! Vectorised columnar execution — a ClickHouse-inspired batch path
//! that runs in parallel to the existing Volcano iterators.
//!
//! The batch path represents in-flight data as [`ColumnBatch`]es of
//! up to `BATCH_SIZE` rows (default 2048). Operators consume and
//! produce batches; SIMD-friendly loops inside each operator can
//! work on one column at a time instead of chasing pointers across
//! rows.
//!
//! ## Invariants
//!
//! * Every column in a batch has `len == batch.len`.
//! * Schema is shared (cheap clone of `Arc<Schema>`).
//! * Batches are immutable after construction — operators produce
//!   fresh batches rather than mutating.
//!
//! The module is self-contained in this sprint: SQL dispatch learns
//! the batch path in a follow-up. Tests cover the operators
//! end-to-end on synthetic data.

pub mod column_batch;
pub mod operators;
pub mod parallel;
pub mod simd;

pub use column_batch::{ColumnBatch, ColumnVector, Schema, ValueRef, BATCH_SIZE};
pub use operators::{batch_aggregate, batch_filter, batch_project, AggregateSpec, Predicate};
