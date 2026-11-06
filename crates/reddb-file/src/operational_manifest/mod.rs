//! Crash-safe operational manifest for mutable collection files.
//!
//! This is a file contract: root paths, collection marker names, manifest
//! JSON shape, checksum coverage, and atomic publish rules live here so
//! runtime crates do not define persistent manifest formats.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::{OpenMode, StdVfs, Vfs, VfsFile};

use crate::append_only_segment::{
    append_only_segment_chunk_checksums, decode_append_only_segment, AppendOnlySegmentBloom,
    AppendOnlySegmentChunkChecksum, AppendOnlySegmentCodec, APPEND_ONLY_SEGMENT_CHUNK_BYTES,
};

mod fork;

// Fork lifecycle types live in the `fork` submodule; re-export so the public
// path (`operational_manifest::ForkInfo`, etc.) is unchanged.
pub use fork::{ForkHydrationState, ForkInfo, ForkOrigin, PromoteForkOutcome};

const FORMAT_VERSION: u32 = 1;
pub const MANIFEST_FILE: &str = "manifest.json";
pub const NEXT_MANIFEST_FILE: &str = "manifest.json.next";
pub const COLLECTIONS_DIR: &str = "collections";
pub const APPEND_ONLY_SEGMENTS_DIR: &str = "segments";
pub const QUARANTINE_DIR: &str = "quarantine";
/// Directory (under a parent store's operational root) that holds store forks.
///
/// This is the store-fork surface of ADR 0070 — deliberately distinct from the
/// VCS data model's `branch`/`CHECKPOINT` vocabulary (#1567). A store fork is
/// storage mechanics for experiment-and-discard workflows; it is never a branch.
pub const FORKS_DIR: &str = "forks";

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
    /// For a store fork: absolute path to the parent's collection file that this
    /// entry is shared-by-reference with. `None` for a normal (owned) collection
    /// file, or once a fork has hydrated a private copy (copy-on-write). ADR 0070:
    /// mutable collection files hydrate lazily on first touch.
    source: Option<String>,
}

#[derive(Debug, Clone)]
struct Manifest {
    generation: u64,
    collections: BTreeMap<String, CollectionEntry>,
    append_only_segments: Vec<AppendOnlySegmentManifestEntry>,
    append_only_retired: BTreeMap<String, AppendOnlyRetiredState>,
    /// Present only for a store fork's own manifest; `None` for a parent store.
    fork_origin: Option<ForkOrigin>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppendOnlySegmentState {
    Active,
    PendingDrop,
}

impl AppendOnlySegmentState {
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
                "unknown append-only segment state: {other}"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendOnlySegmentManifestEntry {
    pub collection: String,
    pub segment_id: u64,
    pub state: AppendOnlySegmentState,
    pub path: String,
    pub codec: AppendOnlySegmentCodec,
    pub chunk_size: u32,
    pub row_count: u64,
    pub retention_min_ms: Option<i64>,
    pub retention_max_ms: Option<i64>,
    pub primary_min: Option<String>,
    pub primary_max: Option<String>,
    pub primary_bloom: Option<AppendOnlySegmentBloom>,
    pub chunk_checksums: Vec<AppendOnlySegmentChunkChecksum>,
}

#[derive(Debug, Clone, Default)]
struct AppendOnlyRetiredState {
    row_count: u64,
    segment_high_water: u64,
}

#[derive(Debug, Clone)]
pub struct OperationalManifest<V = StdVfs> {
    root: PathBuf,
    vfs: V,
}

/// Manifest-owned create operation. Construction durably prepares the physical
/// collection file; [`publish`](Self::publish) is the only legal next phase.
pub struct PreparedCollectionCreate<V: Vfs> {
    manifest: OperationalManifest<V>,
    name: String,
}

impl<V: Vfs> PreparedCollectionCreate<V> {
    pub fn publish(self) -> io::Result<()> {
        self.manifest.publish_collection_create(&self.name)
    }
}

/// Manifest-owned drop operation. Construction durably publishes
/// `pending_drop`; [`finish`](Self::finish) removes the physical file and then
/// publishes the final manifest generation.
pub struct PreparedCollectionDrop<V: Vfs> {
    manifest: OperationalManifest<V>,
    name: String,
}

impl<V: Vfs> PreparedCollectionDrop<V> {
    pub fn finish(self) -> io::Result<()> {
        self.manifest.finish_drop_collection(&self.name)
    }
}

impl OperationalManifest<StdVfs> {
    pub fn for_db_path(path: &Path) -> Self {
        Self::with_vfs(path, StdVfs)
    }
}

impl<V: Vfs> OperationalManifest<V> {
    pub fn with_vfs(path: &Path, vfs: V) -> Self {
        let mut root = path.as_os_str().to_os_string();
        root.push(".ops");
        Self {
            root: PathBuf::from(root),
            vfs,
        }
    }

