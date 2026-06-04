//! Grouped integration-test harness for the related domain files.
//!
//! Cargo builds one linked binary per integration target. Keep the
//! original test files in `tests/` and include them here so test names
//! remain scoped by their source file while link count stays small.

#![allow(dead_code, unexpected_cfgs)]

#[path = "../e2e_collection_retention_policy.rs"]
mod e2e_collection_retention_policy;

#[path = "../e2e_continuous_aggregate.rs"]
mod e2e_continuous_aggregate;

#[path = "../e2e_create_hypertable.rs"]
mod e2e_create_hypertable;

#[path = "../e2e_hypertable_persist_restart.rs"]
mod e2e_hypertable_persist_restart;

#[path = "../e2e_hypertable_prune.rs"]
mod e2e_hypertable_prune;

#[path = "../e2e_hypertable_retention.rs"]
mod e2e_hypertable_retention;

#[path = "../e2e_issue_543_timeseries_tag_json_round_trip.rs"]
mod e2e_issue_543_timeseries_tag_json_round_trip;

#[path = "../e2e_issue_748_red_hypertable_chunks.rs"]
mod e2e_issue_748_red_hypertable_chunks;

#[path = "../e2e_issue_859_columnar_chunk_eviction.rs"]
mod e2e_issue_859_columnar_chunk_eviction;

#[path = "../e2e_retention_sweeper.rs"]
mod e2e_retention_sweeper;

#[path = "../e2e_timeseries_session_descriptor.rs"]
mod e2e_timeseries_session_descriptor;
