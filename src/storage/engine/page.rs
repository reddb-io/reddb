//! Page structure for RedDB storage engine
//!
//! A page is the fundamental unit of storage (4KB aligned for efficient I/O).
//! Each page has a fixed header followed by type-specific content.
//!
//! # Page Layout (4096 bytes)
//!
//! ```text
//! ┌───────────────────────────────────────────────────────────┐
//! │ PageHeader (32 bytes)                                     │
//! ├───────────────────────────────────────────────────────────┤
//! │ Cell Pointer Array (grows downward from header)           │
//! │ [u16, u16, u16, ...]                                      │
//! ├───────────────────────────────────────────────────────────┤
//! │ Free Space (unallocated)                                  │
//! │                                                           │
//! ├───────────────────────────────────────────────────────────┤
//! │ Cell Content Area (grows upward from bottom)              │
//! │ [Cell N] [Cell N-1] ... [Cell 1]                          │
//! └───────────────────────────────────────────────────────────┘
//! ```
//!
//! # References
//!
//! - Turso `core/storage/pager.rs:136-152` - PageInner struct
//! - Turso `core/storage/btree.rs:54-102` - B-tree page header offsets
//! - Turso `core/storage/sqlite3_ondisk.rs` - PageType definitions

use super::crc32::crc32;

/// Page size in bytes (4KB, standard for most file systems)
pub const PAGE_SIZE: usize = 4096;

/// Header size in bytes
pub const HEADER_SIZE: usize = 32;

/// Content area size (page minus header)
pub const CONTENT_SIZE: usize = PAGE_SIZE - HEADER_SIZE;

/// Maximum number of cells per page (limited by cell pointer array)
pub const MAX_CELLS: usize = (CONTENT_SIZE - 4) / 6; // ~676 cells

/// Magic bytes for database file identification
pub const MAGIC_BYTES: [u8; 4] = [0x52, 0x44, 0x44, 0x42]; // "RDDB"

/// Database file version (1.0.0)
pub const DB_VERSION: u32 = 0x00010000;

/// Page type enumeration
///
/// Based on Turso `core/storage/sqlite3_ondisk.rs` PageType definitions.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageType {
    /// Free page (available for allocation)
    Free = 0,
    /// B-tree leaf page (contains key-value pairs)
    BTreeLeaf = 1,
    /// B-tree interior page (contains keys and child pointers)
    BTreeInterior = 2,
    /// Overflow page (continuation of large values)
    Overflow = 3,
    /// Vector data page (dense vector storage)
    Vector = 4,
    /// Freelist trunk page (tracks free pages)
    FreelistTrunk = 5,
    /// Database header page (page 0)
    Header = 6,
    /// Graph node page (packed node records)
    GraphNode = 7,
    /// Graph edge page (packed edge records)
    GraphEdge = 8,
    /// Graph adjacency list page (outgoing edges per node)
    GraphAdjacency = 9,
    /// Graph metadata page (statistics, index roots)
    GraphMeta = 10,
    /// Native physical metadata page (engine-published auxiliary state)
    NativeMeta = 11,
}

impl PageType {
    /// Convert from u8
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(Self::Free),
            1 => Some(Self::BTreeLeaf),
            2 => Some(Self::BTreeInterior),
            3 => Some(Self::Overflow),
            4 => Some(Self::Vector),
            5 => Some(Self::FreelistTrunk),
            6 => Some(Self::Header),
            7 => Some(Self::GraphNode),
            8 => Some(Self::GraphEdge),
            9 => Some(Self::GraphAdjacency),
            10 => Some(Self::GraphMeta),
            11 => Some(Self::NativeMeta),
            _ => None,
        }
    }
}

/// Page flags (bitfield)
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageFlag {
    /// Page has been modified
    Dirty = 0x01,
    /// Page is locked for writing
    Locked = 0x02,
    /// Page data is loaded in memory
    Loaded = 0x04,
    /// Page is pinned in cache (cannot be evicted)
    Pinned = 0x08,
    /// Page content is encrypted
    Encrypted = 0x10,
}

/// Page header structure (32 bytes)
///
/// Layout:
/// ```text
/// Offset  Size  Field
/// ------  ----  -----
///   0      1    page_type
///   1      1    flags
///   2      2    cell_count
///   4      2    free_start (offset to first free byte in cell pointer array)
///   6      2    free_end (offset to first free byte before cell content)
///   8      4    page_id
///  12      4    parent_id (0 for root)
///  16      4    right_child (for interior nodes, 0 otherwise)
///  20      8    lsn (Log Sequence Number for WAL)
///  28      4    checksum (CRC32 of content)
/// ```
#[derive(Debug, Clone, Copy)]
pub struct PageHeader {
    /// Type of this page
    pub page_type: PageType,
    /// Page flags (dirty, locked, etc.)
    pub flags: u8,
    /// Number of cells on this page
    pub cell_count: u16,
    /// Offset to start of free space (cell pointer array end)
    pub free_start: u16,
    /// Offset to end of free space (cell content start)
    pub free_end: u16,
    /// Unique page identifier
    pub page_id: u32,
    /// Parent page ID (0 for root or orphan)
    pub parent_id: u32,
    /// Right-most child page (interior nodes only)
    pub right_child: u32,
    /// Log Sequence Number (for WAL ordering)
    pub lsn: u64,
    /// CRC32 checksum of page content
    pub checksum: u32,
}

