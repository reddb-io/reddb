//! Fast read-authority regression loop, shared by native and WASM conformance.
use super::pager::MemoryFile;
use reddb_engine::storage::{
    btree::BTree,
    collection_read::{CollectionReadChanges, CollectionReadView},
    entity::{EntityId, UnifiedEntity},
    metadata::{Metadata, MetadataValue},
    metadata_codec::serialize_entity_record,
    page::{Page, PageType},
    page_store::PageStore,
    paged_collection::{PagedCollectionReader, PagedEntityRecord},
    pager::{NoWal, Pager, PagerConfig, PagerError},
};
use reddb_types::Value;
use std::{cell::Cell, sync::Arc};

pub const CASES: &[(&str, fn())] = &[
    ("collection_read_overlay_and_schema", overlay_and_schema),
    (
        "collection_read_failure_is_not_absence",
        failure_is_not_absence,
    ),
    ("collection_read_early_stop", early_stop),
    ("collection_read_id_iteration", id_iteration),
    (
        "collection_read_visibility_and_cancellation",
        visibility_and_cancellation,
    ),
    (
        "collection_read_fold_visibility_and_failure",
        fold_visibility_and_failure,
    ),
    (
        "collection_bootstrap_is_complete_or_fails",
        bootstrap_is_complete_or_fails,
    ),
    (
        "collection_vacuum_plan_uses_complete_source",
        vacuum_plan_uses_complete_source,
    ),
];

struct ReadPages {
    pager: Pager<MemoryFile, NoWal>,
    fail: Cell<bool>,
    reads: Cell<usize>,
    fail_after: Cell<Option<usize>>,
}

impl PageStore for ReadPages {
    fn read_page(&self, id: u32) -> Result<Page, PagerError> {
        let reads = self.reads.get();
        self.reads.set(reads + 1);
        if self.fail.get() || self.fail_after.get().is_some_and(|n| reads >= n) {
            return Err(PagerError::Io(std::io::Error::other(
                "injected read failure",
            )));
        }
        self.pager.read_page(id)
    }
    fn write_page(&self, _: u32, _: Page) -> Result<(), PagerError> {
        Err(PagerError::ReadOnly)
    }
    fn allocate_page(&self, _: PageType) -> Result<Page, PagerError> {
        Err(PagerError::ReadOnly)
    }
    fn free_page(&self, _: u32) -> Result<(), PagerError> {
        Err(PagerError::ReadOnly)
    }
    fn page_count(&self) -> Result<u32, PagerError> {
        self.pager.page_count()
    }
}

fn record(id: u64, revision: i64) -> Arc<PagedEntityRecord> {
    let mut entity = UnifiedEntity::table_row(
        EntityId::new(id),
        "rows",
        id,
        vec![Value::UnsignedInteger(id), Value::Integer(revision)],
    );
    entity.xmin = 17;
    entity.xmax = 29;
    entity.sequence_id = id + 100;
    let mut metadata = Metadata::new();
    metadata.set("revision", MetadataValue::Int(revision));
    Arc::new(PagedEntityRecord {
        entity,
        metadata: Some(metadata),
    })
}

fn base() -> (Arc<ReadPages>, Arc<PagedCollectionReader<ReadPages>>) {
    base_from([10u64, 30, 50, 70].into_iter().map(|id| record(id, 1)))
}

fn base_from(
    records: impl IntoIterator<Item = Arc<PagedEntityRecord>>,
) -> (Arc<ReadPages>, Arc<PagedCollectionReader<ReadPages>>) {
    let config = PagerConfig {
        cache_size: 24,
        ..PagerConfig::default()
    };
    let pager = Arc::new(
        Pager::open_owned(
            MemoryFile::default(),
            "read-view.rdb".into(),
            config.clone(),
            NoWal,
            true,
        )
        .expect("fixture pager"),
    );
    let tree = BTree::new(pager.clone());
    for record in records {
        let bytes = serialize_entity_record(
            &record.entity,
            record.metadata.as_ref(),
            reddb_file::STORE_VERSION_CURRENT,
        );
        tree.insert(&record.entity.id.raw().to_be_bytes(), &bytes)
            .expect("base row");
    }
    let root = tree.root_page_id();
    pager.flush().expect("durable fixture");
    let bytes = pager
        .with_file(|file| file.durable.borrow().clone())
        .expect("durable bytes");
    drop(tree);
    drop(pager);
    let pager = Pager::open_owned(
        MemoryFile::from_bytes(bytes),
        "read-view.rdb".into(),
        PagerConfig {
            read_only: true,
            ..config
        },
        NoWal,
        false,
    )
    .expect("reopen fixture");
    let pages = Arc::new(ReadPages {
        pager,
        fail: Cell::new(false),
        reads: Cell::new(0),
        fail_after: Cell::new(None),
    });
    let source = Arc::new(
        PagedCollectionReader::new(
            Arc::new(BTree::with_root(pages.clone(), root)),
            "rows",
            reddb_file::STORE_VERSION_CURRENT,
        )
        .expect("framed source"),
    );
    (pages, source)
}

