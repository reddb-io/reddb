//! B+ Tree Implementation for Page-Based Storage
//!
//! A B+ tree optimized for disk I/O with the following properties:
//! - All values stored in leaf nodes
//! - Interior nodes only contain keys and child pointers
//! - Leaf nodes form a doubly-linked list for range scans
//! - Each node fits in a single 16 KiB page
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
use super::page_store::PageStore;
use super::pager::PagerError;

/// Maximum key size (to ensure at least 2 keys per node)
pub const MAX_KEY_SIZE: usize = 1024;

/// Hard upper bound on logical value size.
///
/// Per ADR 0023 (large-value overflow), values up to 256 MiB are stored
/// transparently — inline below [`value_layout::OVERFLOW_THRESHOLD`],
/// inline+compressed when LZ4 shrinks them under the same threshold, or
/// spilled into an [`overflow::OverflowChain`] otherwise. Values above
/// this ceiling are rejected with [`BTreeError::ValueTooLarge`] before
/// any compression or page allocation runs.
///
/// Re-exported from [`value_layout::MAX_VALUE_SIZE`] so callers that
/// historically read this constant (`unified::store::impl_pages`,
/// `query::planner::stats_catalog`) continue to compile.
pub const MAX_VALUE_SIZE: usize = value_layout::MAX_VALUE_SIZE;

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

// A split must still fit two maximum-key inline cells, including the value
// flag, cell headers and slot pointers, after the native page-size change.
const _: () = assert!(
    2 * (MAX_KEY_SIZE + value_layout::OVERFLOW_THRESHOLD + 1 + 4 + 2)
        <= PAGE_SIZE - LEAF_SLOT_ARRAY_OFFSET
);

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
    /// Large-value layout encode/decode failed (overflow chain or LZ4).
    ValueLayout(String),
    /// Materializing a value would exceed the caller's read memory allowance.
    ReadLimitExceeded {
        required_bytes: usize,
        limit_bytes: usize,
    },
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
            Self::ValueLayout(msg) => write!(f, "Value layout: {}", msg),
            Self::ReadLimitExceeded {
                required_bytes,
                limit_bytes,
            } => write!(
                f,
                "B-tree read needs {required_bytes} bytes over read memory limit {limit_bytes}"
            ),
        }
    }
}

