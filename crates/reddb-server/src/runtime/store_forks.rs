//! Independent checkpoint forks. Only database/WAL files own durable state.
use super::*;
use crate::storage::unified::UnifiedStore;
use reddb_file::{Vfs, VfsFile};
use std::io::{Read, SeekFrom, Write};
use std::path::{Path, PathBuf};

const PREFIX: &str = "fork.entry.";
const ORIGIN: &str = "fork.origin";

#[derive(Clone, Debug, PartialEq, Eq)]
enum ForkState {
    Preparing,
    Ready,
    Dropping,
    Detaching,
    Promoting,
    Detached,
    Promoted,
}

#[derive(Clone, Debug)]
struct ForkRecord {
    name: String,
    identity: String,
    parent_store: String,
    fork_lsn: u64,
    collections_total: u64,
    state: ForkState,
}

impl crate::serde_json::JsonEncode for ForkRecord {
    fn to_json_value(&self) -> crate::serde_json::Value {
        let state = match self.state {
            ForkState::Preparing => "Preparing",
            ForkState::Ready => "Ready",
            ForkState::Dropping => "Dropping",
            ForkState::Detaching => "Detaching",
            ForkState::Promoting => "Promoting",
            ForkState::Detached => "Detached",
            ForkState::Promoted => "Promoted",
        };
        crate::json!({
            "version": "1",
            "name": self.name.clone(),
            "identity": self.identity.clone(),
            "parent_store": self.parent_store.clone(),
            "fork_lsn": self.fork_lsn.to_string(),
            "collections_total": self.collections_total.to_string(),
            "state": state
        })
    }
}

impl crate::serde_json::JsonDecode for ForkRecord {
    fn from_json_value(value: crate::serde_json::Value) -> Result<Self, String> {
        let field = |name: &str| {
            value
                .get(name)
                .and_then(|value| value.as_str())
                .ok_or_else(|| format!("invalid fork field: {name}"))
        };
        if field("version")? != "1" {
            return Err("unsupported fork record version".into());
        }
        let state = match field("state")? {
            "Preparing" => ForkState::Preparing,
            "Ready" => ForkState::Ready,
            "Dropping" => ForkState::Dropping,
            "Detaching" => ForkState::Detaching,
            "Promoting" => ForkState::Promoting,
            "Detached" => ForkState::Detached,
            "Promoted" => ForkState::Promoted,
            _ => return Err("invalid fork state".into()),
        };
        Ok(Self {
            name: field("name")?.to_string(),
            identity: field("identity")?.to_string(),
            parent_store: field("parent_store")?.to_string(),
            fork_lsn: field("fork_lsn")?
                .parse::<u64>()
                .map_err(|error| error.to_string())?,
            collections_total: field("collections_total")?
                .parse::<u64>()
                .map_err(|error| error.to_string())?,
            state,
        })
    }
}

fn failure(error: impl std::fmt::Display) -> RedDBError {
    RedDBError::InvalidOperation(error.to_string())
}

fn key(name: &str) -> RedDBResult<String> {
    if name.is_empty()
        || name.trim() != name
        || name.len() > 128
        || name.chars().any(char::is_control)
    {
        return Err(failure(
            "fork name must contain 1..128 bytes without surrounding whitespace",
        ));
    }
    Ok(format!("{PREFIX}{}", hex::encode(name)))
}

fn read_record(store: &UnifiedStore, key: &str) -> RedDBResult<Option<ForkRecord>> {
    match store.try_get_fork_metadata(key).map_err(failure)? {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Text(text)) => {
            if text.len() > 256 * 1024 {
                return Err(failure("fork control record exceeds its size limit"));
            }
            let record: ForkRecord = crate::serde_json::from_str(&text).map_err(failure)?;
            let record_key = self::key(&record.name)?;
            if record.identity.len() != 32
                || !record.identity.bytes().all(|byte| byte.is_ascii_hexdigit())
                || (key != ORIGIN && record_key != key)
            {
                return Err(failure("invalid internal fork identity"));
            }
            Ok(Some(record))
        }
        Some(_) => Err(failure("invalid internal fork record")),
    }
}

