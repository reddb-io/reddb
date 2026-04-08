//! B+ Tree Implementation for Page-Based Storage
//!
//! A B+ tree optimized for disk I/O with the following properties:
//! - All values stored in leaf nodes
//! - Interior nodes only contain keys and child pointers
//! - Leaf nodes form a doubly-linked list for range scans
//! - Each node fits in a single 4KB page
//!
//! # Page Layout
//!
//! Interior Node:
//! ```text
//! ┌────────────────────────────────────────────────────────────┐
//! │ PageHeader (32 bytes)                                      │
//! ├────────────────────────────────────────────────────────────┤
//! │ right_child: u32 - Rightmost child pointer                 │
//! │ cells: [key_len: u16, key: [u8], child: u32]...           │
//! └────────────────────────────────────────────────────────────┘
//! ```
//!
//! Leaf Node:
//! ```text
//! ┌────────────────────────────────────────────────────────────┐
//! │ PageHeader (32 bytes)                                      │
//! ├────────────────────────────────────────────────────────────┤
//! │ prev_leaf: u32 - Previous leaf (0 = none)                  │
//! │ next_leaf: u32 - Next leaf (0 = none)                      │
//! │ cells: [key_len: u16, val_len: u16, key: [u8], val: [u8]] │
//! └────────────────────────────────────────────────────────────┘
//! ```

use std::cmp::Ordering;
use std::sync::{Arc, RwLock};

use super::page::{Page, PageType, HEADER_SIZE, PAGE_SIZE};
use super::pager::{Pager, PagerError};

/// Maximum key size (to ensure at least 2 keys per node)
pub const MAX_KEY_SIZE: usize = 1024;

/// Maximum value size for inline storage
pub const MAX_VALUE_SIZE: usize = 1024;

/// Minimum fill factor before merge (as percentage)
pub const MIN_FILL_FACTOR: usize = 25;

/// Offset of prev_leaf in leaf page
const LEAF_PREV_OFFSET: usize = HEADER_SIZE;

/// Offset of next_leaf in leaf page
const LEAF_NEXT_OFFSET: usize = HEADER_SIZE + 4;

/// Start of cell data in leaf pages
const LEAF_DATA_OFFSET: usize = HEADER_SIZE + 8;

/// Start of cell data in interior pages (right_child is in header)
const INTERIOR_DATA_OFFSET: usize = HEADER_SIZE;

/// B+ Tree error types
#[derive(Debug, Clone)]
pub enum BTreeError {
    /// Key not found
    NotFound,
    /// Key already exists
    DuplicateKey,
    /// Key too large
    KeyTooLarge(usize),
    /// Value too large
    ValueTooLarge(usize),
    /// Tree is corrupted
    Corrupted(String),
    /// Pager error
    Pager(String),
}

impl std::fmt::Display for BTreeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "Key not found"),
            Self::DuplicateKey => write!(f, "Key already exists"),
            Self::KeyTooLarge(size) => {
                write!(f, "Key too large: {} bytes (max {})", size, MAX_KEY_SIZE)
            }
            Self::ValueTooLarge(size) => write!(
                f,
                "Value too large: {} bytes (max {})",
                size, MAX_VALUE_SIZE
            ),
            Self::Corrupted(msg) => write!(f, "B-tree corrupted: {}", msg),
            Self::Pager(msg) => write!(f, "Pager error: {}", msg),
        }
    }
}

impl std::error::Error for BTreeError {}

impl From<PagerError> for BTreeError {
    fn from(e: PagerError) -> Self {
        Self::Pager(e.to_string())
    }
}

impl From<super::page::PageError> for BTreeError {
    fn from(e: super::page::PageError) -> Self {
        Self::Corrupted(e.to_string())
    }
}

/// Result type for B+ tree operations
pub type BTreeResult<T> = Result<T, BTreeError>;

/// B+ Tree cursor for iteration
pub struct BTreeCursor {
    /// Current leaf page ID
    leaf_page_id: u32,
    /// Current position within leaf
    position: usize,
    /// Pager reference
    pager: Arc<Pager>,
}

