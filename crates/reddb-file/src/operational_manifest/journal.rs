//! Version two publishes a small head after syncing append-only change records.
//! Bytes beyond the committed head are never replayed. A retry overwrites that
//! unpublished tail; a failed head replacement leaves the old prefix intact.
use super::*;
use serde::{Deserialize, Serialize};

pub(super) const JOURNAL_FILE: &str = "manifest.records";
const FRAME_HEADER: usize = 12;

#[derive(Debug, Clone)]
pub(super) struct CachedManifest {
    pub head: Vec<u8>,
    pub manifest: Manifest,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct Head {
    format_version: u32,
    pub generation: u64,
    pub length: u64,
    epoch: String,
    checksum: u32,
}

impl Head {
    fn new(generation: u64, length: u64, epoch: String) -> Self {
        let mut head = Self {
            format_version: 2,
            generation,
            length,
            epoch,
            checksum: 0,
        };
        head.checksum = head.checksum();
        head
    }

    fn checksum(&self) -> u32 {
        let mut hash = crc32fast::Hasher::new();
        hash.update(&self.format_version.to_le_bytes());
        hash.update(&self.generation.to_le_bytes());
        hash.update(&self.length.to_le_bytes());
        hash.update(self.epoch.as_bytes());
        hash.finalize()
    }

    pub fn decode(bytes: &[u8]) -> io::Result<Option<Self>> {
        let value: Value = serde_json::from_slice(bytes).map_err(invalid_data)?;
        if value.get("format_version").and_then(Value::as_u64) != Some(2) {
            return Ok(None);
        }
        let head: Self = serde_json::from_value(value).map_err(invalid_data)?;
        if head.checksum != head.checksum() {
            return Err(invalid_data("manifest journal head checksum mismatch"));
        }
        if head.length < FRAME_HEADER as u64 {
            return Err(invalid_data("invalid manifest journal head length"));
        }
        Ok(Some(head))
    }
}

#[derive(Serialize, Deserialize)]
enum Record {
    Collection(String, Option<CollectionEntry>),
    Segment(String, u64, Option<AppendOnlySegmentManifestEntry>),
    SegmentRetention {
        collection: String,
        id: u64,
        state: AppendOnlySegmentState,
        min: Option<i64>,
        max: Option<i64>,
        column: Option<String>,
    },
    Retired(String, Option<AppendOnlyRetiredState>),
    Origin(Option<ForkOrigin>),
    Generation(u64),
}

/// Holds only the current record's encoded bytes, never the whole journal.
pub(super) struct Replay<F: VfsFile> {
    file: F,
    remaining: u64,
    frame: Vec<u8>,
    body: Vec<u8>,
    manifest: Manifest,
    segments: BTreeMap<(String, u64), AppendOnlySegmentManifestEntry>,
    last_was_generation: bool,
}

impl<F: VfsFile> Replay<F> {
    pub fn new(file: F, length: u64) -> io::Result<Self> {
        if file.file_len()? < length {
            return Err(invalid_data("truncated committed manifest journal"));
        }
        Ok(Self {
            file,
            remaining: length,
            frame: Vec::new(),
            body: Vec::new(),
            manifest: empty_manifest(),
            segments: BTreeMap::new(),
            last_was_generation: false,
        })
    }

    pub fn resume(mut file: F, head: &Head, cached: Option<&CachedManifest>) -> io::Result<Self> {
        if let Some(cached) = cached {
            if let Some(previous) = Head::decode(&cached.head)? {
                if previous.epoch == head.epoch && previous.length <= head.length {
                    if file.file_len()? < head.length {
                        return Err(invalid_data("truncated committed manifest journal"));
                    }
                    file.seek(SeekFrom::Start(previous.length))?;
                    let mut manifest = cached.manifest.clone();
                    let segments = std::mem::take(&mut manifest.append_only_segments)
                        .into_iter()
                        .map(|entry| ((entry.collection.clone(), entry.segment_id), entry))
                        .collect();
                    return Ok(Self {
                        file,
                        remaining: head.length - previous.length,
                        frame: Vec::new(),
                        body: Vec::new(),
                        manifest,
                        segments,
                        last_was_generation: true,
                    });
                }
            }
        }
        Self::new(file, head.length)
    }

