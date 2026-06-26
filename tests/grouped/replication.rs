//! Grouped integration-test harness for the related domain files.
//!
//! Cargo builds one linked binary per integration target. Keep the
//! original test files in `tests/` and include them here so test names
//! remain scoped by their source file while link count stays small.

#![allow(dead_code, unexpected_cfgs)]

#[path = "chaos/chaos_replica_apply_divergence.rs"]
mod chaos_replica_apply_divergence;

#[path = "chaos/chaos_replica_apply_gap.rs"]
mod chaos_replica_apply_gap;

#[path = "../e2e_issue_596_materialized_view_replica_replay.rs"]
mod e2e_issue_596_materialized_view_replica_replay;

#[path = "replication/e2e_issue_813_replica_repull_idempotent.rs"]
mod e2e_issue_813_replica_repull_idempotent;

#[path = "replication/e2e_issue_815_replica_responsive_under_apply.rs"]
mod e2e_issue_815_replica_responsive_under_apply;

#[path = "replication/e2e_issue_817_replication_metrics.rs"]
mod e2e_issue_817_replication_metrics;

#[path = "replication/e2e_issue_820_replication_auth.rs"]
mod e2e_issue_820_replication_auth;

#[path = "replication/e2e_issue_824_replication_ttl.rs"]
mod e2e_issue_824_replication_ttl;

#[path = "replication/e2e_issue_826_replication_flow_control.rs"]
mod e2e_issue_826_replication_flow_control;

#[path = "replication/e2e_issue_827_causal_bookmarks.rs"]
mod e2e_issue_827_causal_bookmarks;

#[path = "replication/e2e_issue_828_replication_tail_follow.rs"]
mod e2e_issue_828_replication_tail_follow;

#[path = "replication/e2e_issue_829_replication_mtls_identity.rs"]
mod e2e_issue_829_replication_mtls_identity;

#[path = "replication/e2e_issue_830_replication_bootstrap.rs"]
mod e2e_issue_830_replication_bootstrap;

#[path = "replication/e2e_issue_833_replication_failover.rs"]
mod e2e_issue_833_replication_failover;

#[path = "replication/e2e_issue_840_replication_auto_rollback.rs"]
mod e2e_issue_840_replication_auto_rollback;

#[path = "replication/e2e_issue_1358_replication_network_sim.rs"]
mod e2e_issue_1358_replication_network_sim;

#[path = "replication/e2e_replica_readonly.rs"]
mod e2e_replica_readonly;
