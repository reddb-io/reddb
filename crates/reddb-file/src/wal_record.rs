//! Main WAL record byte contract.
//!
//! Runtime code owns the semantic meaning of each record. This module owns the
//! persisted record tags, body framing, compression tag, term envelope, and
//! record checksum.
//!
//! Main WAL files are a sequence of frames after the file header. Every frame
//! starts with a stable record type tag, carries a versioned body, and ends with
//! a CRC32 checksum over the persisted bytes that precede the checksum.

use crate::{WAL_FILE_VERSION, WAL_FILE_VERSION_V2};
use std::io::{self, Read};

pub const MAIN_WAL_DEFAULT_COMPRESS_THRESHOLD: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MainWalRecordType {
    Begin = 1,
    Commit = 2,
    Rollback = 3,
    PageWrite = 4,
    Checkpoint = 5,
    PageWriteCompressed = 6,
    TxCommitBatch = 7,
    FullPageImage = 8,
    VectorInsert = 9,
    ProbabilisticDelta = 10,
}

impl MainWalRecordType {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Begin),
            2 => Some(Self::Commit),
            3 => Some(Self::Rollback),
            4 => Some(Self::PageWrite),
            5 => Some(Self::Checkpoint),
            6 => Some(Self::PageWriteCompressed),
            7 => Some(Self::TxCommitBatch),
            8 => Some(Self::FullPageImage),
            9 => Some(Self::VectorInsert),
            10 => Some(Self::ProbabilisticDelta),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MainWalCompression {
    None = 0,
    Zstd = 1,
}

