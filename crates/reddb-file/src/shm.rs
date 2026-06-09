//! `<data>.shm` shared-memory file contract.
//!
//! The server owns provisioning policy and owner-pid recovery decisions.
//! This crate owns the binary header and the file operations that preserve it.
//!
//! ## Binary layout (v1, little-endian, 64-byte fixed header)
//!
//! ```text
//! offset size field             notes
//!      0    8 magic             ASCII "RDBSHM01"
//!      8    4 version           u32 = 1
//!     12    4 owner_pid         u32, host pid of the writer that holds the lease
//!     16    8 generation        u64, bumped on every owner takeover or heal
//!     24    8 reader_count      u64, count of attached embedded readers
//!     32    8 last_heartbeat_ms u64, owner heartbeat in unix-ms
//!     40   16 reserved          zeroed, room for v2 fields
//!     56    8 checksum          checksum fold of bytes [0..56)
//! ```

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};

pub const SHM_MAGIC: &[u8; 8] = b"RDBSHM01";
pub const SHM_VERSION: u32 = 1;
pub const SHM_HEADER_SIZE: usize = 64;
pub const SHM_FILE_SIZE: u64 = 4096;

#[derive(Debug, Clone)]
pub struct ShmHeader {
    pub version: u32,
    pub owner_pid: u32,
    pub generation: u64,
    pub reader_count: u64,
    pub last_heartbeat_ms: u64,
}

impl ShmHeader {
    pub fn new(owner_pid: u32, generation: u64, reader_count: u64, last_heartbeat_ms: u64) -> Self {
        Self {
            version: SHM_VERSION,
            owner_pid,
            generation,
            reader_count,
            last_heartbeat_ms,
        }
    }

    pub fn encode(&self) -> [u8; SHM_HEADER_SIZE] {
        let mut buf = [0u8; SHM_HEADER_SIZE];
        buf[0..8].copy_from_slice(SHM_MAGIC);
        buf[8..12].copy_from_slice(&self.version.to_le_bytes());
        buf[12..16].copy_from_slice(&self.owner_pid.to_le_bytes());
        buf[16..24].copy_from_slice(&self.generation.to_le_bytes());
        buf[24..32].copy_from_slice(&self.reader_count.to_le_bytes());
        buf[32..40].copy_from_slice(&self.last_heartbeat_ms.to_le_bytes());
        let checksum = fold_checksum(&buf[..56]);
        buf[56..64].copy_from_slice(&checksum.to_le_bytes());
        buf
    }

    pub fn decode(buf: &[u8; SHM_HEADER_SIZE]) -> io::Result<Self> {
        if &buf[0..8] != SHM_MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "shm magic mismatch",
            ));
        }
        let stored_checksum = u64::from_le_bytes(buf[56..64].try_into().unwrap());
        let computed = fold_checksum(&buf[..56]);
        if stored_checksum != computed {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "shm checksum mismatch",
            ));
        }
        Ok(Self {
            version: u32::from_le_bytes(buf[8..12].try_into().unwrap()),
            owner_pid: u32::from_le_bytes(buf[12..16].try_into().unwrap()),
            generation: u64::from_le_bytes(buf[16..24].try_into().unwrap()),
            reader_count: u64::from_le_bytes(buf[24..32].try_into().unwrap()),
            last_heartbeat_ms: u64::from_le_bytes(buf[32..40].try_into().unwrap()),
        })
    }
}

pub fn initialize_shm_file(file: &mut File, header: &ShmHeader) -> io::Result<()> {
    file.set_len(SHM_FILE_SIZE)?;
    write_shm_header_to_file(file, header)
}

pub fn read_shm_header_from_file(file: &mut File) -> io::Result<ShmHeader> {
    let mut buf = [0u8; SHM_HEADER_SIZE];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut buf)?;
    ShmHeader::decode(&buf)
}

pub fn write_shm_header_to_file(file: &mut File, header: &ShmHeader) -> io::Result<()> {
    let buf = header.encode();
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&buf)?;
    file.sync_data()?;
    Ok(())
}

fn fold_checksum(bytes: &[u8]) -> u64 {
    let mut acc: u64 = 0xcbf29ce484222325;
    for &byte in bytes {
        acc ^= byte as u64;
        acc = acc.wrapping_mul(0x100000001b3);
    }
    acc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shm_header_round_trips() {
        let header = ShmHeader::new(42, 7, 3, 99);

        let encoded = header.encode();
        assert_eq!(&encoded[0..8], SHM_MAGIC);
        assert_eq!(encoded.len(), SHM_HEADER_SIZE);

        let decoded = ShmHeader::decode(&encoded).expect("decode");
        assert_eq!(decoded.version, header.version);
        assert_eq!(decoded.owner_pid, header.owner_pid);
        assert_eq!(decoded.generation, header.generation);
        assert_eq!(decoded.reader_count, header.reader_count);
        assert_eq!(decoded.last_heartbeat_ms, header.last_heartbeat_ms);
    }

    #[test]
    fn shm_header_rejects_checksum_mismatch() {
        let header = ShmHeader::new(1, 1, 0, 1);
        let mut encoded = header.encode();
        encoded[20] ^= 0xff;

        let err = ShmHeader::decode(&encoded).expect_err("checksum must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn shm_file_helpers_initialize_and_rewrite_header() {
        let path = std::env::temp_dir().join(format!(
            "reddb-shm-file-helper-{}-{}.shm",
            std::process::id(),
            unique_test_suffix()
        ));
        let mut file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(&path)
            .expect("create shm file");

        let header = ShmHeader::new(11, 2, 3, 4);
        initialize_shm_file(&mut file, &header).expect("initialize");
        assert_eq!(
            file.metadata().expect("metadata").len(),
            SHM_FILE_SIZE,
            "helper owns the fixed shm file size"
        );

        let decoded = read_shm_header_from_file(&mut file).expect("read initialized header");
        assert_eq!(decoded.owner_pid, 11);
        assert_eq!(decoded.generation, 2);
        assert_eq!(decoded.reader_count, 3);
        assert_eq!(decoded.last_heartbeat_ms, 4);

        let next = ShmHeader::new(12, 3, 0, 9);
        write_shm_header_to_file(&mut file, &next).expect("rewrite");
        let decoded = read_shm_header_from_file(&mut file).expect("read rewritten header");
        assert_eq!(decoded.owner_pid, 12);
        assert_eq!(decoded.generation, 3);
        assert_eq!(decoded.reader_count, 0);
        assert_eq!(decoded.last_heartbeat_ms, 9);

        drop(file);
        let _ = std::fs::remove_file(path);
    }

    fn unique_test_suffix() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    }
}
