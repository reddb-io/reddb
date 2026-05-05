//! Columnar operators — filter / project / aggregate over batches.
//!
//! Each operator takes one or more [`ColumnBatch`]es and produces
//! fresh batches. No mutation of input. The inner loops stay as
//! simple as possible so the compiler auto-vectorises them; a
//! future B2 sprint plugs explicit SIMD into the hot loops.
//!
//! The operators are plain functions rather than an `Operator` trait
//! for now — keeps the surface small until there's a real planner
//! call site. The trait can land when B1 + B2 + B5 share a pipeline.

pub mod aggregate;
pub mod filter;
pub mod project;

pub use aggregate::{batch_aggregate, AggregateSpec};
pub use filter::{batch_filter, Predicate};
pub use project::batch_project;
