use super::pager::{MemoryFile, Operation};
use reddb_engine::storage::{
    page::{Page, PageType, PAGE_SIZE},
    pager::{NoWal, Pager, PagerConfig},
    pager_io,
};
use reddb_file::file_format as f;

fn vault(value: &[u8]) -> Page {
    let mut page = Page::new(PageType::Vault, 2);
    page.insert_cell(b"key", value).expect("fixture vault cell");
    page.update_checksum();
    page
}

fn ready_snapshot() -> Vec<u8> {
    let pager = Pager::open_owned(
        MemoryFile::default(),
        "truncated-dwb.rdb".into(),
        PagerConfig::default(),
        NoWal,
        true,
    )
    .expect("create guarded fixture");
    pager
        .fence_required_capabilities(f::FILE_CAPABILITY_DWB_PUBLICATION)
        .expect("fence guarded recovery");
    pager.write_page(2, vault(b"before")).expect("seed home");
    pager.sync().expect("durable old home");
    let baseline = pager
        .with_file(|file| file.durable.borrow().clone())
        .expect("durable fixture");
    let updated = [(2, vault(b"committed"))];
    let mut complete = MemoryFile::from_bytes(baseline.clone());
    pager_io::write_pages(&mut complete, &updated, true).expect("count write boundaries");
    let home_write = complete
        .operations
        .borrow()
        .iter()
        .position(|operation| {
            matches!(operation, Operation::Write(offset, _) if *offset == 2 * PAGE_SIZE as u64)
        })
        .expect("home write follows durable Ready fences")
        + 1;
    let mut interrupted = MemoryFile::from_bytes(baseline);
    interrupted.fail_at = Some(home_write);
    interrupted.fail_prefix = Some(0);
    assert!(pager_io::write_pages(&mut interrupted, &updated, true).is_err());
    let bytes = interrupted.durable.into_inner();
    let selected = reddb_file::select_paged_superblock(&bytes).expect("valid Ready header");
    let start = selected.copy_index * reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE;
    assert_eq!(
        f::database_header_dwb_publication(
            &bytes[start..start + reddb_file::PAGED_SUPERBLOCK_SLOT_SIZE]
        )
        .expect("publication")
        .expect("guarded authority")
        .phase,
        f::DwbPhase::Ready
    );
    let mut recovered = MemoryFile::from_bytes(bytes.clone());
    pager_io::recover_from_dwb(&mut recovered).expect("complete extent recovers");
    let page = pager_io::read_page(&mut recovered, 2, true).expect("recovered vault");
    assert_eq!(page.read_cell(0).expect("recovered cell").1, b"committed");
    bytes
}

fn cutoffs() -> [usize; 4] {
    let start = usize::try_from(pager_io::dwb_zone_offset()).expect("bounded DWB offset");
    [
        start,
        start + 4,
        start + reddb_file::PAGED_DWB_HEADER_SIZE + reddb_file::PAGED_DWB_ENTRY_SIZE - 1,
        start + pager_io::DWB_ZONE_BYTES - 1,
    ]
}

#[test]
fn guarded_recovery_rejects_a_truncated_reserved_extent_without_writes() {
    let ready = ready_snapshot();
    for cutoff in cutoffs() {
        let evidence = &ready[..cutoff];
        for read_only in [true, false] {
            let mut file = MemoryFile::from_bytes(evidence.to_vec());
            let result = if read_only {
                pager_io::read_dwb_pages(&mut file).map(|_| ())
            } else {
                pager_io::recover_from_dwb(&mut file)
            };
            assert!(
                result.is_err(),
                "truncated extent {cutoff}, reader {read_only}"
            );
            assert!(file.bytes.borrow().as_slice() == evidence);
            assert!(file.operations.borrow().is_empty());
        }
    }
}

#[test]
fn guarded_cold_open_rejects_truncation_before_serving_a_surviving_old_home() {
    let ready = ready_snapshot();
    for cutoff in cutoffs() {
        for read_only in [true, false] {
            let file = MemoryFile::from_bytes(ready[..cutoff].to_vec());
            let operations = file.operations.clone();
            let opened = Pager::open_owned(
                file,
                "truncated-dwb.rdb".into(),
                PagerConfig {
                    read_only,
                    ..PagerConfig::default()
                },
                NoWal,
                false,
            );
            if let Ok(pager) = opened {
                let home = pager.read_page(2).expect("surviving old vault is readable");
                assert_eq!(home.read_cell(0).expect("old cell").1, b"before");
                panic!("cold open served stale home: extent {cutoff}, reader {read_only}");
            }
            assert!(
                operations.borrow().is_empty(),
                "failed open must preserve evidence"
            );
        }
    }
}

#[test]
fn unguarded_short_bootstrap_preserves_legacy_recovery_behavior() {
    let pager = Pager::open_owned(
        MemoryFile::default(),
        "legacy-bootstrap.rdb".into(),
        PagerConfig::default(),
        NoWal,
        true,
    )
    .expect("legacy bootstrap");
    let bytes = pager
        .with_file(|file| file.durable.borrow().clone())
        .expect("legacy image");
    for cutoff in cutoffs() {
        let evidence = &bytes[..cutoff];
        let mut file = MemoryFile::from_bytes(evidence.to_vec());
        assert!(pager_io::read_dwb_pages(&mut file)
            .expect("legacy reader ignores an absent staging zone")
            .is_empty());
        pager_io::recover_from_dwb(&mut file).expect("legacy bootstrap is unchanged");
        assert!(file.bytes.borrow().as_slice() == evidence);
        assert!(file.operations.borrow().is_empty());
    }
}