    pub fn recover_or_bootstrap(&self, existing_collections: &[String]) -> io::Result<Vec<String>> {
        self.recover_interrupted_fork_promotions()?;
        self.ensure_dirs()?;
        let mut manifest = match self.load_current()? {
            Some(manifest) => manifest,
            None => {
                let mut manifest = Manifest {
                    generation: 0,
                    collections: BTreeMap::new(),
                    append_only_segments: Vec::new(),
                    append_only_retired: BTreeMap::new(),
                    fork_origin: None,
                };
                for collection in existing_collections {
                    let path = self.collection_file_name(collection);
                    self.prepare_collection_file_by_name(&path)?;
                    manifest.collections.insert(
                        collection.clone(),
                        CollectionEntry {
                            state: CollectionState::Active,
                            path,
                            source: None,
                        },
                    );
                }
                self.publish(&manifest)?;
                manifest
            }
        };

        let protected_paths = self.protected_collection_paths(&manifest)?;
        self.quarantine_unreferenced_collection_files(&protected_paths)?;
        self.quarantine_unreferenced_append_only_segments(&manifest)?;
        self.validate_manifest_artifacts(&manifest)?;

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
            let fork_referenced_paths = self.fork_referenced_collection_paths()?;
            for (_, path) in &pending_drops {
                if !fork_referenced_paths.contains(path) {
                    let _ = self.vfs.remove_file(&self.collections_dir().join(path));
                }
            }
            for (name, _) in pending_drops {
                manifest.collections.remove(&name);
            }
            manifest.generation += 1;
            self.publish(&manifest)?;
            let protected_paths = self.protected_collection_paths(&manifest)?;
            self.quarantine_unreferenced_collection_files(&protected_paths)?;
            self.quarantine_unreferenced_append_only_segments(&manifest)?;
            self.validate_manifest_artifacts(&manifest)?;
        }
        let pending_segments = manifest
            .append_only_segments
            .iter()
            .filter(|entry| entry.state == AppendOnlySegmentState::PendingDrop)
            .cloned()
            .collect::<Vec<_>>();
        if !pending_segments.is_empty() {
            for entry in &pending_segments {
                let _ = self
                    .vfs
                    .remove_file(&self.append_only_segments_dir().join(&entry.path));
                record_retired_append_only_segment(&mut manifest, entry);
            }
            manifest
                .append_only_segments
                .retain(|entry| entry.state != AppendOnlySegmentState::PendingDrop);
            manifest.generation += 1;
            self.publish(&manifest)?;
            let protected_paths = self.protected_collection_paths(&manifest)?;
            self.quarantine_unreferenced_collection_files(&protected_paths)?;
            self.quarantine_unreferenced_append_only_segments(&manifest)?;
            self.validate_manifest_artifacts(&manifest)?;
        }

        Ok(completed_pending_drops)
    }

    pub fn create_collection(&self, name: &str) -> io::Result<()> {
        self.prepare_collection_create(name)?.publish()
    }

    /// Atomically change manifest identities while retaining their physical files.
    /// The caller must fence writers and coordinate the paged catalogue and WAL.
    /// Repeating a completed operation is a no-op.
    pub fn rename_collections(&self, names: &[(&str, &str)]) -> io::Result<()> {
        let Some(mut manifest) = self.load_current()? else {
            return Ok(());
        };
        let mut sources = BTreeSet::new();
        let mut targets = BTreeSet::new();
        for &(source, target) in names {
            if source == target || !sources.insert(source) || !targets.insert(target) {
                return Err(invalid_data("invalid collection rename mapping"));
            }
        }
        if sources.iter().any(|name| targets.contains(name)) {
            return Err(invalid_data("overlapping collection rename mappings"));
        }

        for &(source, target) in names {
            let source_exists = manifest.collections.contains_key(source)
                || manifest
                    .append_only_segments
                    .iter()
                    .any(|segment| segment.collection == source)
                || manifest.append_only_retired.contains_key(source);
            if !source_exists {
                continue;
            }
            if manifest.collections.contains_key(target)
                || manifest
                    .append_only_segments
                    .iter()
                    .any(|segment| segment.collection == target)
                || manifest.append_only_retired.contains_key(target)
            {
                return Err(invalid_data(format!(
                    "collection rename collision: {source} and {target}"
                )));
            }
        }

        let mut changed = false;
        for &(source, target) in names {
            if let Some(entry) = manifest.collections.remove(source) {
                manifest.collections.insert(target.to_string(), entry);
                changed = true;
            }
            for segment in &mut manifest.append_only_segments {
                if segment.collection == source {
                    segment.collection = target.to_string();
                    changed = true;
                }
            }
            if let Some(retired) = manifest.append_only_retired.remove(source) {
                manifest
                    .append_only_retired
                    .insert(target.to_string(), retired);
                changed = true;
            }
        }
        if changed {
            manifest.generation = manifest
                .generation
                .checked_add(1)
                .ok_or_else(|| invalid_data("manifest generation overflow"))?;
            self.publish(&manifest)?;
        }
        Ok(())
    }

