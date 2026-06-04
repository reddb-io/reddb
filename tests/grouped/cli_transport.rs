//! Grouped integration-test harness for the related domain files.
//!
//! Cargo builds one linked binary per integration target. Keep the
//! original test files in `tests/` and include them here so test names
//! remain scoped by their source file while link count stays small.

#![allow(dead_code, unexpected_cfgs)]

#[path = "../cli_bootstrap.rs"]
mod cli_bootstrap;

#[path = "../cli_migrate_from_redis.rs"]
mod cli_migrate_from_redis;

#[path = "../cli_query_param.rs"]
mod cli_query_param;

#[path = "../cross_binary_smoke.rs"]
mod cross_binary_smoke;

#[path = "../e2e_issue_556_graph_sql_http_parity_and_limits.rs"]
mod e2e_issue_556_graph_sql_http_parity_and_limits;

#[path = "../e2e_issue_762_redwire_output_stream.rs"]
mod e2e_issue_762_redwire_output_stream;

#[path = "../e2e_issue_764_redwire_input_stream.rs"]
mod e2e_issue_764_redwire_input_stream;

#[path = "../grpc_batch_insert.rs"]
mod grpc_batch_insert;

#[path = "../http_batch_insert.rs"]
mod http_batch_insert;

#[path = "../http_connection_limiter.rs"]
mod http_connection_limiter;

#[path = "../http_handler_deadline.rs"]
mod http_handler_deadline;

#[path = "../http_handler_metrics.rs"]
mod http_handler_metrics;

#[path = "../http_principal_inflight_cap.rs"]
mod http_principal_inflight_cap;

#[path = "../http_query_grimms_graph.rs"]
mod http_query_grimms_graph;

#[path = "../integration_rpc_stdio.rs"]
mod integration_rpc_stdio;

#[path = "../postgres_wire_extended.rs"]
mod postgres_wire_extended;

#[path = "../reddb_client_embedded.rs"]
mod reddb_client_embedded;

#[path = "../redwire_queue_read_wait_smoke.rs"]
mod redwire_queue_read_wait_smoke;

#[path = "../redwire_smoke.rs"]
mod redwire_smoke;

#[path = "../snowplow_adapter_example.rs"]
mod snowplow_adapter_example;
