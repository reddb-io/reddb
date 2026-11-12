//! Construct the pre-proof native byte contract in a new test file. This is
//! fixture generation, never a downgrade path for an existing database.
use super::*;

pub(crate) fn rewrite(source_path: &Path, destination: &Path) -> Result<()> {
    ensure(
        !destination.exists(),
        "legacy fixture destination must be new",
    )?;
    let source = Arc::new(Source::open(source_path)?);
    let catalog = Catalog::read(&source)?;
    let target = Arc::new(Pager::open(
        destination,
        PagerConfig {
            disk_residency: true,
            ..PagerConfig::default()
        },
    )?);
    target.fence_required_capabilities(f::FILE_CAPABILITY_DISK_RESIDENCY)?;
    let mut roots = BTreeMap::new();
    let mut repairs = 0;
    let mut proofs = f::page_proof::CatalogProofs::new();
    for root in &catalog.roots {
        rewrite_tree(
            &source,
            &target,
            root.root_page,
            &mut roots,
            &mut repairs,
            &mut proofs,
        )?;
    }
    if let Some(resume) = &catalog.resume {
        for state in resume.collections.values() {
            for root in state.key_index_root.into_iter().chain(
                state
                    .secondary_indices
                    .values()
                    .map(|index| index.root_page),
            ) {
                rewrite_tree(
                    &source,
                    &target,
                    root,
                    &mut roots,
                    &mut repairs,
                    &mut proofs,
                )?;
            }
        }
    }
    ensure(
        proofs.is_empty(),
        "legacy fixture unexpectedly enabled proofs",
    )?;
    let mut assets = assets::Assets::new(&source, &target);
    assets.vault()?;
    let physical = assets.physical()?;
    let internal = internal_metadata::read(&source, &catalog)?;
    let document = assets.metadata(
        source_path,
        Some(destination),
        internal.as_ref().map(|record| record.document.as_str()),
    )?;
    if let (Some(record), Some(document)) = (internal, document) {
        let root = record.root;
        let key = record.key.clone();
        let value = record.rewrite(document)?;
        let tree = BTree::with_root(Arc::clone(&target), roots[&root]);
        tree.upsert(&key, &value)?;
        roots.insert(root, tree.root_page_id());
    }
    // Keep a genuine detached legacy tree in the durable retirement ledger,
    // including the overflow chain of a serialized application record.
    let spill = catalog
        .roots
        .iter()
        .find(|root| root.collection == "spills")
        .ok_or("legacy fixture needs an overflow collection")?;
    let original = source.tree(spill.root_page)?;
    let retired = BTree::new(Arc::clone(&target));
    let mut cursor = original.cursor_first()?;
    while let Some((key, value)) = cursor.next_with_budget(1024 * 1024)? {
        retired.insert(&key, &value)?;
    }
    let mut data = catalog.encode_with_proofs(&roots, None)?;
    let mut pos = f::METADATA_HEADER_BYTES;
    for _ in 0..catalog.header.collection_count {
        f::decode_native_paged_collection_root(&data, &mut pos)?;
    }
    let count = u32::from_le_bytes(data[pos..pos + 4].try_into()?);
    pos += 4;
    for _ in 0..count {
        f::decode_native_paged_cross_ref(&data, &mut pos)?;
    }
    let start = pos;
    ensure(
        f::decode_native_paged_retired_trees(&data, &mut pos)?.is_empty(),
        "fixture retirement baseline",
    )?;
    let mut encoded = Vec::new();
    f::encode_native_paged_retired_trees(&mut encoded, &[retired.root_page_id()])?;
    data.splice(start..pos, encoded);
    ensure(
        data.len() <= PAGE_SIZE - HEADER_SIZE,
        "legacy fixture catalog overflow",
    )?;
    let mut page = Page::new(PageType::Header, 1);
    page.content_mut()[..data.len()].copy_from_slice(&data);
    target.write_page(1, page)?;
    target.update_physical_header(physical)?;
    target.sync()?;
    target.write_header_and_sync()?;
    target.write_header_and_sync()?;
    let header = target.header()?;
    ensure(
        target.manifest_reference()?.page_id == 0
            && header.required_capabilities == f::FILE_CAPABILITY_DISK_RESIDENCY,
        "fixture must retain the pre-proof bootstrap and reader contract",
    )?;
    Ok(())
}
