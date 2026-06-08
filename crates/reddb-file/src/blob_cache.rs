use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

pub const L2_CONTROL_MAGIC: &[u8; 4] = b"RDB2";
pub const L2_METADATA_MAGIC: &[u8; 4] = b"RDCM";
pub const L2_BLOB_MAGIC: &[u8; 4] = b"RDCB";

pub const L2_FORMAT_V1_RAW: u8 = 0;
pub const L2_FORMAT_V2_FRAMED: u8 = 1;

pub const L2_FRAME_TAG_RAW: u8 = 0;
pub const L2_FRAME_TAG_ZSTD: u8 = 1;

const L2_CONTROL_EXTENSION: &str = "blob-cache.ctl";
const L2_CONTROL_TEMP_EXTENSION: &str = "ctl.tmp";
pub const L2_BACKUP_PAGER_SUFFIX: &str = "l2.pager";
pub const L2_BACKUP_CONTROL_SUFFIX: &str = "l2.ctl";

pub fn blob_cache_control_path(l2_path: &Path) -> PathBuf {
    l2_path.with_extension(L2_CONTROL_EXTENSION)
}

pub fn blob_cache_control_temp_path(control_path: &Path) -> PathBuf {
    control_path.with_extension(L2_CONTROL_TEMP_EXTENSION)
}

pub fn blob_cache_l2_backup_pager_key(prefix: &str) -> String {
    format!(
        "{}{}",
        normalize_backup_prefix(prefix),
        L2_BACKUP_PAGER_SUFFIX
    )
}

pub fn blob_cache_l2_backup_control_key(prefix: &str) -> String {
    format!(
        "{}{}",
        normalize_backup_prefix(prefix),
        L2_BACKUP_CONTROL_SUFFIX
    )
}

fn normalize_backup_prefix(prefix: &str) -> String {
    if prefix.is_empty() || prefix.ends_with('/') {
        prefix.to_string()
    } else {
        format!("{prefix}/")
    }
}

#[derive(Debug, Clone, Default)]
pub struct L2Control {
    pub metadata_root: u32,
    pub bytes_in_use: u64,
}

impl L2Control {
    pub fn read(path: &Path) -> io::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let mut file = File::open(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        if bytes.len() < 16 || &bytes[0..4] != L2_CONTROL_MAGIC {
            return Err(invalid_data("invalid blob-cache L2 control file"));
        }
        Ok(Self {
            metadata_root: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
            bytes_in_use: u64::from_le_bytes([
                bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14],
                bytes[15],
            ]),
        })
    }

    pub fn write(&self, path: &Path) -> io::Result<()> {
        let mut bytes = Vec::with_capacity(16);
        bytes.extend_from_slice(L2_CONTROL_MAGIC);
        bytes.extend_from_slice(&self.metadata_root.to_le_bytes());
        bytes.extend_from_slice(&self.bytes_in_use.to_le_bytes());
        let tmp = blob_cache_control_temp_path(path);
        {
            let mut file = File::create(&tmp)?;
            file.write_all(&bytes).and_then(|_| file.sync_all())?;
        }
        std::fs::rename(&tmp, path)
    }
}

