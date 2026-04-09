//! Local filesystem backend (default).

use super::{BackendError, RemoteBackend};
use std::fs;
use std::path::Path;

/// Local filesystem backend. Copies files between paths.
/// This is the default backend -- operates entirely on local disk.
pub struct LocalBackend;

impl RemoteBackend for LocalBackend {
    fn name(&self) -> &str {
        "local"
    }

    fn download(&self, remote_key: &str, local_path: &Path) -> Result<bool, BackendError> {
        let source = Path::new(remote_key);
        if !source.exists() {
            return Ok(false);
        }
        fs::copy(source, local_path)
            .map_err(|e| BackendError::Transport(format!("copy failed: {e}")))?;
        Ok(true)
    }

    fn upload(&self, local_path: &Path, remote_key: &str) -> Result<(), BackendError> {
        let dest = Path::new(remote_key);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| BackendError::Transport(format!("mkdir failed: {e}")))?;
        }
        fs::copy(local_path, dest)
            .map_err(|e| BackendError::Transport(format!("copy failed: {e}")))?;
        Ok(())
    }

    fn exists(&self, remote_key: &str) -> Result<bool, BackendError> {
        Ok(Path::new(remote_key).exists())
    }

    fn delete(&self, remote_key: &str) -> Result<(), BackendError> {
        let path = Path::new(remote_key);
        if path.exists() {
            fs::remove_file(path)
                .map_err(|e| BackendError::Transport(format!("delete failed: {e}")))?;
        }
        Ok(())
    }
}