impl BTreeCursor {
    /// Move to next entry
    pub fn next(&mut self) -> BTreeResult<Option<(Vec<u8>, Vec<u8>)>> {
        if self.leaf_page_id == 0 {
            return Ok(None);
        }

        let page = self.pager.read_page(self.leaf_page_id)?;
        let cell_count = page.cell_count() as usize;

        // Check if we have more cells in current page
        if self.position < cell_count {
            let (key, value) = read_leaf_cell(&page, self.position)?;
            self.position += 1;
            return Ok(Some((key, value)));
        }

        // Move to next leaf
        let next_leaf = read_next_leaf(&page);
        if next_leaf == 0 {
            self.leaf_page_id = 0;
            return Ok(None);
        }

        self.leaf_page_id = next_leaf;
        self.position = 0;

        // Read from new leaf
        let page = self.pager.read_page(self.leaf_page_id)?;
        if page.cell_count() == 0 {
            return Ok(None);
        }

        let (key, value) = read_leaf_cell(&page, 0)?;
        self.position = 1;
        Ok(Some((key, value)))
    }

    /// Peek at current entry without advancing
    pub fn peek(&self) -> BTreeResult<Option<(Vec<u8>, Vec<u8>)>> {
        if self.leaf_page_id == 0 {
            return Ok(None);
        }

        let page = self.pager.read_page(self.leaf_page_id)?;
        let cell_count = page.cell_count() as usize;

        if self.position >= cell_count {
            return Ok(None);
        }

        let (key, value) = read_leaf_cell(&page, self.position)?;
        Ok(Some((key, value)))
    }
}

/// B+ Tree implementation
pub struct BTree {
    /// Pager for page I/O
    pager: Arc<Pager>,
    /// Root page ID (0 = empty tree)
    root_page_id: RwLock<u32>,
}


#[path = "btree/impl.rs"]
mod btree_impl;
// ==================== Search Helpers ====================

enum SearchResult {
    Found(usize),
    NotFound(usize),
}

fn search_leaf(page: &Page, key: &[u8]) -> BTreeResult<SearchResult> {
    let cell_count = page.cell_count() as usize;

    // Binary search
    let mut low = 0;
    let mut high = cell_count;

    while low < high {
        let mid = (low + high) / 2;
        let (cell_key, _) = read_leaf_cell(page, mid)?;

        match cell_key.as_slice().cmp(key) {
            Ordering::Less => low = mid + 1,
            Ordering::Greater => high = mid,
            Ordering::Equal => return Ok(SearchResult::Found(mid)),
        }
    }

    Ok(SearchResult::NotFound(low))
}

fn find_child(page: &Page, key: &[u8]) -> BTreeResult<u32> {
    let cell_count = page.cell_count() as usize;

    // Binary search for the correct child
    for i in 0..cell_count {
        let (cell_key, child) = read_interior_cell(page, i)?;
        if key < cell_key.as_slice() {
            return Ok(child);
        }
    }

    // Key is >= all keys, use right child
    Ok(page.right_child())
}

fn find_first_child(page: &Page) -> BTreeResult<u32> {
    if page.cell_count() == 0 {
        return Ok(page.right_child());
    }
    let (_, child) = read_interior_cell(page, 0)?;
    Ok(child)
}

fn leaf_min_bytes() -> usize {
    (PAGE_SIZE - LEAF_DATA_OFFSET) * MIN_FILL_FACTOR / 100
}

fn interior_min_bytes() -> usize {
    (PAGE_SIZE - INTERIOR_DATA_OFFSET) * MIN_FILL_FACTOR / 100
}

fn leaf_entry_size(entry: &(Vec<u8>, Vec<u8>)) -> usize {
    4 + entry.0.len() + entry.1.len()
}

fn leaf_entries_size(entries: &[(Vec<u8>, Vec<u8>)]) -> usize {
    entries.iter().map(leaf_entry_size).sum()
}

fn interior_key_size(key: &[u8]) -> usize {
    2 + key.len() + 4
}

fn interior_entries_size(keys: &[Vec<u8>]) -> usize {
    keys.iter().map(|k| interior_key_size(k)).sum()
}

// ==================== Leaf Page Helpers ====================

fn read_leaf_cell(page: &Page, index: usize) -> BTreeResult<(Vec<u8>, Vec<u8>)> {
    let data = page.as_bytes();
    let cell_count = page.cell_count() as usize;

    if index >= cell_count {
        return Err(BTreeError::Corrupted("Cell index out of range".into()));
    }

    // Find cell offset by scanning from start
    let mut offset = LEAF_DATA_OFFSET;
    for _ in 0..index {
        let key_len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
        let val_len = u16::from_le_bytes([data[offset + 2], data[offset + 3]]) as usize;
        offset += 4 + key_len + val_len;
    }

    let key_len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
    let val_len = u16::from_le_bytes([data[offset + 2], data[offset + 3]]) as usize;

    let key = data[offset + 4..offset + 4 + key_len].to_vec();
    let value = data[offset + 4 + key_len..offset + 4 + key_len + val_len].to_vec();

    Ok((key, value))
}

