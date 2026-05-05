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
pub mod continuous_aggregate;
pub mod hypertable;
pub mod log_pipeline;
pub mod retention;
pub mod temporal_index;

pub use aggregation::{time_bucket, AggregationType, WindowAggregator};
pub use chunk::{TimeSeriesChunk, TimeSeriesPoint};
pub use compression::{
    delta_decode_timestamps, delta_encode_timestamps, select_int_codec, t64_decode, t64_encode,
    zstd_compress, zstd_decompress, TsIntCodec,
};
pub use continuous_aggregate::{
    BucketState, ContinuousAggregateColumn, ContinuousAggregateEngine, ContinuousAggregateSource,
    ContinuousAggregateSpec, ContinuousAggregateState, RefreshPoint,
};
pub use hypertable::{ChunkId, ChunkMeta, HypertableRegistry, HypertableSpec};
pub use log_pipeline::{LogIngestStats, LogLine, LogPipeline, LogSeverity};
pub use retention::{
    RetentionBackend, RetentionDaemonHandle, RetentionPolicy, RetentionRegistry, RetentionStats,
};
pub use temporal_index::{ChunkHandle, TemporalIndex};
