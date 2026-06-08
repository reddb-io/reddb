//! UnifiedStore WAL action frame contract.
//!
//! `reddb-server` owns the semantic meaning of each action. This module owns
//! the persisted byte envelope: version byte, action tag, string framing,
//! record framing, and scalar byte order.

use std::io;

pub const STORE_WAL_ACTION_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreWalActionFrame {
    CreateCollection {
        name: String,
    },
    DropCollection {
        name: String,
    },
    UpsertEntityRecord {
        collection: String,
        record: Vec<u8>,
    },
    DeleteEntityRecord {
        collection: String,
        entity_id: u64,
    },
    BulkUpsertEntityRecords {
        collection: String,
        records: Vec<Vec<u8>>,
    },
    RefreshCollection {
        collection: String,
        records: Vec<Vec<u8>>,
    },
}

pub fn encode_store_wal_action_frame(frame: &StoreWalActionFrame) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    out.push(STORE_WAL_ACTION_VERSION);
    match frame {
        StoreWalActionFrame::CreateCollection { name } => {
            out.push(1);
            write_string(&mut out, name)?;
        }
        StoreWalActionFrame::DropCollection { name } => {
            out.push(2);
            write_string(&mut out, name)?;
        }
        StoreWalActionFrame::UpsertEntityRecord { collection, record } => {
            out.push(3);
            write_string(&mut out, collection)?;
            write_bytes(&mut out, record)?;
        }
        StoreWalActionFrame::DeleteEntityRecord {
            collection,
            entity_id,
        } => {
            out.push(4);
            write_string(&mut out, collection)?;
            out.extend_from_slice(&entity_id.to_le_bytes());
        }
        StoreWalActionFrame::BulkUpsertEntityRecords {
            collection,
            records,
        } => {
            out.push(5);
            write_string(&mut out, collection)?;
            write_record_list(&mut out, records)?;
        }
        StoreWalActionFrame::RefreshCollection {
            collection,
            records,
        } => {
            out.push(6);
            write_string(&mut out, collection)?;
            write_record_list(&mut out, records)?;
        }
    }
    Ok(out)
}

pub fn decode_store_wal_action_frame(bytes: &[u8]) -> io::Result<StoreWalActionFrame> {
    if bytes.len() < 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "store wal action too short",
        ));
    }
    if bytes[0] != STORE_WAL_ACTION_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported store wal version: {}", bytes[0]),
        ));
    }

    let mut pos = 2usize;
    match bytes[1] {
        1 => Ok(StoreWalActionFrame::CreateCollection {
            name: read_string(bytes, &mut pos)?,
        }),
        2 => Ok(StoreWalActionFrame::DropCollection {
            name: read_string(bytes, &mut pos)?,
        }),
        3 => Ok(StoreWalActionFrame::UpsertEntityRecord {
            collection: read_string(bytes, &mut pos)?,
            record: read_bytes(bytes, &mut pos)?,
        }),
        4 => {
            let collection = read_string(bytes, &mut pos)?;
            let entity_id = read_u64(bytes, &mut pos)?;
            Ok(StoreWalActionFrame::DeleteEntityRecord {
                collection,
                entity_id,
            })
        }
        5 => {
            let collection = read_string(bytes, &mut pos)?;
            let records = read_record_list(bytes, &mut pos, "bulk upsert wal action")?;
            Ok(StoreWalActionFrame::BulkUpsertEntityRecords {
                collection,
                records,
            })
        }
        6 => {
            let collection = read_string(bytes, &mut pos)?;
            let records = read_record_list(bytes, &mut pos, "refresh collection wal action")?;
            Ok(StoreWalActionFrame::RefreshCollection {
                collection,
                records,
            })
        }
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported store wal action tag: {other}"),
        )),
    }
}

fn write_record_list(out: &mut Vec<u8>, records: &[Vec<u8>]) -> io::Result<()> {
    let count = u32::try_from(records.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "store wal action record count exceeds u32",
        )
    })?;
    out.extend_from_slice(&count.to_le_bytes());
    for record in records {
        write_bytes(out, record)?;
    }
    Ok(())
}