    pub fn prepare_collection_create(&self, name: &str) -> io::Result<PreparedCollectionCreate<V>> {
        self.ensure_dirs()?;
        let manifest = self.load_current()?.unwrap_or_else(empty_manifest);
        if matches!(
            manifest.collections.get(name).map(|entry| entry.state),
            Some(CollectionState::Active)
        ) {
            return Ok(PreparedCollectionCreate {
                manifest: self.clone(),
                name: name.to_string(),
            });
        }

        let path = self.collection_file_name(name);
        if manifest
            .collections
            .values()
            .any(|entry| entry.path == path)
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("collection artifact already belongs to another collection: {name}"),
            ));
        }
        self.prepare_collection_file_by_name(&path)?;
        Ok(PreparedCollectionCreate {
            manifest: self.clone(),
            name: name.to_string(),
        })
    }

    fn publish_collection_create(&self, name: &str) -> io::Result<()> {
        let mut manifest = self.load_current()?.unwrap_or_else(empty_manifest);
        if matches!(
            manifest.collections.get(name).map(|entry| entry.state),
            Some(CollectionState::Active)
        ) {
            return Ok(());
        }
        let path = self.collection_file_name(name);
        if manifest
            .collections
            .values()
            .any(|entry| entry.path == path)
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("collection artifact already belongs to another collection: {name}"),
            ));
        }
        manifest.collections.insert(
            name.to_string(),
            CollectionEntry {
                state: CollectionState::Active,
                path,
                source: None,
            },
        );
        manifest.generation += 1;
        self.publish(&manifest)
    }

    pub fn prepare_collection_drop(&self, name: &str) -> io::Result<PreparedCollectionDrop<V>> {
        self.ensure_dirs()?;
        let mut manifest = match self.load_current()? {
            Some(manifest) => manifest,
            None => {
                return Ok(PreparedCollectionDrop {
                    manifest: self.clone(),
                    name: name.to_string(),
                })
            }
        };
        if let Some(entry) = manifest.collections.get_mut(name) {
            entry.state = CollectionState::PendingDrop;
            manifest.generation += 1;
            self.publish(&manifest)?;
        }
        Ok(PreparedCollectionDrop {
            manifest: self.clone(),
            name: name.to_string(),
        })
    }

    fn finish_drop_collection(&self, name: &str) -> io::Result<()> {
        self.ensure_dirs()?;
        let mut manifest = match self.load_current()? {
            Some(manifest) => manifest,
            None => return Ok(()),
        };
        let Some(entry) = manifest.collections.remove(name) else {
            return Ok(());
        };
        if !self
            .fork_referenced_collection_paths()?
            .contains(&entry.path)
        {
            let _ = self
                .vfs
                .remove_file(&self.collections_dir().join(entry.path));
        }
        manifest.generation += 1;
        self.publish(&manifest)
    }

    /// Identity of this store — its operational manifest root path. This is the
    /// value recorded as `parent_store` on any fork taken from this store, and
    /// reported by the fork listing.
    pub fn store_identity(&self) -> String {
        self.root.to_string_lossy().into_owned()
    }

    pub fn publish_append_only_segment(
        &self,
        collection: &str,
        segment_id: u64,
        codec: AppendOnlySegmentCodec,
        bytes: &[u8],
    ) -> io::Result<AppendOnlySegmentManifestEntry> {
        self.publish_append_only_segment_with_retention_bounds(
            collection, segment_id, codec, bytes, None, None,
        )
    }

    pub fn publish_append_only_segment_with_retention_bounds(
        &self,
        collection: &str,
        segment_id: u64,
        codec: AppendOnlySegmentCodec,
        bytes: &[u8],
        retention_min_ms: Option<i64>,
        retention_max_ms: Option<i64>,
    ) -> io::Result<AppendOnlySegmentManifestEntry> {
        self.ensure_dirs()?;
        let decoded = decode_append_only_segment(bytes).map_err(invalid_data)?;
        if decoded.codec != codec {
            return Err(invalid_data("append-only segment codec metadata mismatch"));
        }
        let mut manifest = self.load_current()?.unwrap_or_else(empty_manifest);
        if manifest
            .append_only_segments
            .iter()
            .any(|entry| entry.collection == collection && entry.segment_id == segment_id)
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("append-only segment already published: {collection}/{segment_id}"),
            ));
        }

        let path = self.append_only_segment_file_name(collection, segment_id);
        let segment_path = self.append_only_segments_dir().join(&path);
        if self.vfs.exists(&segment_path) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("closed append-only segment already exists: {path}"),
            ));
        }
        {
            let mut file = self.vfs.open(&segment_path, OpenMode::create_new())?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        self.vfs.sync_dir(&self.append_only_segments_dir())?;

        let entry = AppendOnlySegmentManifestEntry {
            collection: collection.to_string(),
            segment_id,
            state: AppendOnlySegmentState::Active,
            path,
            codec,
            chunk_size: APPEND_ONLY_SEGMENT_CHUNK_BYTES,
            row_count: decoded.rows.len() as u64,
            retention_min_ms,
            retention_max_ms,
            primary_min: decoded.primary_min.as_ref().map(hex::encode),
            primary_max: decoded.primary_max.as_ref().map(hex::encode),
            primary_bloom: decoded.primary_bloom.clone(),
            chunk_checksums: append_only_segment_chunk_checksums(bytes),
        };
        manifest.append_only_segments.push(entry.clone());
        manifest
            .append_only_segments
            .sort_by(|a, b| (&a.collection, a.segment_id).cmp(&(&b.collection, b.segment_id)));
        let retired = manifest
            .append_only_retired
            .entry(collection.to_string())
            .or_default();
        retired.segment_high_water = retired.segment_high_water.max(segment_id);
        manifest.generation += 1;
        self.publish(&manifest)?;
        Ok(entry)
    }

    pub fn begin_retire_append_only_segment(
        &self,
        collection: &str,
        segment_id: u64,
    ) -> io::Result<bool> {
        self.ensure_dirs()?;
        let mut manifest = match self.load_current()? {
            Some(manifest) => manifest,
            None => return Ok(false),
        };
        let Some(entry) = manifest
            .append_only_segments
            .iter_mut()
            .find(|entry| entry.collection == collection && entry.segment_id == segment_id)
        else {
            return Ok(false);
        };
        if entry.state == AppendOnlySegmentState::PendingDrop {
            return Ok(true);
        }
        entry.state = AppendOnlySegmentState::PendingDrop;
        manifest.generation += 1;
        self.publish(&manifest)?;
        Ok(true)
    }

    pub fn finish_retire_append_only_segment(
        &self,
        collection: &str,
        segment_id: u64,
    ) -> io::Result<bool> {
        self.ensure_dirs()?;
        let mut manifest = match self.load_current()? {
            Some(manifest) => manifest,
            None => return Ok(false),
        };
        let Some(index) = manifest
            .append_only_segments
            .iter()
            .position(|entry| entry.collection == collection && entry.segment_id == segment_id)
        else {
            return Ok(false);
        };
        if manifest.append_only_segments[index].state != AppendOnlySegmentState::PendingDrop {
            manifest.append_only_segments[index].state = AppendOnlySegmentState::PendingDrop;
            manifest.generation += 1;
            self.publish(&manifest)?;
            manifest = self.load_current()?.unwrap_or_else(empty_manifest);
        }
        let Some(index) = manifest
            .append_only_segments
            .iter()
            .position(|entry| entry.collection == collection && entry.segment_id == segment_id)
        else {
            return Ok(false);
        };
        let entry = manifest.append_only_segments.remove(index);
        let _ = self
            .vfs
            .remove_file(&self.append_only_segments_dir().join(&entry.path));
        record_retired_append_only_segment(&mut manifest, &entry);
        manifest.generation += 1;
        self.publish(&manifest)?;
        Ok(true)
    }

    fn protected_collection_paths(&self, manifest: &Manifest) -> io::Result<BTreeSet<String>> {
        let mut paths = active_collection_paths(manifest);
        paths.extend(self.fork_referenced_collection_paths()?);
        Ok(paths)
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

    pub fn append_only_segment_path_for_test(&self, file_name: &str) -> PathBuf {
        self.append_only_segments_dir().join(file_name)
    }

    pub fn append_only_segments_for_test(&self) -> io::Result<Vec<AppendOnlySegmentManifestEntry>> {
        self.append_only_segments()
    }

    pub fn append_only_segments(&self) -> io::Result<Vec<AppendOnlySegmentManifestEntry>> {
        Ok(self
            .load_current()?
            .map(|manifest| active_append_only_segments(&manifest))
            .unwrap_or_default())
    }

    pub fn append_only_segments_with_pending_for_test(
        &self,
    ) -> io::Result<Vec<AppendOnlySegmentManifestEntry>> {
        Ok(self
            .load_current()?
            .map(|manifest| manifest.append_only_segments)
            .unwrap_or_default())
    }

    pub fn append_only_published_rows(&self, collection: &str) -> io::Result<u64> {
        Ok(self
            .load_current()?
            .map(|manifest| append_only_published_rows(&manifest, collection))
            .unwrap_or_default())
    }

    pub fn append_only_published_rows_for_test(&self, collection: &str) -> io::Result<u64> {
        self.append_only_published_rows(collection)
    }

    pub fn next_append_only_segment_id(&self, collection: &str) -> io::Result<u64> {
        Ok(self
            .load_current()?
            .map(|manifest| next_append_only_segment_id(&manifest, collection))
            .unwrap_or(1))
    }

    pub fn write_next_manifest_for_test(&self, name: &str) -> io::Result<()> {
        self.ensure_dirs()?;
        let mut manifest = self.load_current()?.unwrap_or_else(empty_manifest);
        manifest.collections.insert(
            name.to_string(),
            CollectionEntry {
                state: CollectionState::Active,
                path: self.collection_file_name(name),
                source: None,
            },
        );
        manifest.generation += 1;
        let bytes = manifest_to_bytes(&manifest)?;
        self.write_file(&self.root.join(NEXT_MANIFEST_FILE), &bytes, false)
    }

    fn ensure_dirs(&self) -> io::Result<()> {
        self.vfs.create_dir_all(&self.collections_dir())?;
        self.vfs.create_dir_all(&self.append_only_segments_dir())?;
        self.vfs.create_dir_all(&self.quarantine_dir())?;
        self.vfs.sync_dir(&self.root)?;
        Ok(())
    }

    fn load_current(&self) -> io::Result<Option<Manifest>> {
        let path = self.root.join(MANIFEST_FILE);
        match self.read_file(&path) {
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
            let mut file = self.vfs.open(&next, OpenMode::create_truncate())?;
            file.write_all(&bytes)?;
            file.sync_all()?;
        }
        self.vfs.rename(&next, &current)?;
        self.vfs.sync_dir(&self.root)
    }

    fn prepare_collection_file_by_name(&self, file_name: &str) -> io::Result<()> {
        let path = self.collections_dir().join(file_name);
        let file = self.vfs.open(&path, OpenMode::create_keep_write())?;
        file.sync_all()?;
        self.vfs.sync_dir(&self.collections_dir())
    }

    fn quarantine_unreferenced_collection_files(
        &self,
        active_paths: &BTreeSet<String>,
    ) -> io::Result<()> {
        self.quarantine_unreferenced_files_in(&self.collections_dir(), active_paths)
    }

    fn quarantine_unreferenced_append_only_segments(&self, manifest: &Manifest) -> io::Result<()> {
        let active_paths = manifest
            .append_only_segments
            .iter()
            .map(|entry| entry.path.clone())
            .collect::<BTreeSet<_>>();
        self.quarantine_unreferenced_files_in(&self.append_only_segments_dir(), &active_paths)
    }

    fn quarantine_unreferenced_files_in(
        &self,
        dir: &Path,
        active_paths: &BTreeSet<String>,
    ) -> io::Result<()> {
        if !self.vfs.exists(dir) {
            return Ok(());
        }
        for entry in self.vfs.read_dir(dir)? {
            if !entry.is_file {
                continue;
            }
            let file_name = entry.file_name;
            if active_paths.contains(&file_name) {
                continue;
            }
            let to = self.unique_quarantine_path(&file_name);
            self.vfs.rename(&entry.path, &to)?;
        }
        self.vfs.sync_dir(dir)?;
        self.vfs.sync_dir(&self.quarantine_dir())
    }

    fn validate_manifest_artifacts(&self, manifest: &Manifest) -> io::Result<()> {
        for (name, entry) in &manifest.collections {
            if entry.state != CollectionState::Active {
                continue;
            }
            if let Some(source) = &entry.source {
                if !self.vfs.is_file(Path::new(source)) {
                    return Err(invalid_data(format!(
                        "missing collection artifact for shared fork source {name}: {source}"
                    )));
                }
                continue;
            }
            let path = self.collections_dir().join(&entry.path);
            if !self.vfs.is_file(&path) {
                return Err(invalid_data(format!(
                    "missing collection artifact for {name}: {}",
                    path.display()
                )));
            }
        }
        for entry in &manifest.append_only_segments {
            if entry.state != AppendOnlySegmentState::Active {
                continue;
            }
            let path = self.append_only_segments_dir().join(&entry.path);
            if !self.vfs.is_file(&path) {
                return Err(invalid_data(format!(
                    "missing append-only artifact for {}/{}: {}",
                    entry.collection,
                    entry.segment_id,
                    path.display()
                )));
            }
        }
        Ok(())
    }

    fn read_file(&self, path: &Path) -> io::Result<Vec<u8>> {
        let mut file = self.vfs.open(path, OpenMode::read_only())?;
        file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::new();
        let mut chunk = [0; 8 * 1024];
        loop {
            let read = file.read(&mut chunk)?;
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..read]);
        }
        Ok(bytes)
    }

    fn write_file(&self, path: &Path, bytes: &[u8], sync: bool) -> io::Result<()> {
        let mut file = self.vfs.open(path, OpenMode::create_truncate())?;
        file.write_all(bytes)?;
        if sync {
            file.sync_all()?;
        }
        Ok(())
    }

    fn copy_file_durable(&self, source: &Path, dest: &Path) -> io::Result<()> {
        let bytes = self.read_file(source)?;
        self.write_file(dest, &bytes, true)?;
        if let Some(parent) = dest.parent() {
            self.vfs.sync_dir(parent)?;
        }
        Ok(())
    }

    fn unique_quarantine_path(&self, file_name: &str) -> PathBuf {
        let mut candidate = self.quarantine_dir().join(file_name);
        if !self.vfs.exists(&candidate) {
            return candidate;
        }
        for n in 1.. {
            candidate = self.quarantine_dir().join(format!("{file_name}.{n}"));
            if !self.vfs.exists(&candidate) {
                return candidate;
            }
        }
        unreachable!()
    }

    fn collections_dir(&self) -> PathBuf {
        self.root.join(COLLECTIONS_DIR)
    }

    fn append_only_segments_dir(&self) -> PathBuf {
        self.root.join(APPEND_ONLY_SEGMENTS_DIR)
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

    fn append_only_segment_file_name(&self, collection: &str, segment_id: u64) -> String {
        format!("{}-{segment_id:012}.raos", sanitize_component(collection))
    }
}

