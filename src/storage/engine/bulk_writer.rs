//! Page-Based Bulk Writer — like PostgreSQL COPY FROM
//!
//! Writes rows directly into B-tree leaf pages, bypassing:
//! - UnifiedEntity object creation
//! - HashMap allocation
//! - B-tree traversal & splitting
//! - Bloom filter, memtable, cross-refs
//!
//! Each row is serialized as a cell and packed sequentially into pages.
//! Pages are linked as a doubly-linked list and written to the pager in batch.

use std::sync::Arc;

use super::page::{Page, PageType, CONTENT_SIZE, HEADER_SIZE, PAGE_SIZE};
use super::pager::Pager;
use crate::storage::schema::Value;

/// Offset where leaf data starts (after header + prev/next leaf pointers)
const LEAF_DATA_OFFSET: usize = HEADER_SIZE + 8; // 40 bytes

/// Maximum usable space per leaf page for cell data
const MAX_LEAF_DATA: usize = PAGE_SIZE - LEAF_DATA_OFFSET;

/// Serialize a row's fields into a compact byte buffer.
///
/// Format: [num_fields: u8][field1_type: u8][field1_data]...[fieldN_type: u8][fieldN_data]
/// Text: [type=1][len:u16][bytes]
/// Int:  [type=2][i64 LE 8 bytes]
/// Float:[type=3][f64 LE 8 bytes]
/// Bool: [type=4][u8]
/// Null: [type=0]
#[inline]
pub fn serialize_row(values: &[Value]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(64);
    buf.push(values.len() as u8);

    for val in values {
        match val {
            Value::Null => buf.push(0),
            Value::Text(s) => {
                buf.push(1);
                let bytes = s.as_bytes();
                buf.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
                buf.extend_from_slice(bytes);
            }
            Value::Integer(n) => {
                buf.push(2);
                buf.extend_from_slice(&n.to_le_bytes());
            }
            Value::Float(f) => {
                buf.push(3);
                buf.extend_from_slice(&f.to_le_bytes());
            }
            Value::Boolean(b) => {
                buf.push(4);
                buf.push(*b as u8);
            }
            Value::UnsignedInteger(n) => {
                buf.push(5);
                buf.extend_from_slice(&n.to_le_bytes());
            }
            // For all other types, serialize as text representation
            other => {
                buf.push(1);
                let s = format!("{other:?}");
                let bytes = s.as_bytes();
                buf.extend_from_slice(&(bytes.len() as u16).to_le_bytes());
                buf.extend_from_slice(bytes);
            }
        }
    }
    buf
}

/// Bulk writer that packs rows directly into B-tree leaf pages.
pub struct PageBulkWriter {
    pager: Arc<Pager>,
    /// Completed pages ready to write
    pages: Vec<(u32, Page)>,
    /// Current page being filled
    current: Option<Page>,
    /// Current write offset within the page
    offset: usize,
    /// Cell count in current page
    cell_count: u16,
    /// Total rows written
    total_rows: u64,
    /// Auto-incrementing entity ID
    next_id: u64,
}

impl PageBulkWriter {
    pub fn new(pager: Arc<Pager>, start_id: u64) -> Self {
        Self {
            pager,
            pages: Vec::new(),
            current: None,
            offset: LEAF_DATA_OFFSET,
            cell_count: 0,
            total_rows: 0,
            next_id: start_id,
        }
    }

