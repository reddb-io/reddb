//! Serverless file planning and boot artifact codecs.
//!
//! Serverless storage optimizes for cold start, hot restart, and object-store
//! friendliness. The planner in this module deliberately returns deterministic
//! artifact names so a runtime can fetch only the bytes required to boot, then
//! lazily hydrate heavier packs.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::embedded::{RdbFileError, RdbFileResult};

mod boot;
mod cache;
mod extent;
mod hydrate;
mod lease;
mod manifest;
mod plan;
mod pointer;
mod secondary;

pub use boot::*;
pub use cache::*;
pub use extent::*;
pub use hydrate::*;
pub use lease::*;
pub use manifest::*;
pub use plan::*;
pub use pointer::*;
pub use secondary::*;

const SERVERLESS_MANIFEST_MAGIC: &[u8; 8] = b"RDPKMNF1";
const SERVERLESS_BOOT_INDEX_MAGIC: &[u8; 8] = b"RDPKBIX1";
const SERVERLESS_GENERATION_POINTER_MAGIC: &[u8; 8] = b"RDPKCUR1";
const SERVERLESS_EXTENT_INDEX_MAGIC: &[u8; 8] = b"RDPKEXT1";
const SERVERLESS_SECONDARY_INDEX_MAGIC: &[u8; 8] = b"RDPKSIX1";
const SERVERLESS_ARTIFACT_VERSION: u16 = 1;
const CHECKSUM_LEN: usize = 4;
const CONTENT_HASH_LEN: usize = 32;
const SERVERLESS_CRASH_INJECT_ENV: &str = "REDDB_SERVERLESS_CRASH_AT";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServerlessPackKind {
    Manifest,
    BootIndex,
    ExtentIndex,
    HotSnapshot,
    WalTail,
    CollectionData,
    SecondaryIndex,
    ColdArchive,
}

impl ServerlessPackKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Manifest => "manifest",
            Self::BootIndex => "boot-index",
            Self::ExtentIndex => "extent-index",
            Self::HotSnapshot => "hot-snapshot",
            Self::WalTail => "wal-tail",
            Self::CollectionData => "collection-data",
            Self::SecondaryIndex => "secondary-index",
            Self::ColdArchive => "cold-archive",
        }
    }
}

impl TryFrom<u8> for ServerlessPackKind {
    type Error = RdbFileError;

    fn try_from(value: u8) -> RdbFileResult<Self> {
        match value {
            1 => Ok(Self::Manifest),
            2 => Ok(Self::BootIndex),
            3 => Ok(Self::ExtentIndex),
            4 => Ok(Self::HotSnapshot),
            5 => Ok(Self::WalTail),
            6 => Ok(Self::CollectionData),
            7 => Ok(Self::SecondaryIndex),
            8 => Ok(Self::ColdArchive),
            other => Err(RdbFileError::InvalidOperation(format!(
                "unknown serverless pack kind {other}"
            ))),
        }
    }
}

impl From<ServerlessPackKind> for u8 {
    fn from(value: ServerlessPackKind) -> Self {
        match value {
            ServerlessPackKind::Manifest => 1,
            ServerlessPackKind::BootIndex => 2,
            ServerlessPackKind::ExtentIndex => 3,
            ServerlessPackKind::HotSnapshot => 4,
            ServerlessPackKind::WalTail => 5,
            ServerlessPackKind::CollectionData => 6,
            ServerlessPackKind::SecondaryIndex => 7,
            ServerlessPackKind::ColdArchive => 8,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ServerlessContentHash(pub [u8; CONTENT_HASH_LEN]);

impl ServerlessContentHash {
    pub const ZERO: Self = Self([0; CONTENT_HASH_LEN]);

    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(*blake3::hash(bytes).as_bytes())
    }

    pub fn is_zero(self) -> bool {
        self.0 == [0; CONTENT_HASH_LEN]
    }
}

fn kind_for_artifact_path(path: &Path) -> ServerlessPackKind {
    match path.file_stem().and_then(|stem| stem.to_str()) {
        Some("manifest") => ServerlessPackKind::Manifest,
        Some("boot-index") => ServerlessPackKind::BootIndex,
        Some("extent-index") => ServerlessPackKind::ExtentIndex,
        Some("hot-snapshot") => ServerlessPackKind::HotSnapshot,
        Some("wal-tail") => ServerlessPackKind::WalTail,
        Some("collection-data") => ServerlessPackKind::CollectionData,
        Some("secondary-index") => ServerlessPackKind::SecondaryIndex,
        Some("cold-archive") => ServerlessPackKind::ColdArchive,
        _ => ServerlessPackKind::ColdArchive,
    }
}

fn relative_to_generation_dir(path: &Path) -> PathBuf {
    path.file_name()
        .map(PathBuf::from)
        .unwrap_or_else(|| path.to_path_buf())
}

fn write_bytes(path: impl AsRef<Path>, bytes: &[u8]) -> RdbFileResult<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp_path = crate::layout::atomic_temp_path(path);
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp_path)?;
        file.write_all(bytes)?;
        crash_inject("serverless_pack_after_tmp_write");
        file.sync_all()?;
        crash_inject("serverless_pack_after_tmp_sync");
    }
    fs::rename(&tmp_path, path)?;
    crash_inject("serverless_pack_after_rename");
    if let Some(parent) = path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    crash_inject("serverless_pack_after_dir_sync");
    Ok(())
}

