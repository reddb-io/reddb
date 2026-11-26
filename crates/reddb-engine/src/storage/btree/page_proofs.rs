//! Incremental page-image proofs, shared by point reads, cursors and overflow.
//! A host publishes the sealed root with its data root after syncing both.
use super::*;
use reddb_file::page_proof::{self, PageProofRoot, PageReference, PROOF_LEVEL_MAX};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

pub(super) struct TreePageStore<P: PageStore> {
    inner: Arc<P>,
    proof: Option<parking_lot::RwLock<ProofState>>,
    failed: AtomicBool,
}

struct ProofState {
    root: PageProofRoot,
    sealed: bool,
    /// Digests this state has already proven or set, by page. Without it every
    /// page access walked the four proof levels again, copying and hashing a
    /// node at each, and that was most of the cost of a tree operation.
    known: parking_lot::Mutex<HashMap<u32, [u8; 32]>>,
    /// Entries set since the proof nodes were last written. A page rewritten
    /// many times in one generation reaches its node once, and entries that
    /// share a node rewrite it together. Empty whenever the state is sealed.
    pending: BTreeMap<u32, [u8; 32]>,
}

/// Digests remembered per tree: its interior pages and its recent leaves.
const KNOWN_DIGESTS_MAX: usize = 2048;
/// Entries held back from the proof nodes before they are written anyway.
const PENDING_ENTRIES_MAX: usize = 4096;

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
    expected: [u8; 32],
    page_id: u32,
) -> Result<Page, PagerError> {
    let page = pager.read_page(page_id)?;
    if page.page_id() != page_id || page_proof::page_digest(page.as_bytes()) != expected {
        return Err(invalid(
            page_id,
            "page image differs from its published generation",
        ));
    }
    Ok(page)
}