fn write_leaf_cell(page: &mut Page, index: usize, key: &[u8], value: &[u8]) -> BTreeResult<()> {
    let data = page.as_bytes_mut();

    // Find cell offset by scanning from start
    let mut offset = LEAF_DATA_OFFSET;
    for _ in 0..index {
        let key_len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
        let val_len = u16::from_le_bytes([data[offset + 2], data[offset + 3]]) as usize;
        offset += 4 + key_len + val_len;
    }

    // Write lengths
    data[offset..offset + 2].copy_from_slice(&(key.len() as u16).to_le_bytes());
    data[offset + 2..offset + 4].copy_from_slice(&(value.len() as u16).to_le_bytes());

    // Write key and value
    data[offset + 4..offset + 4 + key.len()].copy_from_slice(key);
    data[offset + 4 + key.len()..offset + 4 + key.len() + value.len()].copy_from_slice(value);

    Ok(())
}

fn read_leaf_entries(page: &Page) -> BTreeResult<Vec<(Vec<u8>, Vec<u8>)>> {
    let cell_count = page.cell_count() as usize;
    let mut entries = Vec::with_capacity(cell_count);
    for i in 0..cell_count {
        entries.push(read_leaf_cell(page, i)?);
    }
    Ok(entries)
}

fn write_leaf_entries(page: &mut Page, entries: &[(Vec<u8>, Vec<u8>)]) -> BTreeResult<()> {
    clear_leaf_cells(page);
    for (i, (k, v)) in entries.iter().enumerate() {
        write_leaf_cell(page, i, k, v)?;
    }
    page.set_cell_count(entries.len() as u16);
    Ok(())
}

fn can_insert_leaf(page: &Page, key: &[u8], value: &[u8]) -> bool {
    let data = page.as_bytes();
    let cell_count = page.cell_count() as usize;

    // Calculate current used space
    let mut offset = LEAF_DATA_OFFSET;
    for _ in 0..cell_count {
        let key_len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
        let val_len = u16::from_le_bytes([data[offset + 2], data[offset + 3]]) as usize;
        offset += 4 + key_len + val_len;
    }

    // Check if new cell fits
    let needed = 4 + key.len() + value.len();
    offset + needed <= PAGE_SIZE
}

fn insert_into_leaf(page: &mut Page, key: &[u8], value: &[u8]) -> BTreeResult<()> {
    let cell_count = page.cell_count() as usize;

    // Find insertion position
    let mut entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(cell_count + 1);
    for i in 0..cell_count {
        entries.push(read_leaf_cell(page, i)?);
    }

    let insert_pos = entries.partition_point(|(k, _)| k.as_slice() < key);
    entries.insert(insert_pos, (key.to_vec(), value.to_vec()));

    // Rewrite all cells
    clear_leaf_cells(page);
    for (i, (k, v)) in entries.iter().enumerate() {
        write_leaf_cell(page, i, k, v)?;
    }
    page.set_cell_count(entries.len() as u16);

    Ok(())
}

fn delete_from_leaf(page: &mut Page, index: usize) -> BTreeResult<()> {
    let cell_count = page.cell_count() as usize;

    // Read all cells except the deleted one
    let mut entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::with_capacity(cell_count - 1);
    for i in 0..cell_count {
        if i != index {
            entries.push(read_leaf_cell(page, i)?);
        }
    }

    // Rewrite cells
    clear_leaf_cells(page);
    for (i, (k, v)) in entries.iter().enumerate() {
        write_leaf_cell(page, i, k, v)?;
    }
    page.set_cell_count(entries.len() as u16);

    Ok(())
}

fn clear_leaf_cells(page: &mut Page) {
    let data = page.as_bytes_mut();
    // Zero out cell data area
    for byte in &mut data[LEAF_DATA_OFFSET..] {
        *byte = 0;
    }
}

fn init_leaf_links(page: &mut Page, prev: u32, next: u32) {
    let data = page.as_bytes_mut();
    data[LEAF_PREV_OFFSET..LEAF_PREV_OFFSET + 4].copy_from_slice(&prev.to_le_bytes());
    data[LEAF_NEXT_OFFSET..LEAF_NEXT_OFFSET + 4].copy_from_slice(&next.to_le_bytes());
}

