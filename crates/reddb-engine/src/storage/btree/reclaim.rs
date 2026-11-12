use super::*;

impl<P: PageStore> BTree<P> {
    /// Authenticate a frozen legacy generation without moving or rewriting its
    /// data. The host must fence the reader capability and deferred reclamation,
    /// sync the new proof pages, and publish their root before allowing reuse.
    /// Existing cursors retain their retirement pin across this transition.
    pub fn prepare_legacy_page_proofs(&self) -> BTreeResult<Self> {
        let _writer = self.writer.lock();
        let pages = self.retired_pages()?;
        let pager = self.pager.prepare_legacy(&pages)?;
        Ok(Self {
            retirement_pin: Arc::clone(&self.retirement_pin),
            pager: Arc::new(pager),
            root_page_id: RwLock::new(self.root_page_id()),
            rightmost_leaf: RwLock::new(None),
            writer: parking_lot::ReentrantMutex::new(()),
        })
    }

    pub fn has_retirement_readers(&self) -> bool {
        Arc::strong_count(&self.retirement_pin) != 1
    }

    /// Enumerate a sealed generation's authenticated ownership without reading
    /// its data. Recovery may discard these pages even when their images fail
    /// verification. This does not certify the contents of any data page.
    pub fn published_owned_pages(&self) -> BTreeResult<Option<Vec<u32>>> {
        let pages = self.pager.published_owned_pages()?;
        if let Some(pages) = &pages {
            let root = self.root_page_id();
            if root != 0 && !pages.contains(&root) {
                return Err(BTreeError::Corrupted(
                    "data root is outside its published ownership".into(),
                ));
            }
        }
        Ok(pages)
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
        for id in self.pager.proof_pages()? {
            if !seen.insert(id) {
                return Err(BTreeError::Corrupted(
                    "generation proof aliases tree ownership".into(),
                ));
            }
            pages.push(id);
        }
        Ok(pages)
    }

    /// Stream the verified leaves of a damaged tree. Only checksum failures at
    /// the proven leaf depth may be skipped. The caller must separately prove
    /// that no allocated pages remain unowned before publishing any output.
    pub fn visit_recoverable_leaves<C, R>(
        &self,
        mut claim: C,
        mut record: R,
    ) -> BTreeResult<Vec<u32>>
    where
        C: FnMut(u32) -> BTreeResult<()>,
        R: FnMut(Vec<u8>, Vec<u8>) -> BTreeResult<()>,
    {
        struct Work {
            id: u32,
            depth: usize,
            low: Option<Vec<u8>>,
            high: Option<Vec<u8>>,
        }
        let mut pending = vec![Work {
            id: self.root_page_id(),
            depth: 0,
            low: None,
            high: None,
        }];
        let mut seen = std::collections::HashSet::new();
        let count = self.pager.page_count()?;
        let mut leaves: Vec<(u32, Option<u32>)> = Vec::new();
        let mut skipped = Vec::new();
        let mut leaf_depth = None;
        let mut skipped_depths = Vec::new();
        let mut previous: Option<Vec<u8>> = None;
        while let Some(work) = pending.pop() {
            if work.id == 0 || work.id >= count || work.depth > 64 || !seen.insert(work.id) {
                return Err(BTreeError::Corrupted(
                    "invalid recovery tree ownership".into(),
                ));
            }
            claim(work.id)?;
            let page = match self.pager.read_page(work.id) {
                Ok(page) => page,
                Err(PagerError::Page(super::super::page::PageError::ChecksumMismatch {
                    ..
                })) if work.depth > 0 => {
                    skipped.push(work.id);
                    skipped_depths.push(work.depth);
                    leaves.push((work.id, None));
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            match page.page_type()? {
                PageType::BTreeInterior => {
                    let (keys, children) = read_interior_keys_children(&page)?;
                    if children.len() != keys.len() + 1
                        || children.contains(&0)
                        || keys.windows(2).any(|pair| pair[0] >= pair[1])
                        || keys.iter().any(|key| {
                            work.low.as_ref().is_some_and(|low| key < low)
                                || work.high.as_ref().is_some_and(|high| key >= high)
                        })
                    {
                        return Err(BTreeError::Corrupted(
                            "invalid recovery interior bounds".into(),
                        ));
                    }
                    for i in (0..children.len()).rev() {
                        pending.push(Work {
                            id: children[i],
                            depth: work.depth + 1,
                            low: if i == 0 {
                                work.low.clone()
                            } else {
                                Some(keys[i - 1].clone())
                            },
                            high: if i == keys.len() {
                                work.high.clone()
                            } else {
                                Some(keys[i].clone())
                            },
                        });
                    }
                }
                PageType::BTreeLeaf => {
                    if leaf_depth.is_some_and(|depth| depth != work.depth) {
                        return Err(BTreeError::Corrupted(
                            "inconsistent recovery leaf depth".into(),
                        ));
                    }
                    leaf_depth = Some(work.depth);
                    let bytes = page.content();
                    let next = u32::from_le_bytes(bytes[4..8].try_into().expect("leaf header"));
                    leaves.push((work.id, Some(next)));
                    for index in 0..usize::from(page.cell_count()) {
                        let (key, stored) = read_leaf_cell(&page, index)?;
                        if work.low.as_ref().is_some_and(|low| &key < low)
                            || work.high.as_ref().is_some_and(|high| &key >= high)
                            || previous.as_ref().is_some_and(|old| old >= &key)
                        {
                            return Err(BTreeError::Corrupted(
                                "invalid recovery leaf ordering".into(),
                            ));
                        }
                        previous = Some(key.clone());
                        if let Some(mut overflow) = value_layout::pointer_head(&stored) {
                            while overflow != 0 {
                                if overflow >= count || !seen.insert(overflow) {
                                    return Err(BTreeError::Corrupted(
                                        "invalid recovery overflow ownership".into(),
                                    ));
                                }
                                claim(overflow)?;
                                let page = self.pager.read_page(overflow)?;
                                if page.page_type()? != PageType::Overflow {
                                    return Err(BTreeError::Corrupted(
                                        "invalid recovery overflow page".into(),
                                    ));
                                }
                                let bytes = page.content();
                                let length = u32::from_le_bytes(
                                    bytes[4..8].try_into().expect("overflow header"),
                                );
                                if length as usize
                                    > crate::storage::overflow::OVERFLOW_PAYLOAD_PER_PAGE
                                {
                                    return Err(BTreeError::Corrupted(
                                        "invalid recovery overflow length".into(),
                                    ));
                                }
                                overflow = u32::from_le_bytes(
                                    bytes[..4].try_into().expect("overflow header"),
                                );
                            }
                        }
                        record(
                            key,
                            value_layout::decode_with_budget(
                                self.pager.as_ref(),
                                &stored,
                                32 * 1024 * 1024,
                            )?,
                        )?;
                    }
                }
                _ => {
                    return Err(BTreeError::Corrupted(
                        "invalid recovery tree page type".into(),
                    ))
                }
            }
        }
        if leaf_depth.is_none()
            || skipped_depths
                .iter()
                .any(|depth| Some(*depth) != leaf_depth)
        {
            return Err(BTreeError::Corrupted(
                "damaged page is not at proven leaf depth".into(),
            ));
        }
        for (i, (_, next)) in leaves.iter().enumerate() {
            if next.is_some_and(|id| id != leaves.get(i + 1).map_or(0, |leaf| leaf.0)) {
                return Err(BTreeError::Corrupted(
                    "invalid recovery forward leaf chain".into(),
                ));
            }
        }
        Ok(skipped)
    }
}
