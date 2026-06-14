#![allow(dead_code)]

#[path = "grouped/runtime_persistence/e2e_issue_795_components_tvf.rs"]
mod e2e_issue_795_components_tvf;

#[path = "grouped/runtime_persistence/e2e_issue_859_columnar_chunk_eviction.rs"]
mod e2e_issue_859_columnar_chunk_eviction;

#[path = "grouped/runtime_persistence/e2e_fold_pager_meta_policy.rs"]
mod e2e_fold_pager_meta_policy;

#[path = "grouped/runtime_persistence/e2e_fold_dwb_into_wal_policy.rs"]
mod e2e_fold_dwb_into_wal_policy;

#[path = "grouped/runtime_persistence/e2e_materialized_view_refresh_every.rs"]
mod e2e_materialized_view_refresh_every;

#[path = "grouped/runtime_persistence/e2e_meta_json_sidecar_policy.rs"]
mod e2e_meta_json_sidecar_policy;

#[path = "grouped/runtime_persistence/e2e_query_audit.rs"]
mod e2e_query_audit;

#[path = "grouped/runtime_persistence/e2e_seqn_journal_policy.rs"]
mod e2e_seqn_journal_policy;

#[path = "grouped/runtime_persistence/e2e_vault_sealed_storage.rs"]
mod e2e_vault_sealed_storage;

#[path = "grouped/runtime_persistence/fold_dwb_into_wal_bench.rs"]
mod fold_dwb_into_wal_bench;

#[path = "grouped/runtime_persistence/integration_local_embedding_conformance.rs"]
mod integration_local_embedding_conformance;

#[path = "grouped/runtime_persistence/integration_persistent_grimms_scale.rs"]
mod integration_persistent_grimms_scale;

#[path = "grouped/runtime_persistence/integration_persistent_multimodel.rs"]
mod integration_persistent_multimodel;

#[path = "grouped/runtime_persistence/vault_capacity.rs"]
mod vault_capacity;

#[path = "grouped/runtime_persistence/vault_chain_recovery.rs"]
mod vault_chain_recovery;