fn empty_manifest() -> Manifest {
    Manifest {
        generation: 0,
        collections: BTreeMap::new(),
        append_only_segments: Vec::new(),
        append_only_retired: BTreeMap::new(),
        fork_origin: None,
    }
}

fn active_collection_paths(manifest: &Manifest) -> BTreeSet<String> {
    manifest
        .collections
        .values()
        .filter(|entry| entry.state == CollectionState::Active && entry.source.is_none())
        .map(|entry| entry.path.clone())
        .collect()
}

fn active_append_only_segments(manifest: &Manifest) -> Vec<AppendOnlySegmentManifestEntry> {
    manifest
        .append_only_segments
        .iter()
        .filter(|entry| entry.state == AppendOnlySegmentState::Active)
        .cloned()
        .collect()
}

fn record_retired_append_only_segment(
    manifest: &mut Manifest,
    entry: &AppendOnlySegmentManifestEntry,
) {
    let retired = manifest
        .append_only_retired
        .entry(entry.collection.clone())
        .or_default();
    retired.row_count = retired.row_count.saturating_add(entry.row_count);
    retired.segment_high_water = retired.segment_high_water.max(entry.segment_id);
}

fn append_only_published_rows(manifest: &Manifest, collection: &str) -> u64 {
    manifest
        .append_only_retired
        .get(collection)
        .map(|retired| retired.row_count)
        .unwrap_or_default()
        .saturating_add(
            manifest
                .append_only_segments
                .iter()
                .filter(|entry| entry.collection == collection)
                .map(|entry| entry.row_count)
                .sum::<u64>(),
        )
}

