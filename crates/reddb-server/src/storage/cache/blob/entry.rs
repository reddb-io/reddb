use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

use super::super::compressor::Compressed;
use super::super::extended_ttl::{EffectiveExpiry, ExpiryDecision, ExtendedTtlPolicy};
use super::{BlobCacheHit, BlobCachePolicy, CacheError};

#[derive(Debug)]
pub(super) struct Entry {
    pub(super) bytes: Arc<[u8]>,
    pub(super) content_metadata: BTreeMap<String, String>,
    pub(super) tags: BTreeSet<String>,
    pub(super) dependencies: BTreeSet<String>,
    pub(super) size: usize,
    pub(super) visited: bool,
    pub(super) expires_at_unix_ms: Option<u64>,
    pub(super) priority: u8,
    pub(super) version: Option<u64>,
    pub(super) namespace_generation: u64,
    /// Wall-clock time of the most recent access (`put` or successful
    /// `get`). Updated on hits to drive [`ExtendedTtlPolicy::idle_ttl_ms`].
    /// L1-only — never propagated to the L2 record (cache is the source of
    /// truth for access patterns).
    pub(super) last_access_unix_ms: u64,
    /// Extended TTL knobs captured from the [`BlobCachePolicy`] at insert
    /// time, including any jitter expansion that was already applied to
    /// `expires_at_unix_ms`.
    pub(super) extended: ExtendedTtlPolicy,
}

impl Entry {
    pub(super) fn new(
        bytes: Vec<u8>,
        content_metadata: BTreeMap<String, String>,
        tags: BTreeSet<String>,
        dependencies: BTreeSet<String>,
        policy: BlobCachePolicy,
        namespace_generation: u64,
        now_ms: u64,
        namespace: &str,
        key: &str,
    ) -> Self {
        let size = bytes.len();
        Self {
            bytes: Arc::<[u8]>::from(bytes),
            content_metadata,
            tags,
            dependencies,
            size,
            visited: true,
            expires_at_unix_ms: effective_expires_at_unix_ms(policy, now_ms, namespace, key),
            priority: policy.priority_value(),
            version: policy.version_value(),
            namespace_generation,
            last_access_unix_ms: now_ms,
            extended: policy.extended_value(),
        }
    }

    pub(super) fn hit(&self) -> BlobCacheHit {
        BlobCacheHit::new(
            Arc::clone(&self.bytes),
            self.content_metadata.clone(),
            self.version,
        )
    }

    pub(super) fn hit_stale(&self, window_remaining_ms: u64) -> BlobCacheHit {
        BlobCacheHit::new_stale(
            Arc::clone(&self.bytes),
            self.content_metadata.clone(),
            self.version,
            window_remaining_ms,
        )
    }

    pub(super) fn is_expired_at(&self, now_ms: u64) -> bool {
        self.expires_at_unix_ms
            .is_some_and(|expires_at| now_ms >= expires_at)
    }
}

/// Stable seed for [`EffectiveExpiry::jittered_ttl_ms`] derived from the
/// (namespace, key, now_ms) triple. The same triple always yields the
/// same seed so jitter is deterministic per insert.
pub(super) fn jitter_seed(namespace: &str, key: &str, now_ms: u64) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    namespace.hash(&mut hasher);
    key.hash(&mut hasher);
    now_ms.hash(&mut hasher);
    hasher.finish()
}

pub(super) fn effective_expires_at_unix_ms(
    policy: BlobCachePolicy,
    now_ms: u64,
    namespace: &str,
    key: &str,
) -> Option<u64> {
    let extended = policy.extended_value();
    // Jitter only applies to the relative `ttl_ms` knob; an absolute
    // `expires_at_unix_ms` is treated as a hard ceiling and is never
    // pushed out by jitter.
    let jittered_ttl = policy.ttl_ms_value().map(|base| {
        if extended.jitter_pct > 0 {
            EffectiveExpiry::jittered_ttl_ms(
                base,
                extended.jitter_pct,
                jitter_seed(namespace, key, now_ms),
            )
        } else {
            base
        }
    });
    match (jittered_ttl, policy.expires_at_unix_ms_value()) {
        (Some(ttl), Some(abs)) => Some(now_ms.saturating_add(ttl).min(abs)),
        (Some(ttl), None) => Some(now_ms.saturating_add(ttl)),
        (None, Some(abs)) => Some(abs),
        (None, None) => None,
    }
}

pub(super) const L2_CONTROL_MAGIC: &[u8; 4] = b"RDB2";
pub(super) const L2_METADATA_MAGIC: &[u8; 4] = b"RDCM";
pub(super) const L2_BLOB_MAGIC: &[u8; 4] = b"RDCB";

