#![allow(dead_code)]

#[path = "grouped/multimodel_query/e2e_issue_751_json_patch_path_helpers.rs"]
mod e2e_issue_751_json_patch_path_helpers;

#[path = "grouped/multimodel_query/e2e_multimodel_flow.rs"]
mod e2e_multimodel_flow;

#[path = "grouped/multimodel_query/e2e_nested_queries_multimodel_json.rs"]
mod e2e_nested_queries_multimodel_json;

#[path = "grouped/multimodel_query/e2e_postgres_math_functions.rs"]
mod e2e_postgres_math_functions;

#[path = "grouped/multimodel_query/integration_create_table_partition.rs"]
mod integration_create_table_partition;

#[path = "grouped/multimodel_query/integration_entity_query.rs"]
mod integration_entity_query;
