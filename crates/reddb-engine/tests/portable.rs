#[path = "support/codec_corpus.rs"]
mod codec_corpus;

#[path = "support/portable.rs"]
mod portable;

#[path = "support/storage.rs"]
mod storage;

#[path = "support/pager.rs"]
mod pager;

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

#[test]
fn pager_multiframe_checkpoint_io_and_crash_recovery() {
    pager::pager_multiframe_checkpoint_io_and_crash_recovery();
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
