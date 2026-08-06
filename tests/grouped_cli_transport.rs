//! Grouped integration-test harness for CLI and small transport handlers.
//!
//! Cargo links one binary per top-level integration-test file. Keep the
//! original domain tests as modules under `tests/grouped/` so their function
//! names remain visible while this pilot reduces six linked binaries to one.

#![allow(dead_code)]

#[path = "grouped/cli_transport/cli_migrate_from_redis.rs"]
mod cli_migrate_from_redis;

#[path = "grouped/cli_transport/cli_query_param.rs"]
mod cli_query_param;

#[path = "grouped/cli_transport/e2e_issue_545_transport_listener_readiness.rs"]
mod e2e_issue_545_transport_listener_readiness;

#[path = "grouped/cli_transport/http_batch_insert.rs"]
mod http_batch_insert;
