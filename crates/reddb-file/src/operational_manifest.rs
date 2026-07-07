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

use crate::append_only_segment::{
    append_only_segment_chunk_checksums, decode_append_only_segment, AppendOnlySegmentBloom,
    AppendOnlySegmentChunkChecksum, AppendOnlySegmentCodec, APPEND_ONLY_SEGMENT_CHUNK_BYTES,
};

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

/// Where a store fork came from: the parent store's identity and the durable LSN
/// the fork is pinned at. Recorded in the fork's own operational manifest so a
/// listing can report each fork's parent and fork LSN without opening the parent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkOrigin {
    /// The fork's own name (as given to `FORK STORE AS <name>`).
    pub name: String,
    /// Identity of the parent store this fork was taken from.
    pub parent_store: String,
    /// The durable LSN the fork is pinned at (the parent's current durable LSN
    /// at fork-create time).
    pub fork_lsn: u64,
}

/// A single row of the fork listing (`SHOW FORKS`): the fork name plus origin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkInfo {
    pub name: String,
    pub parent_store: String,
    pub fork_lsn: u64,
    pub hydration_state: ForkHydrationState,
    pub collections_total: u64,
    pub shared_by_reference: u64,
    pub hydrating: u64,
    pub hydrated: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkHydrationState {
    SharedByReference,
    Hydrating,
    Hydrated,
}

