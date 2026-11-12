use super::pager::MemoryFile;
use reddb_engine::storage::{
    btree::BTree,
    page::{Page, PageType, PAGE_SIZE},
    pager::{NoWal, Pager, PagerConfig},
};
use reddb_file::page_proof::PageProofRoot;
use std::sync::Arc;

fn open(bytes: Option<Vec<u8>>) -> Arc<Pager<MemoryFile, NoWal>> {
    let create = bytes.is_none();
    let mut file = bytes.map(MemoryFile::from_bytes).unwrap_or_default();
    file.incremental_sync = true;
    Arc::new(
        Pager::open_owned(
            file,
            "page-proofs.rdb".into(),
            PagerConfig {
                cache_size: 32,
                ..PagerConfig::default()
            },
            NoWal,
            create,
        )
        .expect("open generation-proof fixture"),
    )
}

fn durable(pager: &Pager<MemoryFile, NoWal>) -> Vec<u8> {
    pager.sync().expect("persist data and proof paths");
    pager
        .with_file(|file| file.durable.borrow().clone())
        .expect("snapshot persisted file")
}

fn value(key: u64) -> Vec<u8> {
    vec![u8::try_from(key % 251).expect("bounded byte"); 128]
}

fn replace(bytes: &mut [u8], page_id: u32, page: &Page) {
    assert_eq!(page.page_id(), page_id);
    page.verify_checksum().expect("fault must pass its own CRC");
    let offset = usize::try_from(page_id).expect("page address") * PAGE_SIZE;
    bytes[offset..offset + PAGE_SIZE].copy_from_slice(page.as_bytes());
}

fn restored(
    bytes: Vec<u8>,
    root: u32,
    proof: PageProofRoot,
) -> (
    Arc<Pager<MemoryFile, NoWal>>,
    BTree<Pager<MemoryFile, NoWal>>,
) {
    let pager = open(Some(bytes));
    let tree =
        BTree::with_verified_root(Arc::clone(&pager), root, proof).expect("sealed generation");
    (pager, tree)
}

