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

/// Start of the slot array in leaf pages.
///
/// Slotted-page layout:
/// ```text
/// [PageHeader 32B][prev u32 | next u32 | 8B]
/// [slot 0: u16][slot 1: u16]...[slot N-1: u16]  ← grows forward
/// ... free space ...
/// [cell N-1][cell N-2]...[cell 0]               ← grows backward from free_end
/// ```
/// Each cell is laid out as `[key_len:u16][val_len:u16][key][val]`.
/// `page.cell_count()` is N; `page.free_end()` is the offset of the
/// lowest (most recently written) cell. The slot array lives right
/// after the leaf-chain links and each u16 slot is the absolute page
/// offset of its cell.
const LEAF_SLOT_ARRAY_OFFSET: usize = HEADER_SIZE + 8;

/// Kept for source-compat with older code paths (e.g. interior-node
/// helpers); equivalent to `LEAF_SLOT_ARRAY_OFFSET` for leaves.
const LEAF_DATA_OFFSET: usize = LEAF_SLOT_ARRAY_OFFSET;

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
    /// Internal lock was poisoned by a panicked thread
    LockPoisoned(String),
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
            Self::LockPoisoned(msg) => write!(f, "Lock poisoned: {}", msg),
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

/// Bytes consumed by one leaf entry in the slotted layout: one u16
/// slot in the pointer array plus the cell itself (`[key_len:u16]
/// [val_len:u16][key][val]`).
fn leaf_entry_size(entry: &(Vec<u8>, Vec<u8>)) -> usize {
    2 + 4 + entry.0.len() + entry.1.len()
}

fn leaf_entries_size(entries: &[(Vec<u8>, Vec<u8>)]) -> usize {
    entries.iter().map(leaf_entry_size).sum()
}

#[inline]
fn leaf_slot_offset_for(index: usize) -> usize {
    LEAF_SLOT_ARRAY_OFFSET + index * 2
}

#[inline]
fn leaf_read_slot(page: &Page, index: usize) -> BTreeResult<usize> {
    let data = page.as_bytes();
    let slot_pos = leaf_slot_offset_for(index);
    if slot_pos + 2 > PAGE_SIZE {
        return Err(BTreeError::Corrupted("slot array overflows page".into()));
    }
    Ok(u16::from_le_bytes([data[slot_pos], data[slot_pos + 1]]) as usize)
}

#[inline]
fn leaf_write_slot(page: &mut Page, index: usize, cell_offset: u16) -> BTreeResult<()> {
    let data = page.as_bytes_mut();
    let slot_pos = leaf_slot_offset_for(index);
    if slot_pos + 2 > PAGE_SIZE {
        return Err(BTreeError::Corrupted("slot array overflows page".into()));
    }
    data[slot_pos..slot_pos + 2].copy_from_slice(&cell_offset.to_le_bytes());
    Ok(())
}

#[inline]
fn leaf_slots_end(page: &Page) -> usize {
    LEAF_SLOT_ARRAY_OFFSET + (page.cell_count() as usize) * 2
}

#[inline]
fn leaf_cells_start(page: &Page) -> usize {
    let end = page.free_end() as usize;
    if end == 0 {
        PAGE_SIZE
    } else {
        end
    }
}

#[inline]
fn leaf_free_bytes(page: &Page) -> usize {
    let slot_end = leaf_slots_end(page);
    let cells = leaf_cells_start(page);
    cells.saturating_sub(slot_end)
}

fn interior_key_size(key: &[u8]) -> usize {
    2 + key.len() + 4
}

fn interior_entries_size(keys: &[Vec<u8>]) -> usize {
    keys.iter().map(|k| interior_key_size(k)).sum()
}

// ==================== Leaf Page Helpers ====================

