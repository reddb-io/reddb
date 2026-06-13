#![allow(dead_code)]

#[path = "grouped/timeseries_metrics/e2e_create_hypertable.rs"]
mod e2e_create_hypertable;

#[path = "grouped/timeseries_metrics/e2e_hypertable_persist_restart.rs"]
mod e2e_hypertable_persist_restart;

#[path = "grouped/timeseries_metrics/e2e_hypertable_prune.rs"]
mod e2e_hypertable_prune;

#[path = "grouped/timeseries_metrics/e2e_hypertable_retention.rs"]
mod e2e_hypertable_retention;

#[path = "grouped/timeseries_metrics/e2e_issue_543_timeseries_tag_json_round_trip.rs"]
mod e2e_issue_543_timeseries_tag_json_round_trip;

#[path = "grouped/timeseries_metrics/e2e_issue_785_metric_descriptor_read_by_path.rs"]
mod e2e_issue_785_metric_descriptor_read_by_path;

#[path = "grouped/timeseries_metrics/e2e_issue_790_derived_metric_descriptors.rs"]
mod e2e_issue_790_derived_metric_descriptors;

#[path = "grouped/timeseries_metrics/e2e_metric_descriptor_catalog.rs"]
mod e2e_metric_descriptor_catalog;

#[path = "grouped/timeseries_metrics/e2e_metrics_collection_contract.rs"]
mod e2e_metrics_collection_contract;

#[path = "grouped/timeseries_metrics/e2e_metrics_prometheus_query.rs"]
mod e2e_metrics_prometheus_query;

#[path = "grouped/timeseries_metrics/e2e_slo_descriptor_catalog.rs"]
mod e2e_slo_descriptor_catalog;