fn write_current_pointer_bytes(path: impl AsRef<Path>, bytes: &[u8]) -> RdbFileResult<()> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp_path = crate::layout::atomic_temp_path(path);
    {
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&tmp_path)?;
        file.write_all(bytes)?;
        crash_inject("current_pointer_after_tmp_write");
        file.sync_all()?;
        crash_inject("current_pointer_after_tmp_sync");
    }
    fs::rename(&tmp_path, path)?;
    crash_inject("current_pointer_after_rename");
    if let Some(parent) = path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    crash_inject("current_pointer_after_dir_sync");
    Ok(())
}

fn crash_inject(point: &str) {
    if std::env::var(SERVERLESS_CRASH_INJECT_ENV).ok().as_deref() == Some(point) {
        std::process::exit(173);
    }
}

fn verify_checksum(bytes: &[u8]) -> RdbFileResult<()> {
    let Some(checksum_offset) = bytes.len().checked_sub(CHECKSUM_LEN) else {
        return Err(RdbFileError::InvalidOperation(
            "serverless artifact too short".into(),
        ));
    };
    let stored = u32::from_le_bytes(bytes[checksum_offset..].try_into().unwrap());
    let computed = crc32(&bytes[..checksum_offset]);
    if stored != computed {
        return Err(RdbFileError::InvalidOperation(format!(
            "serverless artifact checksum mismatch: stored {stored:#010x}, computed {computed:#010x}"
        )));
    }
    Ok(())
}

fn crc32(data: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(data);
    hasher.finalize()
}

fn expect_magic(bytes: &[u8], cursor: &mut usize, magic: &[u8]) -> RdbFileResult<()> {
    let actual = take_bytes(bytes, cursor, magic.len())?;
    if actual != magic {
        return Err(RdbFileError::InvalidOperation(
            "invalid serverless artifact magic".into(),
        ));
    }
    Ok(())
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_string(out: &mut Vec<u8>, value: &str) {
    put_u32(out, value.len() as u32);
    out.extend_from_slice(value.as_bytes());
}

fn put_bytes(out: &mut Vec<u8>, value: &[u8]) {
    put_u32(out, value.len() as u32);
    out.extend_from_slice(value);
}

fn put_content_hash(out: &mut Vec<u8>, value: ServerlessContentHash) {
    out.extend_from_slice(&value.0);
}

fn take_bytes<'a>(bytes: &'a [u8], cursor: &mut usize, len: usize) -> RdbFileResult<&'a [u8]> {
    let end = cursor
        .checked_add(len)
        .ok_or_else(|| RdbFileError::InvalidOperation("serverless cursor overflow".into()))?;
    if end > bytes.len().saturating_sub(CHECKSUM_LEN) {
        return Err(RdbFileError::InvalidOperation(
            "serverless artifact truncated".into(),
        ));
    }
    let value = &bytes[*cursor..end];
    *cursor = end;
    Ok(value)
}

fn reject_trailing_bytes(bytes: &[u8], cursor: usize) -> RdbFileResult<()> {
    if cursor != bytes.len().saturating_sub(CHECKSUM_LEN) {
        return Err(RdbFileError::InvalidOperation(
            "serverless artifact has trailing bytes".into(),
        ));
    }
    Ok(())
}

fn take_u8(bytes: &[u8], cursor: &mut usize) -> RdbFileResult<u8> {
    Ok(take_bytes(bytes, cursor, 1)?[0])
}

fn take_u16(bytes: &[u8], cursor: &mut usize) -> RdbFileResult<u16> {
    Ok(u16::from_le_bytes(
        take_bytes(bytes, cursor, 2)?.try_into().unwrap(),
    ))
}

fn take_u32(bytes: &[u8], cursor: &mut usize) -> RdbFileResult<u32> {
    Ok(u32::from_le_bytes(
        take_bytes(bytes, cursor, 4)?.try_into().unwrap(),
    ))
}

fn take_u64(bytes: &[u8], cursor: &mut usize) -> RdbFileResult<u64> {
    Ok(u64::from_le_bytes(
        take_bytes(bytes, cursor, 8)?.try_into().unwrap(),
    ))
}

fn take_string(bytes: &[u8], cursor: &mut usize) -> RdbFileResult<String> {
    let len = take_u32(bytes, cursor)? as usize;
    let raw = take_bytes(bytes, cursor, len)?;
    std::str::from_utf8(raw)
        .map(|value| value.to_string())
        .map_err(|err| RdbFileError::InvalidOperation(format!("invalid utf-8 string: {err}")))
}

fn take_vec_bytes(bytes: &[u8], cursor: &mut usize) -> RdbFileResult<Vec<u8>> {
    let len = take_u32(bytes, cursor)? as usize;
    Ok(take_bytes(bytes, cursor, len)?.to_vec())
}