fn write_record(store: &UnifiedStore, key: &str, record: Option<&ForkRecord>) -> RedDBResult<()> {
    if UnifiedStore::deferred_store_wal_capture_active() {
        return Err(failure(
            "store fork operations cannot join an uncommitted transaction",
        ));
    }
    let value = match record {
        Some(record) => {
            crate::serde_json::Value::String(crate::serde_json::to_string(record).map_err(failure)?)
        }
        None => crate::serde_json::Value::Null,
    };
    store.try_set_fork_metadata(key, &value).map_err(failure)?;
    store.sync_control_metadata().map_err(failure)
}

fn records(store: &UnifiedStore) -> RedDBResult<Vec<ForkRecord>> {
    let Some(manager) = store.get_collection("red.storage.forks") else {
        return Ok(Vec::new());
    };
    let mut keys = std::collections::BTreeSet::new();
    manager
        .try_for_each_entity(usize::MAX, |entity| {
            if let EntityData::Row(row) = &entity.data {
                if let Some(Value::Text(key)) = row.get_field("key") {
                    if key.starts_with(PREFIX) {
                        keys.insert(key.to_string());
                    }
                }
            }
            true
        })
        .map_err(failure)?;
    keys.into_iter()
        .filter_map(|key| read_record(store, &key).transpose())
        .collect()
}

fn fork_path(parent: &Path, record: &ForkRecord) -> PathBuf {
    parent.with_file_name(format!("fork-{}.rdb", record.identity))
}

