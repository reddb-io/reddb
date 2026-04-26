//! Local filesystem backend (default).

use super::{BackendError, BackendObjectVersion, ConditionalDelete, ConditionalPut, RemoteBackend};
use crate::crypto;
use fs2::FileExt;
use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Local filesystem backend. Copies files between paths.
/// This is the default backend -- operates entirely on local disk.
pub struct LocalBackend;

static LOCAL_UPLOAD_TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn local_version_for(path: &Path) -> Result<Option<BackendObjectVersion>, BackendError> {
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(path)
        .map_err(|e| BackendError::Transport(format!("read for version failed: {e}")))?;
    let hash = hex::encode(crypto::sha256::sha256(&bytes));
    Ok(Some(BackendObjectVersion::new(format!(
        "sha256:{}:len:{}",
        hash,
        bytes.len()
    ))))
}

fn lock_path_for(dest: &Path) -> PathBuf {
    let file_name = dest
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("object");
    dest.with_file_name(format!(".{file_name}.cas.lock"))
}

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
        let file_name = dest
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("object");
        let unique = LOCAL_UPLOAD_TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp = dest.with_file_name(format!(".{file_name}.tmp-{}-{unique}", std::process::id()));

        let copy_result = fs::copy(local_path, &temp)
            .map_err(|e| BackendError::Transport(format!("copy failed: {e}")));
        if let Err(err) = copy_result {
            let _ = fs::remove_file(&temp);
            return Err(err);
        }
        fs::File::open(&temp)
            .and_then(|file| file.sync_all())
            .map_err(|e| BackendError::Transport(format!("sync failed: {e}")))?;
        fs::rename(&temp, dest).map_err(|e| {
            let _ = fs::remove_file(&temp);
            BackendError::Transport(format!("rename failed: {e}"))
        })?;
        if let Some(parent) = dest.parent() {
            let _ = fs::File::open(parent).and_then(|dir| dir.sync_all());
        }
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

    fn supports_conditional_writes(&self) -> bool {
        true
    }

    fn object_version(
        &self,
        remote_key: &str,
    ) -> Result<Option<BackendObjectVersion>, BackendError> {
        local_version_for(Path::new(remote_key))
    }

    fn upload_conditional(
        &self,
        local_path: &Path,
        remote_key: &str,
        condition: ConditionalPut,
    ) -> Result<BackendObjectVersion, BackendError> {
        let dest = Path::new(remote_key);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| BackendError::Transport(format!("mkdir failed: {e}")))?;
        }
        let lock_path = lock_path_for(dest);
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|e| BackendError::Transport(format!("open CAS lock failed: {e}")))?;
        lock_file
            .lock_exclusive()
            .map_err(|e| BackendError::Transport(format!("lock CAS file failed: {e}")))?;

        let observed = local_version_for(dest)?;
        let allowed = match (&condition, &observed) {
            (ConditionalPut::IfAbsent, None) => true,
            (ConditionalPut::IfAbsent, Some(_)) => false,
            (ConditionalPut::IfVersion(expected), Some(actual)) => expected == actual,
            (ConditionalPut::IfVersion(_), None) => false,
        };
        if !allowed {
            let _ = lock_file.unlock();
            return Err(BackendError::PreconditionFailed(format!(
                "local object '{}' changed before conditional upload",
                remote_key
            )));
        }

        let upload_result = self.upload(local_path, remote_key);
        let version_result = upload_result.and_then(|_| {
            local_version_for(dest)?.ok_or_else(|| {
                BackendError::Internal(format!(
                    "local object '{}' missing after conditional upload",
                    remote_key
                ))
            })
        });
        let unlock_result = lock_file
            .unlock()
            .map_err(|e| BackendError::Transport(format!("unlock CAS file failed: {e}")));
        match (version_result, unlock_result) {
            (Ok(version), Ok(())) => Ok(version),
            (Err(err), _) => Err(err),
            (Ok(_), Err(err)) => Err(err),
        }
    }

    fn delete_conditional(
        &self,
        remote_key: &str,
        condition: ConditionalDelete,
    ) -> Result<(), BackendError> {
        let dest = Path::new(remote_key);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| BackendError::Transport(format!("mkdir failed: {e}")))?;
        }
        let lock_path = lock_path_for(dest);
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&lock_path)
            .map_err(|e| BackendError::Transport(format!("open CAS lock failed: {e}")))?;
        lock_file
            .lock_exclusive()
            .map_err(|e| BackendError::Transport(format!("lock CAS file failed: {e}")))?;

        let observed = local_version_for(dest)?;
        let allowed = match (&condition, &observed) {
            (ConditionalDelete::IfVersion(expected), Some(actual)) => expected == actual,
            (ConditionalDelete::IfVersion(_), None) => false,
        };
        if !allowed {
            let _ = lock_file.unlock();
            return Err(BackendError::PreconditionFailed(format!(
                "local object '{}' changed before conditional delete",
                remote_key
            )));
        }

        let delete_result = self.delete(remote_key);
        let unlock_result = lock_file
            .unlock()
            .map_err(|e| BackendError::Transport(format!("unlock CAS file failed: {e}")));
        match (delete_result, unlock_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(err), _) => Err(err),
            (Ok(()), Err(err)) => Err(err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "reddb-local-backend-test-{}-{}-{}",
            name,
            std::process::id(),
            crate::utils::now_unix_nanos()
        ))
    }

    #[test]
    fn conditional_create_if_absent_rejects_existing_object() {
        let backend = LocalBackend;
        let src = temp_file("src");
        let remote = temp_file("remote");
        fs::write(&src, b"first").unwrap();

        let first = backend
            .upload_conditional(&src, remote.to_str().unwrap(), ConditionalPut::IfAbsent)
            .unwrap();
        assert!(first.token.contains("sha256:"));

        fs::write(&src, b"second").unwrap();
        let err = backend
            .upload_conditional(&src, remote.to_str().unwrap(), ConditionalPut::IfAbsent)
            .unwrap_err();
        assert!(matches!(err, BackendError::PreconditionFailed(_)));

        let _ = fs::remove_file(src);
        let _ = fs::remove_file(remote);
    }

    #[test]
    fn conditional_replace_rejects_stale_version() {
        let backend = LocalBackend;
        let src = temp_file("src");
        let remote = temp_file("remote");
        fs::write(&src, b"first").unwrap();
        let stale = backend
            .upload_conditional(&src, remote.to_str().unwrap(), ConditionalPut::IfAbsent)
            .unwrap();

        fs::write(&src, b"second").unwrap();
        let fresh = backend
            .upload_conditional(
                &src,
                remote.to_str().unwrap(),
                ConditionalPut::IfVersion(stale.clone()),
            )
            .unwrap();

        fs::write(&src, b"third").unwrap();
        let err = backend
            .upload_conditional(
                &src,
                remote.to_str().unwrap(),
                ConditionalPut::IfVersion(stale),
            )
            .unwrap_err();
        assert!(matches!(err, BackendError::PreconditionFailed(_)));
        assert_eq!(
            fresh.token,
            backend
                .object_version(remote.to_str().unwrap())
                .unwrap()
                .unwrap()
                .token
        );

        let _ = fs::remove_file(src);
        let _ = fs::remove_file(remote);
    }

    #[test]
    fn conditional_delete_rejects_stale_version() {
        let backend = LocalBackend;
        let src = temp_file("src");
        let remote = temp_file("remote");
        fs::write(&src, b"first").unwrap();
        let stale = backend
            .upload_conditional(&src, remote.to_str().unwrap(), ConditionalPut::IfAbsent)
            .unwrap();

        fs::write(&src, b"second").unwrap();
        let fresh = backend
            .upload_conditional(
                &src,
                remote.to_str().unwrap(),
                ConditionalPut::IfVersion(stale.clone()),
            )
            .unwrap();

        let err = backend
            .delete_conditional(
                remote.to_str().unwrap(),
                ConditionalDelete::IfVersion(stale),
            )
            .unwrap_err();
        assert!(matches!(err, BackendError::PreconditionFailed(_)));
        backend
            .delete_conditional(
                remote.to_str().unwrap(),
                ConditionalDelete::IfVersion(fresh),
            )
            .unwrap();
        assert!(!remote.exists());

        let _ = fs::remove_file(src);
    }
}