impl MainWalCompression {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::None),
            1 => Some(Self::Zstd),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum MainWalRecordFrame {
    Begin {
        tx_id: u64,
    },
    Commit {
        tx_id: u64,
    },
    Rollback {
        tx_id: u64,
    },
    PageWrite {
        tx_id: u64,
        page_id: u32,
        data: Vec<u8>,
    },
    TxCommitBatch {
        tx_id: u64,
        actions: Vec<Vec<u8>>,
    },
    FullPageImage {
        tx_id: u64,
        page_id: u32,
        ckpt_epoch: u64,
        data: Vec<u8>,
    },
    VectorInsert {
        collection: String,
        entity_id: u64,
        vector: Vec<f32>,
    },
    ProbabilisticDelta {
        kind: u8,
        operation: u8,
        name: String,
        operands: Vec<Vec<u8>>,
    },
    Checkpoint {
        lsn: u64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MainWalRecordAuthority {
    pub term: u64,
    pub ownership_epoch: Option<u64>,
}

pub fn encode_main_wal_record_frame(frame: &MainWalRecordFrame, term: u64) -> io::Result<Vec<u8>> {
    encode_main_wal_record_frame_with_authority(
        frame,
        MainWalRecordAuthority {
            term,
            ownership_epoch: None,
        },
    )
}

pub fn encode_main_wal_record_frame_with_authority(
    frame: &MainWalRecordFrame,
    authority: MainWalRecordAuthority,
) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    encode_main_wal_record_frame_with_authority_into(frame, authority, &mut out)?;
    Ok(out)
}

pub fn encode_main_wal_record_frame_into(
    frame: &MainWalRecordFrame,
    term: u64,
    out: &mut Vec<u8>,
) -> io::Result<()> {
    encode_main_wal_record_frame_with_authority_into(
        frame,
        MainWalRecordAuthority {
            term,
            ownership_epoch: None,
        },
        out,
    )
}

pub fn encode_main_wal_record_frame_with_authority_into(
    frame: &MainWalRecordFrame,
    authority: MainWalRecordAuthority,
    out: &mut Vec<u8>,
) -> io::Result<()> {
    let start = out.len();
    match frame {
        MainWalRecordFrame::Begin { tx_id } => {
            write_type_and_authority(out, MainWalRecordType::Begin, authority);
            out.extend_from_slice(&tx_id.to_le_bytes());
        }
        MainWalRecordFrame::Commit { tx_id } => {
            write_type_and_authority(out, MainWalRecordType::Commit, authority);
            out.extend_from_slice(&tx_id.to_le_bytes());
        }
        MainWalRecordFrame::Rollback { tx_id } => {
            write_type_and_authority(out, MainWalRecordType::Rollback, authority);
            out.extend_from_slice(&tx_id.to_le_bytes());
        }
        MainWalRecordFrame::PageWrite {
            tx_id,
            page_id,
            data,
        } => {
            if data.len() >= MAIN_WAL_DEFAULT_COMPRESS_THRESHOLD {
                if let Ok(compressed) = zstd::bulk::compress(data.as_slice(), 3) {
                    if compressed.len() < data.len() {
                        write_type_and_authority(
                            out,
                            MainWalRecordType::PageWriteCompressed,
                            authority,
                        );
                        out.extend_from_slice(&tx_id.to_le_bytes());
                        out.extend_from_slice(&page_id.to_le_bytes());
                        out.push(MainWalCompression::Zstd as u8);
                        write_u32_len(out, data.len(), "main wal original page length")?;
                        write_u32_len(out, compressed.len(), "main wal compressed page length")?;
                        out.extend_from_slice(&compressed);
                        append_crc(out, start);
                        return Ok(());
                    }
                }
            }

            write_type_and_authority(out, MainWalRecordType::PageWrite, authority);
            out.extend_from_slice(&tx_id.to_le_bytes());
            out.extend_from_slice(&page_id.to_le_bytes());
            write_u32_len(out, data.len(), "main wal page length")?;
            out.extend_from_slice(data);
        }
        MainWalRecordFrame::TxCommitBatch { tx_id, actions } => {
            write_type_and_authority(out, MainWalRecordType::TxCommitBatch, authority);
            out.extend_from_slice(&tx_id.to_le_bytes());
            write_u32_len(out, actions.len(), "main wal action count")?;
            for action in actions {
                write_u32_len(out, action.len(), "main wal action length")?;
                out.extend_from_slice(action);
            }
        }
        MainWalRecordFrame::FullPageImage {
            tx_id,
            page_id,
            ckpt_epoch,
            data,
        } => {
            write_type_and_authority(out, MainWalRecordType::FullPageImage, authority);
            out.extend_from_slice(&tx_id.to_le_bytes());
            out.extend_from_slice(&page_id.to_le_bytes());
            out.extend_from_slice(&ckpt_epoch.to_le_bytes());
            write_u32_len(out, data.len(), "main wal full-page image length")?;
            out.extend_from_slice(data);
        }
        MainWalRecordFrame::VectorInsert {
            collection,
            entity_id,
            vector,
        } => {
            write_type_and_authority(out, MainWalRecordType::VectorInsert, authority);
            write_u32_len(out, collection.len(), "main wal collection name length")?;
            out.extend_from_slice(collection.as_bytes());
            out.extend_from_slice(&entity_id.to_le_bytes());
            write_u32_len(out, vector.len(), "main wal vector length")?;
            for value in vector {
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
        MainWalRecordFrame::ProbabilisticDelta {
            kind,
            operation,
            name,
            operands,
        } => {
            write_type_and_authority(out, MainWalRecordType::ProbabilisticDelta, authority);
            out.push(*kind);
            out.push(*operation);
            write_u32_len(out, name.len(), "main wal probabilistic name length")?;
            out.extend_from_slice(name.as_bytes());
            write_u32_len(out, operands.len(), "main wal probabilistic operand count")?;
            for operand in operands {
                write_u32_len(out, operand.len(), "main wal probabilistic operand length")?;
                out.extend_from_slice(operand);
            }
        }
        MainWalRecordFrame::Checkpoint { lsn } => {
            write_type_and_authority(out, MainWalRecordType::Checkpoint, authority);
            out.extend_from_slice(&lsn.to_le_bytes());
        }
    }

    append_crc(out, start);
    Ok(())
}

pub fn decode_main_wal_record_frame<R: Read>(
    reader: &mut R,
    format_version: u8,
    default_term: u64,
) -> io::Result<Option<(u64, MainWalRecordFrame)>> {
    Ok(decode_main_wal_record_frame_with_authority(
        reader,
        format_version,
        MainWalRecordAuthority {
            term: default_term,
            ownership_epoch: None,
        },
    )?
    .map(|(authority, frame)| (authority.term, frame)))
}

pub fn decode_main_wal_record_frame_with_authority<R: Read>(
    reader: &mut R,
    format_version: u8,
    default_authority: MainWalRecordAuthority,
) -> io::Result<Option<(MainWalRecordAuthority, MainWalRecordFrame)>> {
    let mut checksum_bytes = Vec::new();
    let mut type_buf = [0u8; 1];
    match reader.read_exact(&mut type_buf) {
        Ok(()) => checksum_bytes.extend_from_slice(&type_buf),
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }

    let record_type = MainWalRecordType::from_u8(type_buf[0])
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "Invalid record type"))?;

    let authority = match format_version {
        WAL_FILE_VERSION => {
            let term = read_u64_tracked(reader, &mut checksum_bytes)?;
            let ownership_epoch = read_u64_tracked(reader, &mut checksum_bytes)?;
            MainWalRecordAuthority {
                term,
                ownership_epoch: if ownership_epoch == 0 {
                    None
                } else {
                    Some(ownership_epoch)
                },
            }
        }
        WAL_FILE_VERSION_V2 => default_authority,
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("Unsupported WAL version: {format_version}"),
            ));
        }
    };

    let frame = match record_type {
        MainWalRecordType::Begin => MainWalRecordFrame::Begin {
            tx_id: read_u64_tracked(reader, &mut checksum_bytes)?,
        },
        MainWalRecordType::Commit => MainWalRecordFrame::Commit {
            tx_id: read_u64_tracked(reader, &mut checksum_bytes)?,
        },
        MainWalRecordType::Rollback => MainWalRecordFrame::Rollback {
            tx_id: read_u64_tracked(reader, &mut checksum_bytes)?,
        },
        MainWalRecordType::PageWrite => {
            let tx_id = read_u64_tracked(reader, &mut checksum_bytes)?;
            let page_id = read_u32_tracked(reader, &mut checksum_bytes)?;
            let data = read_bytes_tracked(reader, &mut checksum_bytes)?;
            MainWalRecordFrame::PageWrite {
                tx_id,
                page_id,
                data,
            }
        }
        MainWalRecordType::PageWriteCompressed => {
            let tx_id = read_u64_tracked(reader, &mut checksum_bytes)?;
            let page_id = read_u32_tracked(reader, &mut checksum_bytes)?;
            let compression = read_compression_tracked(reader, &mut checksum_bytes)?;
            let original_len = read_u32_tracked(reader, &mut checksum_bytes)? as usize;
            let compressed = read_bytes_tracked(reader, &mut checksum_bytes)?;
            let data = match compression {
                MainWalCompression::Zstd => {
                    let mut out = vec![0u8; original_len];
                    zstd::bulk::decompress_to_buffer(&compressed, &mut out).map_err(|err| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("WAL zstd decompress failed: {err}"),
                        )
                    })?;
                    out
                }
                MainWalCompression::None => compressed,
            };
            MainWalRecordFrame::PageWrite {
                tx_id,
                page_id,
                data,
            }
        }
        MainWalRecordType::TxCommitBatch => {
            let tx_id = read_u64_tracked(reader, &mut checksum_bytes)?;
            let count = read_u32_tracked(reader, &mut checksum_bytes)? as usize;
            let mut actions = Vec::with_capacity(count);
            for _ in 0..count {
                actions.push(read_bytes_tracked(reader, &mut checksum_bytes)?);
            }
            MainWalRecordFrame::TxCommitBatch { tx_id, actions }
        }
        MainWalRecordType::FullPageImage => {
            let tx_id = read_u64_tracked(reader, &mut checksum_bytes)?;
            let page_id = read_u32_tracked(reader, &mut checksum_bytes)?;
            let ckpt_epoch = read_u64_tracked(reader, &mut checksum_bytes)?;
            let data = read_bytes_tracked(reader, &mut checksum_bytes)?;
            MainWalRecordFrame::FullPageImage {
                tx_id,
                page_id,
                ckpt_epoch,
                data,
            }
        }
        MainWalRecordType::VectorInsert => {
            let collection = String::from_utf8(read_bytes_tracked(reader, &mut checksum_bytes)?)
                .map_err(|err| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("invalid collection utf8: {err}"),
                    )
                })?;
            let entity_id = read_u64_tracked(reader, &mut checksum_bytes)?;
            let count = read_u32_tracked(reader, &mut checksum_bytes)? as usize;
            let mut vector = Vec::with_capacity(count);
            for _ in 0..count {
                vector.push(f32::from_le_bytes(read_array_tracked(
                    reader,
                    &mut checksum_bytes,
                )?));
            }
            MainWalRecordFrame::VectorInsert {
                collection,
                entity_id,
                vector,
            }
        }
        MainWalRecordType::ProbabilisticDelta => {
            let kind = read_u8_tracked(reader, &mut checksum_bytes)?;
            let operation = read_u8_tracked(reader, &mut checksum_bytes)?;
            let name = String::from_utf8(read_bytes_tracked(reader, &mut checksum_bytes)?)
                .map_err(|err| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("invalid probabilistic name utf8: {err}"),
                    )
                })?;
            let count = read_u32_tracked(reader, &mut checksum_bytes)? as usize;
            let mut operands = Vec::with_capacity(count);
            for _ in 0..count {
                operands.push(read_bytes_tracked(reader, &mut checksum_bytes)?);
            }
            MainWalRecordFrame::ProbabilisticDelta {
                kind,
                operation,
                name,
                operands,
            }
        }
        MainWalRecordType::Checkpoint => MainWalRecordFrame::Checkpoint {
            lsn: read_u64_tracked(reader, &mut checksum_bytes)?,
        },
    };

    let stored_crc = read_u32_untracked(reader)?;
    if crc32(&checksum_bytes) != stored_crc {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "WAL record checksum mismatch",
        ));
    }

    Ok(Some((authority, frame)))
}

