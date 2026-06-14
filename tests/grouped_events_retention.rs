#![allow(dead_code)]

#[path = "grouped/events_retention/e2e_events_backfill.rs"]
mod e2e_events_backfill;

#[path = "grouped/events_retention/e2e_collection_retention_policy.rs"]
mod e2e_collection_retention_policy;

#[path = "grouped/events_retention/e2e_events_cdc_rid.rs"]
mod e2e_events_cdc_rid;

#[path = "grouped/events_retention/e2e_events_foundation.rs"]
mod e2e_events_foundation;

#[path = "grouped/events_retention/e2e_retention_sweeper.rs"]
mod e2e_retention_sweeper;
