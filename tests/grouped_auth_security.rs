//! Grouped integration-test harness for auth, tenant, and IAM SQL coverage.
//!
//! Cargo links one binary per top-level integration-test file. Keep the
//! original domain tests as modules under `tests/grouped/` so their function
//! names remain visible while this slice reduces four linked binaries to one.

#![allow(dead_code)]

#[path = "grouped/auth_security/auth_alter_user.rs"]
mod auth_alter_user;

#[path = "grouped/auth_security/auth_tenant_isolation.rs"]
mod auth_tenant_isolation;

#[path = "grouped/auth_security/auth_tenant_jwt_claim.rs"]
mod auth_tenant_jwt_claim;

#[path = "grouped/auth_security/iam_policy_sql.rs"]
mod iam_policy_sql;