impl PageHeader {
    /// Create a new header for an empty page
    pub fn new(page_type: PageType, page_id: u32) -> Self {
        Self {
            page_type,
            flags: 0,
            cell_count: 0,
            free_start: HEADER_SIZE as u16,
            free_end: PAGE_SIZE as u16,
            page_id,
            parent_id: 0,
            right_child: 0,
            lsn: 0,
            checksum: 0,
        }
    }

    /// Serialize header to bytes
    pub fn to_bytes(&self) -> [u8; HEADER_SIZE] {
        let mut buf = [0u8; HEADER_SIZE];

        buf[0] = self.page_type as u8;
        buf[1] = self.flags;
        buf[2..4].copy_from_slice(&self.cell_count.to_le_bytes());
        buf[4..6].copy_from_slice(&self.free_start.to_le_bytes());
        buf[6..8].copy_from_slice(&self.free_end.to_le_bytes());
        buf[8..12].copy_from_slice(&self.page_id.to_le_bytes());
        buf[12..16].copy_from_slice(&self.parent_id.to_le_bytes());
        buf[16..20].copy_from_slice(&self.right_child.to_le_bytes());
        buf[20..28].copy_from_slice(&self.lsn.to_le_bytes());
        buf[28..32].copy_from_slice(&self.checksum.to_le_bytes());

        buf
    }

    /// Deserialize header from bytes
    pub fn from_bytes(buf: &[u8; HEADER_SIZE]) -> Result<Self, PageError> {
        let page_type = PageType::from_u8(buf[0]).ok_or(PageError::InvalidPageType(buf[0]))?;

        Ok(Self {
            page_type,
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
        })
    }

    /// Check if page has specific flag
    #[inline]
    pub fn has_flag(&self, flag: PageFlag) -> bool {
        self.flags & (flag as u8) != 0
    }

    /// Set a flag
    #[inline]
    pub fn set_flag(&mut self, flag: PageFlag) {
        self.flags |= flag as u8;
    }

    /// Clear a flag
    #[inline]
    pub fn clear_flag(&mut self, flag: PageFlag) {
        self.flags &= !(flag as u8);
    }

    /// Calculate free space available for new cells
    #[inline]
    pub fn free_space(&self) -> usize {
        if self.free_end <= self.free_start {
            0
        } else {
            (self.free_end - self.free_start) as usize
        }
    }
}

/// Page error types
#[derive(Debug, Clone)]
pub enum PageError {
    /// Invalid page type byte
    InvalidPageType(u8),
    /// Page checksum mismatch (corruption detected)
    ChecksumMismatch { expected: u32, actual: u32 },
    /// Invalid page size
    InvalidSize(usize),
    /// Page is full
    PageFull,
    /// Cell index out of bounds
    CellOutOfBounds(usize),
    /// Invalid cell pointer
    InvalidCellPointer(u16),
    /// Overflow required for large value
    OverflowRequired,
}

impl std::fmt::Display for PageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidPageType(t) => write!(f, "Invalid page type: {}", t),
            Self::ChecksumMismatch { expected, actual } => {
                write!(
                    f,
                    "Checksum mismatch: expected 0x{:08X}, got 0x{:08X}",
                    expected, actual
                )
            }
            Self::InvalidSize(s) => write!(f, "Invalid page size: {} (expected {})", s, PAGE_SIZE),
            Self::PageFull => write!(f, "Page is full"),
            Self::CellOutOfBounds(i) => write!(f, "Cell index {} out of bounds", i),
            Self::InvalidCellPointer(p) => write!(f, "Invalid cell pointer: {}", p),
            Self::OverflowRequired => write!(f, "Value too large, overflow page required"),
        }
    }
}

impl std::error::Error for PageError {}

/// A 4KB page with header and content
///
/// This is the core data structure for the storage engine.
#[derive(Clone)]
pub struct Page {
    /// Raw page data
    data: [u8; PAGE_SIZE],
}

impl Page {
    /// Create a new empty page
    pub fn new(page_type: PageType, page_id: u32) -> Self {
        let mut page = Self {
            data: [0u8; PAGE_SIZE],
        };

        let header = PageHeader::new(page_type, page_id);
        page.set_header(&header);
        page
    }

    /// Create a page from raw bytes
    pub fn from_bytes(data: [u8; PAGE_SIZE]) -> Self {
        Self { data }
    }

    /// Create a page from a byte slice (must be exactly PAGE_SIZE)
    pub fn from_slice(slice: &[u8]) -> Result<Self, PageError> {
        if slice.len() != PAGE_SIZE {
            return Err(PageError::InvalidSize(slice.len()));
        }
        let mut data = [0u8; PAGE_SIZE];
        data.copy_from_slice(slice);
        Ok(Self { data })
    }

