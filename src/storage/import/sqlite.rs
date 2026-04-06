use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug)]
pub enum SqliteError {
    Io(io::Error),
    InvalidFormat,
    InvalidPageType(u8),
    TableNotFound(String),
    UnsupportedFeature(String),
}

impl From<io::Error> for SqliteError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

pub struct SqliteReader {
    file: File,
    page_size: u32,
}

#[derive(Debug, Clone)]
pub struct SqliteValue {
    pub data: Vec<u8>,
    pub data_type: SqliteType,
}

impl SqliteValue {
    pub fn as_string(&self) -> Option<String> {
        String::from_utf8(self.data.clone()).ok()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum SqliteType {
    Null,
    Integer,
    Float,
    Text,
    Blob,
}

impl SqliteReader {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, SqliteError> {
        let mut file = File::open(path)?;

        // Read header (100 bytes)
        let mut header = [0u8; 100];
        if file.read(&mut header)? != 100 {
            return Err(SqliteError::InvalidFormat);
        }

        // Check magic
        if &header[0..16] != b"SQLite format 3\0" {
            return Err(SqliteError::InvalidFormat);
        }

        // Page size at offset 16 (BE)
        let page_size_be = u16::from_be_bytes([header[16], header[17]]);
        let page_size = if page_size_be == 1 {
            65536
        } else {
            page_size_be as u32
        };

        Ok(Self { file, page_size })
    }

    /// Read a generic page
    fn read_page(&mut self, page_id: u32) -> Result<Vec<u8>, SqliteError> {
        let offset = (page_id as u64 - 1) * self.page_size as u64;
        self.file.seek(SeekFrom::Start(offset))?;

        let mut buf = vec![0u8; self.page_size as usize];
        self.file.read_exact(&mut buf)?;
        Ok(buf)
    }

    /// Read a varint (1-9 bytes)
    fn read_varint(data: &[u8], pos: &mut usize) -> u64 {
        let mut result = 0u64;
        for _i in 0..8 {
            if *pos >= data.len() {
                return result;
            }
            let byte = data[*pos];
            *pos += 1;
            result = (result << 7) | ((byte & 0x7F) as u64);
            if (byte & 0x80) == 0 {
                return result;
            }
        }
        // 9th byte uses all 8 bits
        if *pos < data.len() {
            let byte = data[*pos];
            *pos += 1;
            result = (result << 8) | (byte as u64);
        }
        result
    }

    /// Parse a record from cell content
    fn parse_record(data: &[u8]) -> Result<Vec<SqliteValue>, SqliteError> {
        let mut pos = 0;
        let _header_len = Self::read_varint(data, &mut pos);

        // Read serial types until we reach the end of header
        // Wait, header_len includes the size varint itself.
        // Let's verify: "The header begins with a single varint which determines the total number of bytes in the header. The varint value is the size of the header in bytes including the size varint itself."

        let header_start = 0;
        // We already read header_len varint. We need to know how many bytes it took.
        // Let's restart to be precise.
        pos = 0;
        let header_len = Self::read_varint(data, &mut pos) as usize;
        let header_end = header_start + header_len;

        let mut serial_types = Vec::new();
        while pos < header_end {
            serial_types.push(Self::read_varint(data, &mut pos));
        }

        let mut values = Vec::new();

        for type_code in serial_types {
            let (len, type_enum) = match type_code {
                0 => (0, SqliteType::Null),
                1 => (1, SqliteType::Integer), // 8-bit
                2 => (2, SqliteType::Integer), // 16-bit
                3 => (3, SqliteType::Integer), // 24-bit
                4 => (4, SqliteType::Integer), // 32-bit
                5 => (6, SqliteType::Integer), // 48-bit
                6 => (8, SqliteType::Integer), // 64-bit
                7 => (8, SqliteType::Float),
                8 => (0, SqliteType::Integer), // 0
                9 => (0, SqliteType::Integer), // 1
                n if n >= 12 && n % 2 == 0 => (((n - 12) / 2) as usize, SqliteType::Blob),
                n if n >= 13 && n % 2 == 1 => (((n - 13) / 2) as usize, SqliteType::Text),
                _ => (0, SqliteType::Null), // Reserved/Internal
            };

            let val_data = if len > 0 {
                if pos + len > data.len() {
                    return Err(SqliteError::InvalidFormat); // Truncated
                }
                let d = data[pos..pos + len].to_vec();
                pos += len;
                d
            } else {
                Vec::new()
            };

            values.push(SqliteValue {
                data: val_data,
                data_type: type_enum,
            });
        }

        Ok(values)
    }

    /// Scan a table for all records
    /// Note: This is a simplified scanner that assumes the table is a B-Tree Leaf or Interior.
    /// It traverses the tree.
    pub fn scan_table(&mut self, root_page: u32) -> Result<Vec<Vec<SqliteValue>>, SqliteError> {
        let mut records = Vec::new();
        let mut queue = vec![root_page];

        while let Some(page_id) = queue.pop() {
            let raw_page = self.read_page(page_id)?;
            let page = &raw_page;

            // Header offset: 0 unless it's page 1, then 100
            let header_offset = if page_id == 1 { 100 } else { 0 };

            if page.len() < header_offset + 8 {
                continue;
            }

            let page_type = page[header_offset];
            let cell_count =
                u16::from_be_bytes([page[header_offset + 3], page[header_offset + 4]]) as usize;

            let cell_arr_start = header_offset + 8 + if page_id == 1 { 0 } else { 0 }; // Page 1 header logic is tricky, usually handled by offset

            // Logic for Leaf Table (0x0D) and Interior Table (0x05)
            match page_type {
                0x0D => {
                    // Leaf Table
                    for i in 0..cell_count {
                        let ptr_offset = cell_arr_start + (i * 2);
                        let cell_ptr =
                            u16::from_be_bytes([page[ptr_offset], page[ptr_offset + 1]]) as usize;
                        if cell_ptr >= page.len() {
                            continue;
                        }

                        // Parse cell
                        let mut pos = cell_ptr;
                        let _payload_len = Self::read_varint(page, &mut pos);
                        let _row_id = Self::read_varint(page, &mut pos);

                        // remaining is payload
                        // Note: If payload is large, it spills to overflow pages.
                        // Simplified: We assume payload fits or we just read what's there (might be truncated).
                        // Chrome logins are small, usually fit.

                        // To handle overflow properly requires reading (payload_len) bytes.
                        // For now let's pass the slice from pos to end, parse_record handles header length.

                        if pos < page.len() {
                            if let Ok(record) = Self::parse_record(&page[pos..]) {
                                records.push(record);
                            }
                        }
                    }
                }
                0x05 => {
                    // Interior Table
                    // Iterate cells to find child pages
                    for i in 0..cell_count {
                        let ptr_offset = cell_arr_start + (i * 2);
                        let cell_ptr =
                            u16::from_be_bytes([page[ptr_offset], page[ptr_offset + 1]]) as usize;

                        let pos = cell_ptr;
                        let left_child = u32::from_be_bytes([
                            page[pos],
                            page[pos + 1],
                            page[pos + 2],
                            page[pos + 3],
                        ]);
                        queue.push(left_child);

                        // Key (rowid) follows, but we don't need it for full scan
                    }
                    // Right-most child
                    let right_child = u32::from_be_bytes([
                        page[header_offset + 8],
                        page[header_offset + 9],
                        page[header_offset + 10],
                        page[header_offset + 11],
                    ]);
                    queue.push(right_child);
                }
                _ => {} // Ignore index pages etc
            }
        }

        Ok(records)
    }

    /// Find root page of a table by name
    pub fn find_table_root(&mut self, name: &str) -> Result<u32, SqliteError> {
        // Scan sqlite_schema (page 1)
        // Note: sqlite_schema is a table rooted at page 1.
        let rows = self.scan_table(1)?;

        for row in rows {
            // Schema: type, name, tbl_name, rootpage, sql
            if row.len() >= 4 {
                if let Some(type_str) = row[0].as_string() {
                    if type_str == "table" {
                        if let Some(tbl_name) = row[1].as_string() {
                            if tbl_name == name {
                                // rootpage is 4th column (index 3), usually Integer
                                if let SqliteType::Integer = row[3].data_type {
                                    // Parse integer manually from LE/BE/Varint? No, parse_record returns raw data
                                    // based on type.
                                    // Wait, parse_record implementation for Integer:
                                    // 1 byte: 8-bit, 2: 16-bit, etc.
                                    // I need a helper to cast data to u32
                                    return Ok(Self::parse_int(&row[3].data) as u32);
                                }
                            }
                        }
                    }
                }
            }
        }

        Err(SqliteError::TableNotFound(name.to_string()))
    }

    fn parse_int(data: &[u8]) -> i64 {
        let mut val = 0i64;
        for &b in data {
            val = (val << 8) | (b as i64);
        }
        val
    }
}
