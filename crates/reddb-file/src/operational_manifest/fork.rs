//! Store-fork lifecycle for the operational manifest (ADR 0070).
//!
//! Fork create/drop/detach/promote, copy-on-write hydration, and the fork
//! listing live here. These are the *store fork* surface — storage mechanics for
//! experiment-and-discard workflows — deliberately distinct from the VCS data
//! model's `branch`/`CHECKPOINT` vocabulary (#1567).
//!
//! The methods are inherent-impl blocks on [`OperationalManifest`], split out
//! from the parent module so each file-contract file stays under the layout
//! authority's per-file line budget. Behavior is identical to the pre-split code.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{
    copy_file_durable, invalid_data, sanitize_component, sync_dir, CollectionEntry,
    CollectionState, Manifest, OperationalManifest, FORKS_DIR,
};

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

#[derive(Debug, Clone)]
pub struct PromoteForkOutcome {
    pub name: String,
    pub fork_lsn: u64,
    pub archived_parent: OperationalManifest,
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

impl OperationalManifest {
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
            append_only_retired: BTreeMap::new(),
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

    /// Promote a store fork to the primary operational root.
    ///
    /// The promoted fork is first hydrated through the same materialization path
    /// used by restore/fork detach. The superseded primary is then moved to a
    /// deterministic retired root, so its disposition is explicit and cannot be
    /// mistaken for the active store.
    pub fn promote_fork(&self, name: &str) -> io::Result<Option<PromoteForkOutcome>> {
        let fork = self.fork_handle(name);
        if !fork.root.exists() {
            return Ok(None);
        }
        let origin = fork
            .fork_origin()?
            .ok_or_else(|| invalid_data(format!("store fork is missing origin: {name}")))?;
        if origin.parent_store != self.store_identity() {
            return Err(invalid_data(format!(
                "store fork {name} belongs to {}, not {}",
                origin.parent_store,
                self.store_identity()
            )));
        }

        let staging = self.promoting_fork_handle(name);
        if staging.root.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("store fork promotion staging path already exists: {name}"),
            ));
        }
        let archived_parent = self.archived_parent_handle(name);
        if archived_parent.root.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("retired parent store already exists for promoted fork: {name}"),
            ));
        }

        fork.hydrate_shared_collections()?;
        if let Some(parent) = staging.root.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::rename(&fork.root, &staging.root)?;
        sync_dir(&self.forks_dir())?;
        if let Some(parent) = staging.root.parent() {
            sync_dir(parent)?;
        }

        fs::rename(&self.root, &archived_parent.root)?;
        if let Some(parent) = self.root.parent() {
            sync_dir(parent)?;
        }
        fs::rename(&staging.root, &self.root)?;
        if let Some(parent) = self.root.parent() {
            sync_dir(parent)?;
        }
        self.clear_fork_origin()?;

        Ok(Some(PromoteForkOutcome {
            name: origin.name,
            fork_lsn: origin.fork_lsn,
            archived_parent,
        }))
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

    pub(super) fn detached_fork_handle(&self, name: &str) -> Self {
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

    fn archived_parent_handle(&self, name: &str) -> Self {
        let root_name = self
            .root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "store.ops".to_string());
        Self {
            root: self.root.with_file_name(format!(
                "{root_name}.retired-by-promote-{}",
                sanitize_component(name)
            )),
        }
    }

    fn promoting_fork_handle(&self, name: &str) -> Self {
        let root_name = self
            .root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "store.ops".to_string());
        Self {
            root: self.root.with_file_name(format!(
                "{root_name}.promoting-{}",
                sanitize_component(name)
            )),
        }
    }

    pub(super) fn hydrate_shared_collections(&self) -> io::Result<()> {
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

    pub(super) fn fork_referenced_collection_paths(&self) -> io::Result<BTreeSet<String>> {
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
}