    /// Get raw page data
    #[inline]
    pub fn as_bytes(&self) -> &[u8; PAGE_SIZE] {
        &self.data
    }

    /// Get mutable raw page data
    #[inline]
    pub fn as_bytes_mut(&mut self) -> &mut [u8; PAGE_SIZE] {
        &mut self.data
    }

    /// Get page header
    pub fn header(&self) -> Result<PageHeader, PageError> {
        let header_bytes: [u8; HEADER_SIZE] = self.data[..HEADER_SIZE]
            .try_into()
            .expect("header size mismatch");
        PageHeader::from_bytes(&header_bytes)
    }

    /// Set page header
    pub fn set_header(&mut self, header: &PageHeader) {
        let bytes = header.to_bytes();
        self.data[..HEADER_SIZE].copy_from_slice(&bytes);
    }

    /// Get page type
    pub fn page_type(&self) -> Result<PageType, PageError> {
        PageType::from_u8(self.data[0]).ok_or(PageError::InvalidPageType(self.data[0]))
    }

    /// Get page ID
    pub fn page_id(&self) -> u32 {
        u32::from_le_bytes([self.data[8], self.data[9], self.data[10], self.data[11]])
    }

    /// Get cell count
    pub fn cell_count(&self) -> u16 {
        u16::from_le_bytes([self.data[2], self.data[3]])
    }

    /// Set cell count
    pub fn set_cell_count(&mut self, count: u16) {
        self.data[2..4].copy_from_slice(&count.to_le_bytes());
    }

    /// Get parent page ID
    pub fn parent_id(&self) -> u32 {
        u32::from_le_bytes([self.data[12], self.data[13], self.data[14], self.data[15]])
    }

    /// Set parent page ID
    pub fn set_parent_id(&mut self, parent_id: u32) {
        self.data[12..16].copy_from_slice(&parent_id.to_le_bytes());
    }

    /// Get right child page ID (for interior nodes)
    pub fn right_child(&self) -> u32 {
        u32::from_le_bytes([self.data[16], self.data[17], self.data[18], self.data[19]])
    }

    /// Set right child page ID (for interior nodes)
    pub fn set_right_child(&mut self, child_id: u32) {
        self.data[16..20].copy_from_slice(&child_id.to_le_bytes());
    }

    /// Get free_start offset
    pub fn free_start(&self) -> u16 {
        u16::from_le_bytes([self.data[4], self.data[5]])
    }

    /// Set free_start offset
    pub fn set_free_start(&mut self, offset: u16) {
        self.data[4..6].copy_from_slice(&offset.to_le_bytes());
    }

    /// Get free_end offset
    pub fn free_end(&self) -> u16 {
        u16::from_le_bytes([self.data[6], self.data[7]])
    }

    /// Set free_end offset
    pub fn set_free_end(&mut self, offset: u16) {
        self.data[6..8].copy_from_slice(&offset.to_le_bytes());
    }

    /// Get content area (everything after header)
    #[inline]
    pub fn content(&self) -> &[u8] {
        &self.data[HEADER_SIZE..]
    }

    /// Get mutable content area
    #[inline]
    pub fn content_mut(&mut self) -> &mut [u8] {
        &mut self.data[HEADER_SIZE..]
    }

    /// Calculate and update checksum
    pub fn update_checksum(&mut self) {
        // Zero out checksum field before calculating
        self.data[28..32].copy_from_slice(&[0u8; 4]);
        // Calculate CRC32 of entire page
        let checksum = crc32(&self.data);
        // Store checksum
        self.data[28..32].copy_from_slice(&checksum.to_le_bytes());
    }

    /// Verify page checksum
    pub fn verify_checksum(&self) -> Result<(), PageError> {
        let stored =
            u32::from_le_bytes([self.data[28], self.data[29], self.data[30], self.data[31]]);

        // Calculate checksum with stored value zeroed
        let mut temp = self.data;
        temp[28..32].copy_from_slice(&[0u8; 4]);
        let calculated = crc32(&temp);

        if stored != calculated {
            Err(PageError::ChecksumMismatch {
                expected: stored,
                actual: calculated,
            })
        } else {
            Ok(())
        }
    }

    /// Get cell pointer at index
    ///
    /// Cell pointers are stored as u16 offsets starting at HEADER_SIZE.
    pub fn get_cell_pointer(&self, index: usize) -> Result<u16, PageError> {
        let count = self.cell_count() as usize;
        if index >= count {
            return Err(PageError::CellOutOfBounds(index));
        }

        let offset = HEADER_SIZE + index * 2;
        Ok(u16::from_le_bytes([
            self.data[offset],
            self.data[offset + 1],
        ]))
    }

