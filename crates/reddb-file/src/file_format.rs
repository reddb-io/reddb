//! Core persisted file-format constants.
//!
//! Runtime crates can own page management and columnar execution, but these
//! durable magic/version values are file compatibility contracts.

use std::fmt;

/// Magic bytes for database file identification: `RDDB`.
pub const PAGE_FILE_MAGIC: [u8; 4] = [0x52, 0x44, 0x44, 0x42];

/// Database file version 1.0.0.
pub const PAGE_FILE_VERSION: u32 = 0x0001_0000;

/// Paged database page size.
pub const PAGED_PAGE_SIZE: usize = 16_384;

/// Paged database page-header size before the page-0 database header.
pub const PAGED_PAGE_HEADER_SIZE: usize = 32;

pub const PAGED_CELL_POINTER_SIZE: usize = 2;
pub const PAGED_CELL_HEADER_SIZE: usize = 6;

/// Raw persisted page header encoded at the start of every paged-store page.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PagedPageHeader {
    pub page_type: u8,
    pub flags: u8,
    pub cell_count: u16,
    pub free_start: u16,
    pub free_end: u16,
    pub page_id: u32,
    pub parent_id: u32,
    pub right_child: u32,
    pub lsn: u64,
    pub checksum: u32,
}

pub fn encode_paged_page_header(header: &PagedPageHeader) -> [u8; PAGED_PAGE_HEADER_SIZE] {
    let mut buf = [0u8; PAGED_PAGE_HEADER_SIZE];
    buf[0] = header.page_type;
    buf[1] = header.flags;
    buf[2..4].copy_from_slice(&header.cell_count.to_le_bytes());
    buf[4..6].copy_from_slice(&header.free_start.to_le_bytes());
    buf[6..8].copy_from_slice(&header.free_end.to_le_bytes());
    buf[8..12].copy_from_slice(&header.page_id.to_le_bytes());
    buf[12..16].copy_from_slice(&header.parent_id.to_le_bytes());
    buf[16..20].copy_from_slice(&header.right_child.to_le_bytes());
    buf[20..28].copy_from_slice(&header.lsn.to_le_bytes());
    buf[28..32].copy_from_slice(&header.checksum.to_le_bytes());
    buf
}

pub fn decode_paged_page_header(buf: &[u8; PAGED_PAGE_HEADER_SIZE]) -> PagedPageHeader {
    PagedPageHeader {
        page_type: buf[0],
        flags: buf[1],
        cell_count: u16::from_le_bytes([buf[2], buf[3]]),
        free_start: u16::from_le_bytes([buf[4], buf[5]]),
        free_end: u16::from_le_bytes([buf[6], buf[7]]),
        page_id: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
        parent_id: u32::from_le_bytes([buf[12], buf[13], buf[14], buf[15]]),
        right_child: u32::from_le_bytes([buf[16], buf[17], buf[18], buf[19]]),
        lsn: u64::from_le_bytes([
            buf[20], buf[21], buf[22], buf[23], buf[24], buf[25], buf[26], buf[27],
        ]),
        checksum: u32::from_le_bytes([buf[28], buf[29], buf[30], buf[31]]),
    }
}

pub fn paged_page_type(page: &[u8; PAGED_PAGE_SIZE]) -> u8 {
    page[0]
}

pub fn paged_page_id(page: &[u8; PAGED_PAGE_SIZE]) -> u32 {
    read_u32(page, 8).expect("paged page has header")
}

pub fn paged_page_lsn(page: &[u8; PAGED_PAGE_SIZE]) -> u64 {
    read_u64(page, 20).expect("paged page has header")
}

pub fn set_paged_page_lsn(page: &mut [u8; PAGED_PAGE_SIZE], lsn: u64) {
    write_u64(page, 20, lsn).expect("paged page has header");
}

pub fn paged_page_cell_count(page: &[u8; PAGED_PAGE_SIZE]) -> u16 {
    read_u16(page, 2).expect("paged page has header")
}

pub fn set_paged_page_cell_count(page: &mut [u8; PAGED_PAGE_SIZE], count: u16) {
    write_u16(page, 2, count).expect("paged page has header");
}

pub fn paged_page_parent_id(page: &[u8; PAGED_PAGE_SIZE]) -> u32 {
    read_u32(page, 12).expect("paged page has header")
}

pub fn set_paged_page_parent_id(page: &mut [u8; PAGED_PAGE_SIZE], parent_id: u32) {
    write_u32(page, 12, parent_id).expect("paged page has header");
}

pub fn paged_page_right_child(page: &[u8; PAGED_PAGE_SIZE]) -> u32 {
    read_u32(page, 16).expect("paged page has header")
}

pub fn set_paged_page_right_child(page: &mut [u8; PAGED_PAGE_SIZE], child_id: u32) {
    write_u32(page, 16, child_id).expect("paged page has header");
}

pub fn paged_page_free_start(page: &[u8; PAGED_PAGE_SIZE]) -> u16 {
    read_u16(page, 4).expect("paged page has header")
}

pub fn set_paged_page_free_start(page: &mut [u8; PAGED_PAGE_SIZE], offset: u16) {
    write_u16(page, 4, offset).expect("paged page has header");
}

pub fn paged_page_free_end(page: &[u8; PAGED_PAGE_SIZE]) -> u16 {
    read_u16(page, 6).expect("paged page has header")
}