#[derive(Debug, Clone)]
pub struct L2Record {
    pub namespace: String,
    pub key: String,
    pub expires_at_unix_ms: Option<u64>,
    pub namespace_generation: u64,
    pub priority: u8,
    pub version: Option<u64>,
    pub root_page: u32,
    pub page_count: u32,
    pub byte_len: u64,
    pub checksum: u32,
    pub format_version: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum L2BlobFrame {
    Raw(Vec<u8>),
    Zstd { bytes: Vec<u8>, original_len: u32 },
}

pub fn encode_l2_v2_frame(frame: &L2BlobFrame) -> Vec<u8> {
    match frame {
        L2BlobFrame::Raw(bytes) => {
            let mut out = Vec::with_capacity(1 + bytes.len());
            out.push(L2_FRAME_TAG_RAW);
            out.extend_from_slice(bytes);
            out
        }
        L2BlobFrame::Zstd {
            bytes,
            original_len,
        } => {
            let mut out = Vec::with_capacity(5 + bytes.len());
            out.push(L2_FRAME_TAG_ZSTD);
            out.extend_from_slice(&original_len.to_le_bytes());
            out.extend_from_slice(bytes);
            out
        }
    }
}

pub fn decode_l2_v2_frame(bytes: &[u8]) -> io::Result<L2BlobFrame> {
    if bytes.is_empty() {
        return Err(invalid_data("empty blob-cache L2 v2 frame"));
    }
    match bytes[0] {
        L2_FRAME_TAG_RAW => Ok(L2BlobFrame::Raw(bytes[1..].to_vec())),
        L2_FRAME_TAG_ZSTD => {
            if bytes.len() < 5 {
                return Err(invalid_data("truncated blob-cache L2 zstd frame"));
            }
            let original_len = u32::from_le_bytes(bytes[1..5].try_into().expect("len checked"));
            Ok(L2BlobFrame::Zstd {
                bytes: bytes[5..].to_vec(),
                original_len,
            })
        }
        other => Err(invalid_data(format!(
            "unknown blob-cache L2 frame tag {other}"
        ))),
    }
}

impl L2Record {
    pub fn encode(&self) -> Vec<u8> {
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

    pub fn decode(mut bytes: &[u8]) -> io::Result<Self> {
        if bytes.len() < 4 || &bytes[0..4] != L2_METADATA_MAGIC {
            return Err(invalid_data("invalid blob-cache L2 metadata"));
        }
        bytes = &bytes[4..];
        let namespace = read_l2_string(&mut bytes)?;
        let key = read_l2_string(&mut bytes)?;
        if bytes.len() < 41 {
            return Err(invalid_data("truncated blob-cache L2 metadata"));
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

    pub fn is_expired_at(&self, now_ms: u64) -> bool {
        self.expires_at_unix_ms
            .is_some_and(|expires_at| now_ms >= expires_at)
    }
}

pub fn encode_l2_key(namespace: &str, key: &str) -> Vec<u8> {
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

fn write_l2_string(out: &mut Vec<u8>, value: &str) {
    out.extend_from_slice(&(value.len() as u16).to_le_bytes());
    out.extend_from_slice(value.as_bytes());
}

fn read_l2_string(bytes: &mut &[u8]) -> io::Result<String> {
    if bytes.len() < 2 {
        return Err(invalid_data("truncated blob-cache L2 string"));
    }
    let len = u16::from_le_bytes([bytes[0], bytes[1]]) as usize;
    *bytes = &bytes[2..];
    if bytes.len() < len {
        return Err(invalid_data("truncated blob-cache L2 string"));
    }
    let value = std::str::from_utf8(&bytes[..len])
        .map_err(|err| invalid_data(err.to_string()))?
        .to_string();
    *bytes = &bytes[len..];
    Ok(value)
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_round_trips() {
        let path = std::env::temp_dir().join(format!(
            "reddb-file-blob-cache-control-{}-{}.ctl",
            std::process::id(),
            unique_nanos()
        ));
        let control = L2Control {
            metadata_root: 42,
            bytes_in_use: 8192,
        };
        control.write(&path).unwrap();
        assert_eq!(L2Control::read(&path).unwrap().metadata_root, 42);
        assert_eq!(L2Control::read(&path).unwrap().bytes_in_use, 8192);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn record_round_trips_and_keeps_legacy_format_default() {
        let record = L2Record {
            namespace: "ns".into(),
            key: "key".into(),
            expires_at_unix_ms: Some(123),
            namespace_generation: 7,
            priority: 9,
            version: Some(11),
            root_page: 13,
            page_count: 17,
            byte_len: 19,
            checksum: 23,
            format_version: L2_FORMAT_V2_FRAMED,
        };
        let decoded = L2Record::decode(&record.encode()).unwrap();
        assert_eq!(decoded.namespace, "ns");
        assert_eq!(decoded.key, "key");
        assert_eq!(decoded.expires_at_unix_ms, Some(123));
        assert_eq!(decoded.namespace_generation, 7);
        assert_eq!(decoded.priority, 9);
        assert_eq!(decoded.version, Some(11));
        assert_eq!(decoded.root_page, 13);
        assert_eq!(decoded.page_count, 17);
        assert_eq!(decoded.byte_len, 19);
        assert_eq!(decoded.checksum, 23);
        assert_eq!(decoded.format_version, L2_FORMAT_V2_FRAMED);

        let mut legacy = record.encode();
        legacy.pop();
        assert_eq!(
            L2Record::decode(&legacy).unwrap().format_version,
            L2_FORMAT_V1_RAW
        );
    }

    #[test]
    fn v2_frame_round_trips_raw_and_zstd_payloads() {
        let raw = L2BlobFrame::Raw(b"payload".to_vec());
        assert_eq!(decode_l2_v2_frame(&encode_l2_v2_frame(&raw)).unwrap(), raw);

        let zstd = L2BlobFrame::Zstd {
            bytes: b"compressed".to_vec(),
            original_len: 1024,
        };
        assert_eq!(
            decode_l2_v2_frame(&encode_l2_v2_frame(&zstd)).unwrap(),
            zstd
        );
    }

    #[test]
    fn blob_cache_control_path_uses_stable_sidecar_extension() {
        assert_eq!(
            blob_cache_control_path(Path::new("/tmp/cache.rdb")),
            PathBuf::from("/tmp/cache.blob-cache.ctl")
        );
    }

    fn unique_nanos() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }
}
