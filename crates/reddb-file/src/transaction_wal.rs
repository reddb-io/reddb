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

fn transaction_wal_record_checksum(bytes: &[u8]) -> u8 {
    bytes.iter().fold(0, |acc, &byte| acc ^ byte)
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
}
