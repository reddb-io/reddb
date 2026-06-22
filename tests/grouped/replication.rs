//! Grouped integration-test harness for the related domain files.
//!
//! Cargo builds one linked binary per integration target. Keep the
//! original test files in `tests/` and include them here so test names
//! remain scoped by their source file while link count stays small.

#![allow(dead_code, unexpected_cfgs)]

#[path = "../chaos_replica_apply_divergence.rs"]
mod chaos_replica_apply_divergence;

#[path = "../chaos_replica_apply_gap.rs"]
mod chaos_replica_apply_gap;

#[path = "../e2e_issue_596_materialized_view_replica_replay.rs"]
mod e2e_issue_596_materialized_view_replica_replay;

#[path = "../e2e_issue_813_replica_repull_idempotent.rs"]
mod e2e_issue_813_replica_repull_idempotent;

#[path = "../e2e_issue_815_replica_responsive_under_apply.rs"]
mod e2e_issue_815_replica_responsive_under_apply;

#[path = "../e2e_issue_817_replication_metrics.rs"]
mod e2e_issue_817_replication_metrics;

#[path = "../e2e_issue_820_replication_auth.rs"]
mod e2e_issue_820_replication_auth;

#[path = "../e2e_issue_824_replication_ttl.rs"]
mod e2e_issue_824_replication_ttl;

#[path = "../e2e_issue_826_replication_flow_control.rs"]
mod e2e_issue_826_replication_flow_control;

#[path = "../e2e_issue_827_causal_bookmarks.rs"]
mod e2e_issue_827_causal_bookmarks;

#[path = "../e2e_issue_828_replication_tail_follow.rs"]
mod e2e_issue_828_replication_tail_follow;

#[path = "../e2e_issue_829_replication_mtls_identity.rs"]
mod e2e_issue_829_replication_mtls_identity;

#[path = "../e2e_issue_830_replication_bootstrap.rs"]
mod e2e_issue_830_replication_bootstrap;

#[path = "../e2e_issue_833_replication_failover.rs"]
mod e2e_issue_833_replication_failover;

#[path = "../e2e_issue_840_replication_auto_rollback.rs"]
mod e2e_issue_840_replication_auto_rollback;

#[path = "../e2e_replica_readonly.rs"]
mod e2e_replica_readonly;
