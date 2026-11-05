use reddb_file::{
    encode_main_wal_commit_batch_with_visitor, encode_main_wal_record_frame,
    encode_store_wal_action_frame, visit_store_wal_entity_records, MainWalRecordFrame,
    StoreWalActionFrame,
};

#[test]
fn retained_entity_offsets_preserve_the_existing_commit_format() {
    let frames = [
        StoreWalActionFrame::CreateCollection {
            name: "rows".into(),
        },
        StoreWalActionFrame::UpsertEntityRecord {
            collection: "rows".into(),
            record: vec![17; 8192],
        },
        StoreWalActionFrame::BulkUpsertEntityRecords {
            collection: "other".into(),
            records: vec![vec![29; 63], vec![31; 4097]],
        },
    ];
    let actions: Vec<_> = frames
        .iter()
        .map(|frame| encode_store_wal_action_frame(frame).unwrap())
        .collect();
    let mut retained = Vec::new();
    let encoded = encode_main_wal_commit_batch_with_visitor(19, &actions, 7, |base, action| {
        visit_store_wal_entity_records(action, |collection, offset, record| {
            assert_eq!(record.as_ptr(), action[offset..].as_ptr());
            retained.push((
                collection.to_owned(),
                base as usize + offset,
                record.to_vec(),
            ));
            Ok(())
        })
    })
    .unwrap();
    let expected =
        encode_main_wal_record_frame(&MainWalRecordFrame::TxCommitBatch { tx_id: 19, actions }, 7)
            .unwrap();
    assert_eq!(encoded, expected);
    assert_eq!(retained.len(), 3);
    for (_, offset, record) in retained {
        assert_eq!(&encoded[offset..offset + record.len()], record);
    }
}

#[test]
fn borrowed_entity_visitor_rejects_truncated_upserts() {
    for frame in [
        StoreWalActionFrame::UpsertEntityRecord {
            collection: "rows".into(),
            record: vec![3; 32],
        },
        StoreWalActionFrame::BulkUpsertEntityRecords {
            collection: "rows".into(),
            records: vec![vec![5; 17], vec![7; 23]],
        },
    ] {
        let encoded = encode_store_wal_action_frame(&frame).unwrap();
        for length in 0..encoded.len() {
            assert!(visit_store_wal_entity_records(&encoded[..length], |_, _, _| Ok(())).is_err());
        }
    }
}
