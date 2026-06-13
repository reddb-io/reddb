//! Crash-safe operational manifest for mutable collection files.
//!
//! This is a file contract: root paths, collection marker names, manifest
//! JSON shape, checksum coverage, and atomic publish rules live here so
//! runtime crates do not define persistent manifest formats.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

const FORMAT_VERSION: u32 = 1;
pub const MANIFEST_FILE: &str = "manifest.json";
pub const NEXT_MANIFEST_FILE: &str = "manifest.json.next";
pub const COLLECTIONS_DIR: &str = "collections";
pub const QUARANTINE_DIR: &str = "quarantine";

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

    fn parse(value: &str) -> io::Result<Self> {
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
pub struct OperationalManifest {
    root: PathBuf,
}

impl OperationalManifest {
    pub fn for_db_path(path: &Path) -> Self {
        let mut root = path.as_os_str().to_os_string();
        root.push(".ops");
        Self {
            root: PathBuf::from(root),
        }
    }

    pub fn recover_or_bootstrap(&self, existing_collections: &[String]) -> io::Result<Vec<String>> {
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

        let active_paths = active_collection_paths(&manifest);
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
            let active_paths = active_collection_paths(&manifest);
            self.quarantine_unreferenced_files(&active_paths)?;
        }

        Ok(completed_pending_drops)
    }

    pub fn create_collection(&self, name: &str) -> io::Result<()> {
        self.ensure_dirs()?;
        let mut manifest = self.load_current()?.unwrap_or_else(empty_manifest);
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

    pub fn begin_drop_collection(&self, name: &str) -> io::Result<()> {
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

    pub fn finish_drop_collection(&self, name: &str) -> io::Result<()> {
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

    pub fn read_generation_for_test(&self) -> io::Result<u64> {
        self.load_current()?
            .map(|manifest| manifest.generation)
            .ok_or_else(|| invalid_data("manifest is missing"))
    }

    pub fn collection_path_for_test(&self, name: &str) -> PathBuf {
        self.collections_dir().join(self.collection_file_name(name))
    }

    pub fn current_manifest_path_for_test(&self) -> PathBuf {
        self.root.join(MANIFEST_FILE)
    }

    pub fn quarantine_path_for_test(&self, file_name: &str) -> PathBuf {
        self.quarantine_dir().join(file_name)
    }

    pub fn write_next_manifest_for_test(&self, name: &str) -> io::Result<()> {
        self.ensure_dirs()?;
        let mut manifest = self.load_current()?.unwrap_or_else(empty_manifest);
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

fn empty_manifest() -> Manifest {
    Manifest {
        generation: 0,
        collections: BTreeMap::new(),
    }
}

fn active_collection_paths(manifest: &Manifest) -> BTreeSet<String> {
    manifest
        .collections
        .values()
        .filter(|entry| entry.state == CollectionState::Active)
        .map(|entry| entry.path.clone())
        .collect()
}

fn manifest_to_bytes(manifest: &Manifest) -> io::Result<Vec<u8>> {
    let checksum = checksum_manifest(manifest);
    let mut object = manifest_body_json(manifest);
    object.insert(
        "checksum".to_string(),
        Value::String(format!("{checksum:08x}")),
    );
    serde_json::to_vec_pretty(&Value::Object(object)).map_err(invalid_data)
}

fn manifest_from_bytes(bytes: &[u8]) -> io::Result<Manifest> {
    let value: Value = serde_json::from_slice(bytes).map_err(invalid_data)?;
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
            .and_then(CollectionState::parse)?;
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
    let body = serde_json::to_vec(&Value::Object(manifest_body_json(manifest)))
        .expect("manifest body is JSON encodable");
    crc32fast::hash(&body)
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
    object.insert("format_version".to_string(), Value::from(FORMAT_VERSION));
    object.insert("generation".to_string(), Value::from(manifest.generation));
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

    fn temp_db_path(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "reddb_file_operational_manifest_{name}_{}_{}",
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&dir).unwrap();
        dir.join("data.rdb")
    }

    #[test]
    fn manifest_round_trip_validates_checksum() {
        let mut manifest = empty_manifest();
        manifest.generation = 7;
        manifest.collections.insert(
            "users".to_string(),
            CollectionEntry {
                state: CollectionState::Active,
                path: "users.rcol".to_string(),
            },
        );

        let bytes = manifest_to_bytes(&manifest).unwrap();
        let decoded = manifest_from_bytes(&bytes).unwrap();

        assert_eq!(decoded.generation, 7);
        assert_eq!(decoded.collections["users"].path, "users.rcol");
    }

    #[test]
    fn checksum_rejects_corruption() {
        let mut manifest = empty_manifest();
        manifest.generation = 1;
        let mut text = String::from_utf8(manifest_to_bytes(&manifest).unwrap()).unwrap();
        text = text.replace("\"generation\": 1", "\"generation\": 9");

        let err = manifest_from_bytes(text.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"));
    }

    #[test]
    fn recover_removes_pending_drop_and_quarantines_orphans() {
        let path = temp_db_path("recover");
        let manifest = OperationalManifest::for_db_path(&path);
        manifest
            .recover_or_bootstrap(&["live".to_string(), "gone".to_string()])
            .unwrap();
        fs::write(manifest.collection_path_for_test("orphan"), b"orphan").unwrap();
        manifest.begin_drop_collection("gone").unwrap();

        let completed = manifest.recover_or_bootstrap(&[]).unwrap();

        assert_eq!(completed, vec!["gone".to_string()]);
        assert!(manifest.collection_path_for_test("live").exists());
        assert!(!manifest.collection_path_for_test("gone").exists());
        assert!(!manifest.collection_path_for_test("orphan").exists());
        assert!(manifest.quarantine_path_for_test("orphan.rcol").exists());
    }

    #[test]
    fn create_and_drop_collection_are_idempotent_and_escape_names() {
        let path = temp_db_path("create_drop");
        let manifest = OperationalManifest::for_db_path(&path);

        manifest.create_collection("tenant/a b").unwrap();
        let first_generation = manifest.read_generation_for_test().unwrap();
        manifest.create_collection("tenant/a b").unwrap();
        assert_eq!(
            manifest.read_generation_for_test().unwrap(),
            first_generation
        );
        assert!(manifest.collection_path_for_test("tenant/a b").exists());
        assert!(manifest
            .collection_path_for_test("tenant/a b")
            .file_name()
            .unwrap()
            .to_string_lossy()
            .contains("%2F"));

        manifest.begin_drop_collection("missing").unwrap();
        assert_eq!(
            manifest.read_generation_for_test().unwrap(),
            first_generation
        );
        manifest.begin_drop_collection("tenant/a b").unwrap();
        manifest.finish_drop_collection("missing").unwrap();
        manifest.finish_drop_collection("tenant/a b").unwrap();
        assert!(!manifest.collection_path_for_test("tenant/a b").exists());
        assert!(manifest.read_generation_for_test().unwrap() > first_generation);
    }

    #[test]
    fn next_manifest_helper_writes_next_file_without_publishing() {
        let path = temp_db_path("next");
        let manifest = OperationalManifest::for_db_path(&path);
        manifest.recover_or_bootstrap(&[]).unwrap();
        manifest.write_next_manifest_for_test("future").unwrap();

        assert!(manifest.root.join(NEXT_MANIFEST_FILE).exists());
        assert!(manifest
            .load_current()
            .unwrap()
            .unwrap()
            .collections
            .is_empty());
    }

    #[test]
    fn manifest_parser_rejects_malformed_shapes() {
        assert!(manifest_from_bytes(b"[]").is_err());
        assert!(manifest_from_bytes(br#"{"format_version":1}"#).is_err());
        assert!(CollectionState::parse("bad").is_err());

        let mut object = Map::new();
        object.insert("format_version".to_string(), Value::from(2));
        object.insert("generation".to_string(), Value::from(0));
        object.insert("collections".to_string(), Value::Object(Map::new()));
        assert!(manifest_from_object(&object).is_err());

        object.insert("format_version".to_string(), Value::from(FORMAT_VERSION));
        object.remove("generation");
        assert!(manifest_from_object(&object).is_err());

        object.insert("generation".to_string(), Value::from(0));
        object.remove("collections");
        assert!(manifest_from_object(&object).is_err());

        let mut collections = Map::new();
        collections.insert("users".to_string(), Value::String("bad".to_string()));
        object.insert("collections".to_string(), Value::Object(collections));
        assert!(manifest_from_object(&object).is_err());

        let mut entry = Map::new();
        entry.insert("path".to_string(), Value::String("users.rcol".to_string()));
        let mut collections = Map::new();
        collections.insert("users".to_string(), Value::Object(entry));
        object.insert("collections".to_string(), Value::Object(collections));
        assert!(manifest_from_object(&object).is_err());

        let mut entry = Map::new();
        entry.insert("state".to_string(), Value::String("active".to_string()));
        let mut collections = Map::new();
        collections.insert("users".to_string(), Value::Object(entry));
        object.insert("collections".to_string(), Value::Object(collections));
        assert!(manifest_from_object(&object).is_err());
    }

    #[test]
    fn unique_quarantine_path_adds_suffix_when_needed() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("orphan.rcol"), b"one").unwrap();
        fs::write(dir.path().join("orphan.rcol.1"), b"two").unwrap();

        assert_eq!(
            unique_quarantine_path(dir.path(), "orphan.rcol"),
            dir.path().join("orphan.rcol.2")
        );
    }
}