/// Read the cell at slot `index` in O(1). Follows the u16 slot pointer
/// into the cell data area; the cell header is `[key_len:u16][val_len:u16]`
/// followed by the raw key and value bytes.
fn read_leaf_cell(page: &Page, index: usize) -> BTreeResult<(Vec<u8>, Vec<u8>)> {
    let cell_count = page.cell_count() as usize;
    if index >= cell_count {
        return Err(BTreeError::Corrupted("Cell index out of range".into()));
    }
    let offset = leaf_read_slot(page, index)?;
    let data = page.as_bytes();
    if offset + 4 > PAGE_SIZE {
        return Err(BTreeError::Corrupted("cell header out of range".into()));
    }
    let key_len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
    let val_len = u16::from_le_bytes([data[offset + 2], data[offset + 3]]) as usize;
    let end = offset + 4 + key_len + val_len;
    if end > PAGE_SIZE {
        return Err(BTreeError::Corrupted("cell body out of range".into()));
    }
    let key = data[offset + 4..offset + 4 + key_len].to_vec();
    let value = data[offset + 4 + key_len..end].to_vec();
    Ok((key, value))
}

fn read_leaf_entries(page: &Page) -> BTreeResult<Vec<(Vec<u8>, Vec<u8>)>> {
    let cell_count = page.cell_count() as usize;
    let mut entries = Vec::with_capacity(cell_count);
    for i in 0..cell_count {
        entries.push(read_leaf_cell(page, i)?);
    }
    Ok(entries)
}

/// Wipe and re-lay an entire leaf in slotted form. Used by the split
/// path (which hands us a pre-sorted `entries` vector) and by
/// `clear_leaf_cells` under the hood. Entries are appended at the end
/// of the free area so the highest-index slot ends up at the lowest
/// offset — same shape the single-key insert path produces.
fn write_leaf_entries(page: &mut Page, entries: &[(Vec<u8>, Vec<u8>)]) -> BTreeResult<()> {
    clear_leaf_cells(page);
    for (i, (k, v)) in entries.iter().enumerate() {
        let cell_size = 4 + k.len() + v.len();
        let cells_start = leaf_cells_start(page);
        let slot_end = LEAF_SLOT_ARRAY_OFFSET + (i + 1) * 2;
        if slot_end + cell_size > cells_start {
            return Err(BTreeError::Corrupted("leaf rebuild overflowed page".into()));
        }
        let cell_offset = cells_start - cell_size;
        {
            let data = page.as_bytes_mut();
            data[cell_offset..cell_offset + 2].copy_from_slice(&(k.len() as u16).to_le_bytes());
            data[cell_offset + 2..cell_offset + 4].copy_from_slice(&(v.len() as u16).to_le_bytes());
            data[cell_offset + 4..cell_offset + 4 + k.len()].copy_from_slice(k);
            data[cell_offset + 4 + k.len()..cell_offset + 4 + k.len() + v.len()].copy_from_slice(v);
        }
        page.set_free_end(cell_offset as u16);
        leaf_write_slot(page, i, cell_offset as u16)?;
    }
    page.set_cell_count(entries.len() as u16);
    page.set_free_start((LEAF_SLOT_ARRAY_OFFSET + entries.len() * 2) as u16);
    Ok(())
}

fn can_insert_leaf(page: &Page, key: &[u8], value: &[u8]) -> bool {
    let needed = 2 + 4 + key.len() + value.len();
    leaf_free_bytes(page) >= needed
}