pub fn set_paged_page_free_end(page: &mut [u8; PAGED_PAGE_SIZE], offset: u16) {
    write_u16(page, 6, offset).expect("paged page has header");
}

pub fn paged_page_checksum(page: &[u8; PAGED_PAGE_SIZE]) -> u32 {
    read_u32(page, 28).expect("paged page has header")
}

pub fn clear_paged_page_checksum(page: &mut [u8; PAGED_PAGE_SIZE]) {
    write_u32(page, 28, 0).expect("paged page has header");
}

pub fn set_paged_page_checksum(page: &mut [u8; PAGED_PAGE_SIZE], checksum: u32) {
    write_u32(page, 28, checksum).expect("paged page has header");
}

pub fn paged_cell_pointer_offset(index: usize) -> Option<usize> {
    let offset = PAGED_PAGE_HEADER_SIZE.checked_add(index.checked_mul(PAGED_CELL_POINTER_SIZE)?)?;
    (offset + PAGED_CELL_POINTER_SIZE <= PAGED_PAGE_SIZE).then_some(offset)
}

pub fn paged_cell_pointer(page: &[u8; PAGED_PAGE_SIZE], index: usize) -> Option<u16> {
    read_u16(page, paged_cell_pointer_offset(index)?).ok()
}

pub fn set_paged_cell_pointer(
    page: &mut [u8; PAGED_PAGE_SIZE],
    index: usize,
    pointer: u16,
) -> bool {
    let Some(offset) = paged_cell_pointer_offset(index) else {
        return false;
    };
    if !paged_cell_pointer_is_valid(pointer) {
        return false;
    }
    write_u16(page, offset, pointer).is_ok()
}

pub fn paged_cell_pointer_is_valid(pointer: u16) -> bool {
    let pointer = pointer as usize;
    (PAGED_PAGE_HEADER_SIZE..PAGED_PAGE_SIZE).contains(&pointer)
}

pub fn paged_cell_len(key_len: usize, value_len: usize) -> Option<usize> {
    PAGED_CELL_HEADER_SIZE
        .checked_add(key_len)?
        .checked_add(value_len)
}

pub fn paged_cell_total_len(page: &[u8; PAGED_PAGE_SIZE], pointer: u16) -> Option<usize> {
    let pointer = pointer as usize;
    if pointer + PAGED_CELL_HEADER_SIZE > PAGED_PAGE_SIZE {
        return None;
    }
    let key_len = read_u16(page, pointer).ok()? as usize;
    let value_len = read_u32(page, pointer + 2).ok()? as usize;
    let total_len = paged_cell_len(key_len, value_len)?;
    (pointer + total_len <= PAGED_PAGE_SIZE).then_some(total_len)
}

pub fn paged_cell_bytes(page: &[u8; PAGED_PAGE_SIZE], pointer: u16) -> Option<&[u8]> {
    let total_len = paged_cell_total_len(page, pointer)?;
    let pointer = pointer as usize;
    Some(&page[pointer..pointer + total_len])
}

pub fn paged_cell_key_value(cell: &[u8]) -> Option<(&[u8], &[u8])> {
    if cell.len() < PAGED_CELL_HEADER_SIZE {
        return None;
    }
    let key_len = u16::from_le_bytes(cell[0..2].try_into().ok()?) as usize;
    let value_len = u32::from_le_bytes(cell[2..6].try_into().ok()?) as usize;
    let total_len = paged_cell_len(key_len, value_len)?;
    if total_len > cell.len() {
        return None;
    }
    let key_start = PAGED_CELL_HEADER_SIZE;
    let value_start = key_start + key_len;
    Some((
        &cell[key_start..value_start],
        &cell[value_start..value_start + value_len],
    ))
}

pub fn write_paged_cell(
    page: &mut [u8; PAGED_PAGE_SIZE],
    offset: u16,
    key: &[u8],
    value: &[u8],
) -> bool {
    let Ok(key_len) = u16::try_from(key.len()) else {
        return false;
    };
    let Ok(value_len) = u32::try_from(value.len()) else {
        return false;
    };
    let Some(total_len) = paged_cell_len(key.len(), value.len()) else {
        return false;
    };
    let offset = offset as usize;
    if offset + total_len > PAGED_PAGE_SIZE {
        return false;
    }

    page[offset..offset + 2].copy_from_slice(&key_len.to_le_bytes());
    page[offset + 2..offset + 6].copy_from_slice(&value_len.to_le_bytes());
    page[offset + PAGED_CELL_HEADER_SIZE..offset + PAGED_CELL_HEADER_SIZE + key.len()]
        .copy_from_slice(key);
    page[offset + PAGED_CELL_HEADER_SIZE + key.len()..offset + total_len].copy_from_slice(value);
    true
}

/// Encryption marker embedded in page 0 when page encryption is enabled.
pub const PAGED_ENCRYPTION_MARKER: [u8; 4] = *b"RDBE";

/// Current page-0 encryption marker offset.
///
/// This preserves the existing on-disk placement. It overlaps the historical
/// physical-header region, so callers must preserve page-0 bytes around normal
/// header writes.
pub const PAGED_ENCRYPTION_MARKER_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 32;

pub const PAGED_ENCRYPTION_SALT_SIZE: usize = 32;
pub const PAGED_ENCRYPTION_KEY_CHECK_PLAINTEXT_SIZE: usize = 32;
pub const PAGED_ENCRYPTION_KEY_CHECK_BLOB_SIZE: usize = 60;
pub const PAGED_ENCRYPTION_HEADER_SIZE: usize =
    PAGED_ENCRYPTION_SALT_SIZE + PAGED_ENCRYPTION_KEY_CHECK_BLOB_SIZE;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PagedEncryptionHeader {
    pub salt: [u8; PAGED_ENCRYPTION_SALT_SIZE],
    pub key_check: Vec<u8>,
}

