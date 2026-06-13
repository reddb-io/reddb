#![allow(dead_code)]

#[path = "grouped/schema_query_core/e2e_append_only.rs"]
mod e2e_append_only;

#[path = "grouped/schema_query_core/e2e_composite_index.rs"]
mod e2e_composite_index;

#[path = "grouped/schema_query_core/e2e_index_replay.rs"]
mod e2e_index_replay;

#[path = "grouped/schema_query_core/e2e_red_collections_acceptance.rs"]
mod e2e_red_collections_acceptance;

#[path = "grouped/schema_query_core/e2e_red_schema.rs"]
mod e2e_red_schema;

#[path = "grouped/schema_query_core/e2e_rid_row_envelope.rs"]
mod e2e_rid_row_envelope;

#[path = "grouped/schema_query_core/e2e_select_range_after_index.rs"]
mod e2e_select_range_after_index;

#[path = "grouped/schema_query_core/e2e_statement_execution_contract.rs"]
mod e2e_statement_execution_contract;