/// Insert `(key, value)` into the slotted leaf in O(log M) search +
/// O(M) slot-array memmove. Cell data is appended at the tail of the
/// free area (backward from `free_end`); the slot pointer is inserted
/// at the sorted position.
fn insert_into_leaf(page: &mut Page, key: &[u8], value: &[u8]) -> BTreeResult<()> {
    // 1. Binary search the slot array to find the insertion index.
    let cell_count = page.cell_count() as usize;
    let mut low = 0;
    let mut high = cell_count;
    while low < high {
        let mid = (low + high) / 2;
        let (cell_key, _) = read_leaf_cell(page, mid)?;
        match cell_key.as_slice().cmp(key) {
            Ordering::Less => low = mid + 1,
            Ordering::Greater => high = mid,
            Ordering::Equal => {
                // Duplicate keys are tolerated by the B-tree (caller
                // decides semantics); append after the existing run.
                low = mid + 1;
                break;
            }
        }
    }
    let insert_pos = low;

    // 2. Reserve the cell at the tail of the free area.
    let cell_size = 4 + key.len() + value.len();
    let slot_end_after = LEAF_SLOT_ARRAY_OFFSET + (cell_count + 1) * 2;
    let cells_start = leaf_cells_start(page);
    if slot_end_after + cell_size > cells_start {
        return Err(BTreeError::Corrupted("leaf page full".into()));
    }
    let cell_offset = cells_start - cell_size;
    {
        let data = page.as_bytes_mut();
        data[cell_offset..cell_offset + 2].copy_from_slice(&(key.len() as u16).to_le_bytes());
        data[cell_offset + 2..cell_offset + 4].copy_from_slice(&(value.len() as u16).to_le_bytes());
        data[cell_offset + 4..cell_offset + 4 + key.len()].copy_from_slice(key);
        data[cell_offset + 4 + key.len()..cell_offset + 4 + key.len() + value.len()]
            .copy_from_slice(value);
    }
    page.set_free_end(cell_offset as u16);

    // 3. Shift the slot-array tail right by one slot, then write the
    //    new pointer into the freed slot. This is a single memmove on
    //    a couple dozen u16s — far cheaper than the O(M²) rebuild.
    {
        let shift_from = leaf_slot_offset_for(insert_pos);
        let shift_to = shift_from + 2;
        let shift_len = (cell_count - insert_pos) * 2;
        if shift_len > 0 {
            let data = page.as_bytes_mut();
            data.copy_within(shift_from..shift_from + shift_len, shift_to);
        }
    }
    leaf_write_slot(page, insert_pos, cell_offset as u16)?;

    // 4. Bump counters.
    page.set_cell_count((cell_count + 1) as u16);
    page.set_free_start((LEAF_SLOT_ARRAY_OFFSET + (cell_count + 1) * 2) as u16);
    Ok(())
}

/// Remove the slot at `index`. Cell bytes are left in place (lazy
/// compaction); the slot-array tail is shifted left to close the gap.
/// The caller is expected to call `clear_leaf_cells` + rebuild if the
/// page wants its free space reclaimed.
fn delete_from_leaf(page: &mut Page, index: usize) -> BTreeResult<()> {
    let cell_count = page.cell_count() as usize;
    if index >= cell_count {
        return Err(BTreeError::Corrupted("delete index out of range".into()));
    }
    if index + 1 < cell_count {
        let shift_from = leaf_slot_offset_for(index + 1);
        let shift_to = leaf_slot_offset_for(index);
        let shift_len = (cell_count - index - 1) * 2;
        let data = page.as_bytes_mut();
        data.copy_within(shift_from..shift_from + shift_len, shift_to);
    }
    page.set_cell_count((cell_count - 1) as u16);
    page.set_free_start((LEAF_SLOT_ARRAY_OFFSET + (cell_count - 1) * 2) as u16);
    Ok(())
}

