#![allow(dead_code)]

#[path = "grouped/config_tier/e2e_config_crud.rs"]
mod e2e_config_crud;

#[path = "grouped/config_tier/e2e_config_matrix.rs"]
mod e2e_config_matrix;

#[path = "grouped/config_tier/e2e_config_secret_ref.rs"]
mod e2e_config_secret_ref;

#[path = "grouped/config_tier/e2e_config_vault_observation.rs"]
mod e2e_config_vault_observation;

#[path = "grouped/config_tier/e2e_secret_sql.rs"]
mod e2e_secret_sql;

#[path = "grouped/config_tier/e2e_issue_480_tier_default_promotion_contract.rs"]
mod e2e_issue_480_tier_default_promotion_contract;

#[path = "grouped/config_tier/e2e_shm_provisioning.rs"]
mod e2e_shm_provisioning;

#[path = "grouped/config_tier/e2e_system_config_vault.rs"]
mod e2e_system_config_vault;

#[path = "grouped/config_tier/e2e_tier_wiring.rs"]
mod e2e_tier_wiring;
