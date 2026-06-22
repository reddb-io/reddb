//! Grouped integration-test harness for the related domain files.
//!
//! Cargo builds one linked binary per integration target. Keep the
//! original test files in `tests/` and include them here so test names
//! remain scoped by their source file while link count stays small.

#![allow(dead_code, unexpected_cfgs)]

#[path = "events_retention/e2e_collection_retention_policy.rs"]
mod e2e_collection_retention_policy;

#[path = "timeseries_remaining/e2e_continuous_aggregate.rs"]
mod e2e_continuous_aggregate;

#[path = "timeseries_metrics/e2e_create_hypertable.rs"]
mod e2e_create_hypertable;

#[path = "timeseries_metrics/e2e_hypertable_persist_restart.rs"]
mod e2e_hypertable_persist_restart;

#[path = "timeseries_metrics/e2e_hypertable_prune.rs"]
mod e2e_hypertable_prune;

#[path = "timeseries_metrics/e2e_hypertable_retention.rs"]
mod e2e_hypertable_retention;

#[path = "timeseries_metrics/e2e_issue_543_timeseries_tag_json_round_trip.rs"]
mod e2e_issue_543_timeseries_tag_json_round_trip;

#[path = "timeseries_remaining/e2e_issue_748_red_hypertable_chunks.rs"]
mod e2e_issue_748_red_hypertable_chunks;

#[path = "runtime_persistence/e2e_issue_859_columnar_chunk_eviction.rs"]
mod e2e_issue_859_columnar_chunk_eviction;

#[path = "events_retention/e2e_retention_sweeper.rs"]
mod e2e_retention_sweeper;

#[path = "timeseries_remaining/e2e_timeseries_session_descriptor.rs"]
mod e2e_timeseries_session_descriptor;
