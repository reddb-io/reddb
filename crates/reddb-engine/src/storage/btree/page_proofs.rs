//! Incremental page-image proofs, shared by point reads, cursors and overflow.
//! A host publishes the sealed root with its data root after syncing both.
use super::*;
use reddb_file::page_proof::{self, PageProofRoot, PageReference, PROOF_LEVEL_MAX};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

pub(super) struct TreePageStore<P: PageStore> {
    inner: Arc<P>,
    proof: Option<parking_lot::RwLock<ProofState>>,
    failed: AtomicBool,
}

struct ProofState {
    root: PageProofRoot,
    sealed: bool,
}

fn invalid(page_id: u32, detail: &str) -> PagerError {
    PagerError::InvalidDatabase(format!("page {page_id} generation proof: {detail}"))
}

fn read_node<P: PageStore>(
    pager: &P,
    reference: PageReference,
    level: u8,
    prefix: u32,
    generation: u64,
) -> Result<Page, PagerError> {
    if reference.page_id == 0 {
        return Err(invalid(0, "missing proof node"));
    }
    let page = pager.read_page(reference.page_id)?;
    let node_generation = page_proof::node_generation(page.content(), level, prefix);
    if page.page_id() != reference.page_id
        || page.page_type()? != PageType::NativeMeta
        || page_proof::page_digest(page.as_bytes()) != reference.digest
        || node_generation.is_none_or(|value| value > generation)
    {
        return Err(invalid(reference.page_id, "unexpected proof node image"));
    }
    Ok(page)
}

fn child_reference(page: &Page, level: u8, index: usize) -> Result<PageReference, PagerError> {
    page_proof::decode_reference(&page.content()[page_proof::entry_range(level, index)])
        .ok_or_else(|| invalid(page.page_id(), "invalid child reference"))
}

fn read_entry<P: PageStore>(
    pager: &P,
    reference: PageReference,
    level: u8,
    prefix: u32,
    generation: u64,
    index: usize,
) -> Result<[u8; page_proof::PROOF_REFERENCE_BYTES], PagerError> {
    let page = read_node(pager, reference, level, prefix, generation)?;
    let entry = &page.content()[page_proof::entry_range(level, index)];
    let mut result = [0; page_proof::PROOF_REFERENCE_BYTES];
    result[..entry.len()].copy_from_slice(entry);
    Ok(result)
}

fn expected_digest<P: PageStore>(
    pager: &P,
    root: PageProofRoot,
    page_id: u32,
) -> Result<[u8; 32], PagerError> {
    let mut reference = root.reference;
    let mut prefix = 0;
    for level in (0..=PROOF_LEVEL_MAX).rev() {
        let index = usize::from(((page_id >> (u32::from(level) * 8)) & 255) as u8);
        let entry = read_entry(pager, reference, level, prefix, root.generation, index)?;
        if level == 0 {
            let digest: [u8; 32] = entry[..32].try_into().expect("leaf digest field");
            if digest == [0; 32] {
                return Err(invalid(page_id, "page is outside this tree's ownership"));
            }
            return Ok(digest);
        }
        reference = page_proof::decode_reference(&entry)
            .ok_or_else(|| invalid(reference.page_id, "invalid child reference"))?;
        if reference.page_id == 0 {
            return Err(invalid(page_id, "page is outside this tree's ownership"));
        }
        prefix = page_proof::child_prefix(prefix, level, index);
    }
    unreachable!("the bounded proof descent reaches its leaf")
}

fn verified_page<P: PageStore>(
    pager: &P,
    root: PageProofRoot,
    page_id: u32,
) -> Result<Page, PagerError> {
    let expected = expected_digest(pager, root, page_id)?;
    let page = pager.read_page(page_id)?;
    if page.page_id() != page_id || page_proof::page_digest(page.as_bytes()) != expected {
        return Err(invalid(
            page_id,
            "page image differs from its published generation",
        ));
    }
    Ok(page)
}

fn verify_stored_page<P: PageStore>(
    pager: &P,
    root: PageProofRoot,
    page_id: u32,
) -> Result<(), PagerError> {
    verified_page(pager, root, page_id).map(|_| ())
}

/// Copy a path on its first mutation after sealing. Later edits reuse private
/// nodes of this generation; the pager's existing cache bounds dirty images.
fn replace_entry<P: PageStore>(
    pager: &P,
    root: PageProofRoot,
    level: u8,
    prefix: u32,
    index: usize,
    value: &[u8],
) -> Result<PageReference, PagerError> {
    let PageProofRoot {
        reference,
        generation,
    } = root;
    let mut page = if reference.page_id == 0 {
        let mut page = pager.allocate_page(PageType::NativeMeta)?;
        page_proof::initialize_node(page.content_mut(), level, prefix, generation);
        page
    } else {
        read_node(pager, reference, level, prefix, generation)?
    };
    let copied = page_proof::node_generation(page.content(), level, prefix) != Some(generation);
    if copied {
        let mut replacement = pager.allocate_page(PageType::NativeMeta)?;
        replacement.content_mut().copy_from_slice(page.content());
        replacement.content_mut()[16..24].copy_from_slice(&generation.to_le_bytes());
        page = replacement;
    }
    let range = page_proof::entry_range(level, index);
    page.content_mut()[range].copy_from_slice(value);
    page.update_checksum();
    let next = PageReference {
        page_id: page.page_id(),
        digest: page_proof::page_digest(page.as_bytes()),
    };
    pager.write_page(next.page_id, page)?;
    if copied {
        // The constructor requires the host's deferred publication fence.
        pager.free_page(reference.page_id)?;
    }
    Ok(next)
}

