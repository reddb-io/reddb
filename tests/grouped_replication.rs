#![allow(dead_code)]

#[path = "grouped/replication/e2e_issue_813_replica_repull_idempotent.rs"]
mod e2e_issue_813_replica_repull_idempotent;

#[path = "grouped/replication/e2e_issue_815_replica_responsive_under_apply.rs"]
mod e2e_issue_815_replica_responsive_under_apply;

#[path = "grouped/replication/e2e_issue_817_replication_metrics.rs"]
mod e2e_issue_817_replication_metrics;

#[path = "grouped/replication/e2e_issue_820_replication_auth.rs"]
mod e2e_issue_820_replication_auth;

#[path = "grouped/replication/e2e_issue_824_replication_ttl.rs"]
mod e2e_issue_824_replication_ttl;

#[path = "grouped/replication/e2e_issue_826_replication_flow_control.rs"]
mod e2e_issue_826_replication_flow_control;

#[path = "grouped/replication/e2e_issue_827_causal_bookmarks.rs"]
mod e2e_issue_827_causal_bookmarks;

#[path = "grouped/replication/e2e_issue_828_replication_tail_follow.rs"]
mod e2e_issue_828_replication_tail_follow;

#[path = "grouped/replication/e2e_issue_829_replication_mtls_identity.rs"]
mod e2e_issue_829_replication_mtls_identity;

#[path = "grouped/replication/e2e_issue_830_replication_bootstrap.rs"]
mod e2e_issue_830_replication_bootstrap;

#[path = "grouped/replication/e2e_issue_833_replication_failover.rs"]
mod e2e_issue_833_replication_failover;

#[path = "grouped/replication/e2e_issue_840_replication_auto_rollback.rs"]
mod e2e_issue_840_replication_auto_rollback;

#[path = "grouped/replication/e2e_issue_1836_ownership_admission.rs"]
mod e2e_issue_1836_ownership_admission;

#[path = "grouped/replication/e2e_replica_readonly.rs"]
mod e2e_replica_readonly;
