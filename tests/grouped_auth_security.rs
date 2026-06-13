//! Grouped integration-test harness for auth, tenant, and IAM SQL coverage.
//!
//! Cargo links one binary per top-level integration-test file. Keep the
//! original domain tests as modules under `tests/grouped/` so their function
//! names remain visible while this target replaces multiple linked binaries.

#![allow(dead_code)]

#[path = "grouped/auth_security/auth_alter_user.rs"]
mod auth_alter_user;

#[path = "grouped/auth_security/auth_tenant_isolation.rs"]
mod auth_tenant_isolation;

#[path = "grouped/auth_security/auth_tenant_jwt_claim.rs"]
mod auth_tenant_jwt_claim;

#[path = "grouped/auth_security/iam_policy_sql.rs"]
mod iam_policy_sql;

#[path = "grouped/auth_security/iam_grant_compat.rs"]
mod iam_grant_compat;

#[path = "grouped/auth_security/iam_policy_conformance.rs"]
mod iam_policy_conformance;

#[path = "grouped/auth_security/iam_policy_coverage_matrix.rs"]
mod iam_policy_coverage_matrix;

#[path = "grouped/auth_security/iam_policy_evaluator.rs"]
mod iam_policy_evaluator;

#[path = "grouped/auth_security/iam_policy_http.rs"]
mod iam_policy_http;

#[path = "grouped/auth_security/iam_policy_http_collections.rs"]
mod iam_policy_http_collections;

#[path = "grouped/auth_security/iam_policy_http_ops.rs"]
mod iam_policy_http_ops;

#[path = "grouped/auth_security/iam_policy_migrate_mode.rs"]
mod iam_policy_migrate_mode;

#[path = "grouped/auth_security/iam_policy_property.rs"]
mod iam_policy_property;

#[path = "grouped/auth_security/sql_grant_revoke.rs"]
mod sql_grant_revoke;

#[path = "grouped/auth_security/sql_privilege_check.rs"]
mod sql_privilege_check;