fn sync_directory(path: &Path) -> RedDBResult<()> {
    reddb_file::StdVfs
        .sync_dir(
            path.parent()
                .filter(|parent| !parent.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
        )
        .map_err(failure)
}

fn crash_point(point: &str) {
    if std::env::var("REDDB_STORE_FORK_CRASH_AT").ok().as_deref() == Some(point) {
        std::process::exit(173);
    }
}

impl ForkRecord {
    fn info(&self) -> reddb_file::ForkInfo {
        reddb_file::ForkInfo {
            name: self.name.clone(),
            parent_store: self.parent_store.clone(),
            fork_lsn: self.fork_lsn,
            hydration_state: reddb_file::ForkHydrationState::Hydrated,
            collections_total: self.collections_total,
            shared_by_reference: 0,
            hydrating: 0,
            hydrated: self.collections_total,
        }
    }
}

impl RedDBRuntime {
    fn fork_parent_path(&self) -> RedDBResult<&Path> {
        let options = self.inner.db.options();
        if options.mode != crate::api::StorageMode::Persistent
            || options
                .metadata
                .get(crate::api::EPHEMERAL_RUNTIME_METADATA_KEY)
                .is_some_and(|value| value == "true")
        {
            return Err(failure(
                "store forks require an explicitly persistent database",
            ));
        }
        self.inner
            .db
            .path()
            .ok_or_else(|| failure("store forks require a persistent database"))
    }

    fn fork_options(&self, path: &Path) -> RedDBOptions {
        let mut options = self.inner.db.options().clone();
        options.data_path = Some(path.to_path_buf());
        options.read_only = false;
        options.create_if_missing = false;
        options.force_create = false;
        options.remote_backend = None;
        options.remote_backend_atomic = None;
        options.remote_key = None;
        options.replica_loop_enabled = false;
        options.replication = crate::replication::ReplicationConfig::standalone();
        options.durability_mode = crate::api::DurabilityMode::Strict;
        options
    }

    /// Each returned fork is a complete independent database, never an external manifest.
    pub fn list_store_forks(&self) -> RedDBResult<Vec<reddb_file::ForkInfo>> {
        let store = self.inner.db.store();
        let _publication = store.vault_publication_lock.lock();
        let mut forks: Vec<_> = records(&store)?
            .into_iter()
            .filter(|record| record.state == ForkState::Ready)
            .map(|record| record.info())
            .collect();
        // Legacy listings are read-only compatibility; never publish a manifest.
        if let Some(path) = self.inner.db.path() {
            forks.extend(
                store
                    .operational_manifest_for_path(path)
                    .list_forks()
                    .map_err(failure)?,
            );
        }
        forks.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(forks)
    }

    /// Also resolves detached/promoted destinations retained in the parent catalog.
    pub fn fork_store_path(&self, name: &str) -> RedDBResult<Option<PathBuf>> {
        let parent = self.fork_parent_path()?;
        Ok(read_record(&self.inner.db.store(), &key(name)?)?
            .filter(|record| {
                matches!(
                    record.state,
                    ForkState::Ready | ForkState::Detached | ForkState::Promoted
                )
            })
            .map(|record| fork_path(parent, &record)))
    }

    pub fn fork_store(&self, name: &str) -> RedDBResult<reddb_file::ForkInfo> {
        self.fork_store_at_lsn(name, None)
    }

    pub(super) fn fork_store_at_lsn(
        &self,
        name: &str,
        at_lsn: Option<u64>,
    ) -> RedDBResult<reddb_file::ForkInfo> {
        self.check_write(crate::runtime::write_gate::WriteKind::Ddl)?;
        let key = key(name)?;
        let parent = self.fork_parent_path()?;
        let store = self.inner.db.store();
        let _publication = store.vault_publication_lock.lock();
        if self.current_xid().is_some() || UnifiedStore::deferred_store_wal_capture_active() {
            return Err(failure(
                "store forks require an independent committed checkpoint",
            ));
        }
        let fork_lsn = self.primary_logical_head_lsn().max(self.cdc_current_lsn());
        if at_lsn.is_some_and(|lsn| lsn != fork_lsn) {
            return Err(failure("FORK STORE AT LSN requires the current committed LSN; historical points require a retained backup and WAL restore"));
        }
        self.recover_store_forks()?;
        if read_record(&store, &key)?.is_some_and(|record| {
            !matches!(record.state, ForkState::Detached | ForkState::Promoted)
        }) || store
            .operational_manifest_for_path(parent)
            .list_forks()
            .map_err(failure)?
            .iter()
            .any(|fork| fork.name == name)
        {
            return Err(failure(format!("store fork already exists: {name}")));
        }
        let mut record = ForkRecord {
            name: name.to_string(),
            identity: hex::encode(crate::auth::store::random_bytes(16)),
            parent_store: parent.to_string_lossy().into_owned(),
            fork_lsn,
            collections_total: store.list_collections().len() as u64,
            state: ForkState::Preparing,
        };
        let destination = fork_path(parent, &record);
        if destination.try_exists().map_err(failure)? {
            return Err(failure("fork destination already exists"));
        }
        write_record(&store, &key, Some(&record))?;
        crash_point("create_intent");
        self.checkpoint_local()?;
        let mut create = std::fs::OpenOptions::new();
        create.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            create.mode(0o600);
        }
        let mut output = create.open(&destination).map_err(failure)?;
        if let Some(pager) = store.pager() {
            pager
                .with_file(|source| -> std::io::Result<()> {
                    source.seek(SeekFrom::Start(0))?;
                    let mut remaining = source.file_len()?;
                    let mut buffer = vec![0; 1024 * 1024];
                    while remaining != 0 {
                        let count = remaining.min(buffer.len() as u64) as usize;
                        source.read_exact(&mut buffer[..count])?;
                        output.write_all(&buffer[..count])?;
                        remaining -= count as u64;
                        crash_point("create_copy");
                    }
                    Ok(())
                })
                .map_err(failure)?
                .map_err(failure)?;
        } else {
            let mut source = std::fs::File::open(parent).map_err(failure)?;
            let mut buffer = vec![0; 1024 * 1024];
            loop {
                let count = source.read(&mut buffer).map_err(failure)?;
                if count == 0 {
                    break;
                }
                output.write_all(&buffer[..count]).map_err(failure)?;
                crash_point("create_copy");
            }
        }
        output.sync_all().map_err(failure)?;
        drop(output);
        sync_directory(&destination)?;
        let fork = crate::storage::RedDB::open_with_options(&self.fork_options(&destination))
            .map_err(failure)?;
        for inherited in records(&fork.store())? {
            write_record(&fork.store(), &self::key(&inherited.name)?, None)?;
        }
        // A clone owns its data, not the parent's replication slots or exported files.
        let fork_store = fork.store();
        if let Some(metadata) = fork_store.get_collection("red.storage.metadata") {
            let mut inherited = Vec::new();
            metadata.try_for_each_entity(usize::MAX, |entity| {
                if let EntityData::Row(row) = &entity.data {
                    if matches!(row.get_field("key"), Some(Value::Text(key)) if key.starts_with("replication.")) {
                        inherited.push(entity.id);
                    }
                }
                true
            }).map_err(failure)?;
            fork_store
                .delete_batch("red.storage.metadata", &inherited)
                .map_err(failure)?;
        }
        if fork_store
            .try_get_storage_metadata("physical")
            .map_err(failure)?
            .is_some()
        {
            fork.update_physical_metadata(|metadata| metadata.exports.clear())
                .map_err(failure)?;
        }
        record.state = ForkState::Ready;
        write_record(&fork.store(), ORIGIN, Some(&record))?;
        crash_point("create_origin");
        fork.flush_local_only().map_err(failure)?;
        drop(fork);
        crash_point("create_snapshot");
        write_record(&store, &key, Some(&record))?;
        crash_point("create_ready");
        Ok(record.info())
    }

    pub fn detach_fork_store(&self, name: &str) -> RedDBResult<bool> {
        self.release_fork(name, ForkState::Detached)
            .map(|path| path.is_some())
    }

    /// Returns the independent database to open as the replacement primary.
    /// The serving parent remains intact; switching a server requires reopening this path.
    pub fn promote_fork_store(&self, name: &str) -> RedDBResult<PathBuf> {
        self.release_fork(name, ForkState::Promoted)?
            .ok_or_else(|| RedDBError::NotFound(format!("store fork '{name}'")))
    }

    fn release_fork(&self, name: &str, state: ForkState) -> RedDBResult<Option<PathBuf>> {
        self.check_write(crate::runtime::write_gate::WriteKind::Ddl)?;
        if self.current_xid().is_some() {
            return Err(failure(
                "store forks cannot join an uncommitted transaction",
            ));
        }
        let store = self.inner.db.store();
        let _publication = store.vault_publication_lock.lock();
        let key = key(name)?;
        let Some(mut record) = read_record(&store, &key)? else {
            return Ok(None);
        };
        if record.state == ForkState::Promoted && state == ForkState::Promoted {
            return Ok(Some(fork_path(self.fork_parent_path()?, &record)));
        }
        if record.state != ForkState::Ready {
            return Ok(None);
        }
        let path = fork_path(self.fork_parent_path()?, &record);
        let fork =
            crate::storage::RedDB::open_with_options(&self.fork_options(&path)).map_err(failure)?;
        let origin =
            read_record(&fork.store(), ORIGIN)?.ok_or_else(|| failure("fork origin is missing"))?;
        if origin.identity != record.identity {
            return Err(failure("fork identity differs from its catalog"));
        }
        // The parent publishes release first. On interruption the owned file remains
        // a valid database; the restartable second phase clears its origin.
        record.state = if state == ForkState::Promoted {
            ForkState::Promoting
        } else {
            ForkState::Detaching
        };
        write_record(&store, &key, Some(&record))?;
        crash_point("release_intent");
        write_record(&fork.store(), ORIGIN, None)?;
        fork.flush_local_only().map_err(failure)?;
        crash_point("release_ready");
        record.state = state;
        write_record(&store, &key, Some(&record))?;
        Ok(Some(path))
    }

    pub fn drop_fork_store(&self, name: &str) -> RedDBResult<bool> {
        self.check_write(crate::runtime::write_gate::WriteKind::Ddl)?;
        if self.current_xid().is_some() {
            return Err(failure(
                "store forks cannot join an uncommitted transaction",
            ));
        }
        let store = self.inner.db.store();
        let _publication = store.vault_publication_lock.lock();
        let key = key(name)?;
        let Some(mut record) = read_record(&store, &key)? else {
            return Ok(false);
        };
        if !matches!(record.state, ForkState::Ready | ForkState::Dropping) {
            return Ok(false);
        }
        let path = fork_path(self.fork_parent_path()?, &record);
        if record.state == ForkState::Ready || path.try_exists().map_err(failure)? {
            self.verify_fork_origin(&path, &record)?;
        }
        record.state = ForkState::Dropping;
        write_record(&store, &key, Some(&record))?;
        crash_point("drop_intent");
        self.remove_owned_fork(&record)?;
        write_record(&store, &key, None)?;
        crash_point("drop_ready");
        Ok(true)
    }

    fn verify_fork_origin(&self, path: &Path, record: &ForkRecord) -> RedDBResult<()> {
        let mut options = self.fork_options(path);
        options.read_only = true;
        let fork = crate::storage::RedDB::open_with_options(&options).map_err(failure)?;
        let origin =
            read_record(&fork.store(), ORIGIN)?.ok_or_else(|| failure("fork origin is missing"))?;
        if origin.identity != record.identity {
            return Err(failure("fork identity differs from its catalog"));
        }
        Ok(())
    }

    fn remove_owned_fork(&self, record: &ForkRecord) -> RedDBResult<()> {
        let path = fork_path(self.fork_parent_path()?, record);
        let lock = if self.inner.embedded_single_file && self.inner.db.store().pager().is_none() {
            Some(reddb_file::EmbeddedRdbWriterLock::acquire(&path).map_err(failure)?)
        } else {
            None
        };
        let file = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
        {
            Ok(file) => Some(file),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(failure(error)),
        };
        if lock.is_none() {
            if let Some(file) = &file {
                fs2::FileExt::try_lock_exclusive(file).map_err(failure)?;
            }
        }
        for owned in [
            reddb_file::unified_wal_path(&path),
            reddb_file::engine_wal_path(&path),
            path.clone(),
        ] {
            match std::fs::symlink_metadata(&owned) {
                Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                    if owned == path {
                        crash_point("drop_wal");
                    }
                    std::fs::remove_file(&owned).map_err(failure)?;
                }
                Ok(_) => return Err(failure("fork artifact is not a regular file")),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(failure(error)),
            }
        }
        sync_directory(&path)?;
        drop(file);
        drop(lock);
        Ok(())
    }

    pub(super) fn recover_store_forks(&self) -> RedDBResult<()> {
        if self.inner.db.options().read_only || self.inner.db.path().is_none() {
            return Ok(());
        }
        let store = self.inner.db.store();
        let _publication = store.vault_publication_lock.lock();
        for mut record in records(&store)? {
            match record.state {
                ForkState::Preparing => {
                    let path = fork_path(self.fork_parent_path()?, &record);
                    let complete = if path.try_exists().map_err(failure)? {
                        let mut options = self.fork_options(&path);
                        options.read_only = true;
                        match crate::storage::RedDB::open_with_options(&options) {
                            Ok(fork) => read_record(&fork.store(), ORIGIN)?
                                .is_some_and(|origin| origin.identity == record.identity),
                            Err(_) => false,
                        }
                    } else {
                        false
                    };
                    if complete {
                        record.state = ForkState::Ready;
                        write_record(&store, &key(&record.name)?, Some(&record))?;
                    } else {
                        self.remove_owned_fork(&record)?;
                        write_record(&store, &key(&record.name)?, None)?;
                    }
                }
                ForkState::Dropping => {
                    let path = fork_path(self.fork_parent_path()?, &record);
                    if path.try_exists().map_err(failure)? {
                        self.verify_fork_origin(&path, &record)?;
                    }
                    self.remove_owned_fork(&record)?;
                    write_record(&store, &key(&record.name)?, None)?;
                }
                ForkState::Detaching | ForkState::Promoting => {
                    let path = fork_path(self.fork_parent_path()?, &record);
                    let fork = crate::storage::RedDB::open_with_options(&self.fork_options(&path))
                        .map_err(failure)?;
                    if let Some(origin) = read_record(&fork.store(), ORIGIN)? {
                        if origin.identity != record.identity {
                            return Err(failure("fork identity differs from its catalog"));
                        }
                        write_record(&fork.store(), ORIGIN, None)?;
                        fork.flush_local_only().map_err(failure)?;
                    }
                    record.state = if record.state == ForkState::Promoting {
                        ForkState::Promoted
                    } else {
                        ForkState::Detached
                    };
                    write_record(&store, &key(&record.name)?, Some(&record))?;
                }
                ForkState::Ready | ForkState::Detached | ForkState::Promoted => (),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_codec_preserves_full_width_lsns_and_rejects_path_injection() {
        let store = UnifiedStore::new();
        let key = key("candidate").expect("name");
        let mut record = ForkRecord {
            name: "candidate".into(),
            identity: "0123456789abcdef0123456789abcdef".into(),
            parent_store: "parent.rdb".into(),
            fork_lsn: u64::MAX,
            collections_total: u64::MAX,
            state: ForkState::Ready,
        };
        write_record(&store, &key, Some(&record)).expect("write");
        let decoded = read_record(&store, &key).expect("decode").expect("record");
        assert_eq!(decoded.fork_lsn, u64::MAX);
        assert_eq!(decoded.collections_total, u64::MAX);
        record.identity = "../../foreign".into();
        write_record(&store, &key, Some(&record)).expect("corrupt fixture");
        assert!(read_record(&store, &key).is_err());
        write_record(&store, &key, None).expect("tombstone");
        assert!(read_record(&store, &key).expect("latest wins").is_none());
    }
}
