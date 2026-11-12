use super::*;

/// A failed tree is unverified; the number of affected records is unknown.
#[derive(Debug)]
pub struct PagedTreeFinding {
    pub collection: String,
    pub tree: String,
    pub root_page: u32,
    pub reason: String,
}

/// This is a tree-only diagnostic, not a complete store integrity check.
#[derive(Debug, Default)]
pub struct PagedTreeScrubReport {
    pub trees_verified: u64,
    pub records_verified: u64,
    pub findings: Vec<PagedTreeFinding>,
}

/// A physical scrub of a supported, closed and checkpointed standalone store.
/// If a tree fails, auxiliary checks are not considered complete.
#[derive(Debug)]
pub struct PagedScrubReport {
    pub trees: PagedTreeScrubReport,
    pub assets_verified: bool,
    /// Allocated pages not identified as free or owned by a verified structure.
    pub unclassified_pages: u64,
}

/// Verify the supported offline layout without writing to the source. Pending
/// recovery, vaults without valid credentials and unsupported sidecars fail closed.
pub fn scrub_paged(path: &Path) -> Result<PagedScrubReport> {
    let canonical_path = path.canonicalize()?;
    let path = canonical_path.as_path();
    let source = Arc::new(Source::open(path)?);
    super::salvage::verify_supported_source_with_audit(path, &source)?;
    let (trees, catalog) = inspect_all_trees(&source)?;
    if !trees.findings.is_empty() {
        return Ok(PagedScrubReport {
            trees,
            assets_verified: false,
            unclassified_pages: source.unclaimed_pages(),
        });
    }
    let internal = super::internal_metadata::read(&source, &catalog)?;
    let mut assets = super::assets::Assets::for_verification(&source);
    assets.vault()?;
    assets.physical()?;
    assets.metadata(
        path,
        None,
        internal.as_ref().map(|record| record.document.as_str()),
    )?;
    catalog.claim_retired(&source)?;
    Ok(PagedScrubReport {
        trees,
        assets_verified: true,
        unclassified_pages: source.unclaimed_pages(),
    })
}

pub(super) fn verify_recovery_vector_pages(path: &Path) -> Result<()> {
    let source = Arc::new(Source::open(path)?);
    super::salvage::verify_supported_source(path, &source)?;
    let (trees, catalog) = inspect_all_trees(&source)?;
    ensure(
        trees.findings.is_empty(),
        "recovered tree verification failed",
    )?;
    let internal = super::internal_metadata::read(&source, &catalog)?;
    let mut assets = super::assets::Assets::for_verification(&source);
    assets.vault()?;
    assets.physical()?;
    assets.metadata(
        path,
        None,
        internal.as_ref().map(|record| record.document.as_str()),
    )?;
    catalog.claim_retired(&source)?;
    source.verify_unclaimed_vector_pages()
}

/// Inspect trees in a closed, checkpointed paged store without writing to it.
/// Vault, physical assets, logical index correctness and replica state are not
/// verified. The same layout and WAL restrictions as offline compaction apply.
pub fn scrub_paged_trees(path: &Path) -> Result<PagedTreeScrubReport> {
    let source = Arc::new(Source::open(path)?);
    inspect_all_trees(&source).map(|(report, _)| report)
}

fn inspect_all_trees(source: &Arc<Source>) -> Result<(PagedTreeScrubReport, Catalog)> {
    let catalog = Catalog::read(source)?;
    ensure(
        !source.header.requires_disk_residency() || catalog.resume.is_some(),
        "prepared disk-resident file has no resume catalog",
    )?;
    let mut report = PagedTreeScrubReport::default();
    for collection in &catalog.roots {
        let state = catalog
            .resume
            .as_ref()
            .and_then(|r| r.collections.get(&collection.collection));
        if catalog.resume.is_some() {
            ensure(state.is_some(), "collection missing from resume catalog")?;
        }
        if let Some(state) = state {
            ensure(
                state.root_page == collection.root_page,
                "collection root differs from resume catalog",
            )?;
        }
        inspect_tree(
            source,
            &mut report,
            &collection.collection,
            "collection",
            collection.root_page,
            state.map(|s| s.entities),
        );
    }
    if let Some(resume) = &catalog.resume {
        ensure(
            resume.collections.len() == catalog.roots.len(),
            "resume catalog has unknown collections",
        )?;
        for (name, state) in &resume.collections {
            if let Some(root) = state.key_index_root {
                inspect_tree(source, &mut report, name, "key index", root, None);
            }
            for (index, state) in &state.secondary_indices {
                inspect_tree(
                    source,
                    &mut report,
                    name,
                    &format!("secondary index {index}"),
                    state.root_page,
                    None,
                );
            }
        }
    }
    Ok((report, catalog))
}

fn inspect_tree(
    source: &Arc<Source>,
    report: &mut PagedTreeScrubReport,
    collection: &str,
    name: &str,
    root: u32,
    declared_count: Option<u64>,
) {
    let result = (|| -> Result<u64> {
        if root == 0 {
            ensure(
                declared_count.unwrap_or(0) == 0,
                "empty tree has nonzero declared count",
            )?;
            return Ok(0);
        }
        let tree = source.tree(root)?;
        let (expected, _) = inventory_tree(source, &tree, true)?;
        let mut cursor = tree.cursor_first()?;
        let mut previous: Option<Vec<u8>> = None;
        let mut count = 0u64;
        while let Some((key, _value)) = cursor.next_with_budget(32 * 1024 * 1024)? {
            ensure(
                previous.as_ref().is_none_or(|old| old < &key),
                "tree keys are not strictly ordered",
            )?;
            previous = Some(key);
            count += 1;
            ensure(
                count <= expected,
                "tree cursor exceeds structural inventory",
            )?;
        }
        ensure(count == expected, "tree cursor omits owned records")?;
        if let Some(declared) = declared_count {
            ensure(
                count == declared,
                "collection count disagrees with resume catalog",
            )?;
        }
        Ok(count)
    })();
    match result {
        Ok(count) => {
            report.trees_verified += 1;
            report.records_verified += count;
        }
        Err(error) => report.findings.push(PagedTreeFinding {
            collection: collection.to_owned(),
            tree: name.to_owned(),
            root_page: root,
            reason: error.to_string(),
        }),
    }
}
