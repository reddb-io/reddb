use super::*;

impl BTree {
    /// Create a new B+ tree using the given pager
    pub fn new(pager: Arc<Pager>) -> Self {
        Self {
            pager,
            root_page_id: RwLock::new(0),
        }
    }

    /// Create a B+ tree with an existing root
    pub fn with_root(pager: Arc<Pager>, root_page_id: u32) -> Self {
        Self {
            pager,
            root_page_id: RwLock::new(root_page_id),
        }
    }

    /// Get the root page ID
    pub fn root_page_id(&self) -> u32 {
        *self
            .root_page_id
            .read()
            .expect("btree root_page_id RwLock poisoned")
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

        // Find the leaf page
        let (leaf_id, _) = self.find_leaf(root_id, key)?;
        let page = self.pager.read_page(leaf_id)?;

        // Search within the leaf
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
            write_leaf_cell(&mut page, 0, key, value)?;
            page.set_cell_count(1);
            init_leaf_links(&mut page, 0, 0);
            page.update_checksum();
            let new_root = page.page_id();
            self.pager.write_page(new_root, page)?;
            *self.root_page_id.write().map_err(|e| {
                BTreeError::LockPoisoned(format!("insert: root_page_id write lock: {e}"))
            })? = new_root;
            return Ok(());
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
            page.update_checksum();
            let page_id = page.page_id();
            self.pager.write_page(page_id, page)?;
            return Ok(());
        }

        // Need to split the leaf
        let (new_leaf, separator_key) = self.split_leaf(&mut page, key, value)?;
        page.update_checksum();
        let page_id = page.page_id();
        self.pager.write_page(page_id, page.clone())?;

        // Propagate split up the tree
        self.insert_into_parent(path, page.page_id(), &separator_key, new_leaf.page_id())?;

        Ok(())
    }

    /// Delete a key
    pub fn delete(&self, key: &[u8]) -> BTreeResult<bool> {
        let root_id = self.root_page_id();
        if root_id == 0 {
            return Ok(false);
        }

        let (leaf_id, path) = self.find_leaf(root_id, key)?;
        let mut page = self.pager.read_page(leaf_id)?;

        // Find the key
        match search_leaf(&page, key)? {
            SearchResult::Found(pos) => {
                delete_from_leaf(&mut page, pos)?;
                page.update_checksum();
                let page_id = page.page_id();
                self.pager.write_page(page_id, page.clone())?;

                // Handle empty root
                if page.cell_count() == 0 && page.page_id() == root_id {
                    self.pager.free_page(root_id)?;
                    *self.root_page_id.write().map_err(|e| {
                        BTreeError::LockPoisoned(format!("delete: root_page_id write lock: {e}"))
                    })? = 0;
                } else {
                    self.rebalance_leaf(leaf_id, path)?;
                }

                Ok(true)
            }
            SearchResult::NotFound(_) => Ok(false),
        }
    }

    /// Create a cursor starting at the first entry
    pub fn cursor_first(&self) -> BTreeResult<BTreeCursor> {
        let root_id = self.root_page_id();
        if root_id == 0 {
            return Ok(BTreeCursor {
                leaf_page_id: 0,
                position: 0,
                pager: self.pager.clone(),
            });
        }

        // Find leftmost leaf
        let first_leaf = self.find_first_leaf(root_id)?;

        Ok(BTreeCursor {
            leaf_page_id: first_leaf,
            position: 0,
            pager: self.pager.clone(),
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

        // Write entries to old page
        clear_leaf_cells(page);
        for (i, (k, v)) in entries[..mid].iter().enumerate() {
            write_leaf_cell(page, i, k, v)?;
        }
        page.set_cell_count(mid as u16);

        // Write entries to new page
        for (i, (k, v)) in entries[mid..].iter().enumerate() {
            write_leaf_cell(&mut new_page, i, k, v)?;
        }
        new_page.set_cell_count((entries.len() - mid) as u16);

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

            // Set right_child in header
            new_root.set_right_child(right_child);

            // Write the single key/child cell
            write_interior_cell(&mut new_root, 0, key, left_child)?;
            new_root.set_cell_count(1);

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
        // Collect all entries
        let mut entries: Vec<(Vec<u8>, u32)> = Vec::new();
        let cell_count = page.cell_count() as usize;

        for i in 0..cell_count {
            entries.push(read_interior_cell(page, i)?);
        }

        // Insert new entry
        let insert_pos = entries.partition_point(|(k, _)| k.as_slice() < new_key);

        // Update children around insertion point
        if insert_pos < entries.len() {
            entries[insert_pos].1 = left_child;
        }
        entries.insert(insert_pos, (new_key.to_vec(), left_child));

        // The key at mid goes up, not into either node
        let mid = entries.len() / 2;
        let separator = entries[mid].0.clone();

        // Create new interior node
        let mut new_page = self.pager.allocate_page(PageType::BTreeInterior)?;

        // Left node gets entries before mid
        clear_interior_cells(page);
        for (i, (k, c)) in entries[..mid].iter().enumerate() {
            write_interior_cell(page, i, k, *c)?;
        }
        page.set_cell_count(mid as u16);
        page.set_right_child(entries[mid].1);

        // Right node gets entries after mid
        for (i, (k, c)) in entries[mid + 1..].iter().enumerate() {
            write_interior_cell(&mut new_page, i, k, *c)?;
        }
        new_page.set_cell_count((entries.len() - mid - 1) as u16);
        new_page.set_right_child(right_child);

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

            left_entries.extend(leaf_entries.into_iter());
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

            leaf_entries.extend(right_entries.into_iter());
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
            left_keys.extend(node_keys.into_iter());
            left_children.extend(node_children.into_iter());

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
            node_keys.extend(right_keys.into_iter());
            node_children.extend(right_children.into_iter());

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
