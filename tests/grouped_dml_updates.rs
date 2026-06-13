#![allow(dead_code)]

#[path = "grouped/dml_updates/e2e_compound_assignment.rs"]
mod e2e_compound_assignment;

#[path = "grouped/dml_updates/e2e_explicit_update_targets.rs"]
mod e2e_explicit_update_targets;

#[path = "grouped/dml_updates/e2e_hot_update.rs"]
mod e2e_hot_update;

#[path = "grouped/dml_updates/e2e_ordered_multimodel_update_batches.rs"]
mod e2e_ordered_multimodel_update_batches;

#[path = "grouped/dml_updates/e2e_ordered_row_update_batches.rs"]
mod e2e_ordered_row_update_batches;

#[path = "grouped/dml_updates/e2e_returning.rs"]
mod e2e_returning;

#[path = "grouped/dml_updates/unit_hot_update.rs"]
mod unit_hot_update;
