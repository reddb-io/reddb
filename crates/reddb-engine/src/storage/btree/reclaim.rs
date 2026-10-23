use super::*;

impl<P: PageStore> BTree<P> {
    pub fn has_retirement_readers(&self) -> bool {
        Arc::strong_count(&self.retirement_pin) != 1
    }

    /// Enumerate a retired generation before changing the allocator. Never
    /// follow leaf sibling links as additional ownership edges: interior
    /// children already own every leaf. Validate overflow chains as well.
    pub fn retired_pages(&self) -> BTreeResult<Vec<u32>> {
        let mut pending = vec![self.root_page_id()];
        let mut seen = std::collections::HashSet::new();
        let mut pages = Vec::new();
        let page_count = self.pager.page_count()?;
        while let Some(id) = pending.pop() {
            if id == 0 {
                continue;
            }
            if id >= page_count || !seen.insert(id) {
                return Err(BTreeError::Corrupted(
                    "invalid retired tree page ownership".into(),
                ));
            }
            let page = self.pager.read_page(id)?;
            match page.page_type()? {
                PageType::BTreeInterior => {
                    let (_, children) = read_interior_keys_children(&page)?;
                    pending.extend(children);
                }
                PageType::BTreeLeaf => {
                    for index in 0..usize::from(page.cell_count()) {
                        let (_, stored) = read_leaf_cell(&page, index)?;
                        if let Some(head) = value_layout::pointer_head(&stored) {
                            pending.push(head);
                        }
                    }
                }
                PageType::Overflow => {
                    let content = page.content();
                    let next = u32::from_le_bytes(content[..4].try_into().expect("chain header"));
                    let len = u32::from_le_bytes(content[4..8].try_into().expect("chain length"));
                    if len as usize > crate::storage::overflow::OVERFLOW_PAYLOAD_PER_PAGE {
                        return Err(BTreeError::Corrupted(
                            "invalid retired overflow length".into(),
                        ));
                    }
                    pending.push(next);
                }
                _ => {
                    return Err(BTreeError::Corrupted(
                        "unexpected retired tree page type".into(),
                    ))
                }
            }
            pages.push(id);
        }
        Ok(pages)
    }
}