fn vacuum_plan_uses_complete_source() {
    fn version(id: u64, logical: u64, xmin: u64, xmax: u64) -> Arc<PagedEntityRecord> {
        let mut record = Arc::try_unwrap(record(id, 1)).ok().expect("unique record");
        record.entity.set_logical_id(EntityId::new(logical));
        record.entity.xmin = xmin;
        record.entity.xmax = xmax;
        Arc::new(record)
    }
    let (pages, base) = base_from([
        version(10, 1, 1, 3),
        version(30, 1, 3, 0),
        version(50, 2, 2, 7),
        version(70, 3, 4, 20),
    ]);
    let mut changes = CollectionReadChanges::new();
    changes.insert(EntityId::new(30), None);
    changes.insert(EntityId::new(80), Some(version(80, 1, 8, 0)));
    let view = CollectionReadView::paged(base.clone(), Arc::new(changes), None);
    let (stats, ids) = view.plan_mvcc_vacuum(10).expect("complete plan");
    assert_eq!(ids, [EntityId::new(10), EntityId::new(50)]);
    assert_eq!(stats.scanned_versions, 3);
    assert_eq!(stats.reclaimed_versions, 2);
    assert_eq!(stats.reclaimed_history_versions, 1);
    assert_eq!(stats.reclaimed_tombstones, 1);
    assert_eq!(stats.retained_tombstones, 1);

    pages.reads.set(0);
    view.visit_entities(|_| true).expect("measure first pass");
    let first_pass_reads = pages.reads.get();
    assert!(first_pass_reads > 0);
    pages.reads.set(0);
    let mut prefix = base.cursor().expect("prefix cursor");
    assert!(prefix.next().expect("prefix read").is_some());
    let prefix_reads = pages.reads.get();
    drop(prefix);
    pages.reads.set(0);
    pages.fail_after.set(Some(first_pass_reads + prefix_reads));
    assert!(
        view.plan_mvcc_vacuum(10).is_err(),
        "a failed second pass must not yield a partial deletion plan"
    );
    pages.fail_after.set(None);
}

fn bootstrap_is_complete_or_fails() {
    use reddb_engine::storage::entity::EntityKind;
    let mut low_id_high_row = Arc::try_unwrap(record(10, 1)).ok().expect("unique row");
    if let EntityKind::TableRow { row_id, .. } = &mut low_id_high_row.entity.kind {
        *row_id = 900;
    } else {
        panic!("table row");
    }
    let mut high_id_low_row = Arc::try_unwrap(record(70, 1)).ok().expect("unique row");
    if let EntityKind::TableRow { row_id, .. } = &mut high_id_low_row.entity.kind {
        *row_id = 3;
    } else {
        panic!("table row");
    }
    let vector = PagedEntityRecord {
        entity: UnifiedEntity::vector(EntityId::new(100), "rows", vec![1.0]),
        metadata: None,
    };
    let (pages, source) = base_from([
        Arc::new(low_id_high_row),
        Arc::new(high_id_low_row),
        Arc::new(vector),
    ]);
    let mut partial = source.bootstrap_cursor().expect("cursor");
    assert!(partial.next().expect("first row").is_some());
    assert!(
        partial.finish().is_err(),
        "partial counts must not be published"
    );

    let mut cursor = source.bootstrap_cursor().expect("cursor");
    let mut observed = Vec::new();
    while let Some(record) = cursor.next().expect("physical row") {
        observed.push((record.entity.id.raw(), record.entity.sequence_id));
    }
    let summary = cursor.finish().expect("only EOF admits a summary");
    assert_eq!(
        summary.physical_count, 3,
        "historical rows remain physical records"
    );
    assert_eq!(summary.max_entity_id, Some(EntityId::new(100)));
    assert_eq!(
        summary.max_row_id,
        Some(900),
        "row IDs are independent of physical IDs"
    );
    assert_eq!(
        observed.iter().map(|v| v.0).collect::<Vec<_>>(),
        [10, 70, 100]
    );
    assert_eq!(
        (observed[0].1, observed[1].1),
        (110, 170),
        "stored sequence stamps remain unchanged"
    );

    let mut failed = source.bootstrap_cursor().expect("cursor");
    assert!(failed.next().expect("prefix").is_some());
    pages.fail.set(true);
    assert!(failed.next().is_err());
    assert!(
        failed.finish().is_err(),
        "read failure cannot yield a partial summary"
    );
    pages.fail.set(false);
}

