//! Transaction WAL record envelope.
//!
//! The runtime owns transaction semantics; this module owns the persisted
//! record envelope: fixed header, payload length, and checksum.

use crate::{RdbFileError, RdbFileResult};

pub const TRANSACTION_WAL_RECORD_HEADER_LEN: usize = 32;
pub const TRANSACTION_WAL_RECORD_LEN_LEN: usize = 4;
pub const TRANSACTION_WAL_RECORD_CHECKSUM_LEN: usize = 1;
pub const TRANSACTION_WAL_RECORD_MIN_LEN: usize = TRANSACTION_WAL_RECORD_HEADER_LEN
    + TRANSACTION_WAL_RECORD_LEN_LEN
    + TRANSACTION_WAL_RECORD_CHECKSUM_LEN;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransactionWalRecordFrame {
    pub lsn: u64,
    pub txn_id: u64,
    pub prev_lsn: Option<u64>,
    pub timestamp: u64,
    pub entry_type_payload: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransactionWalEntryPayload {
    Begin,
    Commit,
    Abort,
    Insert {
        key: Vec<u8>,
        value: Vec<u8>,
    },
    Update {
        key: Vec<u8>,
        old_value: Vec<u8>,
        new_value: Vec<u8>,
    },
    Delete {
        key: Vec<u8>,
        old_value: Vec<u8>,
    },
    Checkpoint {
        active_txns: Vec<u64>,
    },
    Savepoint {
        name: String,
    },
    RollbackToSavepoint {
        name: String,
    },
    Compensate {
        original_lsn: u64,
    },
    End,
}

pub fn encode_transaction_wal_record_frame(frame: &TransactionWalRecordFrame) -> Vec<u8> {
    let mut buf = Vec::with_capacity(
        TRANSACTION_WAL_RECORD_HEADER_LEN
            + TRANSACTION_WAL_RECORD_LEN_LEN
            + frame.entry_type_payload.len()
            + TRANSACTION_WAL_RECORD_CHECKSUM_LEN,
    );

    buf.extend(&frame.lsn.to_le_bytes());
    buf.extend(&frame.txn_id.to_le_bytes());
    buf.extend(&frame.prev_lsn.unwrap_or(0).to_le_bytes());
    buf.extend(&frame.timestamp.to_le_bytes());
    buf.extend(&(frame.entry_type_payload.len() as u32).to_le_bytes());
    buf.extend(&frame.entry_type_payload);

    let checksum = transaction_wal_record_checksum(&buf);
    buf.push(checksum);
    buf
}

pub fn decode_transaction_wal_record_frame(
    data: &[u8],
) -> RdbFileResult<TransactionWalRecordFrame> {
    if data.len() < TRANSACTION_WAL_RECORD_MIN_LEN {
        return Err(invalid("transaction WAL record too short"));
    }

    let mut offset = 0;
    let lsn = read_u64(data, &mut offset, "missing transaction WAL entry LSN")?;
    let txn_id = read_u64(data, &mut offset, "missing transaction WAL entry txn id")?;
    let prev_lsn_raw = read_u64(data, &mut offset, "missing transaction WAL entry prev_lsn")?;
    let prev_lsn = if prev_lsn_raw == 0 {
        None
    } else {
        Some(prev_lsn_raw)
    };
    let timestamp = read_u64(data, &mut offset, "missing transaction WAL entry timestamp")?;
    let payload_len = read_u32(
        data,
        &mut offset,
        "missing transaction WAL entry payload length",
    )? as usize;
    let entry_type_payload = read_bytes(
        data,
        &mut offset,
        payload_len,
        "truncated transaction WAL entry payload",
    )?
    .to_vec();

    let stored_checksum = *data
        .get(offset)
        .ok_or_else(|| invalid("missing transaction WAL entry checksum"))?;
    let computed = transaction_wal_record_checksum(&data[..offset]);
    if stored_checksum != computed {
        return Err(invalid("transaction WAL record checksum mismatch"));
    }
    offset += TRANSACTION_WAL_RECORD_CHECKSUM_LEN;
    if offset != data.len() {
        return Err(invalid("transaction WAL record has trailing bytes"));
    }

    Ok(TransactionWalRecordFrame {
        lsn,
        txn_id,
        prev_lsn,
        timestamp,
        entry_type_payload,
    })
}

pub fn transaction_wal_record_encoded_len(payload_len: usize) -> usize {
    TRANSACTION_WAL_RECORD_HEADER_LEN
        + TRANSACTION_WAL_RECORD_LEN_LEN
        + payload_len
        + TRANSACTION_WAL_RECORD_CHECKSUM_LEN
}

pub fn encode_transaction_wal_entry_payload(payload: &TransactionWalEntryPayload) -> Vec<u8> {
    let mut buf = Vec::new();

    match payload {
        TransactionWalEntryPayload::Begin => buf.push(0),
        TransactionWalEntryPayload::Commit => buf.push(1),
        TransactionWalEntryPayload::Abort => buf.push(2),
        TransactionWalEntryPayload::Insert { key, value } => {
            buf.push(3);
            put_bytes(&mut buf, key);
            put_bytes(&mut buf, value);
        }
        TransactionWalEntryPayload::Update {
            key,
            old_value,
            new_value,
        } => {
            buf.push(4);
            put_bytes(&mut buf, key);
            put_bytes(&mut buf, old_value);
            put_bytes(&mut buf, new_value);
        }
        TransactionWalEntryPayload::Delete { key, old_value } => {
            buf.push(5);
            put_bytes(&mut buf, key);
            put_bytes(&mut buf, old_value);
        }
        TransactionWalEntryPayload::Checkpoint { active_txns } => {
            buf.push(6);
            buf.extend(&(active_txns.len() as u32).to_le_bytes());
            for txn in active_txns {
                buf.extend(&txn.to_le_bytes());
            }
        }
        TransactionWalEntryPayload::Savepoint { name } => {
            buf.push(7);
            put_bytes(&mut buf, name.as_bytes());
        }
        TransactionWalEntryPayload::RollbackToSavepoint { name } => {
            buf.push(8);
            put_bytes(&mut buf, name.as_bytes());
        }
        TransactionWalEntryPayload::Compensate { original_lsn } => {
            buf.push(9);
            buf.extend(&original_lsn.to_le_bytes());
        }
        TransactionWalEntryPayload::End => buf.push(10),
    }

    buf
}

pub fn decode_transaction_wal_entry_payload(
    data: &[u8],
) -> RdbFileResult<(TransactionWalEntryPayload, usize)> {
    if data.is_empty() {
        return Err(invalid("empty transaction WAL entry payload"));
    }

    let mut offset = 0;
    let tag = read_bytes(
        data,
        &mut offset,
        1,
        "missing transaction WAL entry payload tag",
    )?[0];

    let payload = match tag {
        0 => TransactionWalEntryPayload::Begin,
        1 => TransactionWalEntryPayload::Commit,
        2 => TransactionWalEntryPayload::Abort,
        3 => {
            let key = take_len_prefixed_bytes(
                data,
                &mut offset,
                "missing transaction WAL insert key length",
                "truncated transaction WAL insert key",
            )?;
            let value = take_len_prefixed_bytes(
                data,
                &mut offset,
                "missing transaction WAL insert value length",
                "truncated transaction WAL insert value",
            )?;
            TransactionWalEntryPayload::Insert { key, value }
        }
        4 => {
            let key = take_len_prefixed_bytes(
                data,
                &mut offset,
                "missing transaction WAL update key length",
                "truncated transaction WAL update key",
            )?;
            let old_value = take_len_prefixed_bytes(
                data,
                &mut offset,
                "missing transaction WAL update old value length",
                "truncated transaction WAL update old value",
            )?;
            let new_value = take_len_prefixed_bytes(
                data,
                &mut offset,
                "missing transaction WAL update new value length",
                "truncated transaction WAL update new value",
            )?;
            TransactionWalEntryPayload::Update {
                key,
                old_value,
                new_value,
            }
        }
        5 => {
            let key = take_len_prefixed_bytes(
                data,
                &mut offset,
                "missing transaction WAL delete key length",
                "truncated transaction WAL delete key",
            )?;
            let old_value = take_len_prefixed_bytes(
                data,
                &mut offset,
                "missing transaction WAL delete old value length",
                "truncated transaction WAL delete old value",
            )?;
            TransactionWalEntryPayload::Delete { key, old_value }
        }
        6 => {
            let count = read_u32(
                data,
                &mut offset,
                "missing transaction WAL checkpoint txn count",
            )? as usize;
            let mut active_txns = Vec::with_capacity(count);
            for _ in 0..count {
                active_txns.push(read_u64(
                    data,
                    &mut offset,
                    "truncated transaction WAL checkpoint transaction id",
                )?);
            }
            TransactionWalEntryPayload::Checkpoint { active_txns }
        }
        7 => {
            let name = take_len_prefixed_string(
                data,
                &mut offset,
                "missing transaction WAL savepoint name length",
                "truncated transaction WAL savepoint name",
            )?;
            TransactionWalEntryPayload::Savepoint { name }
        }
        8 => {
            let name = take_len_prefixed_string(
                data,
                &mut offset,
                "missing transaction WAL rollback-to-savepoint name length",
                "truncated transaction WAL rollback-to-savepoint name",
            )?;
            TransactionWalEntryPayload::RollbackToSavepoint { name }
        }
        9 => {
            let original_lsn = read_u64(
                data,
                &mut offset,
                "truncated transaction WAL compensate original LSN",
            )?;
            TransactionWalEntryPayload::Compensate { original_lsn }
        }
        10 => TransactionWalEntryPayload::End,
        _ => return Err(invalid("invalid transaction WAL entry payload tag")),
    };

    Ok((payload, offset))
}

fn transaction_wal_record_checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |acc, &byte| acc ^ byte)
}

fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend(&(bytes.len() as u32).to_le_bytes());
    out.extend(bytes);
}

fn take_len_prefixed_bytes(
    data: &[u8],
    offset: &mut usize,
    len_context: &'static str,
    bytes_context: &'static str,
) -> RdbFileResult<Vec<u8>> {
    let len = read_u32(data, offset, len_context)? as usize;
    Ok(read_bytes(data, offset, len, bytes_context)?.to_vec())
}

fn take_len_prefixed_string(
    data: &[u8],
    offset: &mut usize,
    len_context: &'static str,
    bytes_context: &'static str,
) -> RdbFileResult<String> {
    Ok(String::from_utf8_lossy(&take_len_prefixed_bytes(
        data,
        offset,
        len_context,
        bytes_context,
    )?)
    .to_string())
}

fn read_u32(data: &[u8], offset: &mut usize, context: &'static str) -> RdbFileResult<u32> {
    Ok(u32::from_le_bytes(read_array::<4>(data, offset, context)?))
}

fn read_u64(data: &[u8], offset: &mut usize, context: &'static str) -> RdbFileResult<u64> {
    Ok(u64::from_le_bytes(read_array::<8>(data, offset, context)?))
}

fn read_array<const N: usize>(
    data: &[u8],
    offset: &mut usize,
    context: &'static str,
) -> RdbFileResult<[u8; N]> {
    let bytes = read_bytes(data, offset, N, context)?;
    let mut array = [0u8; N];
    array.copy_from_slice(bytes);
    Ok(array)
}