#[test]
fn page_proofs_cover_cold_point_cursor_overflow_and_prior_valid_images() {
    let pager = open(None);
    let tree = BTree::new_verified(Arc::clone(&pager)).expect("create authenticated tree");
    for key in 0_u64..512 {
        tree.insert(&key.to_be_bytes(), &value(key))
            .expect("initial row");
    }
    let (old_root, old_proof) = tree
        .seal_page_proofs()
        .expect("seal first generation")
        .expect("proof enabled");
    let old_interior = pager.read_page(old_root).expect("initial interior root");
    assert_eq!(
        old_interior.page_type().expect("root type"),
        PageType::BTreeInterior
    );
    let old_leaf = tree
        .retired_pages()
        .expect("reachable ownership")
        .into_iter()
        .map(|id| pager.read_page(id).expect("healthy page"))
        .find(|page| {
            page.page_type().expect("page type") == PageType::BTreeLeaf
                && page.content()[..4] == [0; 4]
        })
        .expect("first leaf");
    let _ = durable(&pager);

    tree.upsert(&0_u64.to_be_bytes(), &[77; 128])
        .expect("new value");
    // Copy-on-first-mutation must preserve the prior proof path for an
    // unchanged page. The changed page itself is no longer an old snapshot.
    let prior =
        BTree::with_verified_root(Arc::clone(&pager), old_root, old_proof).expect("prior proof");
    assert_eq!(
        prior
            .get(&400_u64.to_be_bytes())
            .expect("unchanged prior page"),
        Some(value(400))
    );
    drop(prior);
    for key in 512_u64..768 {
        tree.insert(&key.to_be_bytes(), &value(key))
            .expect("append across leaves");
    }
    let mut random = 0x4d595df4d0f33173_u64;
    let large: Vec<u8> = (0..32_000)
        .map(|_| {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            random.to_le_bytes()[0]
        })
        .collect();
    tree.insert(&999_u64.to_be_bytes(), &large)
        .expect("overflow row");
    let (root, proof) = tree
        .seal_page_proofs()
        .expect("seal second generation")
        .expect("proof enabled");
    assert_eq!(
        root, old_root,
        "fixture must modify the same interior address"
    );
    assert_ne!(
        pager.read_page(root).expect("new interior").as_bytes(),
        old_interior.as_bytes()
    );
    assert_ne!(
        pager
            .read_page(old_leaf.page_id())
            .expect("new first leaf")
            .as_bytes(),
        old_leaf.as_bytes()
    );
    let overflow = tree
        .retired_pages()
        .expect("complete ownership")
        .into_iter()
        .map(|id| pager.read_page(id).expect("healthy owned page"))
        .find(|page| page.page_type().expect("owned type") == PageType::Overflow)
        .expect("overflow fixture must spill");
    let proof_page = pager
        .read_page(proof.reference.page_id)
        .expect("proof root image");
    let bytes = durable(&pager);
    drop(tree);
    drop(pager);

    let (healthy_pager, healthy) = restored(bytes.clone(), root, proof);
    assert_eq!(
        healthy
            .get(&999_u64.to_be_bytes())
            .expect("cold overflow point read"),
        Some(large.clone())
    );
    assert_eq!(
        healthy
            .get(&0_u64.to_be_bytes())
            .expect("cold updated point read"),
        Some(vec![77; 128])
    );
    let mut cursor = healthy.cursor_first().expect("cold ordered cursor");
    for key in 0_u64..768 {
        let expected = if key == 0 { vec![77; 128] } else { value(key) };
        assert_eq!(
            cursor.next().expect("ordered row"),
            Some((key.to_be_bytes().to_vec(), expected))
        );
    }
    assert_eq!(
        cursor.next().expect("overflow cursor read"),
        Some((999_u64.to_be_bytes().to_vec(), large.clone()))
    );
    assert_eq!(cursor.next().expect("complete scan"), None);
    drop(cursor);
    drop(healthy);
    drop(healthy_pager);

    let mut damaged_overflow = overflow;
    damaged_overflow.content_mut()[8] ^= 1;
    damaged_overflow.update_checksum();
    let mut damaged_proof = proof_page;
    damaged_proof.content_mut()[16] ^= 1;
    damaged_proof.update_checksum();
    for (role, image, key) in [
        ("old leaf", old_leaf, 0_u64),
        ("old interior", old_interior, 0),
        ("overflow", damaged_overflow, 999),
        ("proof node", damaged_proof, 0),
    ] {
        let mut damaged = bytes.clone();
        replace(&mut damaged, image.page_id(), &image);
        let (reopened_pager, reopened) = restored(damaged.clone(), root, proof);
        let error = reopened.get(&key.to_be_bytes()).expect_err(role);
        assert!(
            error.to_string().contains("generation proof"),
            "{role}: {error}"
        );
        assert!(
            reopened.seal_page_proofs().is_err(),
            "a rejected image must never be blessed"
        );
        assert_eq!(
            reopened_pager
                .with_file(|file| file.bytes.borrow().clone())
                .expect("source evidence"),
            damaged
        );
        drop(reopened);
        drop(reopened_pager);

        let offset = usize::try_from(image.page_id()).expect("page offset") * PAGE_SIZE;
        let current =
            Page::from_slice(&bytes[offset..offset + PAGE_SIZE]).expect("trusted current image");
        let frame = reddb_file::encode_paged_dwb_frame([(image.page_id(), current.as_bytes())]);
        let staging = usize::try_from(reddb_engine::storage::pager_io::dwb_zone_offset())
            .expect("DWB offset");
        damaged[staging..staging + frame.len()].copy_from_slice(&frame);
        let (recovered_pager, recovered) = restored(damaged, root, proof);
        let expected = if key == 999 {
            large.clone()
        } else {
            vec![77; 128]
        };
        assert_eq!(
            recovered
                .get(&key.to_be_bytes())
                .expect("verified DWB recovery"),
            Some(expected)
        );
        let stable = durable(&recovered_pager);
        drop(recovered);
        drop(recovered_pager);
        let (_, again) = restored(stable, root, proof);
        assert!(again
            .get(&key.to_be_bytes())
            .expect("repeated cold reopen")
            .is_some());
    }
}
