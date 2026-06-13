#![allow(dead_code)]

#[path = "grouped/timeseries_remaining/e2e_continuous_aggregate.rs"]
mod e2e_continuous_aggregate;

#[path = "grouped/timeseries_remaining/e2e_issue_747_red_typed_timeseries_metrics.rs"]
mod e2e_issue_747_red_typed_timeseries_metrics;

#[path = "grouped/timeseries_remaining/e2e_issue_748_red_hypertable_chunks.rs"]
mod e2e_issue_748_red_hypertable_chunks;

#[path = "grouped/timeseries_remaining/e2e_metrics_cardinality_budget.rs"]
mod e2e_metrics_cardinality_budget;

#[path = "grouped/timeseries_remaining/e2e_metrics_grafana_compat_smoke.rs"]
mod e2e_metrics_grafana_compat_smoke;

#[path = "grouped/timeseries_remaining/e2e_metrics_remote_write.rs"]
mod e2e_metrics_remote_write;

#[path = "grouped/timeseries_remaining/e2e_metrics_rollup_retention.rs"]
mod e2e_metrics_rollup_retention;

#[path = "grouped/timeseries_remaining/e2e_metrics_tenant_isolation.rs"]
mod e2e_metrics_tenant_isolation;

#[path = "grouped/timeseries_remaining/e2e_sessionize_operator.rs"]
mod e2e_sessionize_operator;