    /// Set cell pointer at index
    pub fn set_cell_pointer(&mut self, index: usize, pointer: u16) -> Result<(), PageError> {
        if pointer < HEADER_SIZE as u16 || pointer >= PAGE_SIZE as u16 {
            return Err(PageError::InvalidCellPointer(pointer));
        }

        let offset = HEADER_SIZE + index * 2;
        self.data[offset..offset + 2].copy_from_slice(&pointer.to_le_bytes());
        Ok(())
    }

    /// Get cell data by index
    pub fn get_cell(&self, index: usize) -> Result<&[u8], PageError> {
        let pointer = self.get_cell_pointer(index)? as usize;

        // Read cell header to determine size
        // Cell format: [key_len: u16][value_len: u32][key][value]
        if pointer + 6 > PAGE_SIZE {
            return Err(PageError::InvalidCellPointer(pointer as u16));
        }

        let key_len = u16::from_le_bytes([self.data[pointer], self.data[pointer + 1]]) as usize;
        let value_len = u32::from_le_bytes([
            self.data[pointer + 2],
            self.data[pointer + 3],
            self.data[pointer + 4],
            self.data[pointer + 5],
        ]) as usize;

        let total_len = 6 + key_len + value_len;
        if pointer + total_len > PAGE_SIZE {
            return Err(PageError::InvalidCellPointer(pointer as u16));
        }

        Ok(&self.data[pointer..pointer + total_len])
    }

    /// Insert a new cell (key-value pair) into the page
    ///
    /// Returns the cell index on success.
    pub fn insert_cell(&mut self, key: &[u8], value: &[u8]) -> Result<usize, PageError> {
        let key_len = key.len();
        let value_len = value.len();

        // Check size limits
        if key_len > u16::MAX as usize {
            return Err(PageError::OverflowRequired);
        }

        // Cell format: [key_len: u16][value_len: u32][key][value]
        let cell_size = 6 + key_len + value_len;

        // Check if we need overflow
        if cell_size > CONTENT_SIZE - 2 {
            return Err(PageError::OverflowRequired);
        }

        // Read current header
        let mut header = self.header()?;

        // Check available space (need room for cell pointer + cell data)
        let space_needed = 2 + cell_size;
        if header.free_space() < space_needed {
            return Err(PageError::PageFull);
        }

        // Allocate cell from end of page (growing upward)
        let cell_offset = header.free_end as usize - cell_size;

        // Write cell data
        self.data[cell_offset..cell_offset + 2].copy_from_slice(&(key_len as u16).to_le_bytes());
        self.data[cell_offset + 2..cell_offset + 6]
            .copy_from_slice(&(value_len as u32).to_le_bytes());
        self.data[cell_offset + 6..cell_offset + 6 + key_len].copy_from_slice(key);
        self.data[cell_offset + 6 + key_len..cell_offset + 6 + key_len + value_len]
            .copy_from_slice(value);

        // Write cell pointer
        let cell_index = header.cell_count as usize;
        let pointer_offset = HEADER_SIZE + cell_index * 2;
        self.data[pointer_offset..pointer_offset + 2]
            .copy_from_slice(&(cell_offset as u16).to_le_bytes());

        // Update header
        header.cell_count += 1;
        header.free_start += 2;
        header.free_end = cell_offset as u16;
        header.set_flag(PageFlag::Dirty);
        self.set_header(&header);

        Ok(cell_index)
    }

    /// Read key and value from cell at index
    pub fn read_cell(&self, index: usize) -> Result<(Vec<u8>, Vec<u8>), PageError> {
        let cell = self.get_cell(index)?;

        let key_len = u16::from_le_bytes([cell[0], cell[1]]) as usize;
        let value_len = u32::from_le_bytes([cell[2], cell[3], cell[4], cell[5]]) as usize;

        let key = cell[6..6 + key_len].to_vec();
        let value = cell[6 + key_len..6 + key_len + value_len].to_vec();

        Ok((key, value))
    }

    /// Binary search for key in sorted cell array
    ///
    /// Returns Ok(index) if key is found, Err(insert_pos) if not.
    pub fn search_key(&self, key: &[u8]) -> Result<usize, usize> {
        let count = self.cell_count() as usize;
        if count == 0 {
            return Err(0);
        }

        let mut low = 0;
        let mut high = count;

        while low < high {
            let mid = (low + high) / 2;
            let (cell_key, _) = self.read_cell(mid).map_err(|_| mid)?;

            match cell_key.as_slice().cmp(key) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Greater => high = mid,
                std::cmp::Ordering::Equal => return Ok(mid),
            }
        }