fn write_type_and_authority(
    out: &mut Vec<u8>,
    record_type: MainWalRecordType,
    authority: MainWalRecordAuthority,
) {
    out.push(record_type as u8);
    out.extend_from_slice(&authority.term.to_le_bytes());
    out.extend_from_slice(&authority.ownership_epoch.unwrap_or(0).to_le_bytes());
}

fn write_u32_len(out: &mut Vec<u8>, len: usize, label: &'static str) -> io::Result<()> {
    let len = u32::try_from(len).map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, label))?;
    out.extend_from_slice(&len.to_le_bytes());
    Ok(())
}

fn append_crc(out: &mut Vec<u8>, start: usize) {
    let checksum = crc32(&out[start..]);
    out.extend_from_slice(&checksum.to_le_bytes());
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(bytes);
    hasher.finalize()
}

fn read_compression_tracked<R: Read>(
    reader: &mut R,
    checksum_bytes: &mut Vec<u8>,
) -> io::Result<MainWalCompression> {
    let value = read_array_tracked::<_, 1>(reader, checksum_bytes)?[0];
    MainWalCompression::from_u8(value).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Unknown WAL compression algorithm: {value}"),
        )
    })
}

fn read_bytes_tracked<R: Read>(
    reader: &mut R,
    checksum_bytes: &mut Vec<u8>,
) -> io::Result<Vec<u8>> {
    let len = read_u32_tracked(reader, checksum_bytes)? as usize;
    let mut bytes = vec![0u8; len];
    reader.read_exact(&mut bytes)?;
    checksum_bytes.extend_from_slice(&bytes);
    Ok(bytes)
}

