//! Grouped integration-test harness for audit logging and routing coverage.
//!
//! Cargo links one binary per top-level integration-test file. Keep the
//! original domain tests as modules under `tests/grouped/` so their function
//! names remain visible while this slice reduces three linked binaries to one.

#![allow(dead_code)]

#[path = "grouped/audit/audit_rotation.rs"]
mod audit_rotation;

#[path = "grouped/audit/audit_structured.rs"]
mod audit_structured;

#[path = "grouped/audit/e2e_audit_slow_routing.rs"]
mod e2e_audit_slow_routing;