fn overlay_and_schema() {
    let (_, base) = base();
    let mut changes = CollectionReadChanges::new();
    for id in [5, 20, 30, 90] {
        changes.insert(EntityId::new(id), Some(record(id, 2)));
    }
    changes.insert(EntityId::new(50), None);
    let schema = Arc::new(vec!["id".to_string(), "revision".to_string()]);
    let view = CollectionReadView::paged(base, Arc::new(changes), Some(schema.clone()));
    let mut ids = Vec::new();
    assert!(view
        .visit_records(|entity, metadata| {
            ids.push(entity.id.raw());
            let revision = if [10, 70].contains(&entity.id.raw()) {
                1
            } else {
                2
            };
            assert_eq!(
                metadata.expect("metadata").get("revision"),
                Some(&MetadataValue::Int(revision))
            );
            assert_eq!(
                (entity.xmin, entity.xmax, entity.sequence_id),
                (17, 29, entity.id.raw() + 100)
            );
            true
        })
        .expect("complete scan"));
    assert_eq!(ids, [5, 10, 20, 30, 70, 90]);
    assert!(view
        .get(EntityId::new(50))
        .expect("explicit delete")
        .is_none());
    let updated = view
        .get(EntityId::new(30))
        .expect("updated row")
        .expect("present");
    assert_eq!(
        updated.data.as_row().expect("row").columns[1],
        Value::Integer(2)
    );
    assert_eq!(
        updated.data.as_row().expect("row").schema.as_ref(),
        Some(&schema)
    );
    for offset in [0, 3, 5, 6, usize::MAX] {
        let (total, page) = view.page_in_id_order(offset, 1).expect("physical page");
        assert_eq!(total, ids.len());
        assert_eq!(
            page.first().map(|row| row.id.raw()),
            ids.get(offset).copied()
        );
    }
}

fn failure_is_not_absence() {
    let (pages, base) = base();
    let mut changes = CollectionReadChanges::new();
    changes.insert(EntityId::new(50), None);
    changes.insert(EntityId::new(30), Some(record(30, 2)));
    let view = CollectionReadView::paged(base, Arc::new(changes), None);
    pages.fail.set(true);
    assert!(view.get(EntityId::new(10)).is_err());
    assert!(view.page_in_id_order(0, 1).is_err());
    assert!(view
        .get(EntityId::new(50))
        .expect("mask needs no source")
        .is_none());
    assert!(view
        .get(EntityId::new(30))
        .expect("replacement needs no source")
        .is_some());
    pages.fail.set(false);
    assert!(view
        .get(EntityId::new(10))
        .expect("new read after fault")
        .is_some());
}

fn early_stop() {
    let (pages, base) = base();
    let view = CollectionReadView::paged(base, Arc::new(CollectionReadChanges::new()), None);
    let mut ids = Vec::new();
    assert!(!view
        .visit_entities(|entity| {
            ids.push(entity.id.raw());
            pages.fail.set(true);
            false
        })
        .expect("no source read after stopping"));
    assert_eq!(ids, [10]);
}

fn fold_visibility_and_failure() {
    use reddb_engine::storage::manager::SegmentManager;
    let (pages, base) = base();
    let mut changes = CollectionReadChanges::new();
    let manager = Arc::new(SegmentManager::new("rows"));
    for id in [30, 90] {
        let mut live = Arc::try_unwrap(record(id, 2))
            .ok()
            .expect("unique live row");
        live.entity.xmax = 0;
        manager
            .restore_snapshot_entity(live.entity.clone())
            .expect("resident row");
        changes.insert(EntityId::new(id), Some(Arc::new(live)));
    }
    // The raw source's other records remain superseded and must be hidden.
    manager
        .restore_snapshot_entity(record(10, 1).entity.clone())
        .expect("resident historical row");
    changes.insert(EntityId::new(50), None);
    let paged = CollectionReadView::paged(base, Arc::new(changes), None);
    let resident = CollectionReadView::<ReadPages>::resident(manager);
    for view in [&paged, &resident] {
        let mut ids = view
            .try_fold_parallel(
                None,
                Vec::<u64>::new,
                |mut ids, entity| {
                    ids.push(entity.id.raw());
                    ids
                },
                |mut a, b| {
                    a.extend(b);
                    a
                },
            )
            .expect("complete visible fold");
        ids.sort_unstable();
        assert_eq!(ids, [30, 90]);
    }
    pages.fail.set(true);
    assert!(
        paged
            .try_fold_parallel(
                None,
                || 0u64,
                |sum, entity| sum + entity.id.raw(),
                |a, b| a + b
            )
            .is_err(),
        "a cold fold cannot hide a source failure as a complete aggregate"
    );
}

