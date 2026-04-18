use super::*;

impl BTree {
    /// Create a new B+ tree using the given pager
    pub fn new(pager: Arc<Pager>) -> Self {
        Self {
            pager,
            root_page_id: RwLock::new(0),
            rightmost_leaf: RwLock::new(None),
        }
    }

    /// Create a B+ tree with an existing root
    pub fn with_root(pager: Arc<Pager>, root_page_id: u32) -> Self {
        Self {
            pager,
            root_page_id: RwLock::new(root_page_id),
            rightmost_leaf: RwLock::new(None),
        }
    }

    /// Get the root page ID
    pub fn root_page_id(&self) -> u32 {
        *self
            .root_page_id
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Check if tree is empty
    pub fn is_empty(&self) -> bool {
        self.root_page_id() == 0
    }

    /// Get value for a key
    pub fn get(&self, key: &[u8]) -> BTreeResult<Option<Vec<u8>>> {
        let root_id = self.root_page_id();
        if root_id == 0 {
            return Ok(None);
        }

        // Descend from root to the target leaf
        let (leaf_id, _) = self.find_leaf(root_id, key)?;

        // D3 — Lehman-Yao right-link traversal.
        //
        // A concurrent insert may have split the leaf between our descent
        // and this read. If the key we're searching for is greater than the
        // current page's high_key (max stored key), the key lives on a
        // right-sibling page — follow the right-link instead of re-descending
        // from the root. Cap at MAX_RIGHTLINK_HOPS to guard against corrupt
        // link chains.
        const MAX_RIGHTLINK_HOPS: usize = 32;
        let mut current_id = leaf_id;
        for _ in 0..MAX_RIGHTLINK_HOPS {
            let page = self.pager.read_page(current_id)?;

            match search_leaf(&page, key)? {
                SearchResult::Found(pos) => {
                    let (_, value) = read_leaf_cell(&page, pos)?;
                    return Ok(Some(value));
                }
                SearchResult::NotFound(_) => {
                    // Check if a split pushed our key to the right sibling
                    let right = leaf_right_sibling(&page);
                    if right != 0 {
                        if let Some(high_key) = leaf_high_key(&page)? {
                            if key > high_key.as_slice() {
                                // Key belongs on a right sibling — follow link
                                current_id = right;
                                continue;
                            }
                        }
                    }
                    // Key genuinely not present
                    return Ok(None);
                }
            }
        }

        // Exhausted hop limit — fall back to root descent (corrupt link chain)
        let (leaf_id, _) = self.find_leaf(self.root_page_id(), key)?;
        let page = self.pager.read_page(leaf_id)?;
        match search_leaf(&page, key)? {
            SearchResult::Found(pos) => {
                let (_, value) = read_leaf_cell(&page, pos)?;
                Ok(Some(value))
            }
            SearchResult::NotFound(_) => Ok(None),
        }
    }

    /// Insert a key-value pair
    pub fn insert(&self, key: &[u8], value: &[u8]) -> BTreeResult<()> {
        // Validate sizes
        if key.len() > MAX_KEY_SIZE {
            return Err(BTreeError::KeyTooLarge(key.len()));
        }
        if value.len() > MAX_VALUE_SIZE {
            return Err(BTreeError::ValueTooLarge(value.len()));
        }

        let root_id = self.root_page_id();

        // Empty tree - create root leaf
        if root_id == 0 {
            let mut page = self.pager.allocate_page(PageType::BTreeLeaf)?;
            clear_leaf_cells(&mut page);
            insert_into_leaf(&mut page, key, value)?;
            init_leaf_links(&mut page, 0, 0);
            page.update_checksum();
            let new_root = page.page_id();
            self.pager.write_page(new_root, page)?;
            *self.root_page_id.write().map_err(|e| {
                BTreeError::LockPoisoned(format!("insert: root_page_id write lock: {e}"))
            })? = new_root;
            return Ok(());
        }

        // D2 fastpath: if key > cached rightmost high_key, go directly to
        // the cached leaf without descending from the root. This cuts
        // tree descent entirely for monotonic append workloads (timeseries,
        // auto-increment entity IDs).
        let cached = {
            self.rightmost_leaf
                .read()
                .map_err(|e| BTreeError::LockPoisoned(format!("rightmost_leaf read: {e}")))?
                .clone()
        };
        if let Some((cached_page_id, ref high_key)) = cached {
            if key > high_key.as_slice() {
                let mut page = self.pager.read_page(cached_page_id)?;
                // Confirm this page is still a valid leaf with a right-link
                // of 0 (i.e., it IS the rightmost leaf — not mid-tree after
                // a concurrent split moved the boundary).
                let right_sibling = leaf_right_sibling(&page);
                if right_sibling == 0 {
                    // Still the rightmost leaf. Try fast append.
                    if can_insert_leaf(&page, key, value) {
                        insert_into_leaf(&mut page, key, value)?;
                        // Skip update_checksum here — pager.write_page
                        // below recomputes it. Saves one CRC32 on 4KB
                        // per insert on the hot append path.
                        let page_id = page.page_id();
                        // Update cached high_key
                        *self.rightmost_leaf.write().map_err(|e| {
                            BTreeError::LockPoisoned(format!("rightmost_leaf write: {e}"))
                        })? = Some((page_id, key.to_vec()));
                        self.pager.write_page(page_id, page)?;
                        return Ok(());
                    }
                    // Leaf is full — split. After split, invalidate cache
                    // (the new rightmost leaf is the split result).
                    let (new_leaf, separator_key) = self.split_leaf(&mut page, key, value)?;
                    let new_leaf_id = new_leaf.page_id();
                    page.update_checksum();
                    let page_id = page.page_id();
                    self.pager.write_page(page_id, page.clone())?;
                    // Cache the new rightmost leaf after split
                    *self.rightmost_leaf.write().map_err(|e| {
                        BTreeError::LockPoisoned(format!("rightmost_leaf write: {e}"))
                    })? = Some((new_leaf_id, key.to_vec()));
                    // Get parent path for the split propagation.
                    // The left child is the original cached leaf.
                    let (_, path) = self.find_leaf(root_id, &separator_key)?;
                    self.insert_into_parent(path, cached_page_id, &separator_key, new_leaf_id)?;
                    return Ok(());
                }
                // right_sibling != 0: cached page is no longer rightmost.
                // Fall through to the full find_leaf path below and
                // refresh the cache.
                *self.rightmost_leaf.write().map_err(|e| {
                    BTreeError::LockPoisoned(format!("rightmost_leaf write: {e}"))
                })? = None;
            }
        }

        // Find the leaf and path to it
        let (leaf_id, path) = self.find_leaf(root_id, key)?;
        let mut page = self.pager.read_page(leaf_id)?;

        // Check for duplicate
        if let SearchResult::Found(_) = search_leaf(&page, key)? {
            return Err(BTreeError::DuplicateKey);
        }

        // Try to insert into leaf
        if can_insert_leaf(&page, key, value) {
            insert_into_leaf(&mut page, key, value)?;
            // Skip update_checksum — pager.write_page recomputes it.
            let page_id = page.page_id();
            // If this is the rightmost leaf (right_sibling == 0), cache it.
            if leaf_right_sibling(&page) == 0 {
                *self.rightmost_leaf.write().map_err(|e| {
                    BTreeError::LockPoisoned(format!("rightmost_leaf write: {e}"))
                })? = Some((page_id, key.to_vec()));
            }
            self.pager.write_page(page_id, page)?;
            return Ok(());
        }

        // Need to split the leaf
        let (new_leaf, separator_key) = self.split_leaf(&mut page, key, value)?;
        let new_leaf_id = new_leaf.page_id();
        page.update_checksum();
        let page_id = page.page_id();
        self.pager.write_page(page_id, page.clone())?;
        // Cache the new rightmost leaf (split result gets the larger keys)
        *self
            .rightmost_leaf
            .write()
            .map_err(|e| BTreeError::LockPoisoned(format!("rightmost_leaf write: {e}")))? =
            Some((new_leaf_id, key.to_vec()));

        // Propagate split up the tree
        self.insert_into_parent(path, page_id, &separator_key, new_leaf_id)?;

        Ok(())
    }

    /// Insert or update a key. If the key already exists and the new
    /// value has the same length as the old, the value bytes are
    /// overwritten in place — no structural changes, no rebalance,
    /// one leaf page write.
    ///
    /// Falls back to `delete + insert` when the key is missing or the
    /// new value has a different length. Callers like
    /// `persist_entities_to_pager` that re-serialize a mutated entity
    /// with a fixed-width schema almost always hit the fast path,
    /// eliminating the `BTree::delete + rebalance` cost that previously
    /// dominated UPDATE workloads (~50% of `bulk_update` CPU).
    pub fn upsert(&self, key: &[u8], value: &[u8]) -> BTreeResult<()> {
        let root_id = self.root_page_id();
        if root_id == 0 {
            return self.insert(key, value);
        }

        let (leaf_id, _path) = self.find_leaf(root_id, key)?;
        let mut page = self.pager.read_page(leaf_id)?;

        if let SearchResult::Found(pos) = search_leaf(&page, key)? {
            let (_, old_value) = read_leaf_cell(&page, pos)?;
            if value.len() <= old_value.len() {
                // Same-length or shrink: update cell in place. Shrinks
                // leave a small gap of dead bytes until the page is
                // naturally rewritten (split / compact) — slotted layout
                // keeps correctness because each cell carries its own
                // length header.
                overwrite_leaf_value(&mut page, pos, value)?;
                let page_id = page.page_id();
                self.pager.write_page(page_id, page)?;
                return Ok(());
            }
            // Grow case: fall through to delete+insert for a proper
            // reallocation + potential rebalance.
            // Same key, different size — structural re-layout needed.
            // Delete + re-insert preserves ordering + handles rebalance.
            let _ = self.delete(key);
            return self.insert(key, value);
        }

        // Key is new — plain insert path.
        self.insert(key, value)
    }

    /// Bulk insert for sorted key-value pairs.
    ///
    /// Optimized for monotonically increasing keys (e.g. entity IDs):
    /// - Walks to the target leaf ONCE, then appends many entries
    ///   before re-walking.
    /// - Writes each leaf only once per batch (amortized over many inserts).
    ///
    /// Falls back to per-entity `insert` on splits.
    pub fn bulk_insert_sorted(&self, items: &[(Vec<u8>, Vec<u8>)]) -> BTreeResult<()> {
        if items.is_empty() {
            return Ok(());
        }
        for (key, value) in items {
            if key.len() > MAX_KEY_SIZE {
                return Err(BTreeError::KeyTooLarge(key.len()));
            }
            if value.len() > MAX_VALUE_SIZE {
                return Err(BTreeError::ValueTooLarge(value.len()));
            }
        }

        // Callers are expected to supply lex-sorted keys. This is the
        // case for the entity-insert path in `impl_entities.rs`, which
        // encodes u64 IDs as big-endian bytes — so sequential IDs are
        // monotonically increasing in lex order too, and the tail-append
        // fast path below triggers naturally.
        //
        // If a caller ever passes out-of-order keys, the fast path will
        // simply bail out on the first violation and delegate to the
        // generic slow path for that item.
        let items: Vec<(&[u8], &[u8])> = items
            .iter()
            .map(|(k, v)| (k.as_slice(), v.as_slice()))
            .collect();

        let mut i = 0;
        while i < items.len() {
            let root_id = self.root_page_id();
            if root_id == 0 {
                // Empty tree — use single insert to create root
                let (k, v) = items[i];
                self.insert(k, v)?;
                i += 1;
                continue;
            }

            // Walk to the leaf for the current key, load once.
            let (leaf_id, _path) = self.find_leaf(root_id, items[i].0)?;
            let mut page = self.pager.read_page(leaf_id)?;

            // Snapshot the leaf's last key (via the O(1) slot lookup)
            // and current free bytes. Both drive the append fast path
            // below. The old O(M) forward-walk that this block used to
            // do was made redundant by the slotted layout.
            let mut last_key_in_leaf: Option<Vec<u8>> = {
                let cell_count = page.cell_count() as usize;
                if cell_count == 0 {
                    None
                } else {
                    Some(read_leaf_cell(&page, cell_count - 1)?.0)
                }
            };

            // Fast path for monotonically-ascending keys: append new
            // cells at the tail of the cell-data area and push new
            // slot pointers onto the slot array. Every key must be
            // strictly greater than `last_key_in_leaf` and must still
            // fit in the leaf's free bytes.
            let mut inserted = 0usize;
            while i + inserted < items.len() {
                let (key, value) = items[i + inserted];

                // Strictly-ascending check, doubles as duplicate
                // detection without a leaf binary search.
                if let Some(lk) = &last_key_in_leaf {
                    match key.cmp(lk.as_slice()) {
                        Ordering::Greater => {}
                        Ordering::Equal => return Err(BTreeError::DuplicateKey),
                        Ordering::Less => break,
                    }
                }

                // Free-space check: account for every slot appended in this
                // batch even though `page.cell_count()` is only updated once
                // at the end. Otherwise we can overrun the slot array into the
                // cell area near the end of the page and persist bad offsets.
                let cell_size = 4 + key.len() + value.len();
                let slot_end_after =
                    LEAF_SLOT_ARRAY_OFFSET + (page.cell_count() as usize + inserted + 1) * 2;
                let cells_start = leaf_cells_start(&page);
                if slot_end_after + cell_size > cells_start {
                    break;
                }

                // Append the cell at the tail of the cell-data area
                // and push a new slot pointer. `page.cell_count()`
                // still reflects the state before this inserted batch,
                // so the new slot index is `cell_count + inserted`.
                let cell_offset = cells_start - cell_size;
                {
                    let data = page.as_bytes_mut();
                    data[cell_offset..cell_offset + 2]
                        .copy_from_slice(&(key.len() as u16).to_le_bytes());
                    data[cell_offset + 2..cell_offset + 4]
                        .copy_from_slice(&(value.len() as u16).to_le_bytes());
                    data[cell_offset + 4..cell_offset + 4 + key.len()].copy_from_slice(key);
                    data[cell_offset + 4 + key.len()..cell_offset + cell_size]
                        .copy_from_slice(value);
                }
                page.set_free_end(cell_offset as u16);

                let new_slot_index = page.cell_count() as usize + inserted;
                let slot_pos = leaf_slot_offset_for(new_slot_index);
                {
                    let data = page.as_bytes_mut();
                    data[slot_pos..slot_pos + 2]
                        .copy_from_slice(&(cell_offset as u16).to_le_bytes());
                }

                last_key_in_leaf = Some(key.to_vec());
                inserted += 1;
            }

            if inserted > 0 {
                let new_count = page.cell_count() as usize + inserted;
                page.set_cell_count(new_count as u16);
                page.set_free_start((LEAF_SLOT_ARRAY_OFFSET + new_count * 2) as u16);
                page.update_checksum();
                self.pager.write_page(leaf_id, page)?;
                i += inserted;
            } else {
                // Couldn't fit even one entry via the fast path —
                // either the leaf is full, or the next key belongs
                // in the middle of the leaf. Delegate to the generic
                // insert path which handles both splitting and
                // mid-leaf positioning.
                let (k, v) = items[i];
                self.insert(k, v)?;
                i += 1;
            }
        }

        Ok(())
    }

    /// Delete a key
    pub fn delete(&self, key: &[u8]) -> BTreeResult<bool> {
        let root_id = self.root_page_id();
        if root_id == 0 {
            return Ok(false);
        }

        // D3 — Lehman-Yao right-link traversal for delete.
        // A split may have moved the key to a right sibling between our
        // descent and the actual page read. Follow right-links before giving up.
        let (leaf_id, path) = self.find_leaf(root_id, key)?;
        let mut current_id = leaf_id;
        const MAX_RIGHTLINK_HOPS: usize = 32;
        for _ in 0..MAX_RIGHTLINK_HOPS {
            let mut page = self.pager.read_page(current_id)?;
            match search_leaf(&page, key)? {
                SearchResult::Found(pos) => {
                    // Found — delete from this page (may differ from leaf_id)
                    delete_from_leaf(&mut page, pos)?;
                    page.update_checksum();
                    let page_id = page.page_id();
                    self.pager.write_page(page_id, page.clone())?;

                    *self
                        .rightmost_leaf
                        .write()
                        .unwrap_or_else(|e| e.into_inner()) = None;

                    if page.cell_count() == 0 && page_id == root_id {
                        self.pager.free_page(root_id)?;
                        *self.root_page_id.write().map_err(|e| {
                            BTreeError::LockPoisoned(format!(
                                "delete: root_page_id write lock: {e}"
                            ))
                        })? = 0;
                    } else {
                        self.rebalance_leaf(current_id, path)?;
                    }
                    return Ok(true);
                }
                SearchResult::NotFound(_) => {
                    let right = leaf_right_sibling(&page);
                    if right != 0 {
                        if let Some(high_key) = leaf_high_key(&page)? {
                            if key > high_key.as_slice() {
                                current_id = right;
                                continue;
                            }
                        }
                    }
                    return Ok(false);
                }
            }
        }
        Ok(false)
    }

    /// Create a cursor starting at the first entry
    pub fn cursor_first(&self) -> BTreeResult<BTreeCursor> {
        let root_id = self.root_page_id();
        if root_id == 0 {
            return Ok(BTreeCursor {
                leaf_page_id: 0,
                position: 0,
                pager: self.pager.clone(),
                prefetched_next: false,
            });
        }

        // Find leftmost leaf
        let first_leaf = self.find_first_leaf(root_id)?;

        Ok(BTreeCursor {
            leaf_page_id: first_leaf,
            position: 0,
            pager: self.pager.clone(),
            prefetched_next: false,
        })
    }

    /// Create a cursor starting at or after the given key
    pub fn cursor_seek(&self, key: &[u8]) -> BTreeResult<BTreeCursor> {
        let root_id = self.root_page_id();
        if root_id == 0 {
            return Ok(BTreeCursor {
                leaf_page_id: 0,
                position: 0,
                pager: self.pager.clone(),
                prefetched_next: false,
            });
        }

        let (leaf_id, _) = self.find_leaf(root_id, key)?;
        let page = self.pager.read_page(leaf_id)?;

        let position = match search_leaf(&page, key)? {
            SearchResult::Found(pos) => pos,
            SearchResult::NotFound(pos) => pos,
        };

        Ok(BTreeCursor {
            leaf_page_id: leaf_id,
            position,
            pager: self.pager.clone(),
            prefetched_next: false,
        })
    }

    /// Range scan from start_key to end_key (inclusive)
    pub fn range(&self, start_key: &[u8], end_key: &[u8]) -> BTreeResult<Vec<(Vec<u8>, Vec<u8>)>> {
        let mut results = Vec::new();
        let mut cursor = self.cursor_seek(start_key)?;

        while let Some((key, value)) = cursor.next()? {
            if key.as_slice() > end_key {
                break;
            }
            results.push((key, value));
        }

        Ok(results)
    }

    /// Count entries in the tree
    pub fn count(&self) -> BTreeResult<usize> {
        let root_id = self.root_page_id();
        if root_id == 0 {
            return Ok(0);
        }

        let mut count = 0;
        let mut cursor = self.cursor_first()?;
        while cursor.next()?.is_some() {
            count += 1;
        }

        Ok(count)
    }

    // ==================== Internal Methods ====================

    /// Find the leaf page containing the key
    fn find_leaf(&self, page_id: u32, key: &[u8]) -> BTreeResult<(u32, Vec<u32>)> {
        let mut current_id = page_id;
        let mut path = Vec::new();

        loop {
            let page = self.pager.read_page(current_id)?;

            match page.page_type()? {
                PageType::BTreeLeaf => {
                    return Ok((current_id, path));
                }
                PageType::BTreeInterior => {
                    path.push(current_id);
                    current_id = find_child(&page, key)?;
                }
                other => {
                    return Err(BTreeError::Corrupted(format!(
                        "Unexpected page type in B-tree: {:?}",
                        other
                    )));
                }
            }
        }
    }

    /// Find the leftmost leaf page
    pub(crate) fn find_first_leaf(&self, page_id: u32) -> BTreeResult<u32> {
        let mut current_id = page_id;

        loop {
            let page = self.pager.read_page(current_id)?;

            match page.page_type()? {
                PageType::BTreeLeaf => return Ok(current_id),
                PageType::BTreeInterior => {
                    // Go to leftmost child
                    current_id = find_first_child(&page)?;
                }
                _ => {
                    return Err(BTreeError::Corrupted("Invalid page type".into()));
                }
            }
        }
    }

    /// Split a leaf page
    fn split_leaf(
        &self,
        page: &mut Page,
        new_key: &[u8],
        new_value: &[u8],
    ) -> BTreeResult<(Page, Vec<u8>)> {
        // Collect all entries including the new one
        let mut entries: Vec<(Vec<u8>, Vec<u8>)> = Vec::new();
        let cell_count = page.cell_count() as usize;

        for i in 0..cell_count {
            entries.push(read_leaf_cell(page, i)?);
        }

        // Insert new entry in sorted position
        let insert_pos = entries.partition_point(|(k, _)| k.as_slice() < new_key);
        entries.insert(insert_pos, (new_key.to_vec(), new_value.to_vec()));

        // Split in half
        let mid = entries.len() / 2;

        // Create new leaf
        let mut new_page = self.pager.allocate_page(PageType::BTreeLeaf)?;

        // Update leaf links
        let old_next = read_next_leaf(page);
        init_leaf_links(&mut new_page, page.page_id(), old_next);
        set_next_leaf(page, new_page.page_id());

        // Rebuild both halves through the slotted layout so their
        // cell_count, free_start and free_end headers are consistent.
        write_leaf_entries(page, &entries[..mid])?;
        write_leaf_entries(&mut new_page, &entries[mid..])?;

        // Separator is first key of new leaf
        let separator = entries[mid].0.clone();

        new_page.update_checksum();
        let new_page_id = new_page.page_id();
        self.pager.write_page(new_page_id, new_page.clone())?;

        Ok((new_page, separator))
    }

    /// Insert into parent after split
    fn insert_into_parent(
        &self,
        mut path: Vec<u32>,
        left_child: u32,
        key: &[u8],
        right_child: u32,
    ) -> BTreeResult<()> {
        // If path is empty, need new root
        if path.is_empty() {
            let mut new_root = self.pager.allocate_page(PageType::BTreeInterior)?;
            // Single separator with two children — seed the page via the
            // canonical slotted builder so the header is consistent.
            write_interior_entries(&mut new_root, &[key.to_vec()], &[left_child, right_child])?;

            new_root.update_checksum();
            let new_root_id = new_root.page_id();
            self.pager.write_page(new_root_id, new_root)?;
            *self.root_page_id.write().map_err(|e| {
                BTreeError::LockPoisoned(format!(
                    "insert_into_parent: root_page_id write lock: {e}"
                ))
            })? = new_root_id;
            return Ok(());
        }

        // Insert into parent — path is non-empty (checked above)
        let parent_id = path.pop().ok_or_else(|| {
            BTreeError::Corrupted("insert_into_parent: path unexpectedly empty".into())
        })?;
        let mut parent = self.pager.read_page(parent_id)?;

        // Can we fit?
        if can_insert_interior(&parent, key) {
            insert_into_interior(&mut parent, key, left_child, right_child)?;
            parent.update_checksum();
            self.pager.write_page(parent_id, parent)?;
            return Ok(());
        }

        // Need to split interior node
        let (new_interior, separator) =
            self.split_interior(&mut parent, key, left_child, right_child)?;
        parent.update_checksum();
        self.pager.write_page(parent_id, parent.clone())?;

        // Propagate up
        self.insert_into_parent(path, parent.page_id(), &separator, new_interior.page_id())
    }

    /// Split an interior node
    fn split_interior(
        &self,
        page: &mut Page,
        new_key: &[u8],
        left_child: u32,
        right_child: u32,
    ) -> BTreeResult<(Page, Vec<u8>)> {
        let (mut keys, mut children) = read_interior_keys_children(page)?;
        let key_insert_pos = keys.partition_point(|key| key.as_slice() < new_key);
        let child_insert_pos = children
            .iter()
            .position(|child| *child == left_child)
            .unwrap_or(key_insert_pos);

        keys.insert(key_insert_pos, new_key.to_vec());
        children.insert(child_insert_pos + 1, right_child);

        // The promoted separator does not stay in either side; each
        // resulting page is rebuilt through the shared slotted writer.
        let mid = keys.len() / 2;
        let separator = keys[mid].clone();

        // Create new interior node
        let mut new_page = self.pager.allocate_page(PageType::BTreeInterior)?;

        write_interior_entries(page, &keys[..mid], &children[..mid + 1])?;
        write_interior_entries(&mut new_page, &keys[mid + 1..], &children[mid + 1..])?;

        new_page.update_checksum();
        let new_page_id = new_page.page_id();
        self.pager.write_page(new_page_id, new_page.clone())?;

        Ok((new_page, separator))
    }

    fn rebalance_leaf(&self, leaf_id: u32, path: Vec<u32>) -> BTreeResult<()> {
        if path.is_empty() {
            return Ok(());
        }

        let root_id = self.root_page_id();
        if leaf_id == root_id {
            return Ok(());
        }

        let mut leaf = self.pager.read_page(leaf_id)?;
        let mut leaf_entries = read_leaf_entries(&leaf)?;
        let min_bytes = leaf_min_bytes();

        let parent_id = *path.last().ok_or_else(|| {
            BTreeError::Corrupted("rebalance_leaf: path unexpectedly empty".into())
        })?;
        let mut parent = self.pager.read_page(parent_id)?;
        let (mut parent_keys, mut parent_children) = read_interior_keys_children(&parent)?;

        let child_index = parent_children
            .iter()
            .position(|&id| id == leaf_id)
            .ok_or_else(|| BTreeError::Corrupted("Leaf missing from parent".into()))?;

        if child_index > 0 {
            if let Some((first_key, _)) = leaf_entries.first() {
                if parent_keys.get(child_index - 1).map(|k| k.as_slice())
                    != Some(first_key.as_slice())
                {
                    parent_keys[child_index - 1] = first_key.clone();
                    write_interior_entries(&mut parent, &parent_keys, &parent_children)?;
                    parent.update_checksum();
                    self.pager.write_page(parent_id, parent.clone())?;
                }
            }
        }

        if leaf_entries_size(&leaf_entries) >= min_bytes {
            return Ok(());
        }

        if child_index > 0 {
            let left_id = parent_children[child_index - 1];
            let mut left = self.pager.read_page(left_id)?;
            let mut left_entries = read_leaf_entries(&left)?;
            let mut borrowed = false;

            while leaf_entries_size(&leaf_entries) < min_bytes {
                let Some(entry) = left_entries.pop() else {
                    break;
                };
                if leaf_entries_size(&left_entries) < min_bytes {
                    left_entries.push(entry);
                    break;
                }
                leaf_entries.insert(0, entry);
                borrowed = true;
            }

            if borrowed {
                write_leaf_entries(&mut left, &left_entries)?;
                left.update_checksum();
                self.pager.write_page(left_id, left)?;

                write_leaf_entries(&mut leaf, &leaf_entries)?;
                leaf.update_checksum();
                self.pager.write_page(leaf_id, leaf)?;

                if let Some((first_key, _)) = leaf_entries.first() {
                    parent_keys[child_index - 1] = first_key.clone();
                    write_interior_entries(&mut parent, &parent_keys, &parent_children)?;
                    parent.update_checksum();
                    self.pager.write_page(parent_id, parent)?;
                }

                return Ok(());
            }
        }

        if child_index + 1 < parent_children.len() {
            let right_id = parent_children[child_index + 1];
            let mut right = self.pager.read_page(right_id)?;
            let mut right_entries = read_leaf_entries(&right)?;
            let mut borrowed = false;

            while leaf_entries_size(&leaf_entries) < min_bytes {
                if right_entries.is_empty() {
                    break;
                }
                let entry = right_entries.remove(0);
                if leaf_entries_size(&right_entries) < min_bytes {
                    right_entries.insert(0, entry);
                    break;
                }
                leaf_entries.push(entry);
                borrowed = true;
            }

            if borrowed {
                write_leaf_entries(&mut right, &right_entries)?;
                right.update_checksum();
                self.pager.write_page(right_id, right)?;

                write_leaf_entries(&mut leaf, &leaf_entries)?;
                leaf.update_checksum();
                self.pager.write_page(leaf_id, leaf)?;

                if let Some((first_key, _)) = right_entries.first() {
                    parent_keys[child_index] = first_key.clone();
                    write_interior_entries(&mut parent, &parent_keys, &parent_children)?;
                    parent.update_checksum();
                    self.pager.write_page(parent_id, parent)?;
                }

                return Ok(());
            }
        }

        if child_index > 0 {
            let left_id = parent_children[child_index - 1];
            let mut left = self.pager.read_page(left_id)?;
            let mut left_entries = read_leaf_entries(&left)?;

            left_entries.extend(leaf_entries);
            write_leaf_entries(&mut left, &left_entries)?;

            let next_leaf = read_next_leaf(&leaf);
            set_next_leaf(&mut left, next_leaf);
            if next_leaf != 0 {
                let mut next = self.pager.read_page(next_leaf)?;
                set_prev_leaf(&mut next, left_id);
                next.update_checksum();
                self.pager.write_page(next_leaf, next)?;
            }

            left.update_checksum();
            self.pager.write_page(left_id, left)?;
            self.pager.free_page(leaf_id)?;

            parent_keys.remove(child_index - 1);
            parent_children.remove(child_index);
            write_interior_entries(&mut parent, &parent_keys, &parent_children)?;
            parent.update_checksum();
            self.pager.write_page(parent_id, parent)?;

            let mut parent_path = path;
            parent_path.pop();
            return self.rebalance_interior(parent_id, parent_path);
        }

        if child_index + 1 < parent_children.len() {
            let right_id = parent_children[child_index + 1];
            let right = self.pager.read_page(right_id)?;
            let right_entries = read_leaf_entries(&right)?;

            leaf_entries.extend(right_entries);
            write_leaf_entries(&mut leaf, &leaf_entries)?;

            let next_leaf = read_next_leaf(&right);
            set_next_leaf(&mut leaf, next_leaf);
            if next_leaf != 0 {
                let mut next = self.pager.read_page(next_leaf)?;
                set_prev_leaf(&mut next, leaf_id);
                next.update_checksum();
                self.pager.write_page(next_leaf, next)?;
            }

            leaf.update_checksum();
            self.pager.write_page(leaf_id, leaf)?;
            self.pager.free_page(right_id)?;

            parent_keys.remove(child_index);
            parent_children.remove(child_index + 1);
            write_interior_entries(&mut parent, &parent_keys, &parent_children)?;
            parent.update_checksum();
            self.pager.write_page(parent_id, parent)?;

            let mut parent_path = path;
            parent_path.pop();
            return self.rebalance_interior(parent_id, parent_path);
        }

        Ok(())
    }

    fn rebalance_interior(&self, node_id: u32, mut path: Vec<u32>) -> BTreeResult<()> {
        let root_id = self.root_page_id();
        let mut node = self.pager.read_page(node_id)?;
        let (mut node_keys, mut node_children) = read_interior_keys_children(&node)?;
        let min_bytes = interior_min_bytes();

        if node_id == root_id {
            if node_keys.is_empty() {
                let next_root = node_children.first().copied().unwrap_or(0);
                self.pager.free_page(node_id)?;
                *self.root_page_id.write().map_err(|e| {
                    BTreeError::LockPoisoned(format!(
                        "rebalance_interior: root_page_id write lock: {e}"
                    ))
                })? = next_root;
            }
            return Ok(());
        }

        if interior_entries_size(&node_keys) >= min_bytes {
            return Ok(());
        }

        let parent_id = match path.pop() {
            Some(id) => id,
            None => return Ok(()),
        };
        let mut parent = self.pager.read_page(parent_id)?;
        let (mut parent_keys, mut parent_children) = read_interior_keys_children(&parent)?;

        let child_index = parent_children
            .iter()
            .position(|&id| id == node_id)
            .ok_or_else(|| BTreeError::Corrupted("Interior missing from parent".into()))?;

        if child_index > 0 {
            let left_id = parent_children[child_index - 1];
            let mut left = self.pager.read_page(left_id)?;
            let (mut left_keys, mut left_children) = read_interior_keys_children(&left)?;

            if let Some(borrow_key) = left_keys.last().cloned() {
                let borrow_size = interior_key_size(&borrow_key);
                if interior_entries_size(&left_keys).saturating_sub(borrow_size) >= min_bytes {
                    let parent_key = parent_keys[child_index - 1].clone();
                    let borrowed_key = left_keys.pop().ok_or_else(|| {
                        BTreeError::Corrupted(
                            "rebalance_interior: left_keys empty after check".into(),
                        )
                    })?;
                    let borrowed_child = left_children.pop().ok_or_else(|| {
                        BTreeError::Corrupted("rebalance_interior: left_children empty".into())
                    })?;

                    node_keys.insert(0, parent_key);
                    node_children.insert(0, borrowed_child);
                    parent_keys[child_index - 1] = borrowed_key;

                    write_interior_entries(&mut left, &left_keys, &left_children)?;
                    left.update_checksum();
                    self.pager.write_page(left_id, left)?;

                    write_interior_entries(&mut node, &node_keys, &node_children)?;
                    node.update_checksum();
                    self.pager.write_page(node_id, node)?;

                    write_interior_entries(&mut parent, &parent_keys, &parent_children)?;
                    parent.update_checksum();
                    self.pager.write_page(parent_id, parent)?;

                    return Ok(());
                }
            }
        }

        if child_index + 1 < parent_children.len() {
            let right_id = parent_children[child_index + 1];
            let mut right = self.pager.read_page(right_id)?;
            let (mut right_keys, mut right_children) = read_interior_keys_children(&right)?;

            if let Some(borrow_key) = right_keys.first().cloned() {
                let borrow_size = interior_key_size(&borrow_key);
                if interior_entries_size(&right_keys).saturating_sub(borrow_size) >= min_bytes {
                    let parent_key = parent_keys[child_index].clone();
                    let new_parent_key = right_keys.remove(0);
                    let borrowed_child = right_children.remove(0);

                    node_keys.push(parent_key);
                    node_children.push(borrowed_child);
                    parent_keys[child_index] = new_parent_key;

                    write_interior_entries(&mut right, &right_keys, &right_children)?;
                    right.update_checksum();
                    self.pager.write_page(right_id, right)?;

                    write_interior_entries(&mut node, &node_keys, &node_children)?;
                    node.update_checksum();
                    self.pager.write_page(node_id, node)?;

                    write_interior_entries(&mut parent, &parent_keys, &parent_children)?;
                    parent.update_checksum();
                    self.pager.write_page(parent_id, parent)?;

                    return Ok(());
                }
            }
        }

        if child_index > 0 {
            let left_id = parent_children[child_index - 1];
            let mut left = self.pager.read_page(left_id)?;
            let (mut left_keys, mut left_children) = read_interior_keys_children(&left)?;
            let parent_key = parent_keys.remove(child_index - 1);
            parent_children.remove(child_index);

            left_keys.push(parent_key);
            left_keys.extend(node_keys);
            left_children.extend(node_children);

            write_interior_entries(&mut left, &left_keys, &left_children)?;
            left.update_checksum();
            self.pager.write_page(left_id, left)?;
            self.pager.free_page(node_id)?;

            write_interior_entries(&mut parent, &parent_keys, &parent_children)?;
            parent.update_checksum();
            self.pager.write_page(parent_id, parent)?;

            return self.rebalance_interior(parent_id, path);
        }

        if child_index + 1 < parent_children.len() {
            let right_id = parent_children[child_index + 1];
            let right = self.pager.read_page(right_id)?;
            let (right_keys, right_children) = read_interior_keys_children(&right)?;
            let parent_key = parent_keys.remove(child_index);
            parent_children.remove(child_index + 1);

            node_keys.push(parent_key);
            node_keys.extend(right_keys);
            node_children.extend(right_children);

            write_interior_entries(&mut node, &node_keys, &node_children)?;
            node.update_checksum();
            self.pager.write_page(node_id, node)?;
            self.pager.free_page(right_id)?;

            write_interior_entries(&mut parent, &parent_keys, &parent_children)?;
            parent.update_checksum();
            self.pager.write_page(parent_id, parent)?;

            return self.rebalance_interior(parent_id, path);
        }

        Ok(())
    }
}