fn take_content_hash(bytes: &[u8], cursor: &mut usize) -> RdbFileResult<ServerlessContentHash> {
    let raw = take_bytes(bytes, cursor, CONTENT_HASH_LEN)?;
    let mut hash = [0u8; CONTENT_HASH_LEN];
    hash.copy_from_slice(raw);
    Ok(ServerlessContentHash(hash))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serverless_paths_are_generation_scoped_and_deterministic() {
        let plan = ServerlessFilePlan::new("/tmp/reddb", "tenant-a/db", 42);
        assert_eq!(
            plan.manifest_path(),
            PathBuf::from("/tmp/reddb/tenant-a/db/g00000000000000000042/manifest.redpack")
        );
        assert_eq!(
            plan.boot_index_path(),
            PathBuf::from("/tmp/reddb/tenant-a/db/g00000000000000000042/boot-index.redpack")
        );
        assert!(ServerlessFilePlan::is_generation_dir(Path::new(
            "g00000000000000000042"
        )));
        assert!(!ServerlessFilePlan::is_generation_dir(Path::new("g42")));
    }

    #[test]
    fn cold_start_fetches_manifest_boot_snapshot_then_wal_tail() {
        let plan = ServerlessFilePlan::new("/tmp/reddb", "db", 7);
        let boot = ServerlessBootPlan::cold(&plan);
        assert_eq!(boot.required_first[0], plan.manifest_path());
        assert_eq!(boot.required_first[1], plan.boot_index_path());
        assert_eq!(boot.required_first[2], plan.extent_index_path());
        assert_eq!(boot.required_first[3], plan.hot_snapshot_path());
        assert_eq!(boot.required_first[4], plan.wal_tail_path());
        assert_eq!(boot.lazy_after_open.len(), 3);
    }

    #[test]
    fn manifest_round_trips_with_crc_checked_binary_codec() {
        let mut manifest = ServerlessManifest::new("tenant/db", 11);
        manifest.push(ServerlessManifestEntry::from_bytes(
            ServerlessPackKind::WalTail,
            "wal-tail.redpack",
            b"wal tail payload",
        ));
        manifest.push(ServerlessManifestEntry::from_bytes(
            ServerlessPackKind::BootIndex,
            "boot-index.redpack",
            b"boot index payload",
        ));

        let encoded = manifest.encode();
        let decoded = ServerlessManifest::decode(&encoded).expect("decode manifest");
        assert_eq!(decoded, manifest);
        assert!(!decoded.entries[0].content_hash.is_zero());

        let mut corrupt = encoded;
        let last_payload_byte = corrupt.len() - CHECKSUM_LEN - 1;
        corrupt[last_payload_byte] ^= 0x01;
        let err = ServerlessManifest::decode(&corrupt).expect_err("checksum catches corruption");
        assert!(err.to_string().contains("checksum mismatch"), "{err}");
    }

    #[test]
    fn boot_index_round_trips_and_preserves_coldstart_order() {
        let plan = ServerlessFilePlan::new("/tmp/reddb", "db", 9);
        let index = ServerlessBootIndex::from_plan(&plan);

        assert_eq!(
            index.required_first(),
            vec![
                PathBuf::from("manifest.redpack"),
                PathBuf::from("boot-index.redpack"),
                PathBuf::from("extent-index.redpack"),
                PathBuf::from("hot-snapshot.redpack"),
                PathBuf::from("wal-tail.redpack"),
            ]
        );
        assert_eq!(
            index.lazy_after_open(),
            vec![
                PathBuf::from("collection-data.redpack"),
                PathBuf::from("secondary-index.redpack"),
                PathBuf::from("cold-archive.redpack"),
            ]
        );

        let decoded = ServerlessBootIndex::decode(&index.encode()).expect("decode boot index");
        assert_eq!(decoded, index);
    }

    #[test]
    fn collection_data_extent_ref_uses_canonical_pack_path() {
        let plan = ServerlessFilePlan::new("/tmp/reddb", "db", 9);
        let payload = b"collection snapshot bytes";
        let extent = plan
            .collection_data_extent_ref("events", 12, payload, true)
            .expect("extent ref");

        assert_eq!(extent.collection, "events");
        assert_eq!(
            extent.relative_path,
            PathBuf::from("collection-data.redpack")
        );
        assert_eq!(extent.offset, 12);
        assert_eq!(extent.bytes, payload.len() as u64);
        assert!(extent.hot);
    }

    #[test]
    fn manifest_rejects_trailing_payload_bytes() {
        let manifest = ServerlessManifest::new("tenant/db", 11);
        let mut encoded = manifest.encode();
        encoded.truncate(encoded.len() - CHECKSUM_LEN);
        encoded.push(0xAA);
        let checksum = crc32(&encoded);
        put_u32(&mut encoded, checksum);

        let err = ServerlessManifest::decode(&encoded).expect_err("trailing bytes rejected");
        assert!(err.to_string().contains("trailing bytes"), "{err}");
    }

    #[test]
    fn generation_pointer_round_trips_and_points_to_immutable_manifest() {
        let plan = ServerlessFilePlan::new("/tmp/reddb", "tenant/db", 19);
        let mut manifest = ServerlessManifest::new("tenant/db", 19);
        manifest.push(ServerlessManifestEntry::from_bytes(
            ServerlessPackKind::HotSnapshot,
            "hot-snapshot.redpack",
            b"snapshot",
        ));

        let pointer = ServerlessGenerationPointer::from_manifest(&plan, &manifest);
        assert_eq!(pointer.generation, 19);
        assert_eq!(
            pointer.manifest_relative_path,
            PathBuf::from("g00000000000000000019/manifest.redpack")
        );
        assert!(!pointer.manifest_content_hash.is_zero());

        let decoded =
            ServerlessGenerationPointer::decode(&pointer.encode()).expect("decode pointer");
        assert_eq!(decoded, pointer);
    }

    #[test]
    fn extent_index_finds_key_ranges_and_hot_prefetch_paths() {
        let mut index = ServerlessExtentIndex::new(21);
        index.push(
            ServerlessExtentRef::new(
                "orders",
                b"a".to_vec(),
                b"m".to_vec(),
                "orders-000.redpack",
                0,
                b"orders-a-m",
                true,
            )
            .expect("extent"),
        );
        index.push(
            ServerlessExtentRef::new(
                "orders",
                b"m".to_vec(),
                b"z".to_vec(),
                "orders-001.redpack",
                0,
                b"orders-m-z",
                false,
            )
            .expect("extent"),
        );

        let matches = index.extents_for_key("orders", b"b");
        assert_eq!(matches.len(), 1);
        assert_eq!(
            matches[0].relative_path,
            PathBuf::from("orders-000.redpack")
        );
        assert_eq!(
            index.hot_prefetch_paths(),
            vec![PathBuf::from("orders-000.redpack")]
        );

        let decoded = ServerlessExtentIndex::decode(&index.encode()).expect("decode extent index");
        assert_eq!(decoded, index);
    }

    #[test]
    fn hydration_plan_uses_only_matching_extents() {
        let mut index = ServerlessExtentIndex::new(22);
        index.push(
            ServerlessExtentRef::new(
                "orders",
                b"a".to_vec(),
                b"m".to_vec(),
                "orders-000.redpack",
                64,
                b"orders-a-m",
                true,
            )
            .expect("extent"),
        );
        index.push(
            ServerlessExtentRef::new(
                "orders",
                b"m".to_vec(),
                b"z".to_vec(),
                "orders-001.redpack",
                128,
                b"orders-m-z",
                false,
            )
            .expect("extent"),
        );

        let plan = index.hydration_plan_for_key("orders", b"n");
        assert_eq!(plan.requests.len(), 1);
        assert_eq!(
            plan.requests[0].relative_path,
            PathBuf::from("orders-001.redpack")
        );
        assert_eq!(plan.requests[0].offset, 128);
        assert_eq!(plan.total_bytes(), b"orders-m-z".len() as u64);

        let hot = index.hot_hydration_plan();
        assert_eq!(hot.requests.len(), 1);
        assert_eq!(
            hot.requests[0].relative_path,
            PathBuf::from("orders-000.redpack")
        );
    }

    #[test]
    fn hydration_plan_for_range_uses_overlapping_extents() {
        let mut index = ServerlessExtentIndex::new(27);
        index.push(
            ServerlessExtentRef::new(
                "orders",
                b"a".to_vec(),
                b"f".to_vec(),
                "orders-000.redpack",
                0,
                b"orders-a-f",
                true,
            )
            .expect("extent"),
        );
        index.push(
            ServerlessExtentRef::new(
                "orders",
                b"f".to_vec(),
                b"p".to_vec(),
                "orders-001.redpack",
                64,
                b"orders-f-p",
                false,
            )
            .expect("extent"),
        );
        index.push(
            ServerlessExtentRef::new(
                "orders",
                b"p".to_vec(),
                b"z".to_vec(),
                "orders-002.redpack",
                128,
                b"orders-p-z",
                false,
            )
            .expect("extent"),
        );
        index.push(
            ServerlessExtentRef::new(
                "users",
                b"a".to_vec(),
                b"z".to_vec(),
                "users-000.redpack",
                0,
                b"users-a-z",
                false,
            )
            .expect("extent"),
        );

        let plan = index
            .hydration_plan_for_range("orders", b"e", b"q")
            .expect("range plan");
        assert_eq!(plan.requests.len(), 3);
        assert_eq!(
            plan.requests
                .iter()
                .map(|request| request.relative_path.clone())
                .collect::<Vec<_>>(),
            vec![
                PathBuf::from("orders-000.redpack"),
                PathBuf::from("orders-001.redpack"),
                PathBuf::from("orders-002.redpack"),
            ]
        );

        let err = index
            .hydration_plan_for_range("orders", b"q", b"e")
            .expect_err("invalid range rejected");
        assert!(err.to_string().contains("range_start"), "{err}");
    }

    #[test]
    fn secondary_index_round_trips_and_builds_collection_hydration_plan() {
        let mut extent_index = ServerlessExtentIndex::new(29);
        extent_index.push(
            ServerlessExtentRef::new(
                "orders",
                b"a".to_vec(),
                b"m".to_vec(),
                "collection-data.redpack",
                0,
                b"orders-left",
                true,
            )
            .expect("orders left"),
        );
        extent_index.push(
            ServerlessExtentRef::new(
                "orders",
                b"m".to_vec(),
                b"z".to_vec(),
                "collection-data.redpack",
                11,
                b"orders-right",
                false,
            )
            .expect("orders right"),
        );
        extent_index.push(
            ServerlessExtentRef::new(
                "users",
                b"a".to_vec(),
                b"z".to_vec(),
                "collection-data.redpack",
                23,
                b"users",
                false,
            )
            .expect("users"),
        );

        let secondary = ServerlessSecondaryIndex::from_extent_index(&extent_index);
        let decoded =
            ServerlessSecondaryIndex::decode(&secondary.encode()).expect("decode secondary index");
        assert_eq!(decoded, secondary);

        let hydration = decoded.hydration_plan_for_collection("orders");
        assert_eq!(hydration.generation, 29);
        assert_eq!(hydration.requests.len(), 2);
        assert_eq!(hydration.total_bytes(), 23);
        assert!(hydration.requests[0].content_hash != ServerlessContentHash::ZERO);
    }

    #[test]
    fn hydrate_local_plan_reads_only_requested_byte_ranges() {
        let root = temp_root("serverless-hydrate-range");
        let plan = ServerlessFilePlan::new(&root, "db", 23);
        let collection_payload = b"aaaabbbbcccc";
        let mut index = ServerlessExtentIndex::new(23);
        index.push(
            ServerlessExtentRef::new(
                "orders",
                b"a".to_vec(),
                b"m".to_vec(),
                "collection-data.redpack",
                4,
                b"bbbb",
                true,
            )
            .expect("extent"),
        );
        plan.publish_core_generation(&index, collection_payload, b"secondary")
            .expect("publish generation");

        let hydration = index.hydration_plan_for_key("orders", b"b");
        let hydrated = plan
            .hydrate_local_plan(&hydration)
            .expect("hydrate local range");
        assert_eq!(hydrated.len(), 1);
        assert_eq!(hydrated[0].payload, b"bbbb");
        assert_eq!(hydrated[0].request.offset, 4);
        assert_eq!(hydrated[0].request.bytes, 4);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn hydrate_local_plan_rejects_corrupt_or_out_of_bounds_ranges() {
        let root = temp_root("serverless-hydrate-corrupt");
        let plan = ServerlessFilePlan::new(&root, "db", 24);
        let mut index = ServerlessExtentIndex::new(24);
        index.push(
            ServerlessExtentRef::new(
                "orders",
                b"a".to_vec(),
                b"m".to_vec(),
                "collection-data.redpack",
                4,
                b"bbbb",
                true,
            )
            .expect("extent"),
        );
        plan.publish_core_generation(&index, b"aaaabbbbcccc", b"secondary")
            .expect("publish generation");
        std::fs::write(plan.collection_data_path(), b"aaaaBBBBcccc").expect("corrupt pack");

        let hydration = index.hydration_plan_for_key("orders", b"b");
        let err = plan
            .hydrate_local_plan(&hydration)
            .expect_err("corrupt range rejected");
        assert!(err.to_string().contains("checksum mismatch"), "{err}");

        let mut out_of_bounds = hydration.clone();
        out_of_bounds.requests[0].offset = 11;
        let err = plan
            .hydrate_local_plan(&out_of_bounds)
            .expect_err("out of bounds rejected");
        assert!(err.to_string().contains("exceeds pack"), "{err}");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn prefetch_hot_extents_hydrates_only_hot_ranges() {
        let root = temp_root("serverless-hot-prefetch");
        let plan = ServerlessFilePlan::new(&root, "db", 25);
        let mut index = ServerlessExtentIndex::new(25);
        index.push(
            ServerlessExtentRef::new(
                "orders",
                b"a".to_vec(),
                b"m".to_vec(),
                "collection-data.redpack",
                0,
                b"hot!",
                true,
            )
            .expect("hot extent"),
        );
        index.push(
            ServerlessExtentRef::new(
                "orders",
                b"m".to_vec(),
                b"z".to_vec(),
                "collection-data.redpack",
                4,
                b"cold",
                false,
            )
            .expect("cold extent"),
        );
        plan.publish_core_generation(&index, b"hot!cold", b"secondary")
            .expect("publish generation");

        let hydrated = plan.prefetch_hot_extents(&index).expect("prefetch hot");
        assert_eq!(hydrated.len(), 1);
        assert_eq!(hydrated[0].payload, b"hot!");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn prefetch_hot_extents_cached_populates_only_hot_cache_entries() {
        let root = temp_root("serverless-hot-prefetch-cache");
        let plan = ServerlessFilePlan::new(&root, "db", 29);
        let cache = ServerlessLocalCache::new(root.join("cache"), 29);
        let mut index = ServerlessExtentIndex::new(29);
        index.push(
            ServerlessExtentRef::new(
                "orders",
                b"a".to_vec(),
                b"m".to_vec(),
                "collection-data.redpack",
                0,
                b"hot!",
                true,
            )
            .expect("hot extent"),
        );
        index.push(
            ServerlessExtentRef::new(
                "orders",
                b"m".to_vec(),
                b"z".to_vec(),
                "collection-data.redpack",
                4,
                b"cold",
                false,
            )
            .expect("cold extent"),
        );
        plan.publish_core_generation(&index, b"hot!cold", b"secondary")
            .expect("publish generation");

        let hot_request = index.hydration_plan_for_key("orders", b"b").requests[0].clone();
        let cold_request = index.hydration_plan_for_key("orders", b"n").requests[0].clone();

        let hydrated = plan
            .prefetch_hot_extents_cached(&index, &cache)
            .expect("prefetch hot into cache");
        assert_eq!(hydrated.len(), 1);
        assert_eq!(hydrated[0].payload, b"hot!");
        assert_eq!(
            std::fs::read(cache.path_for_request(&hot_request)).expect("read hot cache"),
            b"hot!"
        );
        assert!(
            !cache.path_for_request(&cold_request).exists(),
            "cold extent should not be prefetched into cache"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn hydrate_local_request_cached_validates_and_repairs_corrupt_cache() {
        let root = temp_root("serverless-hydrate-cache");
        let plan = ServerlessFilePlan::new(&root, "db", 26);
        let cache = ServerlessLocalCache::new(root.join("cache"), 26);
        let mut index = ServerlessExtentIndex::new(26);
        index.push(
            ServerlessExtentRef::new(
                "orders",
                b"a".to_vec(),
                b"z".to_vec(),
                "collection-data.redpack",
                4,
                b"bbbb",
                true,
            )
            .expect("extent"),
        );
        plan.publish_core_generation(&index, b"aaaabbbbcccc", b"secondary")
            .expect("publish generation");
        let request = index.hydration_plan_for_key("orders", b"m").requests[0].clone();

        let first = plan
            .hydrate_local_request_cached(&request, &cache)
            .expect("hydrate and cache");
        assert_eq!(first.payload, b"bbbb");
        let cache_path = cache.path_for_request(&request);
        assert_eq!(std::fs::read(&cache_path).expect("read cache"), b"bbbb");

        std::fs::write(&cache_path, b"xxxx").expect("corrupt cache");
        let repaired = plan
            .hydrate_local_request_cached(&request, &cache)
            .expect("repair corrupt cache from pack");
        assert_eq!(repaired.payload, b"bbbb");
        assert_eq!(
            std::fs::read(&cache_path).expect("read repaired cache"),
            b"bbbb"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn hydrate_local_plan_cached_populates_multiple_range_entries() {
        let root = temp_root("serverless-hydrate-plan-cache");
        let plan = ServerlessFilePlan::new(&root, "db", 28);
        let cache = ServerlessLocalCache::new(root.join("cache"), 28);
        let mut index = ServerlessExtentIndex::new(28);
        index.push(
            ServerlessExtentRef::new(
                "orders",
                b"a".to_vec(),
                b"m".to_vec(),
                "collection-data.redpack",
                0,
                b"left",
                true,
            )
            .expect("left extent"),
        );
        index.push(
            ServerlessExtentRef::new(
                "orders",
                b"m".to_vec(),
                b"z".to_vec(),
                "collection-data.redpack",
                4,
                b"right",
                false,
            )
            .expect("right extent"),
        );
        plan.publish_core_generation(&index, b"leftright", b"secondary")
            .expect("publish generation");

        let hydration = index
            .hydration_plan_for_range("orders", b"b", b"y")
            .expect("range hydration plan");
        let hydrated = plan
            .hydrate_local_plan_cached(&hydration, &cache)
            .expect("hydrate cached plan");
        assert_eq!(hydrated.len(), 2);
        assert_eq!(hydrated[0].payload, b"left");
        assert_eq!(hydrated[1].payload, b"right");
        for range in hydrated {
            assert!(cache.path_for_request(&range.request).exists());
        }

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cached_hydration_enforces_max_hot_bytes_after_writes() {
        let root = temp_root("serverless-hydrate-cache-budget");
        let plan =
            ServerlessFilePlan::new(&root, "db", 29).with_cache_policy(ServerlessCachePolicy {
                max_hot_bytes: 5,
                ..ServerlessCachePolicy::default()
            });
        let cache = ServerlessLocalCache::new(root.join("cache"), 29);
        let mut index = ServerlessExtentIndex::new(29);
        index.push(
            ServerlessExtentRef::new(
                "orders",
                b"a".to_vec(),
                b"m".to_vec(),
                "collection-data.redpack",
                0,
                b"left",
                true,
            )
            .expect("left extent"),
        );
        index.push(
            ServerlessExtentRef::new(
                "orders",
                b"m".to_vec(),
                b"z".to_vec(),
                "collection-data.redpack",
                4,
                b"right",
                true,
            )
            .expect("right extent"),
        );
        plan.publish_core_generation(&index, b"leftright", b"secondary")
            .expect("publish generation");

        let hydration = index
            .hydration_plan_for_range("orders", b"a", b"z")
            .expect("range hydration plan");
        let hydrated = plan
            .hydrate_local_plan_cached(&hydration, &cache)
            .expect("hydrate cached plan");
        assert_eq!(hydrated.len(), 2);
        assert_eq!(hydrated[0].payload, b"left");
        assert_eq!(hydrated[1].payload, b"right");

        let entries = cache.cached_entries().expect("cache entries");
        let cached_bytes: u64 = entries.iter().map(|entry| entry.bytes).sum();
        assert!(
            cached_bytes <= 5,
            "cache should stay within max_hot_bytes, got {cached_bytes}"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn cache_eviction_prefers_cold_old_entries() {
        let entries = vec![
            ServerlessCacheEntry::new("cold-old.redpack", 100, false, 10),
            ServerlessCacheEntry::new("hot-old.redpack", 100, true, 1),
            ServerlessCacheEntry::new("cold-new.redpack", 100, false, 20),
        ];

        let plan = ServerlessCacheEvictionPlan::plan(&entries, 150);
        assert_eq!(
            plan.evict,
            vec![
                PathBuf::from("cold-old.redpack"),
                PathBuf::from("cold-new.redpack"),
            ]
        );
        assert_eq!(plan.bytes_after_eviction, 100);
    }

    #[test]
    fn extent_index_writes_and_reads_from_disk() {
        let root = temp_root("serverless-extent-index");
        let plan = ServerlessFilePlan::new(&root, "db", 5);
        let mut index = ServerlessExtentIndex::new(5);
        index.push(
            ServerlessExtentRef::new(
                "events",
                b"2026-01".to_vec(),
                b"2026-02".to_vec(),
                "events-2026-01.redpack",
                128,
                b"payload",
                true,
            )
            .expect("extent"),
        );

        index
            .write_to_path(plan.extent_index_path())
            .expect("write extent index");
        assert_eq!(
            ServerlessExtentIndex::read_from_path(plan.extent_index_path())
                .expect("read extent index"),
            index
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn manifest_and_boot_index_write_and_read_from_disk() {
        let root = temp_root("serverless-manifest");
        let plan = ServerlessFilePlan::new(&root, "db", 1);

        let mut manifest = ServerlessManifest::new("db", 1);
        manifest.push(ServerlessManifestEntry::new(
            ServerlessPackKind::Manifest,
            "manifest.redpack",
            128,
            0xCAFE_BABE,
        ));
        manifest
            .write_to_path(plan.manifest_path())
            .expect("write manifest");

        let boot_index = ServerlessBootIndex::from_plan(&plan);
        boot_index
            .write_to_path(plan.boot_index_path())
            .expect("write boot index");

        assert_eq!(
            ServerlessManifest::read_from_path(plan.manifest_path()).expect("read manifest"),
            manifest
        );
        assert_eq!(
            ServerlessBootIndex::read_from_path(plan.boot_index_path()).expect("read boot index"),
            boot_index
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn publish_core_generation_writes_required_packs_before_current_pointer() {
        let root = temp_root("serverless-publish-core");
        let plan = ServerlessFilePlan::new(&root, "db", 12);
        let mut extent_index = ServerlessExtentIndex::new(12);
        extent_index.push(
            ServerlessExtentRef::new(
                "events",
                b"a".to_vec(),
                b"z".to_vec(),
                "collection-data.redpack",
                0,
                b"collection-bytes",
                true,
            )
            .expect("extent"),
        );

        let pointer = plan
            .publish_core_generation(&extent_index, b"collection-bytes", b"secondary-bytes")
            .expect("publish core generation");
        assert_eq!(pointer.generation, 12);
        assert_eq!(plan.read_current_pointer().expect("read current"), pointer);
        assert_eq!(
            plan.read_current_pointer_verified()
                .expect("read verified current"),
            pointer
        );
        assert!(plan.boot_index_path().exists());
        assert!(plan.extent_index_path().exists());
        assert!(plan.collection_data_path().exists());
        assert!(plan.secondary_index_path().exists());

        let manifest =
            ServerlessManifest::read_from_path(plan.manifest_path()).expect("read manifest");
        plan.validate_complete_generation(&manifest)
            .expect("complete generation validates");
        assert!(manifest
            .entries
            .iter()
            .any(|entry| entry.kind == ServerlessPackKind::ExtentIndex));
        assert!(manifest
            .entries
            .iter()
            .any(|entry| entry.kind == ServerlessPackKind::CollectionData));
        assert!(manifest
            .entries
            .iter()
            .any(|entry| entry.kind == ServerlessPackKind::SecondaryIndex));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn verified_current_pointer_rejects_missing_or_corrupt_generation() {
        let root = temp_root("serverless-current-verified");
        let plan = ServerlessFilePlan::new(&root, "db", 14);
        let mut extent_index = ServerlessExtentIndex::new(14);
        extent_index.push(
            ServerlessExtentRef::new(
                "events",
                b"a".to_vec(),
                b"z".to_vec(),
                "collection-data.redpack",
                0,
                b"collection-bytes",
                true,
            )
            .expect("extent"),
        );
        let pointer = plan
            .publish_core_generation(&extent_index, b"collection-bytes", b"secondary-bytes")
            .expect("publish complete generation");
        assert_eq!(
            plan.read_current_pointer_verified()
                .expect("verified pointer before corruption"),
            pointer
        );

        std::fs::remove_file(plan.collection_data_path()).expect("remove required pack");
        let err = plan
            .read_current_pointer_verified()
            .expect_err("verified pointer must reject missing required pack");
        assert!(
            err.to_string().contains("No such file") || err.to_string().contains("not found"),
            "{err}"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn verified_current_pointer_rejects_manifest_hash_mismatch() {
        let root = temp_root("serverless-current-manifest-hash");
        let plan = ServerlessFilePlan::new(&root, "db", 15);
        let mut extent_index = ServerlessExtentIndex::new(15);
        extent_index.push(
            ServerlessExtentRef::new(
                "events",
                b"a".to_vec(),
                b"z".to_vec(),
                "collection-data.redpack",
                0,
                b"collection-bytes",
                true,
            )
            .expect("extent"),
        );
        plan.publish_core_generation(&extent_index, b"collection-bytes", b"secondary-bytes")
            .expect("publish complete generation");

        std::fs::write(plan.manifest_path(), b"corrupt-manifest").expect("corrupt manifest");
        let err = plan
            .read_current_pointer_verified()
            .expect_err("verified pointer must reject manifest hash mismatch");
        assert!(
            err.to_string().contains("manifest")
                && (err.to_string().contains("bytes")
                    || err.to_string().contains("checksum")
                    || err.to_string().contains("hash")),
            "{err}"
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn publish_pointer_rejects_incomplete_or_corrupt_generation() {
        let root = temp_root("serverless-publish-rejects");
        let plan = ServerlessFilePlan::new(&root, "db", 13);
        let mut manifest = ServerlessManifest::new("db", 13);
        manifest.push(ServerlessManifestEntry::from_bytes(
            ServerlessPackKind::BootIndex,
            "boot-index.redpack",
            b"boot",
        ));
        manifest
            .write_to_path(plan.manifest_path())
            .expect("write incomplete manifest");

        let err = plan
            .publish_generation_pointer(&manifest)
            .expect_err("incomplete generation rejected");
        assert!(err.to_string().contains("missing required"), "{err}");
        assert!(!plan.current_pointer_path().exists());

        let mut extent_index = ServerlessExtentIndex::new(13);
        extent_index.push(
            ServerlessExtentRef::new(
                "events",
                b"a".to_vec(),
                b"z".to_vec(),
                "collection-data.redpack",
                0,
                b"collection-bytes",
                true,
            )
            .expect("extent"),
        );
        plan.publish_core_generation(&extent_index, b"collection-bytes", b"secondary-bytes")
            .expect("publish complete generation");
        std::fs::write(plan.collection_data_path(), b"collection-ByTes")
            .expect("corrupt collection pack");
        let manifest =
            ServerlessManifest::read_from_path(plan.manifest_path()).expect("read manifest");
        let err = plan
            .publish_generation_pointer(&manifest)
            .expect_err("corrupt generation rejected");
        assert!(err.to_string().contains("checksum mismatch"), "{err}");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn incomplete_generation_publish_preserves_existing_current_pointer() {
        let root = temp_root("serverless-current-preserved");
        let first = ServerlessFilePlan::new(&root, "db", 1);
        let mut first_index = ServerlessExtentIndex::new(1);
        first_index.push(
            ServerlessExtentRef::new(
                "events",
                b"a".to_vec(),
                b"z".to_vec(),
                "collection-data.redpack",
                0,
                b"first",
                true,
            )
            .expect("extent"),
        );
        let first_pointer = first
            .publish_core_generation(&first_index, b"first", b"secondary")
            .expect("publish first generation");

        let second = ServerlessFilePlan::new(&root, "db", 2);
        let mut incomplete = ServerlessManifest::new("db", 2);
        incomplete.push(ServerlessManifestEntry::from_bytes(
            ServerlessPackKind::BootIndex,
            "boot-index.redpack",
            b"boot",
        ));
        incomplete
            .write_to_path(second.manifest_path())
            .expect("write incomplete manifest");
        let err = second
            .publish_generation_pointer(&incomplete)
            .expect_err("incomplete generation rejected");
        assert!(err.to_string().contains("missing required"), "{err}");

        assert_eq!(
            first.read_current_pointer().expect("read current"),
            first_pointer
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn writer_lease_json_round_trips_and_preserves_fencing_token() {
        let lease = ServerlessWriterLease {
            database_key: "main".to_string(),
            holder_id: "writer-a".to_string(),
            term: 7,
            generation: 3,
            acquired_at_ms: 100,
            expires_at_ms: 200,
        };

        let decoded = decode_serverless_writer_lease_json(
            &encode_serverless_writer_lease_json(&lease).expect("encode lease"),
        )
        .expect("decode lease");

        assert_eq!(decoded, lease);
        assert_eq!(decoded.fencing_token(), (7, 3));
        assert!(!decoded.is_expired(199));
        assert!(decoded.is_expired(200));
        assert!(decoded.fenced_by_term(8));
    }

    #[test]
    fn writer_lease_json_decodes_legacy_missing_term_as_base_term() {
        let decoded = decode_serverless_writer_lease_json(
            br#"{
                "database_key": "main",
                "holder_id": "writer-a",
                "generation": 3,
                "acquired_at_ms": 100,
                "expires_at_ms": 200
            }"#,
        )
        .expect("decode legacy lease");

        assert_eq!(decoded.term, SERVERLESS_WRITER_LEASE_DEFAULT_TERM);
    }

    #[test]
    fn writer_lease_artifact_names_are_deterministic() {
        assert_eq!(
            serverless_writer_lease_key("leases/", "main"),
            "leases/main.lease.json"
        );
        assert_eq!(
            serverless_writer_lease_temp_path("write", 10, 20, 30)
                .file_name()
                .and_then(|name| name.to_str()),
            Some("reddb-lease-write-10-20-30.json")
        );
    }

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "reddb-file-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
}