        Err(low)
    }

    /// Create a database header page (page 0)
    pub fn new_header_page(page_count: u32) -> Self {
        let mut page = Self::new(PageType::Header, 0);

        // Write magic bytes at start of content
        page.data[HEADER_SIZE..HEADER_SIZE + 4].copy_from_slice(&MAGIC_BYTES);

        // Write version
        page.data[HEADER_SIZE + 4..HEADER_SIZE + 8].copy_from_slice(&DB_VERSION.to_le_bytes());

        // Write page size
        page.data[HEADER_SIZE + 8..HEADER_SIZE + 12]
            .copy_from_slice(&(PAGE_SIZE as u32).to_le_bytes());

        // Write page count
        page.data[HEADER_SIZE + 12..HEADER_SIZE + 16].copy_from_slice(&page_count.to_le_bytes());

        // Write freelist head (0 = no free pages)
        page.data[HEADER_SIZE + 16..HEADER_SIZE + 20].copy_from_slice(&0u32.to_le_bytes());

        page.update_checksum();
        page
    }

    /// Read page count from header page
    pub fn read_page_count(&self) -> u32 {
        u32::from_le_bytes([
            self.data[HEADER_SIZE + 12],
            self.data[HEADER_SIZE + 13],
            self.data[HEADER_SIZE + 14],
            self.data[HEADER_SIZE + 15],
        ])
    }

    /// Write page count to header page
    pub fn write_page_count(&mut self, count: u32) {
        self.data[HEADER_SIZE + 12..HEADER_SIZE + 16].copy_from_slice(&count.to_le_bytes());
    }

    /// Read freelist head from header page
    pub fn read_freelist_head(&self) -> u32 {
        u32::from_le_bytes([
            self.data[HEADER_SIZE + 16],
            self.data[HEADER_SIZE + 17],
            self.data[HEADER_SIZE + 18],
            self.data[HEADER_SIZE + 19],
        ])
    }

    /// Write freelist head to header page
    pub fn write_freelist_head(&mut self, page_id: u32) {
        self.data[HEADER_SIZE + 16..HEADER_SIZE + 20].copy_from_slice(&page_id.to_le_bytes());
    }

    /// Verify this is a valid header page
    pub fn verify_header_page(&self) -> Result<(), PageError> {
        // Check magic bytes
        if self.data[HEADER_SIZE..HEADER_SIZE + 4] != MAGIC_BYTES {
            return Err(PageError::InvalidPageType(self.data[0]));
        }

        // Check page size
        let stored_page_size = u32::from_le_bytes([
            self.data[HEADER_SIZE + 8],
            self.data[HEADER_SIZE + 9],
            self.data[HEADER_SIZE + 10],
            self.data[HEADER_SIZE + 11],
        ]) as usize;

        if stored_page_size != PAGE_SIZE {
            return Err(PageError::InvalidSize(stored_page_size));
        }

        Ok(())
    }
}

impl Default for Page {
    fn default() -> Self {
        Self::new(PageType::Free, 0)
    }
}