    pub fn step(&mut self, budget: usize) -> io::Result<usize> {
        let mut total = 0;
        while self.remaining > 0 && total < budget {
            let expected = if self.frame.len() < FRAME_HEADER {
                FRAME_HEADER - self.frame.len()
            } else {
                let length = u64::from_le_bytes(self.frame[..8].try_into().expect("frame length"));
                usize::try_from(length)
                    .map_err(invalid_data)?
                    .checked_sub(self.body.len())
                    .ok_or_else(|| invalid_data("invalid manifest record length"))?
            };
            if expected as u64 > self.remaining {
                return Err(invalid_data("truncated committed manifest record"));
            }
            let amount = expected.min(budget - total).min(64 * 1024);
            let mut chunk = vec![0; amount];
            self.file.read_exact(&mut chunk)?;
            total += amount;
            self.remaining -= amount as u64;
            if self.frame.len() < FRAME_HEADER {
                self.frame.extend_from_slice(&chunk);
            } else {
                self.body.try_reserve(amount).map_err(io::Error::other)?;
                self.body.extend_from_slice(&chunk);
            }
            if self.frame.len() == FRAME_HEADER {
                let length = u64::from_le_bytes(self.frame[..8].try_into().expect("frame length"));
                if length == 0 || length > self.remaining + self.body.len() as u64 {
                    return Err(invalid_data("invalid manifest record length"));
                }
                if self.body.len() as u64 == length {
                    let checksum =
                        u32::from_le_bytes(self.frame[8..].try_into().expect("frame checksum"));
                    if crc32fast::hash(&self.body) != checksum {
                        return Err(invalid_data("manifest record checksum mismatch"));
                    }
                    let record = serde_json::from_slice(&self.body).map_err(invalid_data)?;
                    self.apply(record)?;
                    self.frame.clear();
                    self.body = Vec::new();
                }
            }
        }
        Ok(total)
    }

    fn apply(&mut self, record: Record) -> io::Result<()> {
        self.last_was_generation = matches!(record, Record::Generation(_));
        match record {
            Record::Collection(name, entry) => match entry {
                Some(entry) => {
                    self.manifest.collections.insert(name, entry);
                }
                None => {
                    self.manifest.collections.remove(&name);
                }
            },
            Record::Segment(name, id, entry) => match entry {
                Some(entry) => {
                    if entry.collection != name || entry.segment_id != id {
                        return Err(invalid_data("manifest segment identity mismatch"));
                    }
                    self.segments.insert((name, id), entry);
                }
                None => {
                    self.segments.remove(&(name, id));
                }
            },
            Record::SegmentRetention {
                collection,
                id,
                state,
                min,
                max,
                column,
            } => {
                let entry = self
                    .segments
                    .get_mut(&(collection, id))
                    .ok_or_else(|| invalid_data("retention update for missing segment"))?;
                entry.state = state;
                entry.retention_min_ms = min;
                entry.retention_max_ms = max;
                entry.retention_timestamp_column = column;
            }
            Record::Retired(name, entry) => match entry {
                Some(entry) => {
                    self.manifest.append_only_retired.insert(name, entry);
                }
                None => {
                    self.manifest.append_only_retired.remove(&name);
                }
            },
            Record::Origin(origin) => self.manifest.fork_origin = origin,
            Record::Generation(generation) => self.manifest.generation = generation,
        }
        Ok(())
    }

    pub fn complete(&self) -> bool {
        self.remaining == 0
    }

