use super::pager::{MemoryFile, Operation};
use reddb_engine::storage::{
    page::{Page, PageType, PAGE_SIZE},
    pager::{NoWal, Pager, PagerConfig},
    pager_io,
};
use reddb_file::{file_format as f, PAGED_SUPERBLOCK_SLOT_SIZE};

const HOME: u32 = pager_io::FIRST_ALLOCATABLE_PAGE;

fn page(value: &[u8]) -> Page {
    let mut page = Page::new(PageType::BTreeLeaf, HOME);
    page.insert_cell(b"key", value).expect("fixture cell");
    page.update_checksum();
    page
}

fn baseline() -> Vec<u8> {
    let mut file = MemoryFile::default();
    file.incremental_sync = true;
    let pager = Pager::open_owned(
        file,
        "sealed-dwb.rdb".into(),
        PagerConfig::default(),
        NoWal,
        true,
    )
    .expect("new pager");
    pager
        .fence_required_capabilities(f::FILE_CAPABILITY_DWB_PUBLICATION)
        .expect("fence DWB authority");
    let allocated = pager
        .allocate_page(PageType::BTreeLeaf)
        .expect("allocate home");
    assert_eq!(allocated.page_id(), HOME);
    pager
        .write_page(HOME, page(b"before"))
        .expect("initial image");
    pager.sync().expect("publish initial image");
    pager
        .with_file(|file| file.durable.borrow().clone())
        .expect("durable baseline")
}

fn memory(bytes: Vec<u8>) -> MemoryFile {
    let mut file = MemoryFile::from_bytes(bytes);
    file.incremental_sync = true;
    file
}

fn value(file: &mut MemoryFile) -> Vec<u8> {
    pager_io::read_page(file, HOME, true)
        .expect("healthy home")
        .read_cell(0)
        .expect("fixture cell survives")
        .1
        .to_vec()
}

fn damage_slot(bytes: &mut [u8], slot: usize) {
    let start = slot * PAGED_SUPERBLOCK_SLOT_SIZE;
    bytes[start..start + PAGED_SUPERBLOCK_SLOT_SIZE].fill(0);
}

#[test]
fn guarded_dwb_survives_each_write_sync_and_superblock_fallback() {
    let started = std::time::Instant::now();
    let baseline = baseline();
    let updated = [(HOME, page(b"after"))];
    let mut successful = memory(baseline.clone());
    pager_io::write_pages(&mut successful, &updated, true).expect("complete guarded write");
    let operations = successful.operations.borrow().clone();
    let first_home_write = operations.iter().position(|operation| {
        matches!(operation, Operation::Write(offset, _) if *offset == u64::from(HOME) * PAGE_SIZE as u64)
    }).expect("guarded home write") + 1;
    let staging_write = operations.iter().position(|operation| {
        matches!(operation, Operation::Write(offset, length) if *offset == pager_io::dwb_zone_offset() && *length > PAGE_SIZE)
    }).expect("complete staging write") + 1;
    for fail_at in 1..=operations.len() {
        for retain_pending in [false, true] {
            let mut interrupted = memory(baseline.clone());
            interrupted.fail_at = Some(fail_at);
            assert!(pager_io::write_pages(&mut interrupted, &updated, true).is_err());
            assert_eq!(interrupted.operations.borrow().len(), fail_at);
            let bytes = if retain_pending {
                interrupted.bytes.into_inner()
            } else {
                interrupted.durable.into_inner()
            };
            for damaged_slot in [None, Some(0), Some(1)] {
                let mut snapshot = bytes.clone();
                if let Some(slot) = damaged_slot {
                    damage_slot(&mut snapshot, slot);
                }
                let mut reopened = memory(snapshot);
                pager_io::recover_from_dwb(&mut reopened).unwrap_or_else(|error| {
                    panic!("boundary {fail_at}, pending {retain_pending}, slot {damaged_slot:?}: {error}")
                });
                let recovered = value(&mut reopened);
                if fail_at >= first_home_write {
                    assert_eq!(
                        recovered, b"after",
                        "durable frame must restore the newer image"
                    );
                } else {
                    assert!(recovered == b"before" || recovered == b"after");
                }
                let stable = reopened.bytes.borrow().clone();
                pager_io::recover_from_dwb(&mut reopened).expect("idempotent recovery");
                assert!(reopened.bytes.borrow().as_slice() == stable);
                pager_io::write_pages(&mut reopened, &[(HOME, page(b"progress"))], true)
                    .expect("healthy storage must make progress");
                let mut cold = memory(reopened.durable.into_inner());
                pager_io::recover_from_dwb(&mut cold).expect("cold reopen after progress");
                assert_eq!(value(&mut cold), b"progress");
            }
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(120),
            "recovery stalled at {fail_at}"
        );
    }
    for prefix in [0, 1, 4, 5, 8, 9, 12, 17] {
        let mut interrupted = memory(baseline.clone());
        interrupted.fail_at = Some(staging_write);
        interrupted.fail_prefix = Some(prefix);
        assert!(pager_io::write_pages(&mut interrupted, &updated, true).is_err());
        for bytes in [
            interrupted.bytes.into_inner(),
            interrupted.durable.into_inner(),
        ] {
            let mut reopened = memory(bytes);
            pager_io::recover_from_dwb(&mut reopened)
                .unwrap_or_else(|error| panic!("staging prefix {prefix}: {error}"));
            let recovered = value(&mut reopened);
            assert!(recovered == b"before" || recovered == b"after");
            pager_io::write_pages(&mut reopened, &[(HOME, page(b"progress"))], true)
                .expect("progress after incomplete staging header");
            assert_eq!(value(&mut reopened), b"progress");
        }
    }
}