fn read_next_leaf(page: &Page) -> u32 {
    let data = page.as_bytes();
    u32::from_le_bytes([
        data[LEAF_NEXT_OFFSET],
        data[LEAF_NEXT_OFFSET + 1],
        data[LEAF_NEXT_OFFSET + 2],
        data[LEAF_NEXT_OFFSET + 3],
    ])
}

fn set_prev_leaf(page: &mut Page, prev: u32) {
    let data = page.as_bytes_mut();
    data[LEAF_PREV_OFFSET..LEAF_PREV_OFFSET + 4].copy_from_slice(&prev.to_le_bytes());
}

fn set_next_leaf(page: &mut Page, next: u32) {
    let data = page.as_bytes_mut();
    data[LEAF_NEXT_OFFSET..LEAF_NEXT_OFFSET + 4].copy_from_slice(&next.to_le_bytes());
}

// ==================== Interior Page Helpers ====================

fn read_interior_cell(page: &Page, index: usize) -> BTreeResult<(Vec<u8>, u32)> {
    let data = page.as_bytes();
    let cell_count = page.cell_count() as usize;

    if index >= cell_count {
        return Err(BTreeError::Corrupted("Cell index out of range".into()));
    }

    // Find cell offset by scanning from start
    let mut offset = INTERIOR_DATA_OFFSET;
    for _ in 0..index {
        let key_len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2 + key_len + 4; // key_len + key + child
    }

    let key_len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
    let key = data[offset + 2..offset + 2 + key_len].to_vec();
    let child = u32::from_le_bytes([
        data[offset + 2 + key_len],
        data[offset + 2 + key_len + 1],
        data[offset + 2 + key_len + 2],
        data[offset + 2 + key_len + 3],
    ]);

    Ok((key, child))
}

fn read_interior_keys_children(page: &Page) -> BTreeResult<(Vec<Vec<u8>>, Vec<u32>)> {
    let cell_count = page.cell_count() as usize;
    let mut keys = Vec::with_capacity(cell_count);
    let mut children = Vec::with_capacity(cell_count + 1);

    for i in 0..cell_count {
        let (key, child) = read_interior_cell(page, i)?;
        keys.push(key);
        children.push(child);
    }

    if cell_count == 0 {
        let right_child = page.right_child();
        if right_child != 0 {
            children.push(right_child);
        }
    } else {
        children.push(page.right_child());
    }

    Ok((keys, children))
}

fn write_interior_entries(page: &mut Page, keys: &[Vec<u8>], children: &[u32]) -> BTreeResult<()> {
    if !keys.is_empty() && children.len() != keys.len() + 1 {
        return Err(BTreeError::Corrupted(
            "Interior keys/children length mismatch".into(),
        ));
    }

    clear_interior_cells(page);
    if keys.is_empty() {
        page.set_cell_count(0);
        let right_child = children.first().copied().unwrap_or(0);
        page.set_right_child(right_child);
        return Ok(());
    }

    for (i, key) in keys.iter().enumerate() {
        write_interior_cell(page, i, key, children[i])?;
    }
    page.set_cell_count(keys.len() as u16);
    page.set_right_child(*children.last().unwrap());
    Ok(())
}

fn write_interior_cell(page: &mut Page, index: usize, key: &[u8], child: u32) -> BTreeResult<()> {
    let data = page.as_bytes_mut();

    // Find cell offset by scanning from start
    let mut offset = INTERIOR_DATA_OFFSET;
    for _ in 0..index {
        let key_len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2 + key_len + 4;
    }

    // Write key length
    data[offset..offset + 2].copy_from_slice(&(key.len() as u16).to_le_bytes());

    // Write key
    data[offset + 2..offset + 2 + key.len()].copy_from_slice(key);

    // Write child pointer
    data[offset + 2 + key.len()..offset + 2 + key.len() + 4].copy_from_slice(&child.to_le_bytes());

    Ok(())
}

fn can_insert_interior(page: &Page, key: &[u8]) -> bool {
    let data = page.as_bytes();
    let cell_count = page.cell_count() as usize;

    // Calculate current used space
    let mut offset = INTERIOR_DATA_OFFSET;
    for _ in 0..cell_count {
        let key_len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
        offset += 2 + key_len + 4;
    }

    // Check if new cell fits
    let needed = 2 + key.len() + 4;
    offset + needed <= PAGE_SIZE
}