fn read_bytes<'a>(
    data: &'a [u8],
    offset: &mut usize,
    len: usize,
    context: &'static str,
) -> RdbFileResult<&'a [u8]> {
    let end = offset.saturating_add(len);
    if end > data.len() {
        return Err(invalid(context));
    }
    let bytes = &data[*offset..end];
    *offset = end;
    Ok(bytes)
}

fn invalid(message: impl Into<String>) -> RdbFileError {
    RdbFileError::InvalidOperation(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transaction_wal_record_frame_round_trips() {
        let frame = TransactionWalRecordFrame {
            lsn: 42,
            txn_id: 7,
            prev_lsn: Some(41),
            timestamp: 1234,
            entry_type_payload: b"payload".to_vec(),
        };

        let bytes = encode_transaction_wal_record_frame(&frame);
        assert_eq!(decode_transaction_wal_record_frame(&bytes).unwrap(), frame);
    }

    #[test]
    fn transaction_wal_record_frame_rejects_checksum_mismatch() {
        let frame = TransactionWalRecordFrame {
            lsn: 1,
            txn_id: 2,
            prev_lsn: None,
            timestamp: 3,
            entry_type_payload: b"x".to_vec(),
        };
        let mut bytes = encode_transaction_wal_record_frame(&frame);
        *bytes.last_mut().unwrap() ^= 0xff;

        let err = decode_transaction_wal_record_frame(&bytes).unwrap_err();
        assert!(err.to_string().contains("checksum mismatch"));
    }

    #[test]
    fn transaction_wal_entry_payload_round_trips() {
        let payloads = vec![
            TransactionWalEntryPayload::Begin,
            TransactionWalEntryPayload::Commit,
            TransactionWalEntryPayload::Abort,
            TransactionWalEntryPayload::Insert {
                key: b"k".to_vec(),
                value: b"v".to_vec(),
            },
            TransactionWalEntryPayload::Update {
                key: b"k".to_vec(),
                old_value: b"old".to_vec(),
                new_value: b"new".to_vec(),
            },
            TransactionWalEntryPayload::Delete {
                key: b"k".to_vec(),
                old_value: b"v".to_vec(),
            },
            TransactionWalEntryPayload::Checkpoint {
                active_txns: vec![1, 2, 3],
            },
            TransactionWalEntryPayload::Savepoint {
                name: "sp1".to_string(),
            },
            TransactionWalEntryPayload::RollbackToSavepoint {
                name: "sp1".to_string(),
            },
            TransactionWalEntryPayload::Compensate { original_lsn: 9 },
            TransactionWalEntryPayload::End,
        ];

        for payload in payloads {
            let bytes = encode_transaction_wal_entry_payload(&payload);
            let (decoded, consumed) = decode_transaction_wal_entry_payload(&bytes).unwrap();
            assert_eq!(decoded, payload);
            assert_eq!(consumed, bytes.len());
        }
    }

    #[test]
    fn transaction_wal_entry_payload_rejects_truncated_insert() {
        let err = decode_transaction_wal_entry_payload(&[3, 4, 0, 0, 0, b'k']).unwrap_err();
        assert!(err
            .to_string()
            .contains("truncated transaction WAL insert key"));
    }
}