#[derive(Debug, Clone, Default)]
pub(super) struct L2Control {
    pub(super) metadata_root: u32,
    pub(super) bytes_in_use: u64,
}

impl L2Control {
    pub(super) fn read(path: &Path) -> Result<Self, CacheError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let mut file = File::open(path).map_err(|err| CacheError::L2Io(err.to_string()))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|err| CacheError::L2Io(err.to_string()))?;
        if bytes.len() < 16 || &bytes[0..4] != L2_CONTROL_MAGIC {
            return Err(CacheError::L2Io(
                "invalid blob-cache L2 control file".into(),
            ));
        }
        Ok(Self {
            metadata_root: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            bytes_in_use: u64::from_le_bytes([
                bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                bytes[15],
            ]),
        })
    }

    pub(super) fn write(&self, path: &Path) -> Result<(), CacheError> {
        let mut bytes = Vec::with_capacity(16);
        bytes.extend_from_slice(L2_CONTROL_MAGIC);
        bytes.extend_from_slice(&self.metadata_root.to_le_bytes());
        bytes.extend_from_slice(&self.bytes_in_use.to_le_bytes());
        let tmp = path.with_extension("ctl.tmp");
        {
            let mut file = File::create(&tmp).map_err(|err| CacheError::L2Io(err.to_string()))?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|err| CacheError::L2Io(err.to_string()))?;
        }
        std::fs::rename(&tmp, path).map_err(|err| CacheError::L2Io(err.to_string()))
    }
}

/// On-disk format marker for the bytes the L2 blob-chain holds.
///
/// `V1Raw` (= 0) is the legacy format: the chain bytes are the original
/// payload verbatim. `V2Framed` (= 1) is the post-#192 format: the chain
/// bytes are the [`Compressed`] disk encoding (1-byte tag, optional 4-byte
/// `original_len`, then the encoded payload).
///
/// New writes always emit `V2Framed`. Reads dispatch on this field so older
/// `V1Raw` entries on disk still decode correctly until they age out.
pub(super) const L2_FORMAT_V1_RAW: u8 = 0;
pub(super) const L2_FORMAT_V2_FRAMED: u8 = 1;

pub(super) const L2_FRAME_TAG_RAW: u8 = 0;
pub(super) const L2_FRAME_TAG_ZSTD: u8 = 1;

#[derive(Debug, Clone)]
pub(super) struct L2Record {
    pub(super) namespace: String,
    pub(super) key: String,
    pub(super) expires_at_unix_ms: Option<u64>,
    pub(super) namespace_generation: u64,
    pub(super) priority: u8,
    pub(super) version: Option<u64>,
    pub(super) root_page: u32,
    pub(super) page_count: u32,
    pub(super) byte_len: u64,
    pub(super) checksum: u32,
    /// On-disk format tag for the blob chain. `0` means legacy raw bytes
    /// (entries written before #192); `1` means the post-#192 framed
    /// `Compressed` encoding. Forward-compat read: the field is parsed
    /// optionally so records persisted before this byte was reserved
    /// continue to deserialize as `V1Raw`.
    pub(super) format_version: u8,
}

impl L2Record {
    pub(super) fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(96 + self.namespace.len() + self.key.len());
        out.extend_from_slice(L2_METADATA_MAGIC);
        write_l2_string(&mut out, &self.namespace);
        write_l2_string(&mut out, &self.key);
        out.extend_from_slice(&self.expires_at_unix_ms.unwrap_or(0).to_le_bytes());
        out.extend_from_slice(&self.namespace_generation.to_le_bytes());
        out.push(self.priority);
        out.extend_from_slice(&self.version.unwrap_or(0).to_le_bytes());
        out.extend_from_slice(&self.root_page.to_le_bytes());
        out.extend_from_slice(&self.page_count.to_le_bytes());
        out.extend_from_slice(&self.byte_len.to_le_bytes());
        out.extend_from_slice(&self.checksum.to_le_bytes());
        out.push(self.format_version);
        out
    }

    pub(super) fn decode(mut bytes: &[u8]) -> Result<Self, CacheError> {
        if bytes.len() < 4 || &bytes[0..4] != L2_METADATA_MAGIC {
            return Err(CacheError::L2Io("invalid blob-cache L2 metadata".into()));
        }
        bytes = &bytes[4..];
        let namespace = read_l2_string(&mut bytes)?;
        let key = read_l2_string(&mut bytes)?;
        if bytes.len() < 41 {
            return Err(CacheError::L2Io("truncated blob-cache L2 metadata".into()));
        }
        let expires_at = u64::from_le_bytes(bytes[0..8].try_into().expect("len checked"));
        let namespace_generation =
            u64::from_le_bytes(bytes[8..16].try_into().expect("len checked"));
        let priority = bytes[16];
        let version = u64::from_le_bytes(bytes[17..25].try_into().expect("len checked"));
        let root_page = u32::from_le_bytes(bytes[25..29].try_into().expect("len checked"));
        let page_count = u32::from_le_bytes(bytes[29..33].try_into().expect("len checked"));
        let byte_len = u64::from_le_bytes(bytes[33..41].try_into().expect("len checked"));
        let checksum = if bytes.len() >= 45 {
            u32::from_le_bytes(bytes[41..45].try_into().expect("len checked"))
        } else {
            0
        };
        // Optional `format_version` byte (added in #192 lane 2/5). Records
        // written before this commit do not include it; they describe the
        // legacy `V1Raw` chain layout.
        let format_version = if bytes.len() >= 46 {
            bytes[45]
        } else {
            L2_FORMAT_V1_RAW
        };
        Ok(Self {
            namespace,
            key,
            expires_at_unix_ms: (expires_at != 0).then_some(expires_at),
            namespace_generation,
            priority,
            version: (version != 0).then_some(version),
            root_page,
            page_count,
            byte_len,
            checksum,
            format_version,
        })
    }

    pub(super) fn is_expired_at(&self, now_ms: u64) -> bool {
        self.expires_at_unix_ms
            .is_some_and(|expires_at| now_ms >= expires_at)
    }
}