fn next_append_only_segment_id(manifest: &Manifest, collection: &str) -> u64 {
    let active_high_water = manifest
        .append_only_segments
        .iter()
        .filter(|entry| entry.collection == collection)
        .map(|entry| entry.segment_id)
        .max()
        .unwrap_or_default();
    let retired_high_water = manifest
        .append_only_retired
        .get(collection)
        .map(|retired| retired.segment_high_water)
        .unwrap_or_default();
    active_high_water.max(retired_high_water).saturating_add(1)
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
        let source = entry
            .get("source")
            .and_then(Value::as_str)
            .map(str::to_string);
        collections.insert(
            name.clone(),
            CollectionEntry {
                state,
                path,
                source,
            },
        );
    }
    let append_only_segments = match object.get("append_only_segments") {
        Some(Value::Array(entries)) => entries
            .iter()
            .map(append_only_segment_from_value)
            .collect::<io::Result<Vec<_>>>()?,
        Some(Value::Null) | None => Vec::new(),
        Some(_) => {
            return Err(invalid_data(
                "manifest append_only_segments must be an array",
            ))
        }
    };
    let mut append_only_retired = match object.get("append_only_retired") {
        Some(Value::Object(entries)) => entries
            .iter()
            .map(|(collection, value)| {
                append_only_retired_from_value(value).map(|state| (collection.clone(), state))
            })
            .collect::<io::Result<BTreeMap<_, _>>>()?,
        Some(Value::Null) | None => BTreeMap::new(),
        Some(_) => {
            return Err(invalid_data(
                "manifest append_only_retired must be an object",
            ))
        }
    };
    for entry in &append_only_segments {
        let retired = append_only_retired
            .entry(entry.collection.clone())
            .or_default();
        retired.segment_high_water = retired.segment_high_water.max(entry.segment_id);
    }
    let fork_origin = match object.get("fork_origin") {
        Some(Value::Object(origin)) => Some(fork_origin_from_object(origin)?),
        Some(Value::Null) | None => None,
        Some(_) => return Err(invalid_data("manifest fork_origin must be an object")),
    };
    Ok(Manifest {
        generation,
        collections,
        append_only_segments,
        append_only_retired,
        fork_origin,
    })
}