fn insert_into_interior(
    page: &mut Page,
    key: &[u8],
    left_child: u32,
    right_child: u32,
) -> BTreeResult<()> {
    let cell_count = page.cell_count() as usize;

    // Read all cells
    let mut entries: Vec<(Vec<u8>, u32)> = Vec::with_capacity(cell_count + 1);
    for i in 0..cell_count {
        entries.push(read_interior_cell(page, i)?);
    }

    // Find insertion position
    let insert_pos = entries.partition_point(|(k, _)| k.as_slice() < key);

    // Update child pointer for the key we're displacing
    if insert_pos < entries.len() {
        entries[insert_pos].1 = right_child;
    } else {
        // New key is largest, update right_child
        page.set_right_child(right_child);
    }

    // Insert new entry
    entries.insert(insert_pos, (key.to_vec(), left_child));

    // Rewrite all cells
    clear_interior_cells(page);
    for (i, (k, c)) in entries.iter().enumerate() {
        write_interior_cell(page, i, k, *c)?;
    }
    page.set_cell_count(entries.len() as u16);

    Ok(())
}

fn clear_interior_cells(page: &mut Page) {
    let data = page.as_bytes_mut();
    for byte in &mut data[INTERIOR_DATA_OFFSET..] {
        *byte = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_db_path() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let id = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!("reddb_btree_test_{}_{}.db", std::process::id(), id));
        path
    }

    fn cleanup(path: &PathBuf) {
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_btree_empty() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).unwrap());
        let tree = BTree::new(pager);

        assert!(tree.is_empty());
        assert_eq!(tree.root_page_id(), 0);
        assert_eq!(tree.get(b"key").unwrap(), None);
        assert_eq!(tree.count().unwrap(), 0);

        cleanup(&path);
    }

    #[test]
    fn test_btree_single_insert() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).unwrap());
        let tree = BTree::new(pager);

        tree.insert(b"hello", b"world").unwrap();

        assert!(!tree.is_empty());
        assert_eq!(tree.get(b"hello").unwrap(), Some(b"world".to_vec()));
        assert_eq!(tree.get(b"other").unwrap(), None);
        assert_eq!(tree.count().unwrap(), 1);

        cleanup(&path);
    }

    #[test]
    fn test_btree_multiple_inserts() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).unwrap());
        let tree = BTree::new(pager);

        tree.insert(b"c", b"3").unwrap();
        tree.insert(b"a", b"1").unwrap();
        tree.insert(b"b", b"2").unwrap();

        assert_eq!(tree.get(b"a").unwrap(), Some(b"1".to_vec()));
        assert_eq!(tree.get(b"b").unwrap(), Some(b"2".to_vec()));
        assert_eq!(tree.get(b"c").unwrap(), Some(b"3".to_vec()));
        assert_eq!(tree.count().unwrap(), 3);

        cleanup(&path);
    }

    #[test]
    fn test_btree_duplicate_key() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).unwrap());
        let tree = BTree::new(pager);

        tree.insert(b"key", b"value1").unwrap();
        let result = tree.insert(b"key", b"value2");

        assert!(matches!(result, Err(BTreeError::DuplicateKey)));
        assert_eq!(tree.get(b"key").unwrap(), Some(b"value1".to_vec()));

        cleanup(&path);
    }

    #[test]
    fn test_btree_delete() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).unwrap());
        let tree = BTree::new(pager);

        tree.insert(b"a", b"1").unwrap();
        tree.insert(b"b", b"2").unwrap();
        tree.insert(b"c", b"3").unwrap();

        assert!(tree.delete(b"b").unwrap());
        assert!(!tree.delete(b"d").unwrap());

        assert_eq!(tree.get(b"a").unwrap(), Some(b"1".to_vec()));
        assert_eq!(tree.get(b"b").unwrap(), None);
        assert_eq!(tree.get(b"c").unwrap(), Some(b"3".to_vec()));
        assert_eq!(tree.count().unwrap(), 2);

        cleanup(&path);
    }

    #[test]
    fn test_btree_delete_rebalance_removes_empty_leaf() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).unwrap());
        let tree = BTree::new(pager.clone());

        let value = vec![b'v'; 200];
        for i in 0..60u32 {
            let key = format!("key{:03}", i);
            tree.insert(key.as_bytes(), &value).unwrap();
        }

        let root_id = tree.root_page_id();
        let first_leaf = tree.find_first_leaf(root_id).unwrap();
        let mut leaf_ids = Vec::new();
        let mut current = first_leaf;
        loop {
            leaf_ids.push(current);
            let page = pager.read_page(current).unwrap();
            let next = read_next_leaf(&page);
            if next == 0 {
                break;
            }
            current = next;
        }

        assert!(leaf_ids.len() >= 3);

        let target_leaf = leaf_ids[1];
        let page = pager.read_page(target_leaf).unwrap();
        let cell_count = page.cell_count() as usize;
        let mut keys = Vec::with_capacity(cell_count);
        for i in 0..cell_count {
            let (key, _) = read_leaf_cell(&page, i).unwrap();
            keys.push(key);
        }

        for key in &keys {
            tree.delete(key).unwrap();
        }

        let expected = 60 - keys.len();
        assert_eq!(tree.count().unwrap(), expected);

        let mut cursor = tree.cursor_first().unwrap();
        let mut results = Vec::new();
        while let Some((key, _)) = cursor.next().unwrap() {
            results.push(key);
        }

        assert_eq!(results.len(), expected);
        let last_key = format!("key{:03}", 59).into_bytes();
        assert_eq!(results.last(), Some(&last_key));

        cleanup(&path);
    }

    #[test]
    fn test_btree_cursor() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).unwrap());
        let tree = BTree::new(pager);

        tree.insert(b"c", b"3").unwrap();
        tree.insert(b"a", b"1").unwrap();
        tree.insert(b"b", b"2").unwrap();

        let mut cursor = tree.cursor_first().unwrap();
        let mut results: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();

        while let Some(entry) = cursor.next().unwrap() {
            results.push(entry);
        }

        assert_eq!(results.len(), 3);
        assert_eq!(results[0], (b"a".to_vec(), b"1".to_vec()));
        assert_eq!(results[1], (b"b".to_vec(), b"2".to_vec()));
        assert_eq!(results[2], (b"c".to_vec(), b"3".to_vec()));

        cleanup(&path);
    }

    #[test]
    fn test_btree_range() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).unwrap());
        let tree = BTree::new(pager);

        for i in 0..10u8 {
            let key = format!("key{:02}", i);
            let value = format!("val{:02}", i);
            tree.insert(key.as_bytes(), value.as_bytes()).unwrap();
        }

        let results = tree.range(b"key03", b"key06").unwrap();

        assert_eq!(results.len(), 4);
        assert_eq!(results[0].0, b"key03".to_vec());
        assert_eq!(results[3].0, b"key06".to_vec());

        cleanup(&path);
    }

    #[test]
    fn test_btree_large_keys() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).unwrap());
        let tree = BTree::new(pager);

        let key = vec![b'x'; 500];
        let value = vec![b'y'; 500];

        tree.insert(&key, &value).unwrap();
        assert_eq!(tree.get(&key).unwrap(), Some(value));

        cleanup(&path);
    }

    #[test]
    fn test_btree_key_too_large() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).unwrap());
        let tree = BTree::new(pager);

        let key = vec![b'x'; MAX_KEY_SIZE + 1];
        let result = tree.insert(&key, b"value");

        assert!(matches!(result, Err(BTreeError::KeyTooLarge(_))));

        cleanup(&path);
    }

    #[test]
    fn test_btree_many_inserts() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).unwrap());
        let tree = BTree::new(pager);

        // Insert enough entries to force splits
        for i in 0..100u32 {
            let key = format!("key{:08}", i);
            let value = format!("value{:08}", i);
            tree.insert(key.as_bytes(), value.as_bytes()).unwrap();
        }

        // Verify all entries
        for i in 0..100u32 {
            let key = format!("key{:08}", i);
            let expected = format!("value{:08}", i);
            assert_eq!(
                tree.get(key.as_bytes()).unwrap(),
                Some(expected.into_bytes())
            );
        }

        assert_eq!(tree.count().unwrap(), 100);

        cleanup(&path);
    }

    #[test]
    fn test_btree_sorted_iteration() {
        let path = temp_db_path();
        cleanup(&path);

        let pager = Arc::new(Pager::open_default(&path).unwrap());
        let tree = BTree::new(pager);

        // Insert in random order
        let keys = vec![50, 25, 75, 10, 30, 60, 80, 5, 15, 27, 35, 55, 65, 77, 90];
        for k in &keys {
            let key = format!("{:03}", k);
            tree.insert(key.as_bytes(), key.as_bytes()).unwrap();
        }

        // Should iterate in sorted order
        let mut cursor = tree.cursor_first().unwrap();
        let mut prev: Option<Vec<u8>> = None;

        while let Some((key, _)) = cursor.next().unwrap() {
            if let Some(p) = &prev {
                assert!(p < &key, "Keys not in sorted order");
            }
            prev = Some(key);
        }

        cleanup(&path);
    }
}
