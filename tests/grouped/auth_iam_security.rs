//! Grouped integration-test harness for the related domain files.
//!
//! Cargo builds one linked binary per integration target. Keep the
//! original test files in `tests/` and include them here so test names
//! remain scoped by their source file while link count stays small.

#![allow(dead_code, unexpected_cfgs)]

#[path = "ai_search/support.rs"]
mod support;

#[path = "config_tier/shared.rs"]
mod config_tier_shared;

#[path = "auth_security/auth_alter_user.rs"]
mod auth_alter_user;

#[path = "auth_security/auth_tenant_isolation.rs"]
mod auth_tenant_isolation;

#[path = "auth_security/auth_tenant_jwt_claim.rs"]
mod auth_tenant_jwt_claim;

#[path = "ai_search/e2e_ask_tenant_scoped.rs"]
mod e2e_ask_tenant_scoped;

#[path = "config_tier/e2e_config_secret_ref.rs"]
mod e2e_config_secret_ref;

#[path = "config_tier/e2e_config_vault_observation.rs"]
mod e2e_config_vault_observation;

#[path = "timeseries_remaining/e2e_metrics_tenant_isolation.rs"]
mod e2e_metrics_tenant_isolation;

#[path = "tenancy_policy/e2e_rls_universal.rs"]
mod e2e_rls_universal;

#[path = "config_tier/e2e_secret_sql.rs"]
mod e2e_secret_sql;

#[path = "config_tier/e2e_kv_sql.rs"]
mod e2e_kv_sql;

#[path = "config_tier/e2e_system_config_vault.rs"]
mod e2e_system_config_vault;

#[path = "tenancy_policy/e2e_tenancy_dotted.rs"]
mod e2e_tenancy_dotted;

#[path = "tenancy_policy/e2e_tenant_auto_index.rs"]
mod e2e_tenant_auto_index;

#[path = "runtime_persistence/e2e_vault_sealed_storage.rs"]
mod e2e_vault_sealed_storage;

#[path = "http_grpc_auth/grpc_oauth_smoke.rs"]
mod grpc_oauth_smoke;

#[path = "http_grpc_auth/grpc_tls_smoke.rs"]
mod grpc_tls_smoke;

#[path = "http_grpc_auth/http_oauth_smoke.rs"]
mod http_oauth_smoke;

#[path = "../http_tls_limiter.rs"]
mod http_tls_limiter;

#[path = "http_grpc_auth/http_tls_smoke.rs"]
mod http_tls_smoke;

#[path = "auth_security/iam_grant_compat.rs"]
mod iam_grant_compat;

#[path = "../iam_policy_ai_provider_gate.rs"]
mod iam_policy_ai_provider_gate;

#[path = "auth_security/iam_policy_conformance.rs"]
mod iam_policy_conformance;

#[path = "auth_security/iam_policy_coverage_matrix.rs"]
mod iam_policy_coverage_matrix;

#[path = "auth_security/iam_policy_evaluator.rs"]
mod iam_policy_evaluator;

#[path = "auth_security/iam_policy_http.rs"]
mod iam_policy_http;

#[path = "auth_security/iam_policy_http_collections.rs"]
mod iam_policy_http_collections;

#[path = "auth_security/iam_policy_http_ops.rs"]
mod iam_policy_http_ops;

#[path = "auth_security/iam_policy_migrate_mode.rs"]
mod iam_policy_migrate_mode;

#[path = "auth_security/iam_policy_property.rs"]
mod iam_policy_property;

#[path = "../iam_policy_runtime.rs"]
mod iam_policy_runtime;

#[path = "auth_security/iam_policy_sql.rs"]
mod iam_policy_sql;

#[path = "http_grpc_auth/oauth_jwks_server.rs"]
mod oauth_jwks_server;

#[path = "redwire_protocol/redwire_oauth_e2e.rs"]
mod redwire_oauth_e2e;

#[path = "redwire_protocol/redwire_oauth_smoke.rs"]
mod redwire_oauth_smoke;

#[path = "auth_security/sql_grant_revoke.rs"]
mod sql_grant_revoke;

#[path = "auth_security/sql_privilege_check.rs"]
mod sql_privilege_check;

#[path = "runtime_persistence/vault_capacity.rs"]
mod vault_capacity;

#[path = "runtime_persistence/vault_chain_recovery.rs"]
mod vault_chain_recovery;