fn update_path<P: PageStore>(
    pager: &P,
    root: PageProofRoot,
    page_id: u32,
    digest: [u8; 32],
) -> Result<PageReference, PagerError> {
    let mut path = [PageReference::default(); 4];
    let mut prefixes = [0_u32; 4];
    let mut reference = root.reference;
    let mut prefix = 0;
    // Retain compact references, never a stack of 16-KiB page images.
    for level in (0..=PROOF_LEVEL_MAX).rev() {
        path[usize::from(level)] = reference;
        prefixes[usize::from(level)] = prefix;
        if level != 0 {
            let index = usize::from(((page_id >> (u32::from(level) * 8)) & 255) as u8);
            if reference.page_id != 0 {
                let entry = read_entry(pager, reference, level, prefix, root.generation, index)?;
                reference = page_proof::decode_reference(&entry)
                    .ok_or_else(|| invalid(reference.page_id, "invalid child reference"))?;
            }
            prefix = page_proof::child_prefix(prefix, level, index);
        }
    }
    let mut child = PageReference::default();
    let mut encoded = [0_u8; page_proof::PROOF_REFERENCE_BYTES];
    for level in 0..=PROOF_LEVEL_MAX {
        let index = usize::from(((page_id >> (u32::from(level) * 8)) & 255) as u8);
        page_proof::encode_reference(&mut encoded, child);
        let value = if level == 0 {
            digest.as_slice()
        } else {
            encoded.as_slice()
        };
        child = replace_entry(
            pager,
            PageProofRoot {
                reference: path[usize::from(level)],
                generation: root.generation,
            },
            level,
            prefixes[usize::from(level)],
            index,
            value,
        )?;
    }
    Ok(child)
}

impl ProofState {
    fn begin_mutation(&mut self) -> Result<(), PagerError> {
        if self.sealed {
            self.root.generation = self
                .root
                .generation
                .checked_add(1)
                .ok_or_else(|| invalid(self.root.reference.page_id, "generation exhausted"))?;
            self.sealed = false;
        }
        Ok(())
    }

    fn set<P: PageStore>(
        &mut self,
        pager: &P,
        page_id: u32,
        digest: [u8; 32],
    ) -> Result<(), PagerError> {
        self.begin_mutation()?;
        self.root.reference = update_path(pager, self.root, page_id, digest)?;
        Ok(())
    }
}

impl<P: PageStore> TreePageStore<P> {
    pub(super) fn has_page_proofs(&self) -> bool {
        self.proof.is_some()
    }

    pub(super) fn legacy(inner: Arc<P>) -> Self {
        Self {
            inner,
            proof: None,
            failed: AtomicBool::new(false),
        }
    }

    pub(super) fn verified(inner: Arc<P>, root: PageProofRoot) -> Result<Self, PagerError> {
        if (root.reference.page_id == 0) != (root.reference.digest == [0; 32])
            || (root.reference.page_id != 0 && root.generation == 0)
        {
            return Err(invalid(root.reference.page_id, "invalid sealed root"));
        }
        inner.enable_deferred_reclamation()?;
        Ok(Self {
            inner,
            proof: Some(parking_lot::RwLock::new(ProofState { root, sealed: true })),
            failed: AtomicBool::new(false),
        })
    }

    fn available(&self) -> Result<(), PagerError> {
        if self.failed.load(AtomicOrdering::Acquire) {
            return Err(invalid(
                0,
                "proof mutation failed; reopen before further access",
            ));
        }
        Ok(())
    }

    fn finish<T>(&self, result: Result<T, PagerError>) -> Result<T, PagerError> {
        if result.is_err() {
            self.failed.store(true, AtomicOrdering::Release);
        }
        result
    }

    pub(super) fn seal(&self) -> Result<Option<PageProofRoot>, PagerError> {
        let Some(proof) = &self.proof else {
            return Ok(None);
        };
        let mut state = proof.write();
        self.available()?;
        state.sealed = true;
        Ok(Some(state.root))
    }

