//! Grouped integration-test harness for the related domain files.
//!
//! Cargo builds one linked binary per integration target. Keep the
//! original test files in `tests/` and include them here so test names
//! remain scoped by their source file while link count stays small.

#![allow(dead_code, unexpected_cfgs)]

#[path = "timeseries_remaining/shared.rs"]
mod timeseries_remaining_shared;

#[path = "sql_window/conformance_window.rs"]
mod conformance_window;

#[path = "graph_analytics/e2e_analytics_source_profiles.rs"]
mod e2e_analytics_source_profiles;

#[path = "graph_analytics/e2e_analytics_v0_smoke.rs"]
mod e2e_analytics_v0_smoke;

#[path = "docs_kv_queue/e2e_document_sql_analytics.rs"]
mod e2e_document_sql_analytics;

#[path = "../e2e_issue_1241_query_latency_histogram.rs"]
mod e2e_issue_1241_query_latency_histogram;

#[path = "probabilistic/e2e_issue_542_probabilistic_commands.rs"]
mod e2e_issue_542_probabilistic_commands;

#[path = "probabilistic/e2e_issue_554_probabilistic_sql_read_forms.rs"]
mod e2e_issue_554_probabilistic_sql_read_forms;

#[path = "timeseries_remaining/e2e_issue_747_red_typed_timeseries_metrics.rs"]
mod e2e_issue_747_red_typed_timeseries_metrics;

#[path = "timeseries_metrics/e2e_issue_785_metric_descriptor_read_by_path.rs"]
mod e2e_issue_785_metric_descriptor_read_by_path;

#[path = "graph_analytics/e2e_issue_789_analytics_v0_non_goals.rs"]
mod e2e_issue_789_analytics_v0_non_goals;

#[path = "timeseries_metrics/e2e_issue_790_derived_metric_descriptors.rs"]
mod e2e_issue_790_derived_metric_descriptors;

#[path = "graph_analytics/e2e_issue_800_analytics_views.rs"]
mod e2e_issue_800_analytics_views;

#[path = "graph_analytics/e2e_issue_801_alter_graph_analytics.rs"]
mod e2e_issue_801_alter_graph_analytics;

#[path = "timeseries_metrics/e2e_metric_descriptor_catalog.rs"]
mod e2e_metric_descriptor_catalog;

#[path = "timeseries_remaining/e2e_metrics_cardinality_budget.rs"]
mod e2e_metrics_cardinality_budget;

#[path = "timeseries_metrics/e2e_metrics_collection_contract.rs"]
mod e2e_metrics_collection_contract;

#[path = "timeseries_remaining/e2e_metrics_grafana_compat_smoke.rs"]
mod e2e_metrics_grafana_compat_smoke;

#[path = "timeseries_metrics/e2e_metrics_prometheus_aggregation.rs"]
mod e2e_metrics_prometheus_aggregation;

#[path = "timeseries_metrics/e2e_metrics_prometheus_counter_functions.rs"]
mod e2e_metrics_prometheus_counter_functions;

#[path = "timeseries_metrics/e2e_metrics_prometheus_histogram.rs"]
mod e2e_metrics_prometheus_histogram;

#[path = "timeseries_metrics/e2e_metrics_prometheus_query.rs"]
mod e2e_metrics_prometheus_query;

#[path = "timeseries_metrics/e2e_metrics_prometheus_query_range.rs"]
mod e2e_metrics_prometheus_query_range;

#[path = "timeseries_remaining/e2e_metrics_remote_write.rs"]
mod e2e_metrics_remote_write;

#[path = "timeseries_remaining/e2e_metrics_rollup_retention.rs"]
mod e2e_metrics_rollup_retention;

#[path = "multimodel_query/e2e_postgres_math_functions.rs"]
mod e2e_postgres_math_functions;

#[path = "probabilistic/e2e_probabilistic_public_contract.rs"]
mod e2e_probabilistic_public_contract;

#[path = "timeseries_remaining/e2e_sessionize_operator.rs"]
mod e2e_sessionize_operator;

#[path = "sql_window/e2e_window_aggregate.rs"]
mod e2e_window_aggregate;

#[path = "sql_window/e2e_window_functions.rs"]
mod e2e_window_functions;

#[path = "sql_window/e2e_within_clause.rs"]
mod e2e_within_clause;

#[path = "sql_window/e2e_within_multi_model.rs"]
mod e2e_within_multi_model;

#[path = "sql_window/window_perf_smoke.rs"]
mod window_perf_smoke;
