use super::*;

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

    /// Get the WAL Log Sequence Number stamped on this page.
    ///
    /// `0` means "no WAL guarantee" — the page was modified through a
    /// path that did not append a WAL record (freelist trunks, header
    /// shadow pages). The double-write buffer is responsible for the
    /// integrity of `lsn == 0` pages.
    ///
    /// See `src/storage/engine/btree/README.md` § Invariant 3.
    pub fn lsn(&self) -> u64 {
        u64::from_le_bytes([
            self.data[20],
            self.data[21],
            self.data[22],
            self.data[23],
            self.data[24],
            self.data[25],
            self.data[26],
            self.data[27],
        ])
    }

    /// Stamp the WAL LSN of the record describing the most recent
    /// mutation to this page. The pager's flush path guarantees that
    /// the WAL is durable up to this LSN before writing the page to
    /// disk (WAL-first ordering — see `PLAN.md` § Target 3).
    ///
    /// Callers should pass the LSN returned by `WalWriter::append`
    /// for the change record. Pass `0` only on legacy / non-WAL
    /// write paths (DWB-protected freelist + header writes).
    pub fn set_lsn(&mut self, lsn: u64) {
        self.data[20..28].copy_from_slice(&lsn.to_le_bytes());
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