impl std::fmt::Debug for Page {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Ok(header) = self.header() {
            f.debug_struct("Page")
                .field("page_type", &header.page_type)
                .field("page_id", &header.page_id)
                .field("cell_count", &header.cell_count)
                .field("free_space", &header.free_space())
                .field("lsn", &header.lsn)
                .finish()
        } else {
            f.debug_struct("Page")
                .field("data", &"[invalid header]")
                .finish()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_page_header_roundtrip() {
        let header = PageHeader {
            page_type: PageType::BTreeLeaf,
            flags: 0x05,
            cell_count: 42,
            free_start: 100,
            free_end: 4000,
            page_id: 12345,
            parent_id: 99,
            right_child: 0,
            lsn: 0xDEADBEEF,
            checksum: 0x12345678,
        };

        let bytes = header.to_bytes();
        let decoded = PageHeader::from_bytes(&bytes).unwrap();

        assert_eq!(decoded.page_type, header.page_type);
        assert_eq!(decoded.flags, header.flags);
        assert_eq!(decoded.cell_count, header.cell_count);
        assert_eq!(decoded.free_start, header.free_start);
        assert_eq!(decoded.free_end, header.free_end);
        assert_eq!(decoded.page_id, header.page_id);
        assert_eq!(decoded.parent_id, header.parent_id);
        assert_eq!(decoded.right_child, header.right_child);
        assert_eq!(decoded.lsn, header.lsn);
        assert_eq!(decoded.checksum, header.checksum);
    }

    #[test]
    fn test_page_new() {
        let page = Page::new(PageType::BTreeLeaf, 42);
        let header = page.header().unwrap();

        assert_eq!(header.page_type, PageType::BTreeLeaf);
        assert_eq!(header.page_id, 42);
        assert_eq!(header.cell_count, 0);
        assert_eq!(header.free_start, HEADER_SIZE as u16);
        assert_eq!(header.free_end, PAGE_SIZE as u16);
    }

    #[test]
    fn test_page_checksum() {
        let mut page = Page::new(PageType::BTreeLeaf, 1);
        page.update_checksum();
        assert!(page.verify_checksum().is_ok());

        // Corrupt the page
        page.data[100] ^= 0xFF;
        assert!(page.verify_checksum().is_err());
    }

    #[test]
    fn test_page_insert_cell() {
        let mut page = Page::new(PageType::BTreeLeaf, 1);

        let key = b"hello";
        let value = b"world";

        let index = page.insert_cell(key, value).unwrap();
        assert_eq!(index, 0);
        assert_eq!(page.cell_count(), 1);

        let (read_key, read_value) = page.read_cell(0).unwrap();
        assert_eq!(read_key, key.to_vec());
        assert_eq!(read_value, value.to_vec());
    }

    #[test]
    fn test_page_multiple_cells() {
        let mut page = Page::new(PageType::BTreeLeaf, 1);

        for i in 0..10 {
            let key = format!("key{:03}", i);
            let value = format!("value{}", i);
            page.insert_cell(key.as_bytes(), value.as_bytes()).unwrap();
        }

        assert_eq!(page.cell_count(), 10);

        for i in 0..10 {
            let (key, value) = page.read_cell(i).unwrap();
            assert_eq!(key, format!("key{:03}", i).as_bytes());
            assert_eq!(value, format!("value{}", i).as_bytes());
        }
    }

    #[test]
    fn test_page_search_key() {
        let mut page = Page::new(PageType::BTreeLeaf, 1);

        // Insert sorted keys
        for i in [10, 20, 30, 40, 50] {
            let key = format!("{:03}", i);
            page.insert_cell(key.as_bytes(), b"v").unwrap();
        }

        // Search existing
        assert_eq!(page.search_key(b"020"), Ok(1));
        assert_eq!(page.search_key(b"040"), Ok(3));

        // Search non-existing
        assert_eq!(page.search_key(b"015"), Err(1));
        assert_eq!(page.search_key(b"000"), Err(0));
        assert_eq!(page.search_key(b"060"), Err(5));
    }

    #[test]
    fn test_page_full() {
        let mut page = Page::new(PageType::BTreeLeaf, 1);

        // Fill the page
        let large_value = vec![0xAB; 500];
        let mut count = 0;

        loop {
            let key = format!("key{:05}", count);
            match page.insert_cell(key.as_bytes(), &large_value) {
                Ok(_) => count += 1,
                Err(PageError::PageFull) => break,
                Err(e) => panic!("Unexpected error: {:?}", e),
            }
        }

        assert!(count > 0);
        assert!(count < 10); // With 500 byte values, should fit ~7 cells
    }

    #[test]
    fn test_header_page() {
        let page = Page::new_header_page(100);

        assert!(page.verify_header_page().is_ok());
        assert_eq!(page.read_page_count(), 100);
        assert_eq!(page.read_freelist_head(), 0);
    }

    #[test]
    fn test_page_flags() {
        let mut header = PageHeader::new(PageType::BTreeLeaf, 1);

        assert!(!header.has_flag(PageFlag::Dirty));
        assert!(!header.has_flag(PageFlag::Locked));

        header.set_flag(PageFlag::Dirty);
        assert!(header.has_flag(PageFlag::Dirty));
        assert!(!header.has_flag(PageFlag::Locked));

        header.set_flag(PageFlag::Locked);
        assert!(header.has_flag(PageFlag::Dirty));
        assert!(header.has_flag(PageFlag::Locked));

        header.clear_flag(PageFlag::Dirty);
        assert!(!header.has_flag(PageFlag::Dirty));
        assert!(header.has_flag(PageFlag::Locked));
    }

    #[test]
    fn test_free_space_calculation() {
        let page = Page::new(PageType::BTreeLeaf, 1);
        let header = page.header().unwrap();

        // New page should have max free space
        assert_eq!(header.free_space(), PAGE_SIZE - HEADER_SIZE);
    }

    // ============================================================================
    // Additional comprehensive tests for page operations
    // ============================================================================

    #[test]
    fn test_all_page_types() {
        // Verify all page types can be created and round-tripped
        let page_types = [
            PageType::Free,
            PageType::BTreeLeaf,
            PageType::BTreeInterior,
            PageType::Overflow,
            PageType::Vector,
            PageType::FreelistTrunk,
            PageType::Header,
            PageType::GraphNode,
            PageType::GraphEdge,
            PageType::GraphAdjacency,
            PageType::GraphMeta,
        ];

        for (i, &pt) in page_types.iter().enumerate() {
            let page = Page::new(pt, i as u32);
            assert_eq!(page.page_type().unwrap(), pt);
            assert_eq!(page.page_id(), i as u32);
        }
    }

    #[test]
    fn test_page_type_from_u8() {
        assert_eq!(PageType::from_u8(0), Some(PageType::Free));
        assert_eq!(PageType::from_u8(1), Some(PageType::BTreeLeaf));
        assert_eq!(PageType::from_u8(2), Some(PageType::BTreeInterior));
        assert_eq!(PageType::from_u8(10), Some(PageType::GraphMeta));
        assert_eq!(PageType::from_u8(11), None);
        assert_eq!(PageType::from_u8(255), None);
    }

    #[test]
    fn test_page_from_slice_valid() {
        let original = Page::new(PageType::BTreeLeaf, 123);
        let slice = original.as_bytes();
        let restored = Page::from_slice(slice).unwrap();

        assert_eq!(restored.page_id(), 123);
        assert_eq!(restored.page_type().unwrap(), PageType::BTreeLeaf);
    }

    #[test]
    fn test_page_from_slice_invalid_size() {
        let short_slice = [0u8; 100];
        let result = Page::from_slice(&short_slice);
        assert!(matches!(result, Err(PageError::InvalidSize(100))));

        let long_slice = [0u8; 5000];
        let result = Page::from_slice(&long_slice);
        assert!(matches!(result, Err(PageError::InvalidSize(5000))));
    }

    #[test]
    fn test_page_parent_and_child() {
        let mut page = Page::new(PageType::BTreeInterior, 10);

        page.set_parent_id(5);
        page.set_right_child(15);

        assert_eq!(page.parent_id(), 5);
        assert_eq!(page.right_child(), 15);

        // Verify through header
        let header = page.header().unwrap();
        assert_eq!(header.parent_id, 5);
        assert_eq!(header.right_child, 15);
    }

    #[test]
    fn test_cell_pointer_bounds() {
        let page = Page::new(PageType::BTreeLeaf, 1);

        // No cells, so index 0 is out of bounds
        let result = page.get_cell_pointer(0);
        assert!(matches!(result, Err(PageError::CellOutOfBounds(0))));

        let result = page.get_cell_pointer(100);
        assert!(matches!(result, Err(PageError::CellOutOfBounds(100))));
    }

    #[test]
    fn test_cell_pointer_invalid_value() {
        let mut page = Page::new(PageType::BTreeLeaf, 1);

        // Pointer too low (inside header)
        let result = page.set_cell_pointer(0, 10);
        assert!(matches!(result, Err(PageError::InvalidCellPointer(10))));

        // Pointer too high (past page)
        let result = page.set_cell_pointer(0, PAGE_SIZE as u16 + 1);
        assert!(matches!(result, Err(PageError::InvalidCellPointer(_))));
    }

    #[test]
    fn test_empty_key_value() {
        let mut page = Page::new(PageType::BTreeLeaf, 1);

        // Empty key
        page.insert_cell(b"", b"value").unwrap();
        let (key, value) = page.read_cell(0).unwrap();
        assert!(key.is_empty());
        assert_eq!(value, b"value");

        // Empty value
        page.insert_cell(b"key", b"").unwrap();
        let (key, value) = page.read_cell(1).unwrap();
        assert_eq!(key, b"key");
        assert!(value.is_empty());

        // Both empty
        page.insert_cell(b"", b"").unwrap();
        let (key, value) = page.read_cell(2).unwrap();
        assert!(key.is_empty());
        assert!(value.is_empty());
    }

    #[test]
    fn test_large_value_overflow() {
        let mut page = Page::new(PageType::BTreeLeaf, 1);

        // Value larger than content area should require overflow
        let huge_value = vec![0xAB; CONTENT_SIZE];
        let result = page.insert_cell(b"key", &huge_value);
        assert!(matches!(result, Err(PageError::OverflowRequired)));
    }

    #[test]
    fn test_checksum_stability() {
        let mut page = Page::new(PageType::BTreeLeaf, 42);
        page.insert_cell(b"test", b"data").unwrap();

        page.update_checksum();
        let checksum1 = page.header().unwrap().checksum;

        // Same content should produce same checksum
        page.update_checksum();
        let checksum2 = page.header().unwrap().checksum;

        assert_eq!(checksum1, checksum2);
    }

    #[test]
    fn test_checksum_changes_with_content() {
        let mut page1 = Page::new(PageType::BTreeLeaf, 1);
        let mut page2 = Page::new(PageType::BTreeLeaf, 1);

        page1.insert_cell(b"key1", b"value1").unwrap();
        page2.insert_cell(b"key2", b"value2").unwrap();

        page1.update_checksum();
        page2.update_checksum();

        assert_ne!(
            page1.header().unwrap().checksum,
            page2.header().unwrap().checksum
        );
    }

    #[test]
    fn test_free_space_decreases_with_cells() {
        let mut page = Page::new(PageType::BTreeLeaf, 1);
        let initial_free = page.header().unwrap().free_space();

        page.insert_cell(b"key", b"value").unwrap();
        let after_first = page.header().unwrap().free_space();

        page.insert_cell(b"another_key", b"another_value").unwrap();
        let after_second = page.header().unwrap().free_space();

        assert!(after_first < initial_free);
        assert!(after_second < after_first);
    }

    #[test]
    fn test_search_empty_page() {
        let page = Page::new(PageType::BTreeLeaf, 1);

        // Search on empty page
        assert_eq!(page.search_key(b"anything"), Err(0));
    }

    #[test]
    fn test_search_single_cell() {
        let mut page = Page::new(PageType::BTreeLeaf, 1);
        page.insert_cell(b"middle", b"v").unwrap();

        // Exact match
        assert_eq!(page.search_key(b"middle"), Ok(0));

        // Before
        assert_eq!(page.search_key(b"aaa"), Err(0));

        // After
        assert_eq!(page.search_key(b"zzz"), Err(1));
    }

    #[test]
    fn test_binary_data() {
        let mut page = Page::new(PageType::BTreeLeaf, 1);

        // Binary key and value with null bytes
        let binary_key = [0x00, 0x01, 0x02, 0xFF, 0xFE];
        let binary_value = [0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x00];

        page.insert_cell(&binary_key, &binary_value).unwrap();

        let (key, value) = page.read_cell(0).unwrap();
        assert_eq!(key, binary_key.to_vec());
        assert_eq!(value, binary_value.to_vec());
    }

    #[test]
    fn test_max_cells_stress() {
        let mut page = Page::new(PageType::BTreeLeaf, 1);

        // Insert many small cells
        let mut inserted = 0;
        for i in 0..MAX_CELLS {
            let key = format!("{:04}", i);
            if page.insert_cell(key.as_bytes(), b"x").is_ok() {
                inserted += 1;
            } else {
                break;
            }
        }

        // Verify all inserted cells are readable
        for i in 0..inserted {
            let (key, _) = page.read_cell(i).unwrap();
            assert_eq!(key, format!("{:04}", i).as_bytes());
        }
    }

    #[test]
    fn test_content_mut() {
        let mut page = Page::new(PageType::BTreeLeaf, 1);

        // Get mutable content and modify
        let content = page.content_mut();
        content[0] = 0xAB;
        content[1] = 0xCD;

        // Verify modification persisted
        let content = page.content();
        assert_eq!(content[0], 0xAB);
        assert_eq!(content[1], 0xCD);
    }

    #[test]
    fn test_page_bytes_roundtrip() {
        let mut page = Page::new(PageType::BTreeLeaf, 999);
        page.insert_cell(b"key", b"value").unwrap();
        page.update_checksum();

        // Get bytes and recreate
        let bytes = *page.as_bytes();
        let restored = Page::from_bytes(bytes);

        assert_eq!(restored.page_id(), 999);
        assert!(restored.verify_checksum().is_ok());

        let (key, value) = restored.read_cell(0).unwrap();
        assert_eq!(key, b"key");
        assert_eq!(value, b"value");
    }

    #[test]
    fn test_header_page_operations() {
        let mut page = Page::new_header_page(1000);

        assert!(page.verify_header_page().is_ok());
        assert_eq!(page.read_page_count(), 1000);
        assert_eq!(page.read_freelist_head(), 0);

        // Update page count
        page.write_page_count(2000);
        assert_eq!(page.read_page_count(), 2000);

        // Update freelist head
        page.write_freelist_head(42);
        assert_eq!(page.read_freelist_head(), 42);
    }

    #[test]
    fn test_page_flags_multiple() {
        let mut header = PageHeader::new(PageType::BTreeLeaf, 1);

        // Set multiple flags
        header.set_flag(PageFlag::Dirty);
        header.set_flag(PageFlag::Locked);
        header.set_flag(PageFlag::Encrypted);

        assert!(header.has_flag(PageFlag::Dirty));
        assert!(header.has_flag(PageFlag::Locked));
        assert!(header.has_flag(PageFlag::Encrypted));
        assert!(!header.has_flag(PageFlag::Pinned));

        // Clear one flag
        header.clear_flag(PageFlag::Locked);
        assert!(header.has_flag(PageFlag::Dirty));
        assert!(!header.has_flag(PageFlag::Locked));
        assert!(header.has_flag(PageFlag::Encrypted));
    }

    #[test]
    fn test_page_error_display() {
        let errors = [
            PageError::InvalidPageType(99),
            PageError::ChecksumMismatch {
                expected: 0x1234,
                actual: 0x5678,
            },
            PageError::InvalidSize(100),
            PageError::PageFull,
            PageError::CellOutOfBounds(5),
            PageError::InvalidCellPointer(10),
            PageError::OverflowRequired,
        ];

        for error in &errors {
            // Just verify Display doesn't panic
            let _msg = format!("{}", error);
        }
    }

    #[test]
    fn test_cell_count_consistency() {
        let mut page = Page::new(PageType::BTreeLeaf, 1);

        assert_eq!(page.cell_count(), 0);

        page.insert_cell(b"a", b"1").unwrap();
        assert_eq!(page.cell_count(), 1);

        page.insert_cell(b"b", b"2").unwrap();
        assert_eq!(page.cell_count(), 2);

        page.insert_cell(b"c", b"3").unwrap();
        assert_eq!(page.cell_count(), 3);

        // Set cell count manually (for testing)
        page.set_cell_count(0);
        assert_eq!(page.cell_count(), 0);
    }

    #[test]
    fn test_free_start_end_consistency() {
        let mut page = Page::new(PageType::BTreeLeaf, 1);

        let initial_start = page.free_start();
        let initial_end = page.free_end();

        assert_eq!(initial_start, HEADER_SIZE as u16);
        assert_eq!(initial_end, PAGE_SIZE as u16);

        page.insert_cell(b"test_key", b"test_value").unwrap();

        let after_start = page.free_start();
        let after_end = page.free_end();

        // free_start should increase (cell pointer added)
        assert!(after_start > initial_start);
        // free_end should decrease (cell content added)
        assert!(after_end < initial_end);
    }
}