impl From<value_layout::ValueLayoutError> for BTreeError {
    fn from(e: value_layout::ValueLayoutError) -> Self {
        match e {
            value_layout::ValueLayoutError::ValueTooLarge(n) => Self::ValueTooLarge(n),
            value_layout::ValueLayoutError::ReadLimitExceeded {
                required_bytes,
                limit_bytes,
            } => Self::ReadLimitExceeded {
                required_bytes,
                limit_bytes,
            },
            other => Self::ValueLayout(other.to_string()),
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
pub struct BTreeCursor<P: PageStore> {
    /// A cursor can outlive the tree handle that created it.
    _retirement_pin: Arc<()>,
    /// Current leaf page ID
    leaf_page_id: u32,
    /// Current position within leaf
    position: usize,
    /// Pager reference
    pager: Arc<P>,
    /// A6 — true once we've issued a prefetch hint for the next leaf on
    /// this leaf. Reset to false every time we move to a new leaf so the
    /// hint fires once per leaf, at the halfway point.
    prefetched_next: bool,
    /// Published collection generations are immutable. Retain at most one
    /// leaf while walking them; mutable-tree cursors keep re-reading pages.
    immutable: bool,
    immutable_leaf: Option<Box<Page>>,
}

impl<P: PageStore> BTreeCursor<P> {
    /// Only collection readers whose host pins an immutable published root
    /// may retain a leaf across calls. This is not a mutable-tree snapshot.
    pub(crate) fn with_immutable_leaves(mut self) -> Self {
        self.immutable = true;
        self
    }

    fn prepare_immutable_leaf(&mut self) -> BTreeResult<bool> {
        if self.leaf_page_id == 0 {
            return Ok(false);
        }
        if self.immutable_leaf.is_none() {
            let page = self.pager.read_page(self.leaf_page_id)?;
            if page.page_type()? != PageType::BTreeLeaf {
                return Err(BTreeError::Corrupted(
                    "cursor reached a non-leaf page".into(),
                ));
            }
            self.immutable_leaf = Some(Box::new(page));
        }
        let page = self.immutable_leaf.as_ref().expect("loaded immutable leaf");
        if self.position < usize::from(page.cell_count()) {
            return Ok(true);
        }
        let next = read_next_leaf(page);
        if next == 0 {
            self.leaf_page_id = 0;
            self.immutable_leaf = None;
            return Ok(false);
        }
        let next_page = self.pager.read_page(next)?;
        if next_page.page_type()? != PageType::BTreeLeaf || next_page.cell_count() == 0 {
            return Err(BTreeError::Corrupted(
                "leaf chain reached an invalid leaf".into(),
            ));
        }
        let first_key = read_leaf_cell_borrowed(&next_page, 0)?.0;
        if leaf_high_key(page)?
            .as_ref()
            .is_some_and(|key| first_key <= key.as_slice())
        {
            return Err(BTreeError::Corrupted(
                "leaf chain does not advance its key range".into(),
            ));
        }
        **self.immutable_leaf.as_mut().expect("loaded immutable leaf") = next_page;
        self.leaf_page_id = next;
        self.position = 0;
        self.prefetched_next = false;
        Ok(true)
    }

    /// Inspect the next key without materializing its value or advancing.
    pub fn peek_key(&mut self) -> BTreeResult<Option<Vec<u8>>> {
        if self.immutable {
            if !self.prepare_immutable_leaf()? {
                return Ok(None);
            }
            let page = self.immutable_leaf.as_ref().expect("loaded immutable leaf");
            return Ok(Some(
                read_leaf_cell_borrowed(page, self.position)?.0.to_vec(),
            ));
        }
        let Some(page) = self.prepare_next_leaf()? else {
            return Ok(None);
        };
        Ok(Some(read_leaf_cell(&page, self.position)?.0))
    }

    /// Skip a shadowed or deleted base record without reading its overflow value.
    pub fn skip_next(&mut self) -> BTreeResult<bool> {
        if self.immutable {
            if !self.prepare_immutable_leaf()? {
                return Ok(false);
            }
            self.position += 1;
            return Ok(true);
        }
        if self.prepare_next_leaf()?.is_none() {
            return Ok(false);
        }
        self.position += 1;
        Ok(true)
    }

    fn prepare_next_leaf(&mut self) -> BTreeResult<Option<Page>> {
        if self.leaf_page_id == 0 {
            return Ok(None);
        }
        let page = self.pager.read_page(self.leaf_page_id)?;
        if page.page_type()? != PageType::BTreeLeaf {
            return Err(BTreeError::Corrupted(
                "cursor reached a non-leaf page".into(),
            ));
        }
        if self.position < usize::from(page.cell_count()) {
            return Ok(Some(page));
        }
        let next = read_next_leaf(&page);
        if next == 0 {
            self.leaf_page_id = 0;
            return Ok(None);
        }
        let next_page = self.pager.read_page(next)?;
        if next_page.page_type()? != PageType::BTreeLeaf || next_page.cell_count() == 0 {
            return Err(BTreeError::Corrupted(
                "leaf chain reached an invalid leaf".into(),
            ));
        }
        let first_key = read_leaf_cell(&next_page, 0)?.0;
        if leaf_high_key(&page)?
            .as_ref()
            .is_some_and(|key| first_key <= *key)
        {
            return Err(BTreeError::Corrupted(
                "leaf chain does not advance its key range".into(),
            ));
        }
        self.leaf_page_id = next;
        self.position = 0;
        self.prefetched_next = false;
        Ok(Some(next_page))
    }

    /// Read one value within the caller's materialization allowance.
    pub fn next_with_budget(
        &mut self,
        limit_bytes: usize,
    ) -> BTreeResult<Option<(Vec<u8>, Vec<u8>)>> {
        self.next_impl(limit_bytes)
    }

    /// Move to next entry
    pub fn next(&mut self) -> BTreeResult<Option<(Vec<u8>, Vec<u8>)>> {
        self.next_impl(usize::MAX)
    }

    fn next_impl(&mut self, limit_bytes: usize) -> BTreeResult<Option<(Vec<u8>, Vec<u8>)>> {
        if self.immutable {
            return self.next_immutable(limit_bytes);
        }
        if self.leaf_page_id == 0 {
            return Ok(None);
        }

        let page = self.pager.read_page(self.leaf_page_id)?;
        if page.page_type()? != PageType::BTreeLeaf {
            return Err(BTreeError::Corrupted(
                "cursor reached a non-leaf page".into(),
            ));
        }
        let cell_count = page.cell_count() as usize;

        // A6 — prefetch the next leaf when we cross the halfway point of
        // the current one. The hint fires at most once per leaf (guarded
        // by `prefetched_next`) so the syscall overhead is amortized over
        // ~half a leaf's worth of entries. The kernel uses the hint to
        // start DMA-ing the next page while we finish the current one.
        if !self.prefetched_next && self.position >= cell_count / 2 && cell_count > 0 {
            let next_leaf = read_next_leaf(&page);
            if next_leaf != 0 {
                self.pager.prefetch_hint(next_leaf);
            }
            self.prefetched_next = true;
        }

        // Check if we have more cells in current page
        if self.position < cell_count {
            let (key, stored) = read_leaf_cell(&page, self.position)?;
            let value =
                value_layout::decode_with_budget(self.pager.as_ref(), &stored, limit_bytes)?;
            self.position += 1;
            return Ok(Some((key, value)));
        }

        // Move to next leaf
        let next_leaf = read_next_leaf(&page);
        if next_leaf == 0 {
            self.leaf_page_id = 0;
            return Ok(None);
        }
        let previous_high_key = leaf_high_key(&page)?;

        self.leaf_page_id = next_leaf;
        self.position = 0;
        self.prefetched_next = false; // reset for the new leaf

        // Read from new leaf
        let page = self.pager.read_page(self.leaf_page_id)?;
        if page.page_type()? != PageType::BTreeLeaf {
            return Err(BTreeError::Corrupted(
                "leaf chain reached a non-leaf page".into(),
            ));
        }
        if page.cell_count() == 0 {
            return Ok(None);
        }

        let (key, stored) = read_leaf_cell(&page, 0)?;
        // Every leaf transition must advance the key range. This bounds a
        // corrupt cyclic chain without growing a visited-page set or an
        // unbounded checkpoint buffer (#2376).
        if previous_high_key
            .as_ref()
            .is_some_and(|previous| key <= *previous)
        {
            self.leaf_page_id = 0;
            return Err(BTreeError::Corrupted(
                "leaf chain does not advance its key range".into(),
            ));
        }
        let value = value_layout::decode_with_budget(self.pager.as_ref(), &stored, limit_bytes)?;
        self.position = 1;
        Ok(Some((key, value)))
    }

    fn next_immutable(&mut self, limit_bytes: usize) -> BTreeResult<Option<(Vec<u8>, Vec<u8>)>> {
        if !self.prepare_immutable_leaf()? {
            return Ok(None);
        }
        let page = self.immutable_leaf.as_ref().expect("loaded immutable leaf");
        if !self.prefetched_next && self.position >= usize::from(page.cell_count()) / 2 {
            let next = read_next_leaf(page);
            if next != 0 {
                self.pager.prefetch_hint(next);
            }
            self.prefetched_next = true;
        }
        let (key, stored) = read_leaf_cell_borrowed(page, self.position)?;
        let value = value_layout::decode_with_budget(self.pager.as_ref(), stored, limit_bytes)?;
        self.position += 1;
        Ok(Some((key.to_vec(), value)))
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

        let (key, stored) = read_leaf_cell(&page, self.position)?;
        let value = value_layout::decode(self.pager.as_ref(), &stored)?;
        Ok(Some((key, value)))
    }
}

/// B+ Tree implementation
pub struct BTree<P: PageStore> {
    retirement_pin: Arc<()>,
    /// Pager for page I/O
    pager: Arc<P>,
    /// Root page ID (0 = empty tree)
    root_page_id: RwLock<u32>,
    /// D2 fastpath: cached rightmost leaf (page_id, high_key).
    ///
    /// When the next key to insert is strictly greater than `high_key`,
    /// skip `find_leaf` (tree descent from root) and go directly to the
    /// cached page. Invalidated on splits and deletes.
    ///
    /// Mirrors `BTPageCacheIsValid` / `BTREE_FASTPATH_MIN_LEVEL` in
    /// `nbtinsert.c:31`.
    rightmost_leaf: RwLock<Option<(u32, Vec<u8>)>>,
    /// Serialises structural writers (insert / upsert / delete / bulk).
    ///
    /// Writers copy a page, modify the copy and write it back, and there
    /// are no page latches, so two writers on one tree would lose each
    /// other's leaf updates or tear a split (#2376). Reentrant because the
    /// public mutators call each other (`upsert` -> `insert`, the bulk
    /// paths -> `insert_encoded`). Readers do not take it.
    writer: parking_lot::ReentrantMutex<()>,
}

#[path = "btree/impl.rs"]
mod btree_impl;

#[path = "btree/reclaim.rs"]
mod reclaim;

#[path = "btree/lehman_yao.rs"]
pub mod lehman_yao;

#[path = "btree/value_layout.rs"]
pub mod value_layout;
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
    let (key, value) = read_leaf_cell_borrowed(page, index)?;
    Ok((key.to_vec(), value.to_vec()))
}

fn read_leaf_cell_borrowed(page: &Page, index: usize) -> BTreeResult<(&[u8], &[u8])> {
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
    let key = &data[offset + 4..offset + 4 + key_len];
    let value = &data[offset + 4 + key_len..end];
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
/// Overwrite the value portion of cell at `index` in-place. Valid when
/// `new_value.len() <= existing_value.len()` — the caller
/// (typically `BTree::upsert`) checks this before calling.
///
/// Same-length writes are trivial (byte copy). Shrinks update the cell's
/// `v_len` header and leave the trailing bytes as dead space until the
/// page is naturally rewritten; the slotted layout keeps cells
/// addressable by their own offset+len so the gap is harmless.
///
/// Fixed-size column updates (SET score = X on a typed schema) hit this
/// path, avoiding the delete + re-insert + rebalance cycle that
/// dominates bulk_update.
pub(crate) fn overwrite_leaf_value(
    page: &mut Page,
    index: usize,
    new_value: &[u8],
) -> BTreeResult<()> {
    let cell_offset = leaf_read_slot(page, index)?;
    let data = page.as_bytes();
    if cell_offset + 4 > data.len() {
        return Err(BTreeError::Corrupted(
            "leaf cell header out of bounds".into(),
        ));
    }
    let k_len = u16::from_le_bytes([data[cell_offset], data[cell_offset + 1]]) as usize;
    let old_v_len = u16::from_le_bytes([data[cell_offset + 2], data[cell_offset + 3]]) as usize;
    if new_value.len() > old_v_len {
        return Err(BTreeError::Corrupted(
            "overwrite_leaf_value: new value larger than slot".into(),
        ));
    }
    let value_start = cell_offset + 4 + k_len;
    if value_start + new_value.len() > data.len() {
        return Err(BTreeError::Corrupted(
            "leaf cell value out of bounds".into(),
        ));
    }
    let data = page.as_bytes_mut();
    // Update the v_len header if the value shrank. read_leaf_cell reads
    // v_len from this header so the shortened value deserializes correctly.
    if new_value.len() != old_v_len {
        let new_len = new_value.len() as u16;
        data[cell_offset + 2..cell_offset + 4].copy_from_slice(&new_len.to_le_bytes());
    }
    data[value_start..value_start + new_value.len()].copy_from_slice(new_value);
    Ok(())
}

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

/// Returns the right-sibling page ID for a leaf page.
/// 0 means this is the rightmost leaf — used by the D2 fastpath cache.
#[inline]
fn leaf_right_sibling(page: &Page) -> u32 {
    read_next_leaf(page)
}

/// Returns the maximum (last) key stored in a leaf page, or `None` if empty.
/// This is the Lehman-Yao "high_key" — any key > this value belongs on a
/// right-sibling page. Reads only the last slot so it is O(1).
fn leaf_high_key(page: &Page) -> BTreeResult<Option<Vec<u8>>> {
    let cell_count = page.cell_count() as usize;
    if cell_count == 0 {
        return Ok(None);
    }
    let (key, _) = read_leaf_cell(page, cell_count - 1)?;
    Ok(Some(key))
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
