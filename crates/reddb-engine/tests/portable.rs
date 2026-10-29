#[path = "support/codec_corpus.rs"]
mod codec_corpus;

#[path = "support/portable.rs"]
mod portable;

#[path = "support/catalog.rs"]
mod catalog;

#[path = "support/enforcement.rs"]
mod enforcement;

#[path = "support/enforcement_restart.rs"]
mod enforcement_restart;

#[path = "support/record_filters.rs"]
mod record_filters;

#[path = "support/record_filters_restart.rs"]
mod record_filters_restart;

#[path = "support/record_ordering.rs"]
mod record_ordering;

#[path = "support/record_projection.rs"]
mod record_projection;

#[path = "support/numeric_scalars.rs"]
mod numeric_scalars;

#[test]
fn portable_numeric_scalar_conformance() {
    for (name, run) in numeric_scalars::CASES {
        eprintln!("{name}");
        run();
    }
}

#[test]
fn portable_record_projection_conformance() {
    for (name, run) in record_projection::CASES {
        eprintln!("{name}");
        run();
    }
}

#[test]
fn portable_record_ordering_conformance() {
    for (name, run) in record_ordering::CASES {
        eprintln!("{name}");
        run();
    }
}

#[test]
fn portable_record_filter_conformance() {
    for (name, run) in record_filters::CASES {
        eprintln!("{name}");
        run();
    }
}

#[test]
fn portable_contract_enforcement_conformance() {
    for (name, run) in enforcement::CASES {
        eprintln!("{name}");
        run();
    }
}

#[test]
fn portable_catalog_conformance() {
    for (name, run) in catalog::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/storage.rs"]
mod storage;

#[path = "support/pager.rs"]
mod pager;

#[test]
fn paged_layout_version_is_independent_of_current_writer_version() {
    use reddb_engine::storage::pager::{NoWal, Pager, PagerConfig, PagerError};
    use reddb_file::{PAGED_SUPERBLOCK_SLOT_COUNT, PAGED_SUPERBLOCK_SLOT_SIZE};

    let open = |file, create| {
        Pager::open_owned(
            file,
            "version-test.rdb".into(),
            PagerConfig::default(),
            NoWal,
            create,
        )
    };
    let initial = open(pager::MemoryFile::default(), true).expect("create fixture");
    let bytes = initial
        .with_file(|file| file.durable.borrow().clone())
        .unwrap();
    drop(initial);

    let with_version = |version| {
        let mut image = bytes.clone();
        for copy in 0..PAGED_SUPERBLOCK_SLOT_COUNT {
            let offset = copy * PAGED_SUPERBLOCK_SLOT_SIZE;
            let slot = &mut image[offset..offset + PAGED_SUPERBLOCK_SLOT_SIZE];
            let generation = reddb_file::paged_superblock_slot_generation(slot, copy).unwrap();
            reddb_file::set_database_header_version(slot, version).unwrap();
            reddb_file::seal_paged_superblock_slot(slot, copy, generation).unwrap();
        }
        pager::MemoryFile::from_bytes(image)
    };
    let supported = open(with_version(0x0001_0001), false)
        .expect("the original phase-3 DWB layout must stay readable");
    assert_eq!(supported.header().unwrap().version, 0x0001_0001);
    assert!(matches!(
        open(with_version(0x0001_0000), false),
        Err(PagerError::LegacyPagedStore { .. })
    ));
    assert!(matches!(
        open(with_version(reddb_file::PAGE_FILE_VERSION + 1), false),
        Err(PagerError::InvalidDatabase(_))
    ));
}

#[test]
fn portable_execution_conformance() {
    for (name, run) in portable::CASES {
        eprintln!("{name}");
        run();
    }
}

#[test]
fn portable_storage_conformance() {
    for (name, run) in storage::CASES {
        eprintln!("{name}");
        run();
    }
}

#[test]
fn portable_pager_io_conformance() {
    for (name, run) in pager::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/transaction.rs"]
mod transaction;

#[test]
fn portable_transaction_conformance() {
    for (name, run) in transaction::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/entity.rs"]
mod entity;

#[test]
fn portable_entity_read_conformance() {
    for (name, run) in entity::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/entity_codec.rs"]
mod entity_codec;

#[test]
fn portable_entity_codec_conformance() {
    for (name, run) in entity_codec::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/record_corpus.rs"]
mod record_corpus;
#[path = "support/records.rs"]
mod records;

#[test]
fn portable_record_materialization_conformance() {
    for (name, run) in records::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/segments.rs"]
mod segments;

#[test]
fn portable_resident_segment_conformance() {
    for (name, run) in segments::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/metadata_codec.rs"]
mod metadata_codec;
#[path = "support/metadata_corpus.rs"]
mod metadata_corpus;

#[test]
fn portable_metadata_codec_conformance() {
    for (name, run) in metadata_codec::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/snapshot.rs"]
mod snapshot;

#[test]
fn portable_snapshot_conformance() {
    for (name, run) in snapshot::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/manager.rs"]
mod manager;

#[test]
fn portable_collection_lifecycle_conformance() {
    for (name, run) in manager::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/read_indexes.rs"]
mod read_indexes;

#[test]
fn portable_resident_read_index_conformance() {
    for (name, run) in read_indexes::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/resident_store.rs"]
mod resident_store;

#[test]
fn portable_resident_store_conformance() {
    for (name, run) in resident_store::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/store_wal.rs"]
mod store_wal;

#[test]
fn portable_store_wal_replay_conformance() {
    for (name, run) in store_wal::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/resident_file.rs"]
mod resident_file;
#[path = "support/resident_file_restart.rs"]
mod resident_file_restart;

#[test]
fn portable_resident_file_conformance() {
    for (name, run) in resident_file::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/text_scalars.rs"]
mod text_scalars;

#[test]
fn portable_text_scalar_conformance() {
    for (name, run) in text_scalars::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/temporal_scalars.rs"]
mod temporal_scalars;

#[test]
fn portable_temporal_scalar_conformance() {
    for (name, run) in temporal_scalars::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/geographic.rs"]
mod geographic;

#[test]
fn portable_geographic_conformance() {
    for (name, run) in geographic::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/table_plan.rs"]
mod table_plan;

#[test]
fn portable_table_plan_conformance() {
    for (name, run) in table_plan::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/table_planning.rs"]
mod table_planning;

#[test]
fn portable_table_planning_conformance() {
    for (name, run) in table_planning::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/runtime_expressions.rs"]
mod runtime_expressions;

#[test]
fn portable_runtime_expression_conformance() {
    for (name, run) in runtime_expressions::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/value_functions.rs"]
mod value_functions;

#[test]
fn portable_value_function_conformance() {
    for (name, run) in value_functions::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/window_phase.rs"]
mod window_phase;

#[test]
fn portable_window_phase_conformance() {
    for (name, run) in window_phase::CASES {
        eprintln!("{name}");
        run();
    }
}

#[path = "support/aggregation.rs"]
mod aggregation;
#[test]
fn portable_aggregation_conformance() {
    for (name, run) in aggregation::CASES {
        eprintln!("{name}");
        run();
    }
}
