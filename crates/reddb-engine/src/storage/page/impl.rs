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
        let page_type = reddb_file::paged_page_type(&self.data);
        PageType::from_u8(page_type).ok_or(PageError::InvalidPageType(page_type))
    }

    /// Get page ID
    pub fn page_id(&self) -> u32 {
        reddb_file::paged_page_id(&self.data)
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
        reddb_file::paged_page_lsn(&self.data)
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
        reddb_file::set_paged_page_lsn(&mut self.data, lsn);
    }

    /// Get cell count
    pub fn cell_count(&self) -> u16 {
        reddb_file::paged_page_cell_count(&self.data)
    }

    /// Set cell count
    pub fn set_cell_count(&mut self, count: u16) {
        reddb_file::set_paged_page_cell_count(&mut self.data, count);
    }

    /// Get parent page ID
    pub fn parent_id(&self) -> u32 {
        reddb_file::paged_page_parent_id(&self.data)
    }

    /// Set parent page ID
    pub fn set_parent_id(&mut self, parent_id: u32) {
        reddb_file::set_paged_page_parent_id(&mut self.data, parent_id);
    }

    /// Get right child page ID (for interior nodes)
    pub fn right_child(&self) -> u32 {
        reddb_file::paged_page_right_child(&self.data)
    }

    /// Set right child page ID (for interior nodes)
    pub fn set_right_child(&mut self, child_id: u32) {
        reddb_file::set_paged_page_right_child(&mut self.data, child_id);
    }

    /// Get free_start offset
    pub fn free_start(&self) -> u16 {
        reddb_file::paged_page_free_start(&self.data)
    }

    /// Set free_start offset
    pub fn set_free_start(&mut self, offset: u16) {
        reddb_file::set_paged_page_free_start(&mut self.data, offset);
    }

    /// Get free_end offset
    pub fn free_end(&self) -> u16 {
        reddb_file::paged_page_free_end(&self.data)
    }

    /// Set free_end offset
    pub fn set_free_end(&mut self, offset: u16) {
        reddb_file::set_paged_page_free_end(&mut self.data, offset);
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
        reddb_file::clear_paged_page_checksum(&mut self.data);
        // Calculate CRC32 of entire page
        let checksum = crc32(&self.data);
        // Store checksum
        reddb_file::set_paged_page_checksum(&mut self.data, checksum);
    }

    /// Verify page checksum
    pub fn verify_checksum(&self) -> Result<(), PageError> {
        let stored = reddb_file::paged_page_checksum(&self.data);

        // Calculate checksum with stored value zeroed
        let mut temp = self.data;
        reddb_file::clear_paged_page_checksum(&mut temp);
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

        reddb_file::paged_cell_pointer(&self.data, index).ok_or(PageError::CellOutOfBounds(index))
    }

    /// Set cell pointer at index
    pub fn set_cell_pointer(&mut self, index: usize, pointer: u16) -> Result<(), PageError> {
        if !reddb_file::paged_cell_pointer_is_valid(pointer) {
            return Err(PageError::InvalidCellPointer(pointer));
        }

        if !reddb_file::set_paged_cell_pointer(&mut self.data, index, pointer) {
            return Err(PageError::CellOutOfBounds(index));
        }
        Ok(())
    }

    /// Get cell data by index
    pub fn get_cell(&self, index: usize) -> Result<&[u8], PageError> {
        let pointer = self.get_cell_pointer(index)? as usize;

        reddb_file::paged_cell_bytes(&self.data, pointer as u16)
            .ok_or(PageError::InvalidCellPointer(pointer as u16))
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

        let cell_size =
            reddb_file::paged_cell_len(key_len, value_len).ok_or(PageError::OverflowRequired)?;

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
        if !reddb_file::write_paged_cell(&mut self.data, cell_offset as u16, key, value) {
            return Err(PageError::InvalidCellPointer(cell_offset as u16));
        }

        // Write cell pointer
        let cell_index = header.cell_count as usize;
        if !reddb_file::set_paged_cell_pointer(&mut self.data, cell_index, cell_offset as u16) {
            return Err(PageError::CellOutOfBounds(cell_index));
        }

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

        let (key, value) =
            reddb_file::paged_cell_key_value(cell).ok_or(PageError::InvalidCellPointer(0))?;

        Ok((key.to_vec(), value.to_vec()))
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

        reddb_file::init_database_header_page(&mut page.data, page_count)
            .expect("fixed-size page can hold database header");

        page.update_checksum();
        page
    }

    /// Read page count from header page
    pub fn read_page_count(&self) -> u32 {
        reddb_file::database_header_page_count(&self.data).expect("fixed-size page has page count")
    }

    /// Write page count to header page
    pub fn write_page_count(&mut self, count: u32) {
        reddb_file::set_database_header_page_count(&mut self.data, count)
            .expect("fixed-size page has page count");
    }

    /// Read freelist head from header page
    pub fn read_freelist_head(&self) -> u32 {
        reddb_file::database_header_freelist_head(&self.data)
            .expect("fixed-size page has freelist head")
    }

    /// Write freelist head to header page
    pub fn write_freelist_head(&mut self, page_id: u32) {
        reddb_file::set_database_header_freelist_head(&mut self.data, page_id)
            .expect("fixed-size page has freelist head");
    }

    /// Verify this is a valid header page
    pub fn verify_header_page(&self) -> Result<(), PageError> {
        // Check magic bytes
        if !reddb_file::database_header_magic_matches(&self.data) {
            return Err(PageError::InvalidPageType(self.data[0]));
        }

        // Check page size
        let stored_page_size = reddb_file::database_header_page_size(&self.data)
            .map_err(|_| PageError::InvalidSize(self.data.len()))?
            as usize;

        if stored_page_size != PAGE_SIZE {
            return Err(PageError::InvalidSize(stored_page_size));
        }

        Ok(())
    }
}