fn append_only_segment_from_value(value: &Value) -> io::Result<AppendOnlySegmentManifestEntry> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid_data("append-only segment entry must be an object"))?;
    let collection = object
        .get("collection")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_data("append-only segment collection is missing"))?
        .to_string();
    let segment_id = object
        .get("segment_id")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_data("append-only segment id is missing"))?;
    let state = object
        .get("state")
        .and_then(Value::as_str)
        .map(AppendOnlySegmentState::parse)
        .transpose()?
        .unwrap_or(AppendOnlySegmentState::Active);
    let path = object
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_data("append-only segment path is missing"))?
        .to_string();
    let codec = match object
        .get("codec")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_data("append-only segment codec is missing"))?
    {
        "zstd" => AppendOnlySegmentCodec::Zstd,
        "none" => AppendOnlySegmentCodec::None,
        other => {
            return Err(invalid_data(format!(
                "unknown append-only segment codec: {other}"
            )))
        }
    };
    let chunk_size = object
        .get("chunk_size")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_data("append-only segment chunk_size is missing"))
        .and_then(u32_from_u64)?;
    let row_count = object
        .get("row_count")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_data("append-only segment row_count is missing"))?;
    let primary_min = object
        .get("primary_min")
        .and_then(Value::as_str)
        .map(str::to_string);
    let primary_max = object
        .get("primary_max")
        .and_then(Value::as_str)
        .map(str::to_string);
    let primary_bloom = object
        .get("primary_bloom")
        .map(append_only_segment_bloom_from_value)
        .transpose()?;
    let retention_min_ms = object.get("retention_min_ms").and_then(Value::as_i64);
    let retention_max_ms = object.get("retention_max_ms").and_then(Value::as_i64);
    let chunk_checksums = object
        .get("chunk_checksums")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_data("append-only segment chunk_checksums are missing"))?
        .iter()
        .map(chunk_checksum_from_value)
        .collect::<io::Result<Vec<_>>>()?;
    Ok(AppendOnlySegmentManifestEntry {
        collection,
        segment_id,
        state,
        path,
        codec,
        chunk_size,
        row_count,
        retention_min_ms,
        retention_max_ms,
        primary_min,
        primary_max,
        primary_bloom,
        chunk_checksums,
    })
}