pub fn encode_paged_encryption_header(header: &PagedEncryptionHeader) -> Vec<u8> {
    let mut out = Vec::with_capacity(PAGED_ENCRYPTION_HEADER_SIZE);
    out.extend_from_slice(&header.salt);
    out.extend_from_slice(&header.key_check);
    out
}

pub fn decode_paged_encryption_header(
    data: &[u8],
) -> Result<PagedEncryptionHeader, DatabaseHeaderError> {
    ensure_len(data, PAGED_ENCRYPTION_HEADER_SIZE)?;
    let mut salt = [0u8; PAGED_ENCRYPTION_SALT_SIZE];
    salt.copy_from_slice(&data[..PAGED_ENCRYPTION_SALT_SIZE]);
    let key_check = data[PAGED_ENCRYPTION_SALT_SIZE..PAGED_ENCRYPTION_HEADER_SIZE].to_vec();
    Ok(PagedEncryptionHeader { salt, key_check })
}

pub fn paged_encryption_marker_present(page: &[u8]) -> bool {
    page.get(PAGED_ENCRYPTION_MARKER_OFFSET..PAGED_ENCRYPTION_MARKER_OFFSET + 4)
        == Some(&PAGED_ENCRYPTION_MARKER)
}

pub fn paged_encryption_header_bytes(page: &[u8]) -> Option<&[u8]> {
    let start = PAGED_ENCRYPTION_MARKER_OFFSET + PAGED_ENCRYPTION_MARKER.len();
    page.get(start..start + PAGED_ENCRYPTION_HEADER_SIZE)
}

pub fn write_paged_encryption_marker_and_header(
    page: &mut [u8],
    header_bytes: &[u8],
) -> Result<(), DatabaseHeaderError> {
    let marker_end = PAGED_ENCRYPTION_MARKER_OFFSET + PAGED_ENCRYPTION_MARKER.len();
    let header_end = marker_end + header_bytes.len();
    ensure_len(page, header_end)?;
    page[PAGED_ENCRYPTION_MARKER_OFFSET..marker_end].copy_from_slice(&PAGED_ENCRYPTION_MARKER);
    page[marker_end..header_end].copy_from_slice(header_bytes);
    Ok(())
}

const DB_MAGIC_OFFSET: usize = PAGED_PAGE_HEADER_SIZE;
const DB_VERSION_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 4;
const DB_PAGE_SIZE_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 8;
const DB_PAGE_COUNT_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 12;
const DB_FREELIST_HEAD_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 16;
const DB_SCHEMA_VERSION_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 20;
const DB_CHECKPOINT_LSN_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 24;
const DB_PHYSICAL_FORMAT_VERSION_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 32;
const DB_PHYSICAL_SEQUENCE_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 36;
const DB_MANIFEST_ROOT_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 44;
const DB_MANIFEST_OLDEST_ROOT_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 52;
const DB_FREE_SET_ROOT_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 60;
const DB_MANIFEST_PAGE_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 68;
const DB_MANIFEST_CHECKSUM_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 72;
const DB_COLLECTION_ROOTS_PAGE_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 80;
const DB_COLLECTION_ROOTS_CHECKSUM_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 84;
const DB_COLLECTION_ROOT_COUNT_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 92;
const DB_SNAPSHOT_COUNT_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 96;
const DB_INDEX_COUNT_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 100;
const DB_CATALOG_COLLECTION_COUNT_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 104;
const DB_CATALOG_TOTAL_ENTITIES_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 108;
const DB_EXPORT_COUNT_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 116;
const DB_GRAPH_PROJECTION_COUNT_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 120;
const DB_ANALYTICS_JOB_COUNT_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 124;
const DB_MANIFEST_EVENT_COUNT_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 128;
const DB_REGISTRY_PAGE_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 132;
const DB_REGISTRY_CHECKSUM_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 136;
const DB_RECOVERY_PAGE_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 144;
const DB_RECOVERY_CHECKSUM_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 148;
const DB_CATALOG_PAGE_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 156;
const DB_CATALOG_CHECKSUM_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 160;
const DB_METADATA_STATE_PAGE_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 168;
const DB_METADATA_STATE_CHECKSUM_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 172;
const DB_VECTOR_ARTIFACT_PAGE_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 180;
const DB_VECTOR_ARTIFACT_CHECKSUM_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 184;
const DB_CHECKPOINT_IN_PROGRESS_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 192;
const DB_CHECKPOINT_TARGET_LSN_OFFSET: usize = PAGED_PAGE_HEADER_SIZE + 193;
const DB_HEADER_MIN_LEN: usize = DB_CHECKPOINT_TARGET_LSN_OFFSET + 8;