    /// Write a single row (key = entity_id as u64 LE, value = serialized row).
    #[inline]
    pub fn write_row(&mut self, values: &[Value]) -> Result<u64, String> {
        let id = self.next_id;
        self.next_id += 1;

        let key = id.to_le_bytes();
        let value = serialize_row(values);

        // Cell format: [key_len:u16][val_len:u16][key][value]
        let cell_size = 4 + key.len() + value.len();

        if cell_size > MAX_LEAF_DATA {
            return Err("row too large for page".into());
        }

        // Check if current page has space
        if self.current.is_none() || self.offset + cell_size > PAGE_SIZE {
            self.seal_current_page()?;
            self.allocate_new_page()?;
        }

        // Write cell directly into page bytes
        let page = self.current.as_mut().unwrap();
        let data = page.as_bytes_mut();

        data[self.offset..self.offset + 2].copy_from_slice(&(key.len() as u16).to_le_bytes());
        data[self.offset + 2..self.offset + 4].copy_from_slice(&(value.len() as u16).to_le_bytes());
        data[self.offset + 4..self.offset + 4 + key.len()].copy_from_slice(&key);
        data[self.offset + 4 + key.len()..self.offset + 4 + key.len() + value.len()]
            .copy_from_slice(&value);

        self.offset += cell_size;
        self.cell_count += 1;
        self.total_rows += 1;

        Ok(id)
    }

    /// Write a row DIRECTLY into page buffer — zero intermediate Vec allocation.
    /// Serializes values inline into the current page's byte array.
    #[inline]
    pub fn write_row_direct(&mut self, values: &[Value]) -> Result<u64, String> {
        let id = self.next_id;
        self.next_id += 1;

        // Estimate cell size (overestimate is fine — we check bounds)
        let estimated_size = 4 + 8 + 1 + values.len() * 12; // key_len+val_len+key+header+worst_case
        if self.current.is_none() || self.offset + estimated_size > PAGE_SIZE {
            self.seal_current_page()?;
            self.allocate_new_page()?;
        }

        let page = self.current.as_mut().unwrap();
        let data = page.as_bytes_mut();

        // Reserve space for cell header [key_len:u16][val_len:u16]
        let header_pos = self.offset;
        let key_start = header_pos + 4;

        // Write key (entity ID as u64 LE)
        data[key_start..key_start + 8].copy_from_slice(&id.to_le_bytes());
        let mut pos = key_start + 8;

        // Write value count
        data[pos] = values.len() as u8;
        pos += 1;

        // Serialize each value DIRECTLY into page buffer
        for val in values {
            if pos >= PAGE_SIZE - 16 {
                // Not enough space — fall back to next page
                // (This shouldn't happen with proper estimation, but safety first)
                self.offset = header_pos; // rewind
                self.seal_current_page()?;
                self.allocate_new_page()?;
                // Retry via the Vec-based path
                return self.write_row(values);
            }
            match val {
                Value::Null => {
                    data[pos] = 0;
                    pos += 1;
                }
                Value::Text(s) => {
                    let b = s.as_bytes();
                    data[pos] = 1;
                    pos += 1;
                    data[pos..pos + 2].copy_from_slice(&(b.len() as u16).to_le_bytes());
                    pos += 2;
                    if pos + b.len() < PAGE_SIZE {
                        data[pos..pos + b.len()].copy_from_slice(b);
                        pos += b.len();
                    }
                }
                Value::Integer(n) => {
                    data[pos] = 2;
                    pos += 1;
                    data[pos..pos + 8].copy_from_slice(&n.to_le_bytes());
                    pos += 8;
                }
                Value::Float(f) => {
                    data[pos] = 3;
                    pos += 1;
                    data[pos..pos + 8].copy_from_slice(&f.to_le_bytes());
                    pos += 8;
                }
                Value::Boolean(b) => {
                    data[pos] = 4;
                    pos += 1;
                    data[pos] = *b as u8;
                    pos += 1;
                }
                Value::UnsignedInteger(n) => {
                    data[pos] = 5;
                    pos += 1;
                    data[pos..pos + 8].copy_from_slice(&n.to_le_bytes());
                    pos += 8;
                }
                _ => {
                    data[pos] = 0;
                    pos += 1; // null for unsupported types in direct mode
                }
            }
        }

        // Write cell header retroactively
        let val_len = (pos - key_start - 8) as u16;
        data[header_pos..header_pos + 2].copy_from_slice(&8u16.to_le_bytes()); // key_len = 8
        data[header_pos + 2..header_pos + 4].copy_from_slice(&val_len.to_le_bytes());

        self.offset = pos;
        self.cell_count += 1;
        self.total_rows += 1;

        Ok(id)
    }

