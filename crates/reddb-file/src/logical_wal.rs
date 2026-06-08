use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

pub const LOGICAL_WAL_SPOOL_MAGIC: &[u8; 4] = b"RDLW";
pub const LOGICAL_WAL_SPOOL_VERSION_V1: u8 = 1;
pub const LOGICAL_WAL_SPOOL_VERSION_V2: u8 = 2;
pub const LOGICAL_WAL_SPOOL_VERSION_V3: u8 = 3;
pub const LOGICAL_WAL_SPOOL_VERSION_CURRENT: u8 = LOGICAL_WAL_SPOOL_VERSION_V3;
pub const LOGICAL_WAL_V3_HEADER_LEN: u64 = 4 + 1 + 8 + 8 + 8 + 4;
pub const LOGICAL_WAL_CRC_LEN: u64 = 4;
pub const LOGICAL_WAL_SEEK_INDEX_INTERVAL: u64 = 64;

const MAX_PLAUSIBLE_PAYLOAD: usize = 256 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicalWalEntry {
    pub term: u64,
    pub lsn: u64,
    pub timestamp_ms: u64,
    pub data: Vec<u8>,
    pub version: u8,
}

pub fn encode_logical_wal_v3(
    term: u64,
    lsn: u64,
    timestamp_ms: u64,
    data: &[u8],
) -> io::Result<Vec<u8>> {
    if data.len() > u32::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "logical WAL payload of {} bytes exceeds 4 GiB framing limit",
                data.len()
            ),
        ));
    }

    let mut frame = Vec::with_capacity(
        LOGICAL_WAL_V3_HEADER_LEN as usize + data.len() + LOGICAL_WAL_CRC_LEN as usize,
    );
    frame.extend_from_slice(LOGICAL_WAL_SPOOL_MAGIC);
    frame.push(LOGICAL_WAL_SPOOL_VERSION_CURRENT);
    frame.extend_from_slice(&term.to_le_bytes());
    frame.extend_from_slice(&lsn.to_le_bytes());
    frame.extend_from_slice(&timestamp_ms.to_le_bytes());
    frame.extend_from_slice(&(data.len() as u32).to_le_bytes());
    frame.extend_from_slice(data);
    let crc = compute_logical_v3_crc(
        LOGICAL_WAL_SPOOL_VERSION_CURRENT,
        term,
        lsn,
        timestamp_ms,
        data,
    );
    frame.extend_from_slice(&crc.to_le_bytes());
    Ok(frame)
}

pub fn encode_logical_wal_v2_for_compat(
    lsn: u64,
    timestamp_ms: u64,
    data: &[u8],
) -> io::Result<Vec<u8>> {
    if data.len() > u32::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "logical WAL v2 payload exceeds 4 GiB framing limit",
        ));
    }

    let mut frame = Vec::with_capacity(4 + 1 + 8 + 8 + 4 + data.len() + 4);
    frame.extend_from_slice(LOGICAL_WAL_SPOOL_MAGIC);
    frame.push(LOGICAL_WAL_SPOOL_VERSION_V2);
    frame.extend_from_slice(&lsn.to_le_bytes());
    frame.extend_from_slice(&timestamp_ms.to_le_bytes());
    frame.extend_from_slice(&(data.len() as u32).to_le_bytes());
    frame.extend_from_slice(data);
    let crc = compute_logical_v2_crc(LOGICAL_WAL_SPOOL_VERSION_V2, lsn, timestamp_ms, data);
    frame.extend_from_slice(&crc.to_le_bytes());
    Ok(frame)
}

pub fn read_and_repair_logical_wal_entries(path: &Path) -> io::Result<Vec<LogicalWalEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let mut file = OpenOptions::new().read(true).write(true).open(path)?;
    let mut entries = Vec::new();
    let mut last_good_offset: u64 = 0;
    let mut corrupt = false;

    loop {
        let record_start = file.stream_position()?;
        let mut magic = [0u8; 4];
        match file.read_exact(&mut magic) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(err) => return Err(err),
        }
        if &magic != LOGICAL_WAL_SPOOL_MAGIC {
            corrupt = true;
            break;
        }

        let mut version = [0u8; 1];
        if let Err(err) = file.read_exact(&mut version) {
            if err.kind() == io::ErrorKind::UnexpectedEof {
                corrupt = true;
                break;
            }
            return Err(err);
        }

        let entry = match version[0] {
            LOGICAL_WAL_SPOOL_VERSION_V3 => read_one_v3(&mut file, record_start),
            LOGICAL_WAL_SPOOL_VERSION_V2 => read_one_v2(&mut file, record_start),
            LOGICAL_WAL_SPOOL_VERSION_V1 => read_one_v1(&mut file, record_start),
            _ => {
                corrupt = true;
                break;
            }
        };

        match entry {
            Ok(entry) => {
                entries.push(entry);
                last_good_offset = file.stream_position()?;
            }
            Err(_) => {
                corrupt = true;
                break;
            }
        }
    }

    if corrupt {
        let total_len = file.metadata()?.len();
        if last_good_offset < total_len {
            file.set_len(last_good_offset)?;
            file.sync_all()?;
        }
    }

    Ok(entries)
}