/// Double-write-buffer file magic: `RDDW`.
pub const DWB_MAGIC: [u8; 4] = [0x52, 0x44, 0x44, 0x57];
pub const PAGED_DWB_HEADER_SIZE: usize = 12;
pub const PAGED_DWB_ENTRY_HEADER_SIZE: usize = 4;
pub const PAGED_DWB_ENTRY_SIZE: usize = PAGED_DWB_ENTRY_HEADER_SIZE + PAGED_PAGE_SIZE;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PagedDwbEntry {
    pub page_id: u32,
    pub page: [u8; PAGED_PAGE_SIZE],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PagedDwbFrameError {
    ShortHeader { got: usize },
    InvalidMagic,
    IncompleteFrame { expected: usize, got: usize },
    ChecksumMismatch { expected: u32, actual: u32 },
}

impl fmt::Display for PagedDwbFrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ShortHeader { got } => write!(f, "DWB frame too short: got {got} bytes"),
            Self::InvalidMagic => write!(f, "invalid DWB frame magic"),
            Self::IncompleteFrame { expected, got } => {
                write!(
                    f,
                    "incomplete DWB frame: expected {expected} bytes, got {got}"
                )
            }
            Self::ChecksumMismatch { expected, actual } => write!(
                f,
                "DWB frame checksum mismatch: expected {expected}, actual {actual}"
            ),
        }
    }
}

impl std::error::Error for PagedDwbFrameError {}

pub fn encode_paged_dwb_frame<'a>(
    pages: impl IntoIterator<Item = (u32, &'a [u8; PAGED_PAGE_SIZE])>,
) -> Vec<u8> {
    let pages: Vec<_> = pages.into_iter().collect();
    let total = PAGED_DWB_HEADER_SIZE + pages.len() * PAGED_DWB_ENTRY_SIZE;
    let mut out = Vec::with_capacity(total);

    out.extend_from_slice(&DWB_MAGIC);
    out.extend_from_slice(&(pages.len() as u32).to_le_bytes());
    out.extend_from_slice(&[0u8; 4]);

    for (page_id, page) in pages {
        out.extend_from_slice(&page_id.to_le_bytes());
        out.extend_from_slice(page);
    }

    let checksum = crc32(&out[PAGED_DWB_HEADER_SIZE..]);
    out[8..12].copy_from_slice(&checksum.to_le_bytes());
    out
}

pub fn decode_paged_dwb_frame(bytes: &[u8]) -> Result<Vec<PagedDwbEntry>, PagedDwbFrameError> {
    if bytes.len() < PAGED_DWB_HEADER_SIZE {
        return Err(PagedDwbFrameError::ShortHeader { got: bytes.len() });
    }
    if bytes[0..4] != DWB_MAGIC {
        return Err(PagedDwbFrameError::InvalidMagic);
    }

    let count = u32::from_le_bytes(bytes[4..8].try_into().expect("len checked")) as usize;
    let stored_checksum = u32::from_le_bytes(bytes[8..12].try_into().expect("len checked"));
    let expected_len = PAGED_DWB_HEADER_SIZE + count * PAGED_DWB_ENTRY_SIZE;
    if bytes.len() < expected_len {
        return Err(PagedDwbFrameError::IncompleteFrame {
            expected: expected_len,
            got: bytes.len(),
        });
    }

    let actual_checksum = crc32(&bytes[PAGED_DWB_HEADER_SIZE..expected_len]);
    if actual_checksum != stored_checksum {
        return Err(PagedDwbFrameError::ChecksumMismatch {
            expected: stored_checksum,
            actual: actual_checksum,
        });
    }

    let mut offset = PAGED_DWB_HEADER_SIZE;
    let mut entries = Vec::with_capacity(count);
    for _ in 0..count {
        let page_id =
            u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("len checked"));
        offset += PAGED_DWB_ENTRY_HEADER_SIZE;
        let mut page = [0u8; PAGED_PAGE_SIZE];
        page.copy_from_slice(&bytes[offset..offset + PAGED_PAGE_SIZE]);
        offset += PAGED_PAGE_SIZE;
        entries.push(PagedDwbEntry { page_id, page });
    }
    Ok(entries)
}

fn crc32(data: &[u8]) -> u32 {
    crc32fast::hash(data)
}

/// `b"RDCC"` — RedDB Columnar Chunk. Opens and closes every column block.
pub const COLUMN_BLOCK_MAGIC: [u8; 4] = *b"RDCC";

/// Column block on-disk format version.
pub const COLUMN_BLOCK_VERSION_V1: u16 = 1;

/// Reusable segment-level bloom frame v2 magic.
pub const BLOOM_SEGMENT_V2_MAGIC: u8 = 0xC0;

/// Legacy vector B-tree on-disk page format.
pub const VECTOR_BTREE_FORMAT_VERSION_V1: u16 = 1;

/// Current vector B-tree on-disk page format.
pub const VECTOR_BTREE_FORMAT_VERSION_V2: u16 = 2;

/// Vector B-tree format stamped into freshly-written page headers.
pub const VECTOR_BTREE_FORMAT_VERSION: u16 = VECTOR_BTREE_FORMAT_VERSION_V2;

/// Database file header information persisted in page 0 after the page header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseHeader {
    pub version: u32,
    pub page_size: u32,
    pub page_count: u32,
    pub freelist_head: u32,
    pub schema_version: u32,
    pub checkpoint_lsn: u64,
    pub checkpoint_in_progress: bool,
    pub checkpoint_target_lsn: u64,
    pub physical: PhysicalFileHeader,
}

impl Default for DatabaseHeader {
    fn default() -> Self {
        Self {
            version: PAGE_FILE_VERSION,
            page_size: PAGED_PAGE_SIZE as u32,
            page_count: 1,
            freelist_head: 0,
            schema_version: 0,
            checkpoint_lsn: 0,
            checkpoint_in_progress: false,
            checkpoint_target_lsn: 0,
            physical: PhysicalFileHeader::default(),
        }
    }
}