fn append_only_segment_bloom_from_value(value: &Value) -> io::Result<AppendOnlySegmentBloom> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid_data("append-only segment bloom must be an object"))?;
    let num_hashes = object
        .get("num_hashes")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_data("append-only segment bloom num_hashes is missing"))
        .and_then(u8_from_u64)?;
    let bit_size = object
        .get("bit_size")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_data("append-only segment bloom bit_size is missing"))
        .and_then(u32_from_u64)?;
    let inserted = object
        .get("inserted")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_data("append-only segment bloom inserted is missing"))
        .and_then(u32_from_u64)?;
    let bits_hex = object
        .get("bits_hex")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_data("append-only segment bloom bits_hex is missing"))?
        .to_string();
    Ok(AppendOnlySegmentBloom {
        num_hashes,
        bit_size,
        inserted,
        bits_hex,
    })
}

fn append_only_retired_from_value(value: &Value) -> io::Result<AppendOnlyRetiredState> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid_data("append-only retired entry must be an object"))?;
    let row_count = object
        .get("row_count")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_data("append-only retired row_count is missing"))?;
    let segment_high_water = object
        .get("segment_high_water")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_data("append-only retired segment_high_water is missing"))?;
    Ok(AppendOnlyRetiredState {
        row_count,
        segment_high_water,
    })
}

fn chunk_checksum_from_value(value: &Value) -> io::Result<AppendOnlySegmentChunkChecksum> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid_data("append-only segment chunk checksum must be an object"))?;
    let offset = object
        .get("offset")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_data("append-only segment chunk offset is missing"))?;
    let len = object
        .get("len")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_data("append-only segment chunk len is missing"))
        .and_then(u32_from_u64)?;
    let checksum = object
        .get("checksum")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_data("append-only segment chunk checksum is missing"))?
        .to_string();
    Ok(AppendOnlySegmentChunkChecksum {
        offset,
        len,
        checksum,
    })
}