pub fn read_logical_wal_entries_from(
    path: &Path,
    start_offset: u64,
) -> io::Result<Vec<LogicalWalEntry>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let mut file = OpenOptions::new().read(true).open(path)?;
    file.seek(SeekFrom::Start(start_offset))?;
    let mut entries = Vec::new();
    loop {
        let record_start = file.stream_position()?;
        match read_logical_wal_frame(&mut file, record_start)? {
            Some(entry) => entries.push(entry),
            None => break,
        }
    }
    Ok(entries)
}

pub fn build_logical_wal_seek_index(path: &Path) -> io::Result<(Vec<(u64, u64)>, u64, u64)> {
    if !path.exists() {
        return Ok((Vec::new(), 0, 0));
    }
    let mut file = OpenOptions::new().read(true).open(path)?;
    let mut index = Vec::new();
    let mut ordinal: u64 = 0;
    let mut write_offset: u64 = 0;
    loop {
        let record_start = file.stream_position()?;
        match read_logical_wal_frame(&mut file, record_start)? {
            Some(entry) => {
                if ordinal.is_multiple_of(LOGICAL_WAL_SEEK_INDEX_INTERVAL)
                    && index.last().map(|(l, _)| *l) != Some(entry.lsn)
                {
                    index.push((entry.lsn, record_start));
                }
                ordinal += 1;
                write_offset = file.stream_position()?;
            }
            None => break,
        }
    }
    Ok((index, write_offset, ordinal))
}

pub fn rewrite_logical_wal_entries(
    path: &Path,
    temp_path: &Path,
    entries: &[LogicalWalEntry],
) -> io::Result<u64> {
    let mut temp = File::create(temp_path)?;
    let mut current_lsn = 0;
    for entry in entries {
        let frame = encode_logical_wal_v3(entry.term, entry.lsn, entry.timestamp_ms, &entry.data)?;
        temp.write_all(&frame)?;
        current_lsn = current_lsn.max(entry.lsn);
    }
    temp.sync_all()?;
    fs::rename(temp_path, path)?;
    Ok(current_lsn)
}

fn read_logical_wal_frame(
    file: &mut File,
    record_start: u64,
) -> io::Result<Option<LogicalWalEntry>> {
    let mut magic = [0u8; 4];
    match file.read_exact(&mut magic) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    if &magic != LOGICAL_WAL_SPOOL_MAGIC {
        return Ok(None);
    }
    let mut version = [0u8; 1];
    match file.read_exact(&mut version) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(err) => return Err(err),
    }
    let entry = match version[0] {
        LOGICAL_WAL_SPOOL_VERSION_V3 => read_one_v3(file, record_start),
        LOGICAL_WAL_SPOOL_VERSION_V2 => read_one_v2(file, record_start),
        LOGICAL_WAL_SPOOL_VERSION_V1 => read_one_v1(file, record_start),
        _ => return Ok(None),
    };
    Ok(entry.ok())
}

fn read_one_v3(file: &mut File, record_start: u64) -> Result<LogicalWalEntry, String> {
    let term = read_u64(file, "term", record_start)?;
    let lsn = read_u64(file, "lsn", record_start)?;
    let timestamp_ms = read_u64(file, "timestamp", record_start)?;
    let payload_len = read_u32(file, "payload length", record_start)? as usize;
    let payload = read_payload(file, payload_len, record_start)?;
    let stored_crc = read_u32(file, "crc", record_start)?;
    let expected_crc = compute_logical_v3_crc(
        LOGICAL_WAL_SPOOL_VERSION_V3,
        term,
        lsn,
        timestamp_ms,
        &payload,
    );
    if stored_crc != expected_crc {
        return Err(format!(
            "crc mismatch at offset {record_start}: stored {stored_crc:#010x}, expected {expected_crc:#010x}"
        ));
    }
    Ok(LogicalWalEntry {
        term,
        lsn,
        timestamp_ms,
        data: payload,
        version: LOGICAL_WAL_SPOOL_VERSION_V3,
    })
}

fn read_one_v2(file: &mut File, record_start: u64) -> Result<LogicalWalEntry, String> {
    let lsn = read_u64(file, "lsn", record_start)?;
    let timestamp_ms = read_u64(file, "timestamp", record_start)?;
    let payload_len = read_u32(file, "payload length", record_start)? as usize;
    let payload = read_payload(file, payload_len, record_start)?;
    let stored_crc = read_u32(file, "crc", record_start)?;
    let expected_crc =
        compute_logical_v2_crc(LOGICAL_WAL_SPOOL_VERSION_V2, lsn, timestamp_ms, &payload);
    if stored_crc != expected_crc {
        return Err(format!(
            "crc mismatch at offset {record_start}: stored {stored_crc:#010x}, expected {expected_crc:#010x}"
        ));
    }
    Ok(LogicalWalEntry {
        term: 0,
        lsn,
        timestamp_ms,
        data: payload,
        version: LOGICAL_WAL_SPOOL_VERSION_V2,
    })
}

