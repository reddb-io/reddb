//! Main WAL file header contract.
//!
//! The storage engine owns WAL record semantics. This module owns the file
//! header bytes that let readers identify and version a WAL artifact.
//!
//! ```text
//! [magic    4 bytes = b"RDBW"]
//! [version  1 byte  = current format version]
//! [reserved 3 bytes = zero]
//! ```

use std::io;

pub const WAL_FILE_MAGIC: &[u8; 4] = b"RDBW";
pub const WAL_FILE_VERSION: u8 = 3;
pub const WAL_FILE_VERSION_V2: u8 = 2;
pub const WAL_FILE_HEADER_BYTES: usize = 8;
/// Size of one main-engine WAL segment/preallocation extent.
///
/// This is the file-contract boundary used by the server writer when it
/// reserves disk blocks ahead of the append frontier. Runtime buffering and
/// fsync policy stay in `reddb-server`; segment sizing lives here with the WAL
/// artifact contract.
pub const MAIN_WAL_SEGMENT_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalFileHeader {
    pub version: u8,
}

pub fn encode_wal_file_header() -> [u8; WAL_FILE_HEADER_BYTES] {
    let mut header = [0u8; WAL_FILE_HEADER_BYTES];
    header[0..4].copy_from_slice(WAL_FILE_MAGIC);
    header[4] = WAL_FILE_VERSION;
    header
}

pub fn decode_wal_file_header(header: &[u8; WAL_FILE_HEADER_BYTES]) -> io::Result<WalFileHeader> {
    if &header[0..4] != WAL_FILE_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Invalid WAL magic bytes",
        ));
    }

    let version = header[4];
    if version != WAL_FILE_VERSION && version != WAL_FILE_VERSION_V2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Unsupported WAL version: {version}"),
        ));
    }

    Ok(WalFileHeader { version })
}

/// Next main-WAL segment boundary strictly above `pos`.
///
/// `pos` already at a boundary still rounds up to the following one, so a
/// writer using this for preallocation always keeps at least one boundary ahead
/// of the current append frontier.
pub fn next_main_wal_segment_boundary(pos: u64) -> u64 {
    (pos / MAIN_WAL_SEGMENT_BYTES + 1) * MAIN_WAL_SEGMENT_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wal_file_header_encodes_current_version() {
        let header = encode_wal_file_header();
        assert_eq!(&header[0..4], WAL_FILE_MAGIC);
        assert_eq!(header[4], WAL_FILE_VERSION);
        assert_eq!(
            decode_wal_file_header(&header).unwrap().version,
            WAL_FILE_VERSION
        );
    }

    #[test]
    fn wal_file_header_accepts_legacy_v2() {
        let mut header = encode_wal_file_header();
        header[4] = WAL_FILE_VERSION_V2;
        assert_eq!(
            decode_wal_file_header(&header).unwrap().version,
            WAL_FILE_VERSION_V2
        );
    }

    #[test]
    fn wal_file_header_rejects_bad_magic_and_version() {
        let mut bad_magic = encode_wal_file_header();
        bad_magic[0] = b'X';
        assert_eq!(
            decode_wal_file_header(&bad_magic).unwrap_err().to_string(),
            "Invalid WAL magic bytes"
        );

        let mut bad_version = encode_wal_file_header();
        bad_version[4] = 99;
        assert_eq!(
            decode_wal_file_header(&bad_version)
                .unwrap_err()
                .to_string(),
            "Unsupported WAL version: 99"
        );
    }

    #[test]
    fn main_wal_segment_boundary_rounds_strictly_above_position() {
        assert_eq!(next_main_wal_segment_boundary(0), MAIN_WAL_SEGMENT_BYTES);
        assert_eq!(next_main_wal_segment_boundary(8), MAIN_WAL_SEGMENT_BYTES);
        assert_eq!(
            next_main_wal_segment_boundary(MAIN_WAL_SEGMENT_BYTES - 1),
            MAIN_WAL_SEGMENT_BYTES
        );
        assert_eq!(
            next_main_wal_segment_boundary(MAIN_WAL_SEGMENT_BYTES),
            2 * MAIN_WAL_SEGMENT_BYTES
        );
        assert_eq!(
            next_main_wal_segment_boundary(MAIN_WAL_SEGMENT_BYTES + 1),
            2 * MAIN_WAL_SEGMENT_BYTES
        );
    }
}