/// Reset the leaf to an empty slotted state — cell count 0, free
/// cursors at the extremes. Zeroes the slot array + cell data area so
/// stale bytes never leak through a corrupted read.
fn clear_leaf_cells(page: &mut Page) {
    {
        let data = page.as_bytes_mut();
        for byte in &mut data[LEAF_SLOT_ARRAY_OFFSET..] {
            *byte = 0;
        }
    }
    page.set_cell_count(0);
    page.set_free_start(LEAF_SLOT_ARRAY_OFFSET as u16);
    page.set_free_end(PAGE_SIZE as u16);
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
//
// Slotted-page layout mirroring the leaf format:
//
// ```text
// [PageHeader 32B]
// [slot 0: u16][slot 1: u16]...[slot N-1: u16]  ← grows forward
// ... free space ...
// [cell N-1][cell N-2]...[cell 0]               ← grows backward from free_end
// ```
//
// Each cell is `[key_len:u16][key][child:u32]`. The right-most child
// pointer lives in `PageHeader.right_child` exactly as before.

#[inline]
fn interior_slot_offset_for(index: usize) -> usize {
    INTERIOR_DATA_OFFSET + index * 2
}

#[inline]
fn interior_read_slot(page: &Page, index: usize) -> BTreeResult<usize> {
    let data = page.as_bytes();
    let slot_pos = interior_slot_offset_for(index);
    if slot_pos + 2 > PAGE_SIZE {
        return Err(BTreeError::Corrupted(
            "interior slot array overflows page".into(),
        ));
    }
    Ok(u16::from_le_bytes([data[slot_pos], data[slot_pos + 1]]) as usize)
}

#[inline]
fn interior_write_slot(page: &mut Page, index: usize, cell_offset: u16) -> BTreeResult<()> {
    let data = page.as_bytes_mut();
    let slot_pos = interior_slot_offset_for(index);
    if slot_pos + 2 > PAGE_SIZE {
        return Err(BTreeError::Corrupted(
            "interior slot array overflows page".into(),
        ));
    }
    data[slot_pos..slot_pos + 2].copy_from_slice(&cell_offset.to_le_bytes());
    Ok(())
}

#[inline]
fn interior_cells_start(page: &Page) -> usize {
    let end = page.free_end() as usize;
    if end == 0 {
        PAGE_SIZE
    } else {
        end
    }
}

#[inline]
fn interior_free_bytes(page: &Page) -> usize {
    let slot_end = INTERIOR_DATA_OFFSET + (page.cell_count() as usize) * 2;
    interior_cells_start(page).saturating_sub(slot_end)
}

fn read_interior_cell(page: &Page, index: usize) -> BTreeResult<(Vec<u8>, u32)> {
    let cell_count = page.cell_count() as usize;
    if index >= cell_count {
        return Err(BTreeError::Corrupted("Cell index out of range".into()));
    }
    let offset = interior_read_slot(page, index)?;
    let data = page.as_bytes();
    if offset + 2 > PAGE_SIZE {
        return Err(BTreeError::Corrupted(
            "interior cell header out of range".into(),
        ));
    }
    let key_len = u16::from_le_bytes([data[offset], data[offset + 1]]) as usize;
    let end = offset + 2 + key_len + 4;
    if end > PAGE_SIZE {
        return Err(BTreeError::Corrupted(
            "interior cell body out of range".into(),
        ));
    }
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

/// Bulk-write an interior page from scratch in slotted form. Used by
/// the split path. `keys` and `children` follow the B+ tree contract:
/// `children.len() == keys.len() + 1`, with the last child landing in
/// `PageHeader.right_child` (as before).
fn write_interior_entries(page: &mut Page, keys: &[Vec<u8>], children: &[u32]) -> BTreeResult<()> {
    if !keys.is_empty() && children.len() != keys.len() + 1 {
        return Err(BTreeError::Corrupted(
            "Interior keys/children length mismatch".into(),
        ));
    }

    clear_interior_cells(page);
    if keys.is_empty() {
        let right_child = children.first().copied().unwrap_or(0);
        page.set_right_child(right_child);
        return Ok(());
    }

    for (i, key) in keys.iter().enumerate() {
        let cell_size = 2 + key.len() + 4;
        let cells_start = interior_cells_start(page);
        let slot_end = INTERIOR_DATA_OFFSET + (i + 1) * 2;
        if slot_end + cell_size > cells_start {
            return Err(BTreeError::Corrupted(
                "interior rebuild overflowed page".into(),
            ));
        }
        let cell_offset = cells_start - cell_size;
        {
            let data = page.as_bytes_mut();
            data[cell_offset..cell_offset + 2].copy_from_slice(&(key.len() as u16).to_le_bytes());
            data[cell_offset + 2..cell_offset + 2 + key.len()].copy_from_slice(key);
            data[cell_offset + 2 + key.len()..cell_offset + 2 + key.len() + 4]
                .copy_from_slice(&children[i].to_le_bytes());
        }
        page.set_free_end(cell_offset as u16);
        interior_write_slot(page, i, cell_offset as u16)?;
    }
    page.set_cell_count(keys.len() as u16);
    page.set_free_start((INTERIOR_DATA_OFFSET + keys.len() * 2) as u16);
    page.set_right_child(*children.last().ok_or_else(|| {
        BTreeError::Corrupted("write_interior_entries: children empty with non-empty keys".into())
    })?);
    Ok(())
}

fn can_insert_interior(page: &Page, key: &[u8]) -> bool {
    let needed = 2 + 2 + key.len() + 4;
    interior_free_bytes(page) >= needed
}

/// Insert a `(key, left_child)` separator into the interior page.
/// `right_child` replaces whatever child used to sit to the right of
/// the inserted key (either a middle child's pointer or the page's
/// `right_child` when the key is the new maximum).
fn insert_into_interior(
    page: &mut Page,
    key: &[u8],
    left_child: u32,
    right_child: u32,
) -> BTreeResult<()> {
    // 1. Binary search the slot array for the insert position.
    let cell_count = page.cell_count() as usize;
    let mut low = 0;
    let mut high = cell_count;
    while low < high {
        let mid = (low + high) / 2;
        let (cell_key, _) = read_interior_cell(page, mid)?;
        match cell_key.as_slice().cmp(key) {
            Ordering::Less => low = mid + 1,
            Ordering::Greater | Ordering::Equal => high = mid,
        }
    }
    let insert_pos = low;

    // 2. Redirect the previous owner of the split to `right_child`.
    //    If the insertion is at the tail, the previous owner was the
    //    page's right_child pointer; otherwise it was the child slot
    //    of the cell currently at `insert_pos`.
    if insert_pos < cell_count {
        let old_offset = interior_read_slot(page, insert_pos)?;
        let data = page.as_bytes();
        let key_len = u16::from_le_bytes([data[old_offset], data[old_offset + 1]]) as usize;
        let child_pos = old_offset + 2 + key_len;
        let data = page.as_bytes_mut();
        data[child_pos..child_pos + 4].copy_from_slice(&right_child.to_le_bytes());
    } else {
        page.set_right_child(right_child);
    }

    // 3. Reserve the new cell at the tail of the free area.
    let cell_size = 2 + key.len() + 4;
    let slot_end_after = INTERIOR_DATA_OFFSET + (cell_count + 1) * 2;
    let cells_start = interior_cells_start(page);
    if slot_end_after + cell_size > cells_start {
        return Err(BTreeError::Corrupted("interior page full".into()));
    }
    let cell_offset = cells_start - cell_size;
    {
        let data = page.as_bytes_mut();
        data[cell_offset..cell_offset + 2].copy_from_slice(&(key.len() as u16).to_le_bytes());
        data[cell_offset + 2..cell_offset + 2 + key.len()].copy_from_slice(key);
        data[cell_offset + 2 + key.len()..cell_offset + 2 + key.len() + 4]
            .copy_from_slice(&left_child.to_le_bytes());
    }
    page.set_free_end(cell_offset as u16);

    // 4. Shift the slot-array tail right by one slot, write the new
    //    pointer into the freed slot, bump counters.
    {
        let shift_from = interior_slot_offset_for(insert_pos);
        let shift_to = shift_from + 2;
        let shift_len = (cell_count - insert_pos) * 2;
        if shift_len > 0 {
            let data = page.as_bytes_mut();
            data.copy_within(shift_from..shift_from + shift_len, shift_to);
        }
    }
    interior_write_slot(page, insert_pos, cell_offset as u16)?;
    page.set_cell_count((cell_count + 1) as u16);
    page.set_free_start((INTERIOR_DATA_OFFSET + (cell_count + 1) * 2) as u16);
    Ok(())
}

fn clear_interior_cells(page: &mut Page) {
    {
        let data = page.as_bytes_mut();
        for byte in &mut data[INTERIOR_DATA_OFFSET..] {
            *byte = 0;
        }
    }
    page.set_cell_count(0);
    page.set_free_start(INTERIOR_DATA_OFFSET as u16);
    page.set_free_end(PAGE_SIZE as u16);
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
