use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use reddb_server::replication::primary::PrimaryReplication;
use reddb_server::RedDBRuntime;

pub fn temp_data_path(name: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("reddb_{name}_{suffix}.rdb"))
}

pub fn cleanup(data_path: &Path) {
    let _ = fs::remove_file(data_path);
    let _ = fs::remove_file(PrimaryReplication::slot_path_for(data_path));
    let _ =
        fs::remove_file(reddb_server::replication::primary::LogicalWalSpool::path_for(data_path));
    let _ = fs::remove_dir_all(PrimaryReplication::primary_replica_root_for(data_path));
    let _ = fs::remove_dir_all(reddb_file::layout::serverless_root(data_path));
    reddb_file::cleanup_rebootstrap_artifacts(data_path);
}

#[allow(dead_code)]
pub fn show_config_value(runtime: &RedDBRuntime, key: &str) -> String {
    let config = runtime
        .execute_query(&format!("SHOW CONFIG {key}"))
        .unwrap_or_else(|err| panic!("show config {key}: {err:?}"));
    config
        .result
        .records
        .first()
        .and_then(|record| record.get("value"))
        .map(|value| format!("{value:?}"))
        .unwrap_or_default()
}