/// Minimal physical state mirrored into page 0 for paged databases.
///
/// The pager owns when this is read and written, but the field layout is a
/// durable file compatibility contract.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PhysicalFileHeader {
    pub format_version: u32,
    pub sequence: u64,
    pub manifest_oldest_root: u64,
    pub manifest_root: u64,
    pub free_set_root: u64,
    pub manifest_page: u32,
    pub manifest_checksum: u64,
    pub collection_roots_page: u32,
    pub collection_roots_checksum: u64,
    pub collection_root_count: u32,
    pub snapshot_count: u32,
    pub index_count: u32,
    pub catalog_collection_count: u32,
    pub catalog_total_entities: u64,
    pub export_count: u32,
    pub graph_projection_count: u32,
    pub analytics_job_count: u32,
    pub manifest_event_count: u32,
    pub registry_page: u32,
    pub registry_checksum: u64,
    pub recovery_page: u32,
    pub recovery_checksum: u64,
    pub catalog_page: u32,
    pub catalog_checksum: u64,
    pub metadata_state_page: u32,
    pub metadata_state_checksum: u64,
    pub vector_artifact_page: u32,
    pub vector_artifact_checksum: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatabaseHeaderError {
    ShortPage { need: usize, got: usize },
    InvalidMagic,
    UnsupportedPageSize(u32),
    UnsupportedDatabaseVersion { file_version: u32, supported: u32 },
}

impl fmt::Display for DatabaseHeaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ShortPage { need, got } => {
                write!(f, "database header page too short: need {need} bytes, got {got}")
            }
            Self::InvalidMagic => write!(f, "invalid database header magic"),
            Self::UnsupportedPageSize(size) => write!(f, "Unsupported page size: {size}"),
            Self::UnsupportedDatabaseVersion {
                file_version,
                supported,
            } => write!(
                f,
                "Unsupported database version: file version {file_version} is newer than supported {supported}"
            ),
        }
    }
}

impl std::error::Error for DatabaseHeaderError {}

pub fn database_header_magic_matches(page: &[u8]) -> bool {
    page.get(DB_MAGIC_OFFSET..DB_MAGIC_OFFSET + PAGE_FILE_MAGIC.len()) == Some(&PAGE_FILE_MAGIC)
}

pub fn init_database_header_page(
    page: &mut [u8],
    page_count: u32,
) -> Result<(), DatabaseHeaderError> {
    encode_database_header(
        page,
        &DatabaseHeader {
            page_count,
            ..DatabaseHeader::default()
        },
    )
}

pub fn database_header_page_count(page: &[u8]) -> Result<u32, DatabaseHeaderError> {
    read_u32(page, DB_PAGE_COUNT_OFFSET)
}

pub fn set_database_header_version(
    page: &mut [u8],
    version: u32,
) -> Result<(), DatabaseHeaderError> {
    write_u32(page, DB_VERSION_OFFSET, version)
}

pub fn set_database_header_page_count(
    page: &mut [u8],
    page_count: u32,
) -> Result<(), DatabaseHeaderError> {
    write_u32(page, DB_PAGE_COUNT_OFFSET, page_count)
}

pub fn database_header_freelist_head(page: &[u8]) -> Result<u32, DatabaseHeaderError> {
    read_u32(page, DB_FREELIST_HEAD_OFFSET)
}

pub fn set_database_header_freelist_head(
    page: &mut [u8],
    page_id: u32,
) -> Result<(), DatabaseHeaderError> {
    write_u32(page, DB_FREELIST_HEAD_OFFSET, page_id)
}

pub fn database_header_page_size(page: &[u8]) -> Result<u32, DatabaseHeaderError> {
    read_u32(page, DB_PAGE_SIZE_OFFSET)
}