/// Copy a path on its first mutation after sealing. Later edits reuse private
/// nodes of this generation; the pager's existing cache bounds dirty images.
fn replace_entries<'a, P: PageStore>(
    pager: &P,
    root: PageProofRoot,
    level: u8,
    prefix: u32,
    entries: impl Iterator<Item = (usize, &'a [u8])>,
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
    for (index, value) in entries {
        let range = page_proof::entry_range(level, index);
        page.content_mut()[range].copy_from_slice(value);
    }
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

/// `entries` are pages of one leaf node: they differ only in their last byte.
fn update_path<P: PageStore>(
    pager: &P,
    root: PageProofRoot,
    entries: &[(u32, [u8; 32])],
) -> Result<PageReference, PagerError> {
    let page_id = entries[0].0;
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
        let node = PageProofRoot {
            reference: path[usize::from(level)],
            generation: root.generation,
        };
        let prefix = prefixes[usize::from(level)];
        child = if level == 0 {
            let leaves = entries
                .iter()
                .map(|(id, digest)| (usize::from(*id as u8), digest.as_slice()));
            replace_entries(pager, node, level, prefix, leaves)?
        } else {
            let link = std::iter::once((index, encoded.as_slice()));
            replace_entries(pager, node, level, prefix, link)?
        };
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

    fn new(root: PageProofRoot) -> Self {
        Self {
            root,
            sealed: true,
            known: parking_lot::Mutex::new(HashMap::new()),
            pending: BTreeMap::new(),
        }
    }

    /// The digest the proof holds for a page: remembered, set and not yet
    /// written to its node, or read from the nodes.
    fn expected<P: PageStore>(&self, pager: &P, page_id: u32) -> Result<[u8; 32], PagerError> {
        if let Some(digest) = self.known.lock().get(&page_id) {
            return Ok(*digest);
        }
        let digest = match self.pending.get(&page_id) {
            Some(digest) if *digest == [0; 32] => {
                return Err(invalid(page_id, "page is outside this tree's ownership"))
            }
            Some(digest) => *digest,
            None => expected_digest(pager, self.root, page_id)?,
        };
        let mut known = self.known.lock();
        if known.len() >= KNOWN_DIGESTS_MAX {
            known.clear();
        }
        known.insert(page_id, digest);
        Ok(digest)
    }

    fn verified_page<P: PageStore>(&self, pager: &P, page_id: u32) -> Result<Page, PagerError> {
        verified_page(pager, self.expected(pager, page_id)?, page_id)
    }

    fn set<P: PageStore>(
        &mut self,
        pager: &P,
        page_id: u32,
        digest: [u8; 32],
    ) -> Result<(), PagerError> {
        self.begin_mutation()?;
        let known = self.known.get_mut();
        if digest == [0; 32] {
            known.remove(&page_id);
        } else {
            if known.len() >= KNOWN_DIGESTS_MAX {
                known.clear();
            }
            known.insert(page_id, digest);
        }
        self.pending.insert(page_id, digest);
        if self.pending.len() >= PENDING_ENTRIES_MAX {
            self.write_pending(pager)?;
        }
        Ok(())
    }

    /// Write the entries held back by `set` into the proof nodes, one node at
    /// a time. An error leaves the nodes partly written: the store fences.
    fn write_pending<P: PageStore>(&mut self, pager: &P) -> Result<(), PagerError> {
        let pending: Vec<_> = std::mem::take(&mut self.pending).into_iter().collect();
        for node in pending.chunk_by(|left, right| left.0 >> 8 == right.0 >> 8) {
            self.root.reference = update_path(pager, self.root, node)?;
        }
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
            proof: Some(parking_lot::RwLock::new(ProofState::new(root))),
            failed: AtomicBool::new(false),
        })
    }

    pub(super) fn prepare_legacy(&self, pages: &[u32]) -> Result<Self, PagerError> {
        if self.proof.is_some() {
            return Err(invalid(0, "legacy preparation requires an unverified tree"));
        }
        let prepared = Self::verified(Arc::clone(&self.inner), PageProofRoot::default())?;
        {
            let mut state = prepared.proof.as_ref().expect("verified state").write();
            for &id in pages {
                let page = self.inner.read_page(id)?;
                if page.page_id() != id {
                    return Err(invalid(id, "legacy preparation found a misdirected page"));
                }
                page.verify_checksum()?;
                state.set(
                    self.inner.as_ref(),
                    id,
                    page_proof::page_digest(page.as_bytes()),
                )?;
            }
            state.write_pending(self.inner.as_ref())?;
            state.sealed = true;
        }
        Ok(prepared)
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
        let _ownership = self.inner.ownership_guard();
        let mut state = proof.write();
        self.available()?;
        let written = state.write_pending(self.inner.as_ref());
        self.finish(written)?;
        state.sealed = true;
        Ok(Some(state.root))
    }

    pub(super) fn reset_empty(&self) -> Result<(), PagerError> {
        let Some(proof) = &self.proof else {
            return Ok(());
        };
        let _ownership = self.inner.ownership_guard();
        let result = (|| {
            let pages = self.proof_pages()?;
            let mut state = proof.write();
            self.available()?;
            for id in pages {
                // The host's deferred fence retains all previously published paths.
                self.inner.free_page(id)?;
            }
            *state = ProofState::new(PageProofRoot::default());
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
        // Walks proof pages, any of which may have to be loaded.
        let _ownership = self.inner.ownership_guard();
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

/// Reads through a store without ever waiting for its ownership epoch, and
/// records whether a read would have had to.
struct WithoutWaiting<'a, P> {
    inner: &'a P,
    would_wait: std::cell::Cell<bool>,
}

impl<P: PageStore> PageStore for WithoutWaiting<'_, P> {
    fn read_page(&self, page_id: u32) -> Result<Page, PagerError> {
        match self.inner.try_read_page(page_id)? {
            Some(page) => Ok(page),
            None => {
                self.would_wait.set(true);
                Err(invalid(page_id, "page read would wait for a publication"))
            }
        }
    }
    fn write_page(&self, page_id: u32, page: Page) -> Result<(), PagerError> {
        self.inner.write_page(page_id, page)
    }
    fn allocate_page(&self, kind: PageType) -> Result<Page, PagerError> {
        self.inner.allocate_page(kind)
    }
    fn free_page(&self, page_id: u32) -> Result<(), PagerError> {
        self.inner.free_page(page_id)
    }
    fn page_count(&self) -> Result<u32, PagerError> {
        self.inner.page_count()
    }
}

// Lock order: the store's ownership epoch, then this tree's proof lock.
//
// A publication holds the epoch for its whole duration and takes the proof
// lock inside it (to seal, or to write pages). So nobody may wait for the
// epoch while holding the proof lock: a reader whose page is not cached must
// let go of the proof lock, enter the epoch, and take the lock again. Holding
// it across that wait deadlocks with a publication sealing the same tree.
impl<P: PageStore> PageStore for TreePageStore<P> {
    fn read_page(&self, page_id: u32) -> Result<Page, PagerError> {
        let Some(proof) = &self.proof else {
            return self.inner.read_page(page_id);
        };
        {
            let state = proof.read();
            self.available()?;
            let cached = WithoutWaiting {
                inner: self.inner.as_ref(),
                would_wait: std::cell::Cell::new(false),
            };
            let result = state.verified_page(&cached, page_id);
            if !cached.would_wait.get() {
                return self.finish(result);
            }
        }
        let _ownership = self.inner.ownership_guard();
        let state = proof.read();
        self.available()?;
        self.finish(state.verified_page(self.inner.as_ref(), page_id))
    }

    fn write_page(&self, page_id: u32, mut page: Page) -> Result<(), PagerError> {
        let Some(proof) = &self.proof else {
            return self.inner.write_page(page_id, page);
        };
        let _ownership = self.inner.ownership_guard();
        let mut state = proof.write();
        self.available()?;
        let result = (|| {
            state.verified_page(self.inner.as_ref(), page_id)?;
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
        let _ownership = self.inner.ownership_guard();
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
        let _ownership = self.inner.ownership_guard();
        let mut state = proof.write();
        self.available()?;
        let result = (|| {
            state.verified_page(self.inner.as_ref(), page_id)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, AtomicUsize};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Pages {
        next: AtomicU32,
        pages: Mutex<HashMap<u32, Page>>,
        node_writes: AtomicUsize,
    }

    impl PageStore for Pages {
        fn read_page(&self, id: u32) -> Result<Page, PagerError> {
            self.pages
                .lock()
                .expect("pages")
                .get(&id)
                .cloned()
                .ok_or_else(|| invalid(id, "missing page"))
        }
        fn write_page(&self, id: u32, page: Page) -> Result<(), PagerError> {
            if page.page_type()? == PageType::NativeMeta {
                self.node_writes.fetch_add(1, AtomicOrdering::Relaxed);
            }
            self.pages.lock().expect("pages").insert(id, page);
            Ok(())
        }
        fn allocate_page(&self, kind: PageType) -> Result<Page, PagerError> {
            let id = self.next.fetch_add(1, AtomicOrdering::Relaxed) + 1;
            Ok(Page::new(kind, id))
        }
        fn free_page(&self, id: u32) -> Result<(), PagerError> {
            self.pages.lock().expect("pages").remove(&id);
            Ok(())
        }
        fn page_count(&self) -> Result<u32, PagerError> {
            Ok(self.next.load(AtomicOrdering::Relaxed) + 1)
        }
        fn enable_deferred_reclamation(&self) -> Result<(), PagerError> {
            Ok(())
        }
    }

    fn store(pages: &Arc<Pages>, root: PageProofRoot) -> TreePageStore<Pages> {
        TreePageStore::verified(Arc::clone(pages), root).expect("proof store")
    }

    fn node_writes(pages: &Pages) -> usize {
        pages.node_writes.load(AtomicOrdering::Relaxed)
    }

    fn leaf(store: &TreePageStore<Pages>) -> u32 {
        store
            .allocate_page(PageType::BTreeLeaf)
            .expect("allocate")
            .page_id()
    }

    fn rewrite(store: &TreePageStore<Pages>, id: u32, byte: u8) {
        let mut page = store.read_page(id).expect("proven page");
        page.content_mut()[..8].fill(byte);
        store.write_page(id, page).expect("rewrite");
    }

    #[test]
    fn a_page_rewritten_in_one_generation_reaches_its_proof_node_once() {
        let pages = Arc::new(Pages::default());
        let live = store(&pages, PageProofRoot::default());
        let id = leaf(&live);
        for byte in 1..=50 {
            rewrite(&live, id, byte);
        }
        assert_eq!(node_writes(&pages), 0, "an entry reached a node early");
        let root = live.seal().expect("seal").expect("proof");
        assert_eq!(node_writes(&pages), 4, "one node per proof level");

        // A store that remembers nothing proves the page from the nodes.
        let cold = store(&pages, root);
        assert_eq!(cold.read_page(id).expect("cold proof").content()[0], 50);

        // The next generation copies each node once, however many rewrites.
        for byte in 51..=60 {
            rewrite(&live, id, byte);
        }
        let next = live.seal().expect("seal").expect("proof");
        assert_eq!(node_writes(&pages), 8);
        assert_eq!(
            store(&pages, next).read_page(id).expect("next").content()[0],
            60
        );
    }

    #[test]
    fn entries_that_share_a_proof_node_rewrite_it_together() {
        let pages = Arc::new(Pages::default());
        let live = store(&pages, PageProofRoot::default());
        let ids: Vec<u32> = (0..300).map(|_| leaf(&live)).collect();
        let root = live.seal().expect("seal").expect("proof");
        // Pages 1..=255 share one leaf node and 256..=300 another; each group
        // writes its leaf node and the three levels above it.
        assert_eq!(node_writes(&pages), 8);
        let cold = store(&pages, root);
        for &id in &ids {
            cold.read_page(id).expect("cold proof");
        }
    }

    #[test]
    fn a_stale_image_fails_a_remembered_digest() {
        let pages = Arc::new(Pages::default());
        let live = store(&pages, PageProofRoot::default());
        let id = leaf(&live);
        rewrite(&live, id, 1);
        let old = pages.read_page(id).expect("first image");
        rewrite(&live, id, 2);
        live.read_page(id).expect("remembered and current");
        // The file gives back the image it held before the last write.
        pages.pages.lock().expect("pages").insert(id, old);
        assert!(live.read_page(id).is_err(), "stale image was accepted");
    }

    #[test]
    fn a_freed_page_leaves_the_tree_before_its_entry_reaches_a_node() {
        let pages = Arc::new(Pages::default());
        let live = store(&pages, PageProofRoot::default());
        let (kept, freed) = (leaf(&live), leaf(&live));
        live.free_page(freed).expect("free");
        let root = live.seal().expect("seal").expect("proof");
        let cold = store(&pages, root);
        cold.read_page(kept).expect("kept page");
        pages
            .write_page(freed, Page::new(PageType::BTreeLeaf, freed))
            .expect("reuse the address");
        assert!(cold.read_page(freed).is_err(), "freed page was proven");
    }

    #[test]
    fn forgetting_remembered_digests_loses_no_entry() {
        let pages = Arc::new(Pages::default());
        let live = store(&pages, PageProofRoot::default());
        // More pages than the store remembers and than it holds back, so both
        // limits are crossed with entries on every side of them.
        let ids: Vec<u32> = (0..PENDING_ENTRIES_MAX + KNOWN_DIGESTS_MAX / 2)
            .map(|_| leaf(&live))
            .collect();
        for &id in ids.iter().step_by(7) {
            rewrite(&live, id, 9);
        }
        for &id in &ids {
            live.read_page(id).expect("live proof");
        }
        let root = live.seal().expect("seal").expect("proof");
        let cold = store(&pages, root);
        for (index, &id) in ids.iter().enumerate() {
            let page = cold.read_page(id).expect("cold proof");
            assert_eq!(page.content()[0], if index % 7 == 0 { 9 } else { 0 });
        }
    }
}
