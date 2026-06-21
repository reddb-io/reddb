#![allow(dead_code)]

#[path = "grouped/sql_window/conformance_window.rs"]
mod conformance_window;

#[path = "grouped/sql_window/e2e_explain.rs"]
mod e2e_explain;

#[path = "grouped/sql_window/e2e_global_select.rs"]
mod e2e_global_select;

#[path = "grouped/sql_window/e2e_show_sample.rs"]
mod e2e_show_sample;

#[path = "grouped/sql_window/e2e_sql_cte.rs"]
mod e2e_sql_cte;

#[path = "grouped/sql_window/e2e_views.rs"]
mod e2e_views;

#[path = "grouped/sql_window/e2e_window_aggregate.rs"]
mod e2e_window_aggregate;

#[path = "grouped/sql_window/e2e_window_functions.rs"]
mod e2e_window_functions;

#[path = "grouped/sql_window/e2e_within_clause.rs"]
mod e2e_within_clause;

#[path = "grouped/sql_window/e2e_within_multi_model.rs"]
mod e2e_within_multi_model;

#[path = "grouped/sql_window/window_perf_smoke.rs"]
mod window_perf_smoke;
