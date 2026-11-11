#![allow(dead_code)]

#[path = "support/codec_corpus.rs"]
mod codec_corpus;
#[path = "support/pager.rs"]
mod support;

use reddb_engine::storage::{
    page::{Page, PageType},
    pager::{NoWal, Pager, PagerConfig},
    pager_io::FIRST_ALLOCATABLE_PAGE,
};
use support::MemoryFile;

fn open(bytes: Option<Vec<u8>>) -> Pager<MemoryFile, NoWal> {
    let create = bytes.is_none();
    Pager::open_owned(
        bytes.map(MemoryFile::from_bytes).unwrap_or_default(),
        "batch.rdb".into(),
        PagerConfig {
            cache_size: 2,
            ..PagerConfig::default()
        },
        NoWal,
        create,
    )
    .expect("native pager")
}

fn page(id: u32, value: u8) -> Page {
    let mut page = Page::new(PageType::BTreeLeaf, id);
    page.as_bytes_mut()[128] = value;
    page
}

fn prepare(pager: &Pager<MemoryFile, NoWal>) -> Result<(), Box<dyn std::error::Error>> {
    for id in FIRST_ALLOCATABLE_PAGE..FIRST_ALLOCATABLE_PAGE + 4 {
        pager.write_page(id, page(id, 2))?;
    }
    for id in FIRST_ALLOCATABLE_PAGE..FIRST_ALLOCATABLE_PAGE + 4 {
        assert_eq!(pager.read_page(id)?.as_bytes()[128], 2);
        assert_eq!(
            pager
                .with_file(|file| reddb_engine::storage::pager_io::read_page(file, id, true))??
                .as_bytes()[128],
            1
        );
    }
    Ok(())
}

#[test]
fn prepared_pages_are_atomic_at_every_write_and_barrier() {
    let pager = open(None);
    for id in FIRST_ALLOCATABLE_PAGE..FIRST_ALLOCATABLE_PAGE + 4 {
        assert_eq!(
            pager
                .allocate_page(PageType::BTreeLeaf)
                .expect("allocate")
                .page_id(),
            id
        );
        pager.write_page(id, page(id, 1)).expect("seed");
    }
    pager.sync().expect("baseline");
    let baseline = pager
        .with_file(|file| file.durable.borrow().clone())
        .expect("image");
    drop(pager);

    let pager = open(Some(baseline.clone()));
    pager.begin_write_batch().expect("begin");
    pager
        .with_file(|file| file.operations.borrow_mut().clear())
        .expect("reset");
    prepare(&pager).expect("prepare");
    pager.commit_write_batch().expect("commit");
    let operations = pager
        .with_file(|file| file.operations.borrow().len())
        .expect("operations");
    assert!(operations > 10);
    drop(pager);

    for fail_at in 1..=operations {
        let pager = open(Some(baseline.clone()));
        pager.begin_write_batch().expect("begin");
        pager
            .with_file(|file| {
                file.operations.borrow_mut().clear();
                file.fail_at = Some(fail_at);
                file.fail_prefix = Some(1);
            })
            .expect("inject");
        let result = prepare(&pager).and_then(|()| pager.commit_write_batch().map_err(Into::into));
        assert!(result.is_err(), "failure {fail_at}");
        pager.fence_write_batch();
        let durable = pager
            .with_file(|file| file.durable.borrow().clone())
            .expect("crash image");
        drop(pager);
        let recovered = open(Some(durable));
        let values: Vec<_> = (FIRST_ALLOCATABLE_PAGE..FIRST_ALLOCATABLE_PAGE + 4)
            .map(|id| recovered.read_page(id).expect("recover page").as_bytes()[128])
            .collect();
        assert!(
            values == [1, 1, 1, 1] || values == [2, 2, 2, 2],
            "failure {fail_at}: {values:?}"
        );
    }
}

#[test]
fn oversized_preparation_never_publishes_a_prefix() {
    let pager = open(None);
    let end = FIRST_ALLOCATABLE_PAGE + 65;
    for id in FIRST_ALLOCATABLE_PAGE..end {
        pager.allocate_page(PageType::BTreeLeaf).expect("allocate");
        pager.write_page(id, page(id, 1)).expect("seed");
    }
    pager.sync().expect("baseline");
    pager.begin_write_batch().expect("begin");
    let result = (|| -> Result<(), reddb_engine::storage::pager::PagerError> {
        for id in FIRST_ALLOCATABLE_PAGE..end {
            pager.write_page(id, page(id, 2))?;
        }
        pager.commit_write_batch()
    })();
    assert!(result.is_err());
    pager.fence_write_batch();
    let durable = pager
        .with_file(|file| file.durable.borrow().clone())
        .expect("crash image");
    drop(pager);
    let recovered = open(Some(durable));
    for id in FIRST_ALLOCATABLE_PAGE..end {
        assert_eq!(
            recovered.read_page(id).expect("original home").as_bytes()[128],
            1
        );
    }
}