fn id_iteration() {
    let (pages, base) = base();
    let mut changes = CollectionReadChanges::new();
    changes.insert(EntityId::new(50), None);
    changes.insert(EntityId::new(30), Some(record(30, 2)));
    let view = CollectionReadView::paged(base, Arc::new(changes), None);
    let ids = [70, 9, 30, 50, 10, 30].map(EntityId::new);
    let rows: Vec<_> = view
        .read_ids(&ids)
        .map(|row| row.expect("fallible lookup").map(|row| row.id.raw()))
        .collect();
    assert_eq!(rows, [Some(70), None, Some(30), None, Some(10), Some(30)]);
    pages.fail.set(true);
    let mut pending = view.read_ids(&ids); // Construction must be lazy for cold data.
    assert!(pending.next().expect("read attempt").is_err());
    assert!(pending.next().is_none(), "a failed ID iterator is fenced");
    assert!(!view
        .visit_ids(&ids, || false, |_, _| panic!("cancelled visitor"))
        .expect("cancellation before I/O"));
    pages.fail.set(false);
    let mut seen = Vec::new();
    assert!(!view
        .visit_ids(
            &ids,
            || true,
            |index, entity| {
                seen.push((index, entity.id.raw()));
                pages.fail.set(true);
                false
            }
        )
        .expect("stop before another ID lookup"));
    assert_eq!(seen, [(0, 70)]);
}

fn visibility_and_cancellation() {
    use reddb_engine::storage::entity::{EntityData, RowData};
    use reddb_engine::storage::visibility::{MODERATION_STATUS_FIELD, MODERATION_STATUS_PENDING};
    use reddb_engine::transaction::{
        read::SnapshotContext,
        snapshot::{Snapshot, SnapshotManager},
    };
    let (pages, base) = base();
    let mut changes = CollectionReadChanges::new();
    let mut hidden = Arc::try_unwrap(record(20, 2))
        .ok()
        .expect("unique record owner");
    hidden.entity.data = EntityData::Row(RowData::with_names(
        vec![Value::text(MODERATION_STATUS_PENDING)],
        vec![MODERATION_STATUS_FIELD.into()],
    ));
    changes.insert(EntityId::new(20), Some(Arc::new(hidden)));
    let view = CollectionReadView::paged(base, Arc::new(changes), None);
    pages.fail.set(true);
    assert!(!view
        .scan_for_each(None, || false, |_| panic!("no I/O after cancellation"))
        .expect("cancelled before constructing cursor"));
    pages.fail.set(false);
    let mut work = 0;
    let mut count = 0;
    assert!(view
        .scan_for_each(
            None,
            || {
                work += 1;
                true
            },
            |_| {
                count += 1;
                true
            }
        )
        .expect("physical work with hidden versions"));
    assert_eq!(
        count, 0,
        "superseded versions stay hidden without an explicit snapshot"
    );
    assert!(
        work >= 4,
        "hidden source records still check work/cancellation"
    );
    let manager = Arc::new(SnapshotManager::new());
    manager.observe_committed_xid(50);
    let snapshot = SnapshotContext {
        snapshot: Snapshot {
            xid: 25,
            in_progress: Default::default(),
        },
        manager: manager.clone(),
        own_xids: Default::default(),
        requires_index_fallback: true,
        serializable_reader: None,
    };
    let mut visible = Vec::new();
    assert!(view
        .scan_for_each(
            Some(&snapshot),
            || true,
            |entity| {
                visible.push(entity.id.raw());
                true
            }
        )
        .expect("explicit old snapshot"));
    assert_eq!(
        visible,
        [10, 30, 50, 70],
        "moderation stays hidden while the old snapshot sees history"
    );
    manager.rollback(17);
    assert!(view
        .scan_for_each(
            Some(&snapshot),
            || true,
            |_| panic!("aborted creator is hidden")
        )
        .expect("aborted outcomes checked after snapshot capture"));
}
