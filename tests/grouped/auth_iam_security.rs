//! Grouped integration-test harness for the related domain files.
//!
//! Cargo builds one linked binary per integration target. Keep the
//! original test files in `tests/` and include them here so test names
//! remain scoped by their source file while link count stays small.

#![allow(dead_code, unexpected_cfgs)]

#[path = "../auth_alter_user.rs"]
mod auth_alter_user;

#[path = "../auth_tenant_isolation.rs"]
mod auth_tenant_isolation;

#[path = "../auth_tenant_jwt_claim.rs"]
mod auth_tenant_jwt_claim;

#[path = "../e2e_ask_tenant_scoped.rs"]
mod e2e_ask_tenant_scoped;

#[path = "../e2e_config_secret_ref.rs"]
mod e2e_config_secret_ref;

#[path = "../e2e_config_vault_observation.rs"]
mod e2e_config_vault_observation;

#[path = "../e2e_metrics_tenant_isolation.rs"]
mod e2e_metrics_tenant_isolation;

#[path = "../e2e_rls_universal.rs"]
mod e2e_rls_universal;

#[path = "../e2e_secret_sql.rs"]
mod e2e_secret_sql;

#[path = "../e2e_system_config_vault.rs"]
mod e2e_system_config_vault;

#[path = "../e2e_tenancy_dotted.rs"]
mod e2e_tenancy_dotted;

#[path = "../e2e_tenant_auto_index.rs"]
mod e2e_tenant_auto_index;

#[path = "../e2e_vault_sealed_storage.rs"]
mod e2e_vault_sealed_storage;

#[path = "../grpc_oauth_smoke.rs"]
mod grpc_oauth_smoke;

#[path = "../grpc_tls_smoke.rs"]
mod grpc_tls_smoke;

#[path = "../http_oauth_smoke.rs"]
mod http_oauth_smoke;

#[path = "../http_tls_limiter.rs"]
mod http_tls_limiter;

#[path = "../http_tls_smoke.rs"]
mod http_tls_smoke;

#[path = "../iam_grant_compat.rs"]
mod iam_grant_compat;

#[path = "../iam_policy_ai_provider_gate.rs"]
mod iam_policy_ai_provider_gate;

#[path = "../iam_policy_conformance.rs"]
mod iam_policy_conformance;

#[path = "../iam_policy_coverage_matrix.rs"]
mod iam_policy_coverage_matrix;

#[path = "../iam_policy_evaluator.rs"]
mod iam_policy_evaluator;

#[path = "../iam_policy_http.rs"]
mod iam_policy_http;

#[path = "../iam_policy_http_collections.rs"]
mod iam_policy_http_collections;

#[path = "../iam_policy_http_ops.rs"]
mod iam_policy_http_ops;

#[path = "../iam_policy_migrate_mode.rs"]
mod iam_policy_migrate_mode;

#[path = "../iam_policy_property.rs"]
mod iam_policy_property;

#[path = "../iam_policy_runtime.rs"]
mod iam_policy_runtime;

#[path = "../iam_policy_sql.rs"]
mod iam_policy_sql;

#[path = "../oauth_jwks_server.rs"]
mod oauth_jwks_server;

#[path = "../redwire_oauth_e2e.rs"]
mod redwire_oauth_e2e;

#[path = "../redwire_oauth_smoke.rs"]
mod redwire_oauth_smoke;

#[path = "../sql_grant_revoke.rs"]
mod sql_grant_revoke;

#[path = "../sql_privilege_check.rs"]
mod sql_privilege_check;

#[path = "../vault_capacity.rs"]
mod vault_capacity;

#[path = "../vault_chain_recovery.rs"]
mod vault_chain_recovery;