impl ForkHydrationState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SharedByReference => "shared_by_reference",
            Self::Hydrating => "hydrating",
            Self::Hydrated => "hydrated",
        }
    }
}

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
    /// Present only for a store fork's own manifest; `None` for a parent store.
    fork_origin: Option<ForkOrigin>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendOnlySegmentManifestEntry {
    pub collection: String,
    pub segment_id: u64,
    pub path: String,
    pub codec: AppendOnlySegmentCodec,
    pub chunk_size: u32,
    pub row_count: u64,
    pub primary_min: Option<String>,
    pub primary_max: Option<String>,
    pub primary_bloom: Option<AppendOnlySegmentBloom>,
    pub chunk_checksums: Vec<AppendOnlySegmentChunkChecksum>,
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
                    append_only_segments: Vec::new(),
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
                    let _ = fs::remove_file(self.collections_dir().join(path));
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
                source: None,
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
        if !self
            .fork_referenced_collection_paths()?
            .contains(&entry.path)
        {
            let _ = fs::remove_file(self.collections_dir().join(entry.path));
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

    /// A fork's own operational manifest, rooted under this store's `forks/` dir.
    /// The fork is a full operational store root in its own right.
    pub fn fork_handle(&self, name: &str) -> Self {
        Self {
            root: self.forks_dir().join(sanitize_component(name)),
        }
    }

    /// Create a store fork pinned at `fork_lsn` (ADR 0070). O(metadata): every
    /// active parent collection is referenced by absolute source path — no data
    /// file is copied at create time. The fork gets its own operational manifest
    /// carrying [`ForkOrigin`]; mutable collection files hydrate lazily on first
    /// write (see [`hydrate_collection`](Self::hydrate_collection)).
    ///
    /// This is the *store fork* surface, distinct from the VCS `CHECKPOINT`/branch
    /// model (#1567): forks live on the storage/deploy axis.
    pub fn create_fork(&self, name: &str, fork_lsn: u64) -> io::Result<()> {
        let parent = self
            .load_current()?
            .ok_or_else(|| invalid_data("cannot fork a store with no operational manifest"))?;
        let fork = self.fork_handle(name);
        if fork.load_current()?.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("store fork already exists: {name}"),
            ));
        }
        fork.ensure_dirs()?;

        let mut collections = BTreeMap::new();
        for (cname, entry) in &parent.collections {
            if entry.state != CollectionState::Active {
                continue;
            }
            let source = self.collections_dir().join(&entry.path);
            collections.insert(
                cname.clone(),
                CollectionEntry {
                    state: CollectionState::Active,
                    path: entry.path.clone(),
                    source: Some(source.to_string_lossy().into_owned()),
                },
            );
        }

        let manifest = Manifest {
            generation: 0,
            collections,
            append_only_segments: Vec::new(),
            fork_origin: Some(ForkOrigin {
                name: name.to_string(),
                parent_store: self.store_identity(),
                fork_lsn,
            }),
        };
        fork.publish(&manifest)?;
        sync_dir(&self.forks_dir())
    }

    /// Hydrate a fork collection's private copy (copy-on-write): called on the
    /// *fork* handle before the fork first writes a shared collection. Copies the
    /// referenced parent bytes into the fork's own `collections/` dir and drops
    /// the shared reference. Idempotent and a no-op for owned collections.
    pub fn hydrate_collection(&self, name: &str) -> io::Result<()> {
        let mut manifest = match self.load_current()? {
            Some(manifest) => manifest,
            None => return Ok(()),
        };
        let Some(entry) = manifest.collections.get_mut(name) else {
            return Ok(());
        };
        let Some(source) = entry.source.clone() else {
            return Ok(());
        };
        self.ensure_dirs()?;
        let dest = self.collections_dir().join(&entry.path);
        copy_file_durable(Path::new(&source), &dest)?;
        entry.source = None;
        manifest.generation += 1;
        self.publish(&manifest)
    }

    /// Preserve fork isolation before the *parent* mutates a collection file in
    /// place: any live fork still sharing that collection by reference gets its
    /// as-of-fork snapshot materialized first (copy-on-write on the parent side).
    /// Call on the parent handle immediately before an in-place collection write.
    pub fn materialize_forks_before_write(&self, collection: &str) -> io::Result<()> {
        let parent = match self.load_current()? {
            Some(manifest) => manifest,
            None => return Ok(()),
        };
        let Some(entry) = parent.collections.get(collection) else {
            return Ok(());
        };
        let source = self.collections_dir().join(&entry.path);
        for fork in self.fork_handles()? {
            let mut manifest = match fork.load_current()? {
                Some(manifest) => manifest,
                None => continue,
            };
            let Some(fork_entry) = manifest.collections.get_mut(collection) else {
                continue;
            };
            if fork_entry.source.is_none() {
                continue;
            }
            fork.ensure_dirs()?;
            let dest = fork.collections_dir().join(&fork_entry.path);
            copy_file_durable(&source, &dest)?;
            fork_entry.source = None;
            manifest.generation += 1;
            fork.publish(&manifest)?;
        }
        Ok(())
    }

    /// List every store fork of this store: name, parent identity, and fork LSN.
    pub fn list_forks(&self) -> io::Result<Vec<ForkInfo>> {
        let mut out = Vec::new();
        let entries = match fs::read_dir(self.forks_dir()) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(err) => return Err(err),
        };
        for entry in entries {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            let fork = Self { root: entry.path() };
            if let Some(manifest) = fork.load_current()? {
                let Some(origin) = manifest.fork_origin.as_ref() else {
                    continue;
                };
                let progress = fork.hydration_progress(&manifest);
                out.push(ForkInfo {
                    name: origin.name.clone(),
                    parent_store: origin.parent_store.clone(),
                    fork_lsn: origin.fork_lsn,
                    hydration_state: progress.state,
                    collections_total: progress.collections_total,
                    shared_by_reference: progress.shared_by_reference,
                    hydrating: progress.hydrating,
                    hydrated: progress.hydrated,
                });
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    /// Drop a store fork: remove its manifest and garbage-collect its private
    /// (hydrated) files. Shared-by-reference entries are only path strings, so
    /// the parent store's data is never touched. Idempotent. Returns whether a
    /// fork was actually removed.
    pub fn drop_fork(&self, name: &str) -> io::Result<bool> {
        let fork = self.fork_handle(name);
        if !fork.root.exists() {
            return Ok(false);
        }
        fs::remove_dir_all(&fork.root)?;
        let _ = sync_dir(&self.forks_dir());
        if let Some(manifest) = self.load_current()? {
            let protected_paths = self.protected_collection_paths(&manifest)?;
            self.quarantine_unreferenced_collection_files(&protected_paths)?;
            self.quarantine_unreferenced_append_only_segments(&manifest)?;
        }
        Ok(true)
    }

    pub fn publish_append_only_segment(
        &self,
        collection: &str,
        segment_id: u64,
        codec: AppendOnlySegmentCodec,
        bytes: &[u8],
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
        if segment_path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("closed append-only segment already exists: {path}"),
            ));
        }
        {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&segment_path)?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        sync_dir(&self.append_only_segments_dir())?;

        let entry = AppendOnlySegmentManifestEntry {
            collection: collection.to_string(),
            segment_id,
            path,
            codec,
            chunk_size: APPEND_ONLY_SEGMENT_CHUNK_BYTES,
            row_count: decoded.rows.len() as u64,
            primary_min: decoded.primary_min.as_ref().map(hex::encode),
            primary_max: decoded.primary_max.as_ref().map(hex::encode),
            primary_bloom: decoded.primary_bloom.clone(),
            chunk_checksums: append_only_segment_chunk_checksums(bytes),
        };
        manifest.append_only_segments.push(entry.clone());
        manifest
            .append_only_segments
            .sort_by(|a, b| (&a.collection, a.segment_id).cmp(&(&b.collection, b.segment_id)));
        manifest.generation += 1;
        self.publish(&manifest)?;
        Ok(entry)
    }

    /// Detach a store fork into an independent operational store root. All
    /// shared-by-reference collections are hydrated before the fork is moved out
    /// from under the parent store; the final manifest then drops its
    /// [`ForkOrigin`], so parent retention and WAL pruning no longer see it as a
    /// live fork.
    ///
    /// The operation is restartable across the two durable phases:
    /// - if hydration finished but the fork is still nested, rerun hydrates as a
    ///   no-op and moves it;
    /// - if the move finished but origin clearing did not, rerun finishes the
    ///   detached manifest in place.
    pub fn detach_fork(&self, name: &str) -> io::Result<Option<Self>> {
        let fork = self.fork_handle(name);
        let detached = self.detached_fork_handle(name);

        if detached.root.exists() {
            if fork.root.exists() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("detached store already exists for fork: {name}"),
                ));
            }
            let was_fork = detached.fork_origin()?.is_some();
            detached.clear_fork_origin()?;
            return Ok(was_fork.then_some(detached));
        }

        if !fork.root.exists() {
            return Ok(None);
        }

        fork.hydrate_shared_collections()?;
        if let Some(parent) = detached.root.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&fork.root, &detached.root)?;
        sync_dir(&self.forks_dir())?;
        if let Some(parent) = detached.root.parent() {
            sync_dir(parent)?;
        }
        detached.clear_fork_origin()?;
        Ok(Some(detached))
    }

    /// Read this manifest's fork origin, if it is a fork.
    pub fn fork_origin(&self) -> io::Result<Option<ForkOrigin>> {
        Ok(self
            .load_current()?
            .and_then(|manifest| manifest.fork_origin))
    }

    fn forks_dir(&self) -> PathBuf {
        self.root.join(FORKS_DIR)
    }

    fn fork_handles(&self) -> io::Result<Vec<Self>> {
        let mut out = Vec::new();
        let entries = match fs::read_dir(self.forks_dir()) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(err) => return Err(err),
        };
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                out.push(Self { root: entry.path() });
            }
        }
        Ok(out)
    }

    fn detached_fork_handle(&self, name: &str) -> Self {
        let root_name = self
            .root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "store.ops".to_string());
        Self {
            root: self
                .root
                .with_file_name(format!("{root_name}.detached-{}", sanitize_component(name))),
        }
    }

    fn hydrate_shared_collections(&self) -> io::Result<()> {
        let mut manifest = match self.load_current()? {
            Some(manifest) => manifest,
            None => return Ok(()),
        };
        let mut changed = false;
        self.ensure_dirs()?;
        for entry in manifest.collections.values_mut() {
            let Some(source) = entry.source.clone() else {
                continue;
            };
            let dest = self.collections_dir().join(&entry.path);
            copy_file_durable(Path::new(&source), &dest)?;
            entry.source = None;
            changed = true;
        }
        if changed {
            manifest.generation += 1;
            self.publish(&manifest)?;
        }
        Ok(())
    }

    fn clear_fork_origin(&self) -> io::Result<()> {
        let mut manifest = match self.load_current()? {
            Some(manifest) => manifest,
            None => return Ok(()),
        };
        if manifest.fork_origin.is_none() {
            return Ok(());
        }
        manifest.fork_origin = None;
        manifest.generation += 1;
        self.publish(&manifest)
    }

    fn protected_collection_paths(&self, manifest: &Manifest) -> io::Result<BTreeSet<String>> {
        let mut paths = active_collection_paths(manifest);
        paths.extend(self.fork_referenced_collection_paths()?);
        Ok(paths)
    }

    fn fork_referenced_collection_paths(&self) -> io::Result<BTreeSet<String>> {
        let mut paths = BTreeSet::new();
        let parent_identity = self.store_identity();
        let parent_collections_dir = self.collections_dir();
        for fork in self.fork_handles()? {
            let Some(manifest) = fork.load_current()? else {
                continue;
            };
            if manifest
                .fork_origin
                .as_ref()
                .map(|origin| origin.parent_store.as_str())
                != Some(parent_identity.as_str())
            {
                continue;
            }
            for entry in manifest.collections.values() {
                let Some(source) = &entry.source else {
                    continue;
                };
                if Path::new(source) == parent_collections_dir.join(&entry.path) {
                    paths.insert(entry.path.clone());
                }
            }
        }
        Ok(paths)
    }

    fn hydration_progress(&self, manifest: &Manifest) -> ForkHydrationProgress {
        let mut progress = ForkHydrationProgress::default();
        for entry in manifest.collections.values() {
            if entry.state != CollectionState::Active {
                continue;
            }
            progress.collections_total += 1;
            if entry.source.is_none() {
                progress.hydrated += 1;
                continue;
            }
            if self.collections_dir().join(&entry.path).exists() {
                progress.hydrating += 1;
            } else {
                progress.shared_by_reference += 1;
            }
        }
        progress.state = if progress.hydrating > 0 {
            ForkHydrationState::Hydrating
        } else if progress.shared_by_reference > 0 {
            ForkHydrationState::SharedByReference
        } else {
            ForkHydrationState::Hydrated
        };
        progress
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
            .map(|manifest| manifest.append_only_segments)
            .unwrap_or_default())
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
        fs::write(self.root.join(NEXT_MANIFEST_FILE), bytes)
    }

    fn ensure_dirs(&self) -> io::Result<()> {
        fs::create_dir_all(self.collections_dir())?;
        fs::create_dir_all(self.append_only_segments_dir())?;
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
        if !dir.exists() {
            return Ok(());
        }
        for entry in fs::read_dir(dir)? {
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
        sync_dir(dir)?;
        sync_dir(&self.quarantine_dir())
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

#[derive(Debug, Clone, Copy)]
struct ForkHydrationProgress {
    state: ForkHydrationState,
    collections_total: u64,
    shared_by_reference: u64,
    hydrating: u64,
    hydrated: u64,
}

impl Default for ForkHydrationProgress {
    fn default() -> Self {
        Self {
            state: ForkHydrationState::Hydrated,
            collections_total: 0,
            shared_by_reference: 0,
            hydrating: 0,
            hydrated: 0,
        }
    }
}

fn empty_manifest() -> Manifest {
    Manifest {
        generation: 0,
        collections: BTreeMap::new(),
        append_only_segments: Vec::new(),
        fork_origin: None,
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
    let fork_origin = match object.get("fork_origin") {
        Some(Value::Object(origin)) => Some(fork_origin_from_object(origin)?),
        Some(Value::Null) | None => None,
        Some(_) => return Err(invalid_data("manifest fork_origin must be an object")),
    };
    Ok(Manifest {
        generation,
        collections,
        append_only_segments,
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
        path,
        codec,
        chunk_size,
        row_count,
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
    object.insert("path".to_string(), Value::String(entry.path.clone()));
    object.insert(
        "codec".to_string(),
        Value::String(entry.codec.as_str().to_string()),
    );
    object.insert("chunk_size".to_string(), Value::from(entry.chunk_size));
    object.insert("row_count".to_string(), Value::from(entry.row_count));
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
fn copy_file_durable(source: &Path, dest: &Path) -> io::Result<()> {
    let bytes = fs::read(source)?;
    let mut file = File::create(dest)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    if let Some(parent) = dest.parent() {
        sync_dir(parent)?;
    }
    Ok(())
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
                source: None,
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

    // Write bytes straight to a collection's file so the fork isolation tests
    // have observable per-collection content to diverge.
    fn write_collection(manifest: &OperationalManifest, name: &str, bytes: &[u8]) {
        fs::write(manifest.collection_path_for_test(name), bytes).unwrap();
    }

    fn read_collection(manifest: &OperationalManifest, name: &str) -> Vec<u8> {
        fs::read(manifest.collection_path_for_test(name)).unwrap()
    }

    #[test]
    fn fork_create_is_metadata_only_and_copies_no_data() {
        let path = temp_db_path("fork_metadata");
        let parent = OperationalManifest::for_db_path(&path);
        parent.recover_or_bootstrap(&["users".to_string()]).unwrap();
        write_collection(&parent, "users", b"parent-rows");

        parent.create_fork("exp", 42).unwrap();
        let fork = parent.fork_handle("exp");

        // The fork references the parent's collection file by path; it did not
        // materialize its own copy at create time.
        assert!(!fork.collection_path_for_test("users").exists());
        // Listing reports the parent identity and the pinned fork LSN.
        let forks = parent.list_forks().unwrap();
        assert_eq!(forks.len(), 1);
        assert_eq!(forks[0].name, "exp");
        assert_eq!(forks[0].fork_lsn, 42);
        assert_eq!(forks[0].parent_store, parent.store_identity());
        // The fork's own manifest carries the origin.
        assert_eq!(fork.fork_origin().unwrap().unwrap().fork_lsn, 42);
        // The parent is not itself a fork.
        assert!(parent.fork_origin().unwrap().is_none());
    }

    #[test]
    fn fork_writes_are_invisible_to_parent() {
        let path = temp_db_path("fork_to_parent");
        let parent = OperationalManifest::for_db_path(&path);
        parent.recover_or_bootstrap(&["users".to_string()]).unwrap();
        write_collection(&parent, "users", b"v0");
        parent.create_fork("exp", 1).unwrap();
        let fork = parent.fork_handle("exp");

        // Fork writes: hydrate (copy-on-write) then mutate the fork's own copy.
        fork.hydrate_collection("users").unwrap();
        write_collection(&fork, "users", b"fork-only");

        assert_eq!(read_collection(&fork, "users"), b"fork-only");
        // Parent is untouched by the fork's write.
        assert_eq!(read_collection(&parent, "users"), b"v0");
        // Hydration dropped the shared reference.
        assert!(fork.fork_origin().unwrap().is_some());
    }

    #[test]
    fn parent_writes_after_fork_are_invisible_to_fork() {
        let path = temp_db_path("parent_to_fork");
        let parent = OperationalManifest::for_db_path(&path);
        parent.recover_or_bootstrap(&["users".to_string()]).unwrap();
        write_collection(&parent, "users", b"as-of-fork");
        parent.create_fork("exp", 5).unwrap();
        let fork = parent.fork_handle("exp");

        // Parent write: snapshot referencing forks first (copy-on-write on the
        // parent side), then mutate the parent's file in place.
        parent.materialize_forks_before_write("users").unwrap();
        write_collection(&parent, "users", b"parent-moved-on");

        // Fork still sees the as-of-fork snapshot; parent's later write is invisible.
        assert_eq!(read_collection(&fork, "users"), b"as-of-fork");
        assert_eq!(read_collection(&parent, "users"), b"parent-moved-on");
    }

    #[test]
    fn drop_fork_removes_private_files_and_leaves_parent_intact() {
        let path = temp_db_path("drop_fork");
        let parent = OperationalManifest::for_db_path(&path);
        parent.recover_or_bootstrap(&["users".to_string()]).unwrap();
        write_collection(&parent, "users", b"keep-me");
        parent.create_fork("exp", 9).unwrap();
        let fork = parent.fork_handle("exp");
        fork.hydrate_collection("users").unwrap();
        write_collection(&fork, "users", b"fork-private");
        let fork_root = fork.root.clone();

        assert!(parent.drop_fork("exp").unwrap());
        // Fork-private files are gone; the parent's data survives untouched.
        assert!(!fork_root.exists());
        assert_eq!(read_collection(&parent, "users"), b"keep-me");
        assert!(parent.list_forks().unwrap().is_empty());
        // Dropping again is idempotent.
        assert!(!parent.drop_fork("exp").unwrap());
    }

    #[test]
    fn parent_recovery_retains_dropped_collection_file_referenced_by_live_fork() {
        let path = temp_db_path("fork_retains_parent_drop");
        let parent = OperationalManifest::for_db_path(&path);
        parent.recover_or_bootstrap(&["users".to_string()]).unwrap();
        write_collection(&parent, "users", b"as-of-fork");
        parent.create_fork("exp", 12).unwrap();

        parent.begin_drop_collection("users").unwrap();
        let completed = parent.recover_or_bootstrap(&[]).unwrap();

        assert_eq!(completed, vec!["users".to_string()]);
        assert!(
            parent.collection_path_for_test("users").exists(),
            "live fork source must keep the parent artifact available"
        );
        let fork = parent.fork_handle("exp");
        fork.hydrate_collection("users").unwrap();
        assert_eq!(read_collection(&fork, "users"), b"as-of-fork");
    }

    #[test]
    fn dropping_last_referencing_fork_releases_parent_file_to_retention() {
        let path = temp_db_path("fork_drop_releases_parent_file");
        let parent = OperationalManifest::for_db_path(&path);
        parent.recover_or_bootstrap(&["users".to_string()]).unwrap();
        write_collection(&parent, "users", b"as-of-fork");
        parent.create_fork("exp", 12).unwrap();
        parent.begin_drop_collection("users").unwrap();
        parent.recover_or_bootstrap(&[]).unwrap();

        assert!(parent.drop_fork("exp").unwrap());

        assert!(!parent.collection_path_for_test("users").exists());
        assert!(parent.quarantine_path_for_test("users.rcol").exists());
    }

    #[test]
    fn detach_fork_hydrates_and_survives_parent_removal() {
        let path = temp_db_path("detach_fork");
        let parent = OperationalManifest::for_db_path(&path);
        parent.recover_or_bootstrap(&["users".to_string()]).unwrap();
        write_collection(&parent, "users", b"successful-experiment");
        parent.create_fork("exp", 21).unwrap();

        let detached = parent.detach_fork("exp").unwrap().unwrap();

        assert!(parent.list_forks().unwrap().is_empty());
        assert!(detached.fork_origin().unwrap().is_none());
        assert_eq!(
            read_collection(&detached, "users"),
            b"successful-experiment"
        );

        fs::remove_dir_all(&parent.root).unwrap();
        assert_eq!(
            detached.recover_or_bootstrap(&[]).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            read_collection(&detached, "users"),
            b"successful-experiment"
        );
    }

    #[test]
    fn detach_fork_resumes_after_move_before_origin_clear() {
        let path = temp_db_path("detach_fork_resume");
        let parent = OperationalManifest::for_db_path(&path);
        parent.recover_or_bootstrap(&["users".to_string()]).unwrap();
        write_collection(&parent, "users", b"as-of-fork");
        parent.create_fork("exp", 34).unwrap();

        let fork = parent.fork_handle("exp");
        fork.hydrate_shared_collections().unwrap();
        let detached = parent.detached_fork_handle("exp");
        fs::rename(&fork.root, &detached.root).unwrap();

        let resumed = parent.detach_fork("exp").unwrap().unwrap();

        assert_eq!(resumed.root, detached.root);
        assert!(parent.list_forks().unwrap().is_empty());
        assert!(resumed.fork_origin().unwrap().is_none());
        assert_eq!(read_collection(&resumed, "users"), b"as-of-fork");
    }

    #[test]
    fn detach_fork_releases_parent_retention_pin() {
        let path = temp_db_path("detach_releases_parent_pin");
        let parent = OperationalManifest::for_db_path(&path);
        parent.recover_or_bootstrap(&["users".to_string()]).unwrap();
        write_collection(&parent, "users", b"as-of-fork");
        parent.create_fork("exp", 55).unwrap();
        parent.begin_drop_collection("users").unwrap();
        parent.recover_or_bootstrap(&[]).unwrap();
        assert!(parent.collection_path_for_test("users").exists());

        let detached = parent.detach_fork("exp").unwrap().unwrap();
        parent.recover_or_bootstrap(&[]).unwrap();

        assert!(!parent.collection_path_for_test("users").exists());
        assert_eq!(read_collection(&detached, "users"), b"as-of-fork");
    }

    #[test]
    fn fork_creation_rejects_duplicate_and_missing_parent() {
        let path = temp_db_path("fork_errors");
        let parent = OperationalManifest::for_db_path(&path);
        // A store with no manifest cannot be forked.
        assert_eq!(
            parent.create_fork("exp", 0).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        parent.recover_or_bootstrap(&[]).unwrap();
        parent.create_fork("exp", 0).unwrap();
        assert_eq!(
            parent.create_fork("exp", 0).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
    }
}