    pub(super) fn reset_empty(&self) -> Result<(), PagerError> {
        let Some(proof) = &self.proof else {
            return Ok(());
        };
        let result = (|| {
            let pages = self.proof_pages()?;
            let mut state = proof.write();
            self.available()?;
            for id in pages {
                // The host's deferred fence retains all previously published paths.
                self.inner.free_page(id)?;
            }
            state.root = PageProofRoot::default();
            state.sealed = true;
            Ok(())
        })();
        self.finish(result)
    }

    pub(super) fn proof_pages(&self) -> Result<Vec<u32>, PagerError> {
        Ok(self.page_inventory(false)?.unwrap_or_default())
    }

    /// A sealed map proves ownership independently of damaged data images.
    /// Reading it cannot clear the mutation fence or authorize a new seal.
    pub(super) fn published_owned_pages(&self) -> Result<Option<Vec<u32>>, PagerError> {
        self.page_inventory(true)
    }

    fn page_inventory(&self, include_data: bool) -> Result<Option<Vec<u32>>, PagerError> {
        let Some(proof) = &self.proof else {
            return Ok(None);
        };
        let state = proof.read();
        if include_data {
            if !state.sealed {
                return Err(invalid(
                    0,
                    "ownership inventory requires a sealed generation",
                ));
            }
        } else {
            self.available()?;
        }
        let mut pages = Vec::new();
        if state.root.reference.page_id == 0 {
            return Ok(Some(pages));
        }
        let count = self.inner.page_count()?;
        let mut seen = std::collections::HashSet::new();
        let mut pending = vec![(state.root.reference, PROOF_LEVEL_MAX, 0)];
        while let Some((reference, level, prefix)) = pending.pop() {
            if reference.page_id == 0
                || reference.page_id >= count
                || !seen.insert(reference.page_id)
            {
                return Err(invalid(reference.page_id, "invalid proof node ownership"));
            }
            let page = read_node(
                self.inner.as_ref(),
                reference,
                level,
                prefix,
                state.root.generation,
            )?;
            pages.push(reference.page_id);
            if level != 0 {
                for index in 0..page_proof::PROOF_FANOUT {
                    let child = child_reference(&page, level, index)?;
                    if child.page_id != 0 {
                        pending.push((
                            child,
                            level - 1,
                            page_proof::child_prefix(prefix, level, index),
                        ));
                    }
                }
            } else if include_data {
                for index in 0..page_proof::PROOF_FANOUT {
                    if page.content()[page_proof::entry_range(level, index)] != [0; 32] {
                        let id = page_proof::child_prefix(prefix, level, index);
                        if id == 0 || id >= count || !seen.insert(id) {
                            return Err(invalid(id, "invalid proven data ownership"));
                        }
                        pages.push(id);
                    }
                }
            }
        }
        Ok(Some(pages))
    }
}

impl<P: PageStore> PageStore for TreePageStore<P> {
    fn read_page(&self, page_id: u32) -> Result<Page, PagerError> {
        let Some(proof) = &self.proof else {
            return self.inner.read_page(page_id);
        };
        let state = proof.read();
        self.available()?;
        self.finish(verified_page(self.inner.as_ref(), state.root, page_id))
    }

    fn write_page(&self, page_id: u32, mut page: Page) -> Result<(), PagerError> {
        let Some(proof) = &self.proof else {
            return self.inner.write_page(page_id, page);
        };
        let mut state = proof.write();
        self.available()?;
        let result = (|| {
            verify_stored_page(self.inner.as_ref(), state.root, page_id)?;
            if page.page_id() != page_id {
                return Err(invalid(page_id, "write address mismatch"));
            }
            page.update_checksum();
            let digest = page_proof::page_digest(page.as_bytes());
            self.inner.write_page(page_id, page)?;
            state.set(self.inner.as_ref(), page_id, digest)
        })();
        self.finish(result)
    }

    fn allocate_page(&self, kind: PageType) -> Result<Page, PagerError> {
        let Some(proof) = &self.proof else {
            return self.inner.allocate_page(kind);
        };
        let mut state = proof.write();
        self.available()?;
        let result = (|| {
            let mut page = self.inner.allocate_page(kind)?;
            page.update_checksum();
            self.inner.write_page(page.page_id(), page.clone())?;
            state.set(
                self.inner.as_ref(),
                page.page_id(),
                page_proof::page_digest(page.as_bytes()),
            )?;
            Ok(page)
        })();
        self.finish(result)
    }

    fn free_page(&self, page_id: u32) -> Result<(), PagerError> {
        let Some(proof) = &self.proof else {
            return self.inner.free_page(page_id);
        };
        let mut state = proof.write();
        self.available()?;
        let result = (|| {
            verify_stored_page(self.inner.as_ref(), state.root, page_id)?;
            state.set(self.inner.as_ref(), page_id, [0; 32])?;
            self.inner.free_page(page_id)
        })();
        self.finish(result)
    }

    fn page_count(&self) -> Result<u32, PagerError> {
        self.inner.page_count()
    }
    fn prefetch_hint(&self, page_id: u32) {
        self.inner.prefetch_hint(page_id)
    }
}