    pub fn finish(mut self, head: &Head) -> io::Result<Manifest> {
        if !self.complete()
            || !self.frame.is_empty()
            || !self.last_was_generation
            || self.manifest.generation != head.generation
        {
            return Err(invalid_data("incomplete manifest transaction"));
        }
        self.manifest.append_only_segments = self.segments.into_values().collect();
        Ok(self.manifest)
    }
}

fn append(file: &mut impl VfsFile, record: &Record) -> io::Result<()> {
    let bytes = serde_json::to_vec(record).map_err(invalid_data)?;
    file.write_all(&(bytes.len() as u64).to_le_bytes())?;
    file.write_all(&crc32fast::hash(&bytes).to_le_bytes())?;
    file.write_all(&bytes)
}

fn append_changes(file: &mut impl VfsFile, old: &Manifest, new: &Manifest) -> io::Result<()> {
    for name in old
        .collections
        .keys()
        .chain(new.collections.keys())
        .collect::<BTreeSet<_>>()
    {
        if old.collections.get(name) != new.collections.get(name) {
            append(
                file,
                &Record::Collection(name.clone(), new.collections.get(name).cloned()),
            )?;
        }
    }
    let old_segments = old
        .append_only_segments
        .iter()
        .map(|entry| ((&entry.collection, entry.segment_id), entry))
        .collect::<BTreeMap<_, _>>();
    let new_segments = new
        .append_only_segments
        .iter()
        .map(|entry| ((&entry.collection, entry.segment_id), entry))
        .collect::<BTreeMap<_, _>>();
    for key in old_segments
        .keys()
        .chain(new_segments.keys())
        .collect::<BTreeSet<_>>()
    {
        if old_segments.get(key) == new_segments.get(key) {
            continue;
        }
        if let (Some(old), Some(new)) = (old_segments.get(key), new_segments.get(key)) {
            if old.path == new.path
                && old.codec == new.codec
                && old.chunk_size == new.chunk_size
                && old.row_count == new.row_count
                && old.primary_min == new.primary_min
                && old.primary_max == new.primary_max
                && old.primary_bloom == new.primary_bloom
                && old.chunk_checksums == new.chunk_checksums
            {
                append(
                    file,
                    &Record::SegmentRetention {
                        collection: key.0.clone(),
                        id: key.1,
                        state: new.state,
                        min: new.retention_min_ms,
                        max: new.retention_max_ms,
                        column: new.retention_timestamp_column.clone(),
                    },
                )?;
                continue;
            }
        }
        append(
            file,
            &Record::Segment(
                key.0.clone(),
                key.1,
                new_segments.get(key).map(|entry| (*entry).clone()),
            ),
        )?;
    }
    for name in old
        .append_only_retired
        .keys()
        .chain(new.append_only_retired.keys())
        .collect::<BTreeSet<_>>()
    {
        if old.append_only_retired.get(name) != new.append_only_retired.get(name) {
            append(
                file,
                &Record::Retired(name.clone(), new.append_only_retired.get(name).cloned()),
            )?;
        }
    }
    if old.fork_origin != new.fork_origin {
        append(file, &Record::Origin(new.fork_origin.clone()))?;
    }
    append(file, &Record::Generation(new.generation))
}

impl<V: Vfs> OperationalManifest<V> {
    pub(super) fn publish_journal(&self, manifest: &Manifest) -> io::Result<()> {
        self.ensure_dirs()?;
        // Writers are fenced by the host. Every mutator first loads the head
        // (or validates its pinned snapshot), which populates this baseline.
        let cached = self
            .cache
            .lock()
            .map_err(|_| io::Error::other("manifest cache poisoned"))?
            .clone();
        let old_head = cached
            .as_ref()
            .map(|cached| Head::decode(&cached.head))
            .transpose()?
            .flatten();
        let mut file = self
            .vfs
            .open(&self.root.join(JOURNAL_FILE), OpenMode::create_keep())?;
        let offset = old_head.as_ref().map_or(0, |head| head.length);
        file.set_len(offset)?;
        file.seek(SeekFrom::Start(offset))?;
        let empty = empty_manifest();
        let old = if old_head.is_some() {
            &cached.as_ref().expect("journal cache exists").manifest
        } else {
            &empty
        };
        append_changes(&mut file, old, manifest)?;
        let length = file.seek(SeekFrom::Current(0))?;
        file.sync_all()?;
        // Make a newly created journal durable before publishing its head.
        self.vfs.sync_dir(&self.root)?;
        let epoch = match old_head {
            Some(head) => head.epoch,
            None => {
                static SEQUENCE: std::sync::atomic::AtomicU64 =
                    std::sync::atomic::AtomicU64::new(0);
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_err(io::Error::other)?
                    .as_nanos();
                format!(
                    "{nanos}-{}-{}",
                    std::process::id(),
                    SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                )
            }
        };
        let head = serde_json::to_vec(&Head::new(manifest.generation, length, epoch))
            .map_err(invalid_data)?;
        let next = self.root.join(NEXT_MANIFEST_FILE);
        self.write_file(&next, &head, true)?;
        self.vfs.rename(&next, &self.root.join(MANIFEST_FILE))?;
        self.vfs.sync_dir(&self.root)?;
        *self
            .cache
            .lock()
            .map_err(|_| io::Error::other("manifest cache poisoned"))? = Some(CachedManifest {
            head,
            manifest: manifest.clone(),
        });
        Ok(())
    }
}