fn read_one_v1(file: &mut File, record_start: u64) -> Result<LogicalWalEntry, String> {
    let lsn = read_u64(file, "v1 lsn", record_start)?;
    let payload_len = read_u64(file, "v1 payload length", record_start)? as usize;
    let payload = read_payload(file, payload_len, record_start)?;
    Ok(LogicalWalEntry {
        term: 0,
        lsn,
        timestamp_ms: 0,
        data: payload,
        version: LOGICAL_WAL_SPOOL_VERSION_V1,
    })
}

fn read_u64(file: &mut File, field: &'static str, record_start: u64) -> Result<u64, String> {
    let mut bytes = [0u8; 8];
    file.read_exact(&mut bytes)
        .map_err(|err| format!("torn {field} at offset {record_start}: {err}"))?;
    Ok(u64::from_le_bytes(bytes))
}

fn read_u32(file: &mut File, field: &'static str, record_start: u64) -> Result<u32, String> {
    let mut bytes = [0u8; 4];
    file.read_exact(&mut bytes)
        .map_err(|err| format!("torn {field} at offset {record_start}: {err}"))?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_payload(file: &mut File, payload_len: usize, record_start: u64) -> Result<Vec<u8>, String> {
    if payload_len > MAX_PLAUSIBLE_PAYLOAD {
        return Err(format!(
            "implausible payload_len {payload_len} at offset {record_start}"
        ));
    }
    let mut payload = vec![0u8; payload_len];
    file.read_exact(&mut payload).map_err(|err| {
        format!("torn payload at offset {record_start} (expected {payload_len} bytes): {err}")
    })?;
    Ok(payload)
}

fn compute_logical_v2_crc(version: u8, lsn: u64, timestamp_ms: u64, payload: &[u8]) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&[version]);
    hasher.update(&lsn.to_le_bytes());
    hasher.update(&timestamp_ms.to_le_bytes());
    hasher.update(&(payload.len() as u32).to_le_bytes());
    hasher.update(payload);
    hasher.finalize()
}

fn compute_logical_v3_crc(
    version: u8,
    term: u64,
    lsn: u64,
    timestamp_ms: u64,
    payload: &[u8],
) -> u32 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(&[version]);
    hasher.update(&term.to_le_bytes());
    hasher.update(&lsn.to_le_bytes());
    hasher.update(&timestamp_ms.to_le_bytes());
    hasher.update(&(payload.len() as u32).to_le_bytes());
    hasher.update(payload);
    hasher.finalize()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_path(name: &str) -> std::path::PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!("reddb-file-logical-wal-{name}-{suffix}.wal"))
    }

    #[test]
    fn v3_roundtrip_preserves_framing_term_and_timestamp() {
        let path = temp_path("v3");
        let frame = encode_logical_wal_v3(7, 42, 99, b"payload").expect("encode");
        std::fs::write(&path, frame).expect("write");

        let entries = read_and_repair_logical_wal_entries(&path).expect("read");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].term, 7);
        assert_eq!(entries[0].lsn, 42);
        assert_eq!(entries[0].timestamp_ms, 99);
        assert_eq!(entries[0].data, b"payload");
        assert_eq!(entries[0].version, LOGICAL_WAL_SPOOL_VERSION_V3);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn repair_truncates_torn_tail_to_last_valid_record() {
        let path = temp_path("repair");
        let first = encode_logical_wal_v3(1, 1, 10, b"ok").expect("first");
        let mut bytes = first.clone();
        bytes.extend_from_slice(&encode_logical_wal_v3(1, 2, 11, b"torn").expect("second")[..12]);
        std::fs::write(&path, bytes).expect("write");

        let entries = read_and_repair_logical_wal_entries(&path).expect("repair");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].lsn, 1);
        assert_eq!(
            std::fs::metadata(&path).expect("metadata").len(),
            first.len() as u64
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn v2_compat_roundtrip_marks_missing_framing_term() {
        let path = temp_path("v2");
        let frame = encode_logical_wal_v2_for_compat(3, 44, b"legacy").expect("encode");
        std::fs::write(&path, frame).expect("write");

        let entries = read_and_repair_logical_wal_entries(&path).expect("read");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].term, 0);
        assert_eq!(entries[0].lsn, 3);
        assert_eq!(entries[0].timestamp_ms, 44);
        assert_eq!(entries[0].version, LOGICAL_WAL_SPOOL_VERSION_V2);

        let _ = std::fs::remove_file(path);
    }
}
