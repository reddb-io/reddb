//! Crash-safe operational manifest tracer for mutable collection files.
//!
//! This is intentionally narrow: it records the durable per-collection marker
//! files used by the current paged store, without changing where row data lives.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::serde_json::{Map, Value};
use crate::storage::engine::crc32::crc32;

const FORMAT_VERSION: u32 = 1;
const MANIFEST_FILE: &str = "manifest.json";
const NEXT_MANIFEST_FILE: &str = "manifest.json.next";
const COLLECTIONS_DIR: &str = "collections";
const QUARANTINE_DIR: &str = "quarantine";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CollectionState {
    Active,
    PendingDrop,
}

impl CollectionState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::PendingDrop => "pending_drop",
        }
    }

    fn from_str(value: &str) -> io::Result<Self> {
        match value {
            "active" => Ok(Self::Active),
            "pending_drop" => Ok(Self::PendingDrop),
            other => Err(invalid_data(format!(
                "unknown manifest collection state: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone)]
struct CollectionEntry {
    state: CollectionState,
    path: String,
}

#[derive(Debug, Clone)]
struct Manifest {
    generation: u64,
    collections: BTreeMap<String, CollectionEntry>,
}

#[derive(Debug, Clone)]
pub(crate) struct OperationalManifest {
    root: PathBuf,
}

impl OperationalManifest {
    pub(crate) fn for_db_path(path: &Path) -> Self {
        let mut root = path.as_os_str().to_os_string();
        root.push(".ops");
        Self {
            root: PathBuf::from(root),
        }
    }

    pub(crate) fn validate_read_only(path: &Path) -> io::Result<Option<u64>> {
        let manifest_path = Self::for_db_path(path).root.join(MANIFEST_FILE);
        match fs::read(&manifest_path) {
            Ok(bytes) => manifest_from_bytes(&bytes).map(|manifest| Some(manifest.generation)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    pub(crate) fn recover_or_bootstrap(
        &self,
        existing_collections: &[String],
    ) -> io::Result<Vec<String>> {
        self.ensure_dirs()?;
        let mut manifest = match self.load_current()? {
            Some(manifest) => manifest,
            None => {
                let mut manifest = Manifest {
                    generation: 0,
                    collections: BTreeMap::new(),
                };
                for collection in existing_collections {
                    let path = self.collection_file_name(collection);
                    self.prepare_collection_file_by_name(&path)?;
                    manifest.collections.insert(
                        collection.clone(),
                        CollectionEntry {
                            state: CollectionState::Active,
                            path,
                        },
                    );
                }
                self.publish(&manifest)?;
                manifest
            }
        };

        let active_paths = manifest
            .collections
            .values()
            .filter(|entry| entry.state == CollectionState::Active)
            .map(|entry| entry.path.clone())
            .collect::<BTreeSet<_>>();
        self.quarantine_unreferenced_files(&active_paths)?;

        let pending_drops = manifest
            .collections
            .iter()
            .filter(|(_, entry)| entry.state == CollectionState::PendingDrop)
            .map(|(name, entry)| (name.clone(), entry.path.clone()))
            .collect::<Vec<_>>();
        let completed_pending_drops = pending_drops
            .iter()
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>();
        if !pending_drops.is_empty() {
            for (_, path) in &pending_drops {
                let _ = fs::remove_file(self.collections_dir().join(path));
            }
            for (name, _) in pending_drops {
                manifest.collections.remove(&name);
            }
            manifest.generation += 1;
            self.publish(&manifest)?;
            let active_paths = manifest
                .collections
                .values()
                .filter(|entry| entry.state == CollectionState::Active)
                .map(|entry| entry.path.clone())
                .collect::<BTreeSet<_>>();
            self.quarantine_unreferenced_files(&active_paths)?;
        }

        Ok(completed_pending_drops)
    }

    pub(crate) fn create_collection(&self, name: &str) -> io::Result<()> {
        self.ensure_dirs()?;
        let mut manifest = self.load_current()?.unwrap_or(Manifest {
            generation: 0,
            collections: BTreeMap::new(),
        });
        if matches!(
            manifest.collections.get(name).map(|entry| entry.state),
            Some(CollectionState::Active)
        ) {
            return Ok(());
        }

        let path = self.collection_file_name(name);
        self.prepare_collection_file_by_name(&path)?;
        manifest.collections.insert(
            name.to_string(),
            CollectionEntry {
                state: CollectionState::Active,
                path,
            },
        );
        manifest.generation += 1;
        self.publish(&manifest)
    }

    pub(crate) fn begin_drop_collection(&self, name: &str) -> io::Result<()> {
        self.ensure_dirs()?;
        let mut manifest = match self.load_current()? {
            Some(manifest) => manifest,
            None => return Ok(()),
        };
        let Some(entry) = manifest.collections.get_mut(name) else {
            return Ok(());
        };
        entry.state = CollectionState::PendingDrop;
        manifest.generation += 1;
        self.publish(&manifest)
    }

    pub(crate) fn finish_drop_collection(&self, name: &str) -> io::Result<()> {
        self.ensure_dirs()?;
        let mut manifest = match self.load_current()? {
            Some(manifest) => manifest,
            None => return Ok(()),
        };
        let Some(entry) = manifest.collections.remove(name) else {
            return Ok(());
        };
        let _ = fs::remove_file(self.collections_dir().join(entry.path));
        manifest.generation += 1;
        self.publish(&manifest)
    }

    #[cfg(test)]
    pub(crate) fn read_generation_for_test(&self) -> io::Result<u64> {
        self.load_current()?
            .map(|manifest| manifest.generation)
            .ok_or_else(|| invalid_data("manifest is missing"))
    }

    #[cfg(test)]
    pub(crate) fn collection_path_for_test(&self, name: &str) -> PathBuf {
        self.collections_dir().join(self.collection_file_name(name))
    }

    #[cfg(test)]
    pub(crate) fn write_next_manifest_for_test(&self, name: &str) -> io::Result<()> {
        self.ensure_dirs()?;
        let mut manifest = self.load_current()?.unwrap_or(Manifest {
            generation: 0,
            collections: BTreeMap::new(),
        });
        manifest.collections.insert(
            name.to_string(),
            CollectionEntry {
                state: CollectionState::Active,
                path: self.collection_file_name(name),
            },
        );
        manifest.generation += 1;
        let bytes = manifest_to_bytes(&manifest)?;
        fs::write(self.root.join(NEXT_MANIFEST_FILE), bytes)
    }

    fn ensure_dirs(&self) -> io::Result<()> {
        fs::create_dir_all(self.collections_dir())?;
        fs::create_dir_all(self.quarantine_dir())?;
        sync_dir(&self.root)?;
        Ok(())
    }

    fn load_current(&self) -> io::Result<Option<Manifest>> {
        let path = self.root.join(MANIFEST_FILE);
        match fs::read(&path) {
            Ok(bytes) => manifest_from_bytes(&bytes).map(Some),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn publish(&self, manifest: &Manifest) -> io::Result<()> {
        self.ensure_dirs()?;
        let current = self.root.join(MANIFEST_FILE);
        let next = self.root.join(NEXT_MANIFEST_FILE);
        let bytes = manifest_to_bytes(manifest)?;
        {
            let mut file = File::create(&next)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        fs::rename(&next, &current)?;
        sync_dir(&self.root)
    }

    fn prepare_collection_file_by_name(&self, file_name: &str) -> io::Result<()> {
        let path = self.collections_dir().join(file_name);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&path)?;
        file.sync_all()?;
        sync_dir(&self.collections_dir())
    }

    fn quarantine_unreferenced_files(&self, active_paths: &BTreeSet<String>) -> io::Result<()> {
        for entry in fs::read_dir(self.collections_dir())? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if !file_type.is_file() {
                continue;
            }
            let file_name = entry.file_name().to_string_lossy().into_owned();
            if active_paths.contains(&file_name) {
                continue;
            }
            let from = entry.path();
            let to = unique_quarantine_path(&self.quarantine_dir(), &file_name);
            fs::rename(from, to)?;
        }
        sync_dir(&self.collections_dir())?;
        sync_dir(&self.quarantine_dir())
    }

    fn collections_dir(&self) -> PathBuf {
        self.root.join(COLLECTIONS_DIR)
    }

    fn quarantine_dir(&self) -> PathBuf {
        self.root.join(QUARANTINE_DIR)
    }

    fn collection_file_name(&self, name: &str) -> String {
        let mut out = String::with_capacity(name.len() + 5);
        for byte in name.as_bytes() {
            match *byte {
                b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b'.' => {
                    out.push(*byte as char);
                }
                other => out.push_str(&format!("%{other:02X}")),
            }
        }
        out.push_str(".rcol");
        out
    }
}

fn manifest_to_bytes(manifest: &Manifest) -> io::Result<Vec<u8>> {
    let checksum = checksum_manifest(manifest);
    let mut object = manifest_body_json(manifest);
    object.insert(
        "checksum".to_string(),
        Value::String(format!("{checksum:08x}")),
    );
    Ok(Value::Object(object).to_string_pretty().into_bytes())
}

fn manifest_from_bytes(bytes: &[u8]) -> io::Result<Manifest> {
    let value: Value = crate::serde_json::from_slice(bytes).map_err(invalid_data)?;
    let object = value
        .as_object()
        .ok_or_else(|| invalid_data("manifest root must be an object"))?;
    let checksum = object
        .get("checksum")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_data("manifest checksum is missing"))?;
    let manifest = manifest_from_object(object)?;
    let expected = format!("{:08x}", checksum_manifest(&manifest));
    if checksum != expected {
        return Err(invalid_data(format!(
            "manifest checksum mismatch: expected {expected}, got {checksum}"
        )));
    }
    Ok(manifest)
}

fn manifest_from_object(object: &Map<String, Value>) -> io::Result<Manifest> {
    let format_version = object
        .get("format_version")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_data("manifest format_version is missing"))?;
    if format_version != FORMAT_VERSION as u64 {
        return Err(invalid_data(format!(
            "unsupported manifest format version: {format_version}"
        )));
    }
    let generation = object
        .get("generation")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_data("manifest generation is missing"))?;
    let collections_value = object
        .get("collections")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_data("manifest collections are missing"))?;
    let mut collections = BTreeMap::new();
    for (name, value) in collections_value {
        let entry = value
            .as_object()
            .ok_or_else(|| invalid_data("manifest collection entry must be an object"))?;
        let state = entry
            .get("state")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_data("manifest collection state is missing"))
            .and_then(CollectionState::from_str)?;
        let path = entry
            .get("path")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid_data("manifest collection path is missing"))?
            .to_string();
        collections.insert(name.clone(), CollectionEntry { state, path });
    }
    Ok(Manifest {
        generation,
        collections,
    })
}

fn checksum_manifest(manifest: &Manifest) -> u32 {
    let body = Value::Object(manifest_body_json(manifest)).to_string_compact();
    crc32(body.as_bytes())
}

fn manifest_body_json(manifest: &Manifest) -> Map<String, Value> {
    let mut collections = Map::new();
    for (name, entry) in &manifest.collections {
        let mut entry_object = Map::new();
        entry_object.insert(
            "state".to_string(),
            Value::String(entry.state.as_str().to_string()),
        );
        entry_object.insert("path".to_string(), Value::String(entry.path.clone()));
        collections.insert(name.clone(), Value::Object(entry_object));
    }

    let mut object = Map::new();
    object.insert(
        "format_version".to_string(),
        Value::Number(FORMAT_VERSION as f64),
    );
    object.insert(
        "generation".to_string(),
        Value::Number(manifest.generation as f64),
    );
    object.insert("collections".to_string(), Value::Object(collections));
    object
}

fn unique_quarantine_path(dir: &Path, file_name: &str) -> PathBuf {
    let mut candidate = dir.join(file_name);
    if !candidate.exists() {
        return candidate;
    }
    for n in 1.. {
        candidate = dir.join(format!("{file_name}.{n}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

fn invalid_data(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::unified::UnifiedStore;

    fn temp_db_path(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "reddb_operational_manifest_{name}_{}_{}",
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.join("data.rdb")
    }

    #[test]
    fn manifest_update_ignores_interrupted_next_generation_publish() {
        let path = temp_db_path("interrupted_update");
        {
            let store = UnifiedStore::open(&path).unwrap();
            store.create_collection("users").unwrap();
        }

        let manifest = OperationalManifest::for_db_path(&path);
        let generation = manifest.read_generation_for_test().unwrap();
        manifest.write_next_manifest_for_test("ghost").unwrap();

        let reopened = UnifiedStore::open(&path).unwrap();
        assert!(reopened.get_collection("users").is_some());
        assert!(reopened.get_collection("ghost").is_none());
        assert_eq!(manifest.read_generation_for_test().unwrap(), generation);
    }

    #[test]
    fn interrupted_create_quarantines_prepared_unpublished_file() {
        let path = temp_db_path("interrupted_create");
        {
            let _store = UnifiedStore::open(&path).unwrap();
        }
        let manifest = OperationalManifest::for_db_path(&path);
        let orphan = manifest.collection_path_for_test("half_created");
        fs::create_dir_all(orphan.parent().unwrap()).unwrap();
        fs::write(&orphan, b"prepared but unpublished").unwrap();

        let reopened = UnifiedStore::open(&path).unwrap();
        assert!(reopened.get_collection("half_created").is_none());
        assert!(!orphan.exists());
        let quarantined = manifest.quarantine_dir().join("half_created.rcol");
        assert!(quarantined.exists(), "orphan marker should be quarantined");
    }

    #[test]
    fn interrupted_drop_pending_state_is_completed_on_recovery() {
        let path = temp_db_path("interrupted_drop");
        {
            let store = UnifiedStore::open(&path).unwrap();
            store.create_collection("gone").unwrap();
        }
        let manifest = OperationalManifest::for_db_path(&path);
        manifest.begin_drop_collection("gone").unwrap();

        let reopened = UnifiedStore::open(&path).unwrap();
        assert!(reopened.get_collection("gone").is_none());
        assert!(!manifest.collection_path_for_test("gone").exists());
    }

    #[test]
    fn recovery_quarantines_unreferenced_physical_files() {
        let path = temp_db_path("orphan_quarantine");
        {
            let store = UnifiedStore::open(&path).unwrap();
            store.create_collection("live").unwrap();
        }
        let manifest = OperationalManifest::for_db_path(&path);
        let orphan = manifest.collection_path_for_test("orphan");
        fs::write(&orphan, b"unreferenced").unwrap();

        let reopened = UnifiedStore::open(&path).unwrap();
        assert!(reopened.get_collection("live").is_some());
        assert!(!orphan.exists());
        assert!(manifest.quarantine_dir().join("orphan.rcol").exists());
    }

    #[test]
    fn checksum_validation_rejects_corrupted_current_manifest() {
        let path = temp_db_path("checksum");
        {
            let store = UnifiedStore::open(&path).unwrap();
            store.create_collection("users").unwrap();
        }
        let manifest_path = OperationalManifest::for_db_path(&path)
            .root
            .join(MANIFEST_FILE);
        let mut text = fs::read_to_string(&manifest_path).unwrap();
        text = text.replace("\"generation\": 1", "\"generation\": 9");
        fs::write(&manifest_path, text).unwrap();

        let err = match UnifiedStore::open(&path) {
            Ok(_) => panic!("corrupt manifest must fail closed"),
            Err(err) => err,
        };
        assert!(
            err.to_string().contains("checksum mismatch"),
            "unexpected error: {err}"
        );
    }
}