    /// Write many rows at once.
    pub fn write_rows(&mut self, rows: &[Vec<Value>]) -> Result<Vec<u64>, String> {
        let mut ids = Vec::with_capacity(rows.len());
        for row in rows {
            ids.push(self.write_row(row)?);
        }
        Ok(ids)
    }

    /// Finish writing — seal current page, link all pages, write to pager.
    pub fn finish(mut self) -> Result<BulkWriteResult, String> {
        self.seal_current_page()?;

        if self.pages.is_empty() {
            return Ok(BulkWriteResult {
                total_rows: 0,
                total_pages: 0,
                first_page_id: 0,
                first_entity_id: 0,
            });
        }

        // Link leaf pages (doubly-linked list)
        let page_ids: Vec<u32> = self.pages.iter().map(|(id, _)| *id).collect();
        for i in 0..self.pages.len() {
            let prev = if i > 0 { page_ids[i - 1] } else { 0 };
            let next = if i + 1 < page_ids.len() {
                page_ids[i + 1]
            } else {
                0
            };
            let data = self.pages[i].1.as_bytes_mut();
            data[HEADER_SIZE..HEADER_SIZE + 4].copy_from_slice(&prev.to_le_bytes());
            data[HEADER_SIZE + 4..HEADER_SIZE + 8].copy_from_slice(&next.to_le_bytes());
        }

        // Write all pages to pager — skip per-page checksum for speed
        let first_page_id = page_ids[0];
        let total_pages = self.pages.len();
        for (page_id, page) in self.pages {
            self.pager
                .write_page_no_checksum(page_id, page)
                .map_err(|e| format!("pager write: {e}"))?;
        }

        Ok(BulkWriteResult {
            total_rows: self.total_rows,
            total_pages: total_pages as u64,
            first_page_id,
            first_entity_id: self.next_id - self.total_rows,
        })
    }

    fn seal_current_page(&mut self) -> Result<(), String> {
        if let Some(mut page) = self.current.take() {
            page.set_cell_count(self.cell_count);
            let page_id = page.page_id();
            self.pages.push((page_id, page));
            self.cell_count = 0;
            self.offset = LEAF_DATA_OFFSET;
        }
        Ok(())
    }

    fn allocate_new_page(&mut self) -> Result<(), String> {
        let page = self
            .pager
            .allocate_page(PageType::BTreeLeaf)
            .map_err(|e| format!("allocate page: {e}"))?;
        self.current = Some(page);
        self.offset = LEAF_DATA_OFFSET;
        self.cell_count = 0;
        Ok(())
    }
}

/// Result of a bulk write operation.
#[derive(Debug)]
pub struct BulkWriteResult {
    pub total_rows: u64,
    pub total_pages: u64,
    pub first_page_id: u32,
    pub first_entity_id: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_serialize_row() {
        let row = vec![
            Value::Text("Alice".to_string()),
            Value::Integer(30),
            Value::Float(95.5),
            Value::Boolean(true),
            Value::Null,
        ];
        let bytes = serialize_row(&row);
        assert_eq!(bytes[0], 5); // num_fields
        assert_eq!(bytes[1], 1); // type=text
        assert!(bytes.len() < 64);
    }

    #[test]
    fn test_serialize_row_compact() {
        // A typical user row: name(text), email(text), age(int), city(text), score(float), ts(text)
        let row = vec![
            Value::Text("User_123".to_string()),
            Value::Text("user_123@test.com".to_string()),
            Value::Integer(35),
            Value::Text("NYC".to_string()),
            Value::Float(95.5),
            Value::Text("2024-01-01".to_string()),
        ];
        let bytes = serialize_row(&row);
        // Should be very compact: ~60 bytes for a typical row
        println!("Row size: {} bytes", bytes.len());
        assert!(
            bytes.len() < 100,
            "Row should be < 100 bytes, got {}",
            bytes.len()
        );
    }
}
