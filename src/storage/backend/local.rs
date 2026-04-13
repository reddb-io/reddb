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

    fn list(&self, prefix: &str) -> Result<Vec<String>, BackendError> {
        let prefix_path = Path::new(prefix);
        let mut results = Vec::new();

        if prefix_path.is_dir() {
            fn walk(dir: &Path, out: &mut Vec<String>) -> Result<(), BackendError> {
                for entry in fs::read_dir(dir)
                    .map_err(|e| BackendError::Transport(format!("read_dir failed: {e}")))?
                {
                    let entry = entry
                        .map_err(|e| BackendError::Transport(format!("dir entry failed: {e}")))?;
                    let path = entry.path();
                    if path.is_dir() {
                        walk(&path, out)?;
                    } else if path.is_file() {
                        out.push(path.to_string_lossy().to_string());
                    }
                }
                Ok(())
            }

            walk(prefix_path, &mut results)?;
        } else {
            let parent = prefix_path.parent().unwrap_or_else(|| Path::new("."));
            let needle = prefix_path.to_string_lossy().to_string();
            if parent.exists() {
                for entry in fs::read_dir(parent)
                    .map_err(|e| BackendError::Transport(format!("read_dir failed: {e}")))?
                {
                    let entry = entry
                        .map_err(|e| BackendError::Transport(format!("dir entry failed: {e}")))?;
                    let path = entry.path();
                    if path.is_file() {
                        let candidate = path.to_string_lossy().to_string();
                        if candidate.starts_with(&needle) {
                            results.push(candidate);
                        }
                    }
                }
            }
        }

        results.sort();
        Ok(results)
    }
}