fn read_record_list(data: &[u8], pos: &mut usize, label: &'static str) -> io::Result<Vec<Vec<u8>>> {
    if data.len().saturating_sub(*pos) < 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("{label}: missing record count"),
        ));
    }
    let count = read_u32(data, pos)? as usize;
    let mut records = Vec::with_capacity(count);
    for _ in 0..count {
        records.push(read_bytes(data, pos)?);
    }
    Ok(records)
}

fn write_string(out: &mut Vec<u8>, value: &str) -> io::Result<()> {
    write_bytes(out, value.as_bytes())
}

fn write_bytes(out: &mut Vec<u8>, value: &[u8]) -> io::Result<()> {
    let len = u32::try_from(value.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "store wal action field exceeds u32",
        )
    })?;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(value);
    Ok(())
}

fn read_u32(data: &[u8], pos: &mut usize) -> io::Result<u32> {
    if data.len().saturating_sub(*pos) < 4 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected eof while reading u32",
        ));
    }
    let value = u32::from_le_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
    *pos += 4;
    Ok(value)
}

fn read_u64(data: &[u8], pos: &mut usize) -> io::Result<u64> {
    if data.len().saturating_sub(*pos) < 8 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected eof while reading u64",
        ));
    }
    let value = u64::from_le_bytes([
        data[*pos],
        data[*pos + 1],
        data[*pos + 2],
        data[*pos + 3],
        data[*pos + 4],
        data[*pos + 5],
        data[*pos + 6],
        data[*pos + 7],
    ]);
    *pos += 8;
    Ok(value)
}

fn read_string(data: &[u8], pos: &mut usize) -> io::Result<String> {
    let bytes = read_bytes(data, pos)?;
    String::from_utf8(bytes).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

fn read_bytes(data: &[u8], pos: &mut usize) -> io::Result<Vec<u8>> {
    let len = read_u32(data, pos)? as usize;
    if data.len().saturating_sub(*pos) < len {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "unexpected eof while reading bytes",
        ));
    }
    let value = data[*pos..*pos + len].to_vec();
    *pos += len;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn store_wal_action_frames_round_trip() {
        let frames = [
            StoreWalActionFrame::CreateCollection {
                name: "users".into(),
            },
            StoreWalActionFrame::DropCollection { name: "old".into() },
            StoreWalActionFrame::UpsertEntityRecord {
                collection: "users".into(),
                record: vec![1, 2, 3],
            },
            StoreWalActionFrame::DeleteEntityRecord {
                collection: "users".into(),
                entity_id: 42,
            },
            StoreWalActionFrame::BulkUpsertEntityRecords {
                collection: "users".into(),
                records: vec![vec![1], vec![2]],
            },
            StoreWalActionFrame::RefreshCollection {
                collection: "users".into(),
                records: vec![vec![3], vec![4]],
            },
        ];

        for frame in frames {
            let encoded = encode_store_wal_action_frame(&frame).unwrap();
            assert_eq!(encoded[0], STORE_WAL_ACTION_VERSION);
            assert_eq!(decode_store_wal_action_frame(&encoded).unwrap(), frame);
        }
    }

    #[test]
    fn store_wal_action_frames_preserve_legacy_errors() {
        assert_eq!(
            decode_store_wal_action_frame(&[STORE_WAL_ACTION_VERSION])
                .unwrap_err()
                .to_string(),
            "store wal action too short"
        );
        assert_eq!(
            decode_store_wal_action_frame(&[99, 1])
                .unwrap_err()
                .to_string(),
            "unsupported store wal version: 99"
        );
        assert_eq!(
            decode_store_wal_action_frame(&[STORE_WAL_ACTION_VERSION, 99])
                .unwrap_err()
                .to_string(),
            "unsupported store wal action tag: 99"
        );
    }
}