pub fn decode_database_header(page: &[u8]) -> Result<DatabaseHeader, DatabaseHeaderError> {
    ensure_len(page, DB_HEADER_MIN_LEN)?;
    if !database_header_magic_matches(page) {
        return Err(DatabaseHeaderError::InvalidMagic);
    }

    let version = read_u32(page, DB_VERSION_OFFSET)?;
    let page_size = read_u32(page, DB_PAGE_SIZE_OFFSET)?;
    if page_size != PAGED_PAGE_SIZE as u32 {
        return Err(DatabaseHeaderError::UnsupportedPageSize(page_size));
    }
    if version > PAGE_FILE_VERSION {
        return Err(DatabaseHeaderError::UnsupportedDatabaseVersion {
            file_version: version,
            supported: PAGE_FILE_VERSION,
        });
    }

    Ok(DatabaseHeader {
        version,
        page_size,
        page_count: read_u32(page, DB_PAGE_COUNT_OFFSET)?,
        freelist_head: read_u32(page, DB_FREELIST_HEAD_OFFSET)?,
        schema_version: read_u32(page, DB_SCHEMA_VERSION_OFFSET)?,
        checkpoint_lsn: read_u64(page, DB_CHECKPOINT_LSN_OFFSET)?,
        checkpoint_in_progress: page[DB_CHECKPOINT_IN_PROGRESS_OFFSET] != 0,
        checkpoint_target_lsn: read_u64(page, DB_CHECKPOINT_TARGET_LSN_OFFSET)?,
        physical: PhysicalFileHeader {
            format_version: read_u32(page, DB_PHYSICAL_FORMAT_VERSION_OFFSET)?,
            sequence: read_u64(page, DB_PHYSICAL_SEQUENCE_OFFSET)?,
            manifest_oldest_root: read_u64(page, DB_MANIFEST_OLDEST_ROOT_OFFSET)?,
            manifest_root: read_u64(page, DB_MANIFEST_ROOT_OFFSET)?,
            free_set_root: read_u64(page, DB_FREE_SET_ROOT_OFFSET)?,
            manifest_page: read_u32(page, DB_MANIFEST_PAGE_OFFSET)?,
            manifest_checksum: read_u64(page, DB_MANIFEST_CHECKSUM_OFFSET)?,
            collection_roots_page: read_u32(page, DB_COLLECTION_ROOTS_PAGE_OFFSET)?,
            collection_roots_checksum: read_u64(page, DB_COLLECTION_ROOTS_CHECKSUM_OFFSET)?,
            collection_root_count: read_u32(page, DB_COLLECTION_ROOT_COUNT_OFFSET)?,
            snapshot_count: read_u32(page, DB_SNAPSHOT_COUNT_OFFSET)?,
            index_count: read_u32(page, DB_INDEX_COUNT_OFFSET)?,
            catalog_collection_count: read_u32(page, DB_CATALOG_COLLECTION_COUNT_OFFSET)?,
            catalog_total_entities: read_u64(page, DB_CATALOG_TOTAL_ENTITIES_OFFSET)?,
            export_count: read_u32(page, DB_EXPORT_COUNT_OFFSET)?,
            graph_projection_count: read_u32(page, DB_GRAPH_PROJECTION_COUNT_OFFSET)?,
            analytics_job_count: read_u32(page, DB_ANALYTICS_JOB_COUNT_OFFSET)?,
            manifest_event_count: read_u32(page, DB_MANIFEST_EVENT_COUNT_OFFSET)?,
            registry_page: read_u32(page, DB_REGISTRY_PAGE_OFFSET)?,
            registry_checksum: read_u64(page, DB_REGISTRY_CHECKSUM_OFFSET)?,
            recovery_page: read_u32(page, DB_RECOVERY_PAGE_OFFSET)?,
            recovery_checksum: read_u64(page, DB_RECOVERY_CHECKSUM_OFFSET)?,
            catalog_page: read_u32(page, DB_CATALOG_PAGE_OFFSET)?,
            catalog_checksum: read_u64(page, DB_CATALOG_CHECKSUM_OFFSET)?,
            metadata_state_page: read_u32(page, DB_METADATA_STATE_PAGE_OFFSET)?,
            metadata_state_checksum: read_u64(page, DB_METADATA_STATE_CHECKSUM_OFFSET)?,
            vector_artifact_page: read_u32(page, DB_VECTOR_ARTIFACT_PAGE_OFFSET)?,
            vector_artifact_checksum: read_u64(page, DB_VECTOR_ARTIFACT_CHECKSUM_OFFSET)?,
        },
    })
}

pub fn encode_database_header(
    page: &mut [u8],
    header: &DatabaseHeader,
) -> Result<(), DatabaseHeaderError> {
    ensure_len(page, DB_HEADER_MIN_LEN)?;
    page[DB_MAGIC_OFFSET..DB_MAGIC_OFFSET + PAGE_FILE_MAGIC.len()]
        .copy_from_slice(&PAGE_FILE_MAGIC);
    write_u32(page, DB_VERSION_OFFSET, header.version)?;
    write_u32(page, DB_PAGE_SIZE_OFFSET, header.page_size)?;
    write_u32(page, DB_PAGE_COUNT_OFFSET, header.page_count)?;
    write_u32(page, DB_FREELIST_HEAD_OFFSET, header.freelist_head)?;
    write_u32(page, DB_SCHEMA_VERSION_OFFSET, header.schema_version)?;
    write_u64(page, DB_CHECKPOINT_LSN_OFFSET, header.checkpoint_lsn)?;
    write_u32(
        page,
        DB_PHYSICAL_FORMAT_VERSION_OFFSET,
        header.physical.format_version,
    )?;
    write_u64(page, DB_PHYSICAL_SEQUENCE_OFFSET, header.physical.sequence)?;
    write_u64(page, DB_MANIFEST_ROOT_OFFSET, header.physical.manifest_root)?;
    write_u64(
        page,
        DB_MANIFEST_OLDEST_ROOT_OFFSET,
        header.physical.manifest_oldest_root,
    )?;
    write_u64(page, DB_FREE_SET_ROOT_OFFSET, header.physical.free_set_root)?;
    write_u32(page, DB_MANIFEST_PAGE_OFFSET, header.physical.manifest_page)?;
    write_u64(
        page,
        DB_MANIFEST_CHECKSUM_OFFSET,
        header.physical.manifest_checksum,
    )?;
    write_u32(
        page,
        DB_COLLECTION_ROOTS_PAGE_OFFSET,
        header.physical.collection_roots_page,
    )?;
    write_u64(
        page,
        DB_COLLECTION_ROOTS_CHECKSUM_OFFSET,
        header.physical.collection_roots_checksum,
    )?;
    write_u32(
        page,
        DB_COLLECTION_ROOT_COUNT_OFFSET,
        header.physical.collection_root_count,
    )?;
    write_u32(
        page,
        DB_SNAPSHOT_COUNT_OFFSET,
        header.physical.snapshot_count,
    )?;
    write_u32(page, DB_INDEX_COUNT_OFFSET, header.physical.index_count)?;
    write_u32(
        page,
        DB_CATALOG_COLLECTION_COUNT_OFFSET,
        header.physical.catalog_collection_count,
    )?;
    write_u64(
        page,
        DB_CATALOG_TOTAL_ENTITIES_OFFSET,
        header.physical.catalog_total_entities,
    )?;
    write_u32(page, DB_EXPORT_COUNT_OFFSET, header.physical.export_count)?;
    write_u32(
        page,
        DB_GRAPH_PROJECTION_COUNT_OFFSET,
        header.physical.graph_projection_count,
    )?;
    write_u32(
        page,
        DB_ANALYTICS_JOB_COUNT_OFFSET,
        header.physical.analytics_job_count,
    )?;
    write_u32(
        page,
        DB_MANIFEST_EVENT_COUNT_OFFSET,
        header.physical.manifest_event_count,
    )?;
    write_u32(page, DB_REGISTRY_PAGE_OFFSET, header.physical.registry_page)?;
    write_u64(
        page,
        DB_REGISTRY_CHECKSUM_OFFSET,
        header.physical.registry_checksum,
    )?;
    write_u32(page, DB_RECOVERY_PAGE_OFFSET, header.physical.recovery_page)?;
    write_u64(
        page,
        DB_RECOVERY_CHECKSUM_OFFSET,
        header.physical.recovery_checksum,
    )?;
    write_u32(page, DB_CATALOG_PAGE_OFFSET, header.physical.catalog_page)?;
    write_u64(
        page,
        DB_CATALOG_CHECKSUM_OFFSET,
        header.physical.catalog_checksum,
    )?;
    write_u32(
        page,
        DB_METADATA_STATE_PAGE_OFFSET,
        header.physical.metadata_state_page,
    )?;
    write_u64(
        page,
        DB_METADATA_STATE_CHECKSUM_OFFSET,
        header.physical.metadata_state_checksum,
    )?;
    write_u32(
        page,
        DB_VECTOR_ARTIFACT_PAGE_OFFSET,
        header.physical.vector_artifact_page,
    )?;
    write_u64(
        page,
        DB_VECTOR_ARTIFACT_CHECKSUM_OFFSET,
        header.physical.vector_artifact_checksum,
    )?;
    page[DB_CHECKPOINT_IN_PROGRESS_OFFSET] = if header.checkpoint_in_progress { 1 } else { 0 };
    write_u64(
        page,
        DB_CHECKPOINT_TARGET_LSN_OFFSET,
        header.checkpoint_target_lsn,
    )
}