/// Encode a [`Compressed`] payload into the V2 chain layout: `[tag]` for
/// `Raw`, or `[tag, original_len_le32, encoded_bytes...]` for `Zstd`.
///
/// The header overhead (1 byte for `Raw`, 5 bytes for `Zstd`) is intentional
/// — it lets the read path recover the original payload length without
/// trusting the [`L2Record::byte_len`] field, and lets `decode_v2_frame`
/// fail loudly on corruption rather than silently mis-slicing.
pub(super) fn encode_v2_frame(c: &Compressed) -> Vec<u8> {
    match c {
        Compressed::Raw(bytes) => {
            let mut out = Vec::with_capacity(1 + bytes.len());
            out.push(L2_FRAME_TAG_RAW);
            out.extend_from_slice(bytes);
            out
        }
        Compressed::Zstd { bytes, original_len } => {
            let mut out = Vec::with_capacity(5 + bytes.len());
            out.push(L2_FRAME_TAG_ZSTD);
            out.extend_from_slice(&original_len.to_le_bytes());
            out.extend_from_slice(bytes);
            out
        }
    }
}

/// Decode the V2 chain layout produced by [`encode_v2_frame`].
pub(super) fn decode_v2_frame(bytes: &[u8]) -> Result<Compressed, CacheError> {
    if bytes.is_empty() {
        return Err(CacheError::L2Io(
            "empty blob-cache L2 v2 frame".into(),
        ));
    }
    match bytes[0] {
        L2_FRAME_TAG_RAW => Ok(Compressed::Raw(bytes[1..].to_vec())),
        L2_FRAME_TAG_ZSTD => {
            if bytes.len() < 5 {
                return Err(CacheError::L2Io(
                    "truncated blob-cache L2 zstd frame".into(),
                ));
            }
            let original_len = u32::from_le_bytes(bytes[1..5].try_into().expect("len checked"));
            Ok(Compressed::Zstd {
                bytes: bytes[5..].to_vec(),
                original_len,
            })
        }
        other => Err(CacheError::L2Io(format!(
            "unknown blob-cache L2 frame tag {other}"
        ))),
    }
}

pub(super) fn write_l2_string(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as u16).to_le_bytes());
    out.extend_from_slice(value.as_bytes());
}

pub(super) fn read_l2_string(bytes: &mut &[u8]) -> Result<String, CacheError> {
    if bytes.len() < 2 {
        return Err(CacheError::L2Io("truncated blob-cache L2 string".into()));
    }
    let len = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
    *bytes = &bytes[2..];
    if bytes.len() < len {
        return Err(CacheError::L2Io("truncated blob-cache L2 string".into()));
    }
    let value = std::str::from_utf8(&bytes[..len])
        .map_err(|err| CacheError::L2Io(err.to_string()))?
        .to_string();
    *bytes = &bytes[len..];
    Ok(value)
}

pub(super) fn encode_l2_key(namespace: &str, key: &str) -> Vec<u8> {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    namespace.hash(&mut hasher);
    let namespace_hash = hasher.finish();
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    key.hash(&mut hasher);
    let key_hash = hasher.finish();
    let mut out = Vec::with_capacity(20 + namespace.len() + key.len());
    out.extend_from_slice(&namespace_hash.to_be_bytes());
    out.extend_from_slice(&key_hash.to_be_bytes());
    write_l2_string(&mut out, namespace);
    write_l2_string(&mut out, key);
    out
}
