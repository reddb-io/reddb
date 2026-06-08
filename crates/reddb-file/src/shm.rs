use std::io;

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
        let header = ShmHeader {
            version: SHM_VERSION,
            owner_pid: 42,
            generation: 7,
            reader_count: 3,
            last_heartbeat_ms: 99,
        };

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
        let header = ShmHeader {
            version: SHM_VERSION,
            owner_pid: 1,
            generation: 1,
            reader_count: 0,
            last_heartbeat_ms: 1,
        };
        let mut encoded = header.encode();
        encoded[20] ^= 0xff;

        let err = ShmHeader::decode(&encoded).expect_err("checksum must fail");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }
}