fn ensure_len(bytes: &[u8], need: usize) -> Result<(), DatabaseHeaderError> {
    if bytes.len() < need {
        return Err(DatabaseHeaderError::ShortPage {
            need,
            got: bytes.len(),
        });
    }
    Ok(())
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, DatabaseHeaderError> {
    ensure_len(bytes, offset + 4)?;
    Ok(u32::from_le_bytes(
        bytes[offset..offset + 4].try_into().expect("len checked"),
    ))
}

fn read_u16(bytes: &[u8], offset: usize) -> Result<u16, DatabaseHeaderError> {
    ensure_len(bytes, offset + 2)?;
    Ok(u16::from_le_bytes(
        bytes[offset..offset + 2].try_into().expect("len checked"),
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, DatabaseHeaderError> {
    ensure_len(bytes, offset + 8)?;
    Ok(u64::from_le_bytes(
        bytes[offset..offset + 8].try_into().expect("len checked"),
    ))
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) -> Result<(), DatabaseHeaderError> {
    ensure_len(bytes, offset + 4)?;
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn write_u16(bytes: &mut [u8], offset: usize, value: u16) -> Result<(), DatabaseHeaderError> {
    ensure_len(bytes, offset + 2)?;
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    Ok(())
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) -> Result<(), DatabaseHeaderError> {
    ensure_len(bytes, offset + 8)?;
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paged_page_header_round_trips_raw_layout() {
        let header = PagedPageHeader {
            page_type: 13,
            flags: 0b1010_0001,
            cell_count: 42,
            free_start: 44,
            free_end: 16_000,
            page_id: 99,
            parent_id: 88,
            right_child: 77,
            lsn: 66,
            checksum: 55,
        };

        let encoded = encode_paged_page_header(&header);

        assert_eq!(encoded.len(), PAGED_PAGE_HEADER_SIZE);
        assert_eq!(decode_paged_page_header(&encoded), header);
    }

    #[test]
    fn paged_page_header_accessors_update_expected_offsets() {
        let mut page = [0u8; PAGED_PAGE_SIZE];
        page[0] = 13;

        set_paged_page_cell_count(&mut page, 2);
        set_paged_page_free_start(&mut page, 34);
        set_paged_page_free_end(&mut page, 4096);
        set_paged_page_parent_id(&mut page, 7);
        set_paged_page_right_child(&mut page, 8);
        set_paged_page_lsn(&mut page, 9);
        set_paged_page_checksum(&mut page, 10);

        assert_eq!(paged_page_type(&page), 13);
        assert_eq!(paged_page_cell_count(&page), 2);
        assert_eq!(paged_page_free_start(&page), 34);
        assert_eq!(paged_page_free_end(&page), 4096);
        assert_eq!(paged_page_parent_id(&page), 7);
        assert_eq!(paged_page_right_child(&page), 8);
        assert_eq!(paged_page_lsn(&page), 9);
        assert_eq!(paged_page_checksum(&page), 10);
        clear_paged_page_checksum(&mut page);
        assert_eq!(paged_page_checksum(&page), 0);
    }

    #[test]
    fn paged_cell_helpers_round_trip_pointer_and_payload() {
        let mut page = [0u8; PAGED_PAGE_SIZE];
        let pointer = (PAGED_PAGE_SIZE - 14) as u16;

        assert!(write_paged_cell(&mut page, pointer, b"key", b"value"));
        assert!(set_paged_cell_pointer(&mut page, 0, pointer));

        assert_eq!(paged_cell_pointer(&page, 0), Some(pointer));
        let cell = paged_cell_bytes(&page, pointer).expect("cell bytes");
        let (key, value) = paged_cell_key_value(cell).expect("key value");

        assert_eq!(key, b"key");
        assert_eq!(value, b"value");
        assert_eq!(paged_cell_total_len(&page, pointer), Some(14));
    }

    #[test]
    fn database_header_field_helpers_preserve_page_zero_offsets() {
        let mut page = vec![0u8; PAGED_PAGE_SIZE];

        init_database_header_page(&mut page, 7).expect("init database header page");
        set_database_header_version(&mut page, PAGE_FILE_VERSION + 1).expect("set version");
        set_database_header_page_count(&mut page, 9).expect("set page count");
        set_database_header_freelist_head(&mut page, 4).expect("set freelist head");

        assert!(database_header_magic_matches(&page));
        assert!(matches!(
            decode_database_header(&page),
            Err(DatabaseHeaderError::UnsupportedDatabaseVersion { .. })
        ));
        set_database_header_version(&mut page, PAGE_FILE_VERSION).expect("restore version");
        assert_eq!(
            database_header_page_size(&page).unwrap(),
            PAGED_PAGE_SIZE as u32
        );
        assert_eq!(database_header_page_count(&page).unwrap(), 9);
        assert_eq!(database_header_freelist_head(&page).unwrap(), 4);
    }

    #[test]
    fn paged_dwb_frame_round_trips_entries_and_validates_checksum() {
        let mut page = [0u8; PAGED_PAGE_SIZE];
        page[0] = 7;
        let frame = encode_paged_dwb_frame([(42, &page)]);

        let entries = decode_paged_dwb_frame(&frame).expect("decode DWB frame");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].page_id, 42);
        assert_eq!(entries[0].page, page);

        let mut corrupted = frame;
        let last = corrupted.len() - 1;
        corrupted[last] ^= 0xFF;
        assert!(matches!(
            decode_paged_dwb_frame(&corrupted),
            Err(PagedDwbFrameError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn paged_encryption_header_round_trips_marker_and_raw_bytes() {
        let header = PagedEncryptionHeader {
            salt: [7u8; PAGED_ENCRYPTION_SALT_SIZE],
            key_check: vec![9u8; PAGED_ENCRYPTION_KEY_CHECK_BLOB_SIZE],
        };
        let mut page = vec![0u8; PAGED_PAGE_SIZE];
        let bytes = encode_paged_encryption_header(&header);

        write_paged_encryption_marker_and_header(&mut page, &bytes)
            .expect("write encryption marker");

        assert!(paged_encryption_marker_present(&page));
        let raw = paged_encryption_header_bytes(&page).expect("header bytes");
        assert_eq!(decode_paged_encryption_header(raw).unwrap(), header);
    }

    #[test]
    fn database_header_round_trips_page_zero_contract() {
        let header = DatabaseHeader {
            version: PAGE_FILE_VERSION,
            page_size: PAGED_PAGE_SIZE as u32,
            page_count: 42,
            freelist_head: 7,
            schema_version: 3,
            checkpoint_lsn: 99,
            checkpoint_in_progress: true,
            checkpoint_target_lsn: 123,
            physical: PhysicalFileHeader {
                format_version: 11,
                sequence: 12,
                manifest_oldest_root: 13,
                manifest_root: 14,
                free_set_root: 15,
                manifest_page: 16,
                manifest_checksum: 17,
                collection_roots_page: 18,
                collection_roots_checksum: 19,
                collection_root_count: 20,
                snapshot_count: 21,
                index_count: 22,
                catalog_collection_count: 23,
                catalog_total_entities: 24,
                export_count: 25,
                graph_projection_count: 26,
                analytics_job_count: 27,
                manifest_event_count: 28,
                registry_page: 29,
                registry_checksum: 30,
                recovery_page: 31,
                recovery_checksum: 32,
                catalog_page: 33,
                catalog_checksum: 34,
                metadata_state_page: 35,
                metadata_state_checksum: 36,
                vector_artifact_page: 37,
                vector_artifact_checksum: 38,
            },
        };
        let mut page = vec![0u8; PAGED_PAGE_SIZE];

        encode_database_header(&mut page, &header).expect("encode header");

        assert!(database_header_magic_matches(&page));
        let decoded = decode_database_header(&page).expect("decode header");
        assert_eq!(decoded, header);
    }

    #[test]
    fn database_header_rejects_newer_versions() {
        let mut page = vec![0u8; PAGED_PAGE_SIZE];
        let header = DatabaseHeader {
            version: PAGE_FILE_VERSION + 1,
            ..DatabaseHeader::default()
        };
        encode_database_header(&mut page, &header).expect("encode header");

        assert!(matches!(
            decode_database_header(&page),
            Err(DatabaseHeaderError::UnsupportedDatabaseVersion { .. })
        ));
    }
}
