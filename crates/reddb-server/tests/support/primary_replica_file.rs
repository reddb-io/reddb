use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

use reddb_server::replication::primary::PrimaryReplication;
use reddb_server::RedDBRuntime;

/// Auto-cleaning data path for primary/replica file tests.
///
/// Holds the backing [`tempfile::TempDir`] guard so the temp directory and
/// every WAL/sidecar artifact under it are removed on drop — including when a
/// test panics. The value derefs/coerces to a `&Path`, so callers can keep
/// passing `&data_path` where a `&Path` (or `&OsStr`, or `Into<PathBuf>`) is
/// expected and the directory still lives for the whole test.
pub struct TempDataPath {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl TempDataPath {
    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl std::ops::Deref for TempDataPath {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TempDataPath {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<OsStr> for TempDataPath {
    fn as_ref(&self) -> &OsStr {
        self.path.as_os_str()
    }
}

// `From<&TempDataPath> for PathBuf` is provided by std's blanket
// `impl<T: AsRef<OsStr>> From<&T> for PathBuf` via the `AsRef<OsStr>` impl above,
// so an explicit impl would conflict (E0119).

pub fn temp_data_path(name: &str) -> TempDataPath {
    let dir = tempfile::Builder::new()
        .prefix(&format!("reddb-test-{name}-"))
        .tempdir()
        .expect("temp dir");
    let path = dir.path().join(format!("{name}.rdb"));
    TempDataPath { _dir: dir, path }
}

#[allow(dead_code)]
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