fn fork_origin_from_object(object: &Map<String, Value>) -> io::Result<ForkOrigin> {
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_data("manifest fork_origin name is missing"))?
        .to_string();
    let parent_store = object
        .get("parent_store")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_data("manifest fork_origin parent_store is missing"))?
        .to_string();
    let fork_lsn = object
        .get("fork_lsn")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_data("manifest fork_origin fork_lsn is missing"))?;
    Ok(ForkOrigin {
        name,
        parent_store,
        fork_lsn,
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
        // Only emit `source` for shared-by-reference fork entries, so a normal
        // parent manifest stays byte-identical (and checksum-stable) to before.
        if let Some(source) = &entry.source {
            entry_object.insert("source".to_string(), Value::String(source.clone()));
        }
        collections.insert(name.clone(), Value::Object(entry_object));
    }
    let append_only_segments = manifest
        .append_only_segments
        .iter()
        .map(append_only_segment_to_value)
        .collect::<Vec<_>>();

    let mut object = Map::new();
    object.insert("format_version".to_string(), Value::from(FORMAT_VERSION));
    object.insert("generation".to_string(), Value::from(manifest.generation));
    object.insert("collections".to_string(), Value::Object(collections));
    if !append_only_segments.is_empty() {
        object.insert(
            "append_only_segments".to_string(),
            Value::Array(append_only_segments),
        );
    }
    if !manifest.append_only_retired.is_empty() {
        let mut retired = Map::new();
        for (collection, state) in &manifest.append_only_retired {
            let mut state_object = Map::new();
            state_object.insert("row_count".to_string(), Value::from(state.row_count));
            state_object.insert(
                "segment_high_water".to_string(),
                Value::from(state.segment_high_water),
            );
            retired.insert(collection.clone(), Value::Object(state_object));
        }
        object.insert("append_only_retired".to_string(), Value::Object(retired));
    }
    // Only emit `fork_origin` for a fork's own manifest, so a parent manifest is
    // unchanged from the pre-fork format.
    if let Some(origin) = &manifest.fork_origin {
        let mut origin_object = Map::new();
        origin_object.insert("name".to_string(), Value::String(origin.name.clone()));
        origin_object.insert(
            "parent_store".to_string(),
            Value::String(origin.parent_store.clone()),
        );
        origin_object.insert("fork_lsn".to_string(), Value::from(origin.fork_lsn));
        object.insert("fork_origin".to_string(), Value::Object(origin_object));
    }
    object
}

fn append_only_segment_to_value(entry: &AppendOnlySegmentManifestEntry) -> Value {
    let mut object = Map::new();
    object.insert(
        "collection".to_string(),
        Value::String(entry.collection.clone()),
    );
    object.insert("segment_id".to_string(), Value::from(entry.segment_id));
    object.insert(
        "state".to_string(),
        Value::String(entry.state.as_str().to_string()),
    );
    object.insert("path".to_string(), Value::String(entry.path.clone()));
    object.insert(
        "codec".to_string(),
        Value::String(entry.codec.as_str().to_string()),
    );
    object.insert("chunk_size".to_string(), Value::from(entry.chunk_size));
    object.insert("row_count".to_string(), Value::from(entry.row_count));
    if let Some(retention_min_ms) = entry.retention_min_ms {
        object.insert(
            "retention_min_ms".to_string(),
            Value::from(retention_min_ms),
        );
    }
    if let Some(retention_max_ms) = entry.retention_max_ms {
        object.insert(
            "retention_max_ms".to_string(),
            Value::from(retention_max_ms),
        );
    }
    if let Some(primary_min) = &entry.primary_min {
        object.insert(
            "primary_min".to_string(),
            Value::String(primary_min.clone()),
        );
    }
    if let Some(primary_max) = &entry.primary_max {
        object.insert(
            "primary_max".to_string(),
            Value::String(primary_max.clone()),
        );
    }
    if let Some(primary_bloom) = &entry.primary_bloom {
        object.insert(
            "primary_bloom".to_string(),
            append_only_segment_bloom_to_value(primary_bloom),
        );
    }
    object.insert(
        "chunk_checksums".to_string(),
        Value::Array(
            entry
                .chunk_checksums
                .iter()
                .map(chunk_checksum_to_value)
                .collect(),
        ),
    );
    Value::Object(object)
}

fn append_only_segment_bloom_to_value(bloom: &AppendOnlySegmentBloom) -> Value {
    let mut object = Map::new();
    object.insert("num_hashes".to_string(), Value::from(bloom.num_hashes));
    object.insert("bit_size".to_string(), Value::from(bloom.bit_size));
    object.insert("inserted".to_string(), Value::from(bloom.inserted));
    object.insert(
        "bits_hex".to_string(),
        Value::String(bloom.bits_hex.clone()),
    );
    Value::Object(object)
}

fn chunk_checksum_to_value(chunk: &AppendOnlySegmentChunkChecksum) -> Value {
    let mut object = Map::new();
    object.insert("offset".to_string(), Value::from(chunk.offset));
    object.insert("len".to_string(), Value::from(chunk.len));
    object.insert(
        "checksum".to_string(),
        Value::String(chunk.checksum.clone()),
    );
    Value::Object(object)
}

fn u32_from_u64(value: u64) -> io::Result<u32> {
    u32::try_from(value).map_err(|_| invalid_data(format!("value does not fit u32: {value}")))
}

fn u8_from_u64(value: u64) -> io::Result<u8> {
    u8::try_from(value).map_err(|_| invalid_data(format!("value does not fit u8: {value}")))
}

/// Escape an arbitrary name into a single safe path component (no separators),
/// mirroring the collection-file escaping so a fork name like `tenant/exp 1`
/// becomes one directory.
fn sanitize_component(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for byte in name.as_bytes() {
        match *byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-' | b'.' => {
                out.push(*byte as char);
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Copy a file's bytes to `dest` durably (fsync file + parent dir). Used to
/// hydrate a fork's private collection copy on copy-on-write.
fn invalid_data(message: impl ToString) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}

#[cfg(test)]
mod tests;