fn read_u64_tracked<R: Read>(reader: &mut R, checksum_bytes: &mut Vec<u8>) -> io::Result<u64> {
    Ok(u64::from_le_bytes(read_array_tracked(
        reader,
        checksum_bytes,
    )?))
}

fn read_u32_tracked<R: Read>(reader: &mut R, checksum_bytes: &mut Vec<u8>) -> io::Result<u32> {
    Ok(u32::from_le_bytes(read_array_tracked(
        reader,
        checksum_bytes,
    )?))
}

fn read_u8_tracked<R: Read>(reader: &mut R, checksum_bytes: &mut Vec<u8>) -> io::Result<u8> {
    Ok(read_array_tracked::<_, 1>(reader, checksum_bytes)?[0])
}

fn read_array_tracked<R: Read, const N: usize>(
    reader: &mut R,
    checksum_bytes: &mut Vec<u8>,
) -> io::Result<[u8; N]> {
    let mut bytes = [0u8; N];
    reader.read_exact(&mut bytes)?;
    checksum_bytes.extend_from_slice(&bytes);
    Ok(bytes)
}

fn read_u32_untracked<R: Read>(reader: &mut R) -> io::Result<u32> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn main_wal_record_types_are_stable() {
        assert_eq!(
            MainWalRecordType::from_u8(1),
            Some(MainWalRecordType::Begin)
        );
        assert_eq!(
            MainWalRecordType::from_u8(9),
            Some(MainWalRecordType::VectorInsert)
        );
        assert_eq!(
            MainWalRecordType::from_u8(10),
            Some(MainWalRecordType::ProbabilisticDelta)
        );
        assert_eq!(MainWalRecordType::from_u8(11), None);
    }

    #[test]
    fn main_wal_records_round_trip_current_format() {
        let frames = vec![
            MainWalRecordFrame::Begin { tx_id: 1 },
            MainWalRecordFrame::Commit { tx_id: 2 },
            MainWalRecordFrame::Rollback { tx_id: 3 },
            MainWalRecordFrame::Checkpoint { lsn: 4 },
            MainWalRecordFrame::PageWrite {
                tx_id: 5,
                page_id: 6,
                data: vec![1, 2, 3],
            },
            MainWalRecordFrame::TxCommitBatch {
                tx_id: 7,
                actions: vec![b"insert".to_vec(), b"update".to_vec()],
            },
            MainWalRecordFrame::FullPageImage {
                tx_id: 8,
                page_id: 9,
                ckpt_epoch: 10,
                data: vec![0xAA; 128],
            },
            MainWalRecordFrame::VectorInsert {
                collection: "vectors".into(),
                entity_id: 11,
                vector: vec![1.0, -0.5, 0.25],
            },
            MainWalRecordFrame::ProbabilisticDelta {
                kind: 1,
                operation: 1,
                name: "visitors".into(),
                operands: vec![b"alice".to_vec()],
            },
        ];

        for frame in frames {
            let encoded = encode_main_wal_record_frame(&frame, 42).unwrap();
            let mut cursor = Cursor::new(encoded);
            let (term, decoded) = decode_main_wal_record_frame(&mut cursor, WAL_FILE_VERSION, 0)
                .unwrap()
                .unwrap();
            assert_eq!(term, 42);
            assert_eq!(decoded, frame);
        }
    }

    #[test]
    fn main_wal_record_round_trip_current_format_authority_epoch() {
        let frame = MainWalRecordFrame::Begin { tx_id: 42 };
        let authority = MainWalRecordAuthority {
            term: 7,
            ownership_epoch: Some(11),
        };
        let encoded = encode_main_wal_record_frame_with_authority(&frame, authority).unwrap();

        let mut cursor = Cursor::new(encoded);
        let (decoded_authority, decoded) = decode_main_wal_record_frame_with_authority(
            &mut cursor,
            WAL_FILE_VERSION,
            MainWalRecordAuthority {
                term: 1,
                ownership_epoch: None,
            },
        )
        .unwrap()
        .unwrap();

        assert_eq!(decoded_authority, authority);
        assert_eq!(decoded, frame);
    }

    #[test]
    fn main_wal_record_accepts_legacy_v2_without_term() {
        let mut encoded = Vec::new();
        encoded.push(MainWalRecordType::Begin as u8);
        encoded.extend_from_slice(&42u64.to_le_bytes());
        let checksum = crc32(&encoded);
        encoded.extend_from_slice(&checksum.to_le_bytes());

        let mut cursor = Cursor::new(encoded);
        let (term, frame) = decode_main_wal_record_frame(&mut cursor, WAL_FILE_VERSION_V2, 99)
            .unwrap()
            .unwrap();
        assert_eq!(term, 99);
        assert_eq!(frame, MainWalRecordFrame::Begin { tx_id: 42 });
    }

    #[test]
    fn main_wal_record_detects_checksum_mismatch() {
        let frame = MainWalRecordFrame::Begin { tx_id: 42 };
        let mut encoded = encode_main_wal_record_frame(&frame, 1).unwrap();
        let last = encoded.len() - 1;
        encoded[last] ^= 0xFF;

        let mut cursor = Cursor::new(encoded);
        assert_eq!(
            decode_main_wal_record_frame(&mut cursor, WAL_FILE_VERSION, 0)
                .unwrap_err()
                .to_string(),
            "WAL record checksum mismatch"
        );
    }

    #[test]
    fn main_wal_record_compresses_and_decompresses_page_writes() {
        let frame = MainWalRecordFrame::PageWrite {
            tx_id: 7,
            page_id: 3,
            data: vec![0xAB; 1024],
        };
        let encoded = encode_main_wal_record_frame(&frame, 1).unwrap();
        assert_eq!(encoded[0], MainWalRecordType::PageWriteCompressed as u8);

        let mut cursor = Cursor::new(encoded);
        let (_, decoded) = decode_main_wal_record_frame(&mut cursor, WAL_FILE_VERSION, 0)
            .unwrap()
            .unwrap();
        assert_eq!(decoded, frame);
    }
}