#[test]
fn guarded_dwb_rejects_a_complete_old_frame_without_consuming_evidence() {
    let mut file = memory(baseline());
    let old = page(b"before");
    pager_io::write_pages(&mut file, &[(HOME, page(b"current"))], true).expect("newer image");
    let frame = reddb_file::encode_paged_dwb_frame([(HOME, old.as_bytes())]);
    let start = usize::try_from(pager_io::dwb_zone_offset()).expect("DWB offset");
    file.bytes.borrow_mut()[start..start + frame.len()].copy_from_slice(&frame);
    let evidence = file.bytes.borrow().clone();
    assert!(
        pager_io::read_dwb_pages(&mut file).is_err(),
        "read-only recovery must reject old authority"
    );
    let error = pager_io::recover_from_dwb(&mut file).expect_err("old complete frame");
    assert!(
        error.to_string().contains("DWB generation authority"),
        "{error}"
    );
    assert!(file.bytes.borrow().as_slice() == evidence);
    assert_eq!(value(&mut file), b"current");
}

#[test]
fn guarded_dwb_recovery_survives_a_second_crash_before_consumption() {
    let baseline = baseline();
    let mut interrupted = memory(baseline);
    interrupted.fail_write_range =
        Some(u64::from(HOME) * PAGE_SIZE as u64..u64::from(HOME + 1) * PAGE_SIZE as u64);
    assert!(pager_io::write_pages(&mut interrupted, &[(HOME, page(b"committed"))], true).is_err());
    let pending = interrupted.bytes.into_inner();
    let mut successful = memory(pending.clone());
    pager_io::recover_from_dwb(&mut successful).expect("measure actual recovery boundaries");
    let operations = successful.operations.borrow().clone();
    for (index, operation) in operations.iter().enumerate() {
        // Every superblock write/sync, home write/sync, and the first/last
        // clearing write cover the phase transitions without 64 identical cases.
        if matches!(operation, Operation::Write(offset, _) if *offset >= pager_io::dwb_zone_offset() && *offset < u64::from(pager_io::FIRST_ALLOCATABLE_PAGE - 1) * PAGE_SIZE as u64 && *offset != pager_io::dwb_zone_offset())
        {
            continue;
        }
        for retain_pending in [false, true] {
            let mut recovering = memory(pending.clone());
            recovering.fail_at = Some(index + 1);
            assert!(pager_io::recover_from_dwb(&mut recovering).is_err());
            let bytes = if retain_pending {
                recovering.bytes.into_inner()
            } else {
                recovering.durable.into_inner()
            };
            for damaged_slot in [None, Some(0), Some(1)] {
                let mut snapshot = bytes.clone();
                if let Some(slot) = damaged_slot {
                    damage_slot(&mut snapshot, slot);
                }
                let mut restarted = memory(snapshot);
                pager_io::recover_from_dwb(&mut restarted).unwrap_or_else(|error| panic!("second crash {index}, pending {retain_pending}, fallback {damaged_slot:?}: {error}"));
                assert_eq!(value(&mut restarted), b"committed");
                pager_io::write_pages(&mut restarted, &[(HOME, page(b"progress"))], true)
                    .expect("progress after repeated recovery failure");
                let mut cold = memory(restarted.durable.into_inner());
                pager_io::recover_from_dwb(&mut cold).expect("cold progress");
                assert_eq!(value(&mut cold), b"progress");
            }
        }
    }
}

#[test]
fn guarded_dwb_recovers_the_last_frame_without_rolling_back_previous_frames() {
    let count = pager_io::DWB_MAX_PAGES_PER_FRAME + 1;
    let pager = Pager::open_owned(
        memory(baseline()),
        "multi-frame.rdb".into(),
        PagerConfig::default(),
        NoWal,
        false,
    )
    .expect("allocator for all frames");
    for index in 1..count {
        let page = pager
            .allocate_page(PageType::BTreeLeaf)
            .expect("owned frame home");
        assert_eq!(
            page.page_id(),
            HOME + u32::try_from(index).expect("bounded address")
        );
    }
    pager.sync().expect("durable allocation boundary");
    let mut file = pager
        .with_file(|file| memory(file.durable.borrow().clone()))
        .expect("allocated fixture");
    drop(pager);
    let pages = |value: &[u8]| {
        (0..count)
            .map(|index| {
                let id = HOME + u32::try_from(index).expect("bounded fixture address");
                let mut page = Page::new(PageType::BTreeLeaf, id);
                page.insert_cell(b"key", value).expect("fixture cell");
                page.update_checksum();
                (id, page)
            })
            .collect::<Vec<_>>()
    };
    pager_io::write_pages(&mut file, &pages(b"before"), true).expect("seed every frame");
    let last = HOME + u32::try_from(count - 1).expect("last fixture address");
    file.fail_write_range =
        Some(u64::from(last) * PAGE_SIZE as u64..u64::from(last + 1) * PAGE_SIZE as u64);
    assert!(pager_io::write_pages(&mut file, &pages(b"after"), true).is_err());
    let mut reopened = memory(file.bytes.into_inner());
    pager_io::recover_from_dwb(&mut reopened).expect("recover the second frame");
    let mut cold = memory(reopened.durable.into_inner());
    pager_io::recover_from_dwb(&mut cold).expect("repeated multi-frame recovery");
    for index in 0..count {
        let id = HOME + u32::try_from(index).expect("fixture address");
        let page = pager_io::read_page(&mut cold, id, true).expect("each home survives");
        assert_eq!(page.read_cell(0).expect("cell").1, b"after");
    }
}
