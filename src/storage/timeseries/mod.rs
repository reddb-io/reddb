//! Time-Series Storage Module
//!
//! Optimized storage for time-stamped metric data with:
//! - Delta-of-delta timestamp encoding
//! - Gorilla XOR float compression
//! - Automatic downsampling
//! - Retention policies
//! - Time-bucket aggregation

pub mod aggregation;
pub mod chunk;
pub mod compression;
pub mod retention;

pub use aggregation::{time_bucket, AggregationType, WindowAggregator};
pub use chunk::{TimeSeriesChunk, TimeSeriesPoint};
pub use compression::{delta_decode_timestamps, delta_encode_timestamps};
pub use retention::RetentionPolicy;
